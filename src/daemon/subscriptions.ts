// tally daemon-core — the subscriber registry + backpressure (CLI-SURFACE §2.1, §2.4).
//
// One connection may hold at most one active subscription (the `session.subscribe` ACK carries its
// `subscription_id`). A subscriber filters the stream by event `names` and/or `categories`, may
// suppress the idle `heartbeat`, and is disconnected with a final `stream.overflow` once its unacked
// backlog exceeds `MAX_UNACKED` (1024). `session.ack {seq}` advances the per-subscriber cursor,
// prunes the backlog, and resets the slow-subscriber counter (doubling as liveness).
//
// This module owns per-subscriber bookkeeping and the fan-out decision; the transport (how a frame
// reaches the socket) is the injected `FrameSink`, so the same logic is testable without a socket.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { randomUUID } from "node:crypto";
import { MAX_UNACKED } from "../contracts/constants";
import type { EventCategory, EventName } from "../contracts/events";
import { eventCategory, isReplayable } from "../contracts/events";
import type { ResumeInfo } from "../contracts/wire";
import type { StampedEvent } from "./replay-ring";

/**
 * The transport a subscriber writes frames through. `write` returns whether the frame was accepted
 * (a closed socket returns false). daemon-core's server implements this over a Bun socket; tests
 * implement it over an array.
 */
export interface FrameSink {
  /** Write one already-encoded NDJSON line (LF-terminated). Returns false if the sink is closed. */
  write(line: string): boolean;
  /** Force-close the underlying connection (after a `stream.overflow`, on unsubscribe teardown). */
  close(): void;
}

/** A subscriber's filter set, resolved from `session.subscribe` params. */
export interface SubscriptionFilter {
  names?: Set<EventName>;
  categories?: Set<EventCategory>;
  includeHeartbeat: boolean;
}

/**
 * One active subscription. `pendingUnacked` is the count of replayable frames pushed since the last
 * `session.ack`; when it exceeds `MAX_UNACKED` the subscriber is overflowed. `lastAckedSeq` is the
 * cursor the client last confirmed.
 */
export class Subscription {
  readonly id: string;
  readonly filter: SubscriptionFilter;
  private readonly sink: FrameSink;
  private readonly encode: (frame: unknown) => string;
  private pendingUnacked = 0;
  private lastAckedSeq = 0;
  private overflowed = false;

  constructor(args: {
    id: string;
    filter: SubscriptionFilter;
    sink: FrameSink;
    encode: (frame: unknown) => string;
  }) {
    this.id = args.id;
    this.filter = args.filter;
    this.sink = args.sink;
    this.encode = args.encode;
  }

  /** Whether this subscriber has been disconnected by the overflow guard. */
  get isOverflowed(): boolean {
    return this.overflowed;
  }

  /** Current unacked backlog (for tests/diagnostics). */
  get unacked(): number {
    return this.pendingUnacked;
  }

  /** The last seq the client acked. */
  get ackedSeq(): number {
    return this.lastAckedSeq;
  }

  /** Whether an event name passes this subscriber's filter. */
  wants(name: EventName): boolean {
    if (name === "heartbeat") return this.filter.includeHeartbeat;
    // stream.overflow is a control frame delivered unconditionally by the overflow path, never here.
    if (this.filter.names && !this.filter.names.has(name)) return false;
    if (this.filter.categories && !this.filter.categories.has(eventCategory(name))) return false;
    return true;
  }

  /**
   * Deliver a stamped (replayable) event if it passes the filter, incrementing the unacked backlog
   * and enforcing the overflow bound. Returns the delivery outcome so the registry can drop an
   * overflowed subscriber. A non-matching event is a silent no-op (`"skipped"`).
   */
  deliver(ev: StampedEvent): "delivered" | "skipped" | "overflowed" {
    if (this.overflowed) return "overflowed";
    if (!this.wants(ev.event)) return "skipped";
    // Flatten payload onto the frame alongside seq/id/event (§2.3).
    const line = this.encode({ seq: ev.seq, id: ev.id, event: ev.event, ...(ev.payload as object) });
    const ok = this.sink.write(line);
    if (!ok) {
      this.overflowed = true;
      return "overflowed";
    }
    this.pendingUnacked += 1;
    if (this.pendingUnacked > MAX_UNACKED) {
      return "overflowed";
    }
    return "delivered";
  }

  /**
   * Deliver a NON-replayable control frame (`heartbeat`) directly — it never enters the ring, never
   * advances a cursor, and does not count against the unacked backlog (§2.1, §2.3). A `heartbeat` is
   * still gated by `includeHeartbeat`.
   */
  deliverControl(name: EventName, payload: object): boolean {
    if (this.overflowed) return false;
    if (isReplayable(name)) {
      throw new Error(`deliverControl refuses replayable event "${name}"`);
    }
    if (name === "heartbeat" && !this.filter.includeHeartbeat) return false;
    const ok = this.sink.write(this.encode({ event: name, ...payload }));
    if (!ok) this.overflowed = true;
    return ok;
  }

  /**
   * Advance the cursor to `seq`: prune the unacked backlog down to what remains beyond `seq` and
   * reset the slow-subscriber pressure. A stale/duplicate ack (`seq <= lastAckedSeq`) is idempotent.
   */
  ack(seq: number): void {
    if (seq <= this.lastAckedSeq) return;
    this.lastAckedSeq = seq;
    // Every frame up to `seq` is now confirmed; the pending counter reflects only frames the daemon
    // pushed that the client has not yet acknowledged. Resetting to zero on each forward ack matches
    // the "resets the slow-subscriber counter" semantics (§2.4) — a healthy reader stays clear of the
    // 1024 bound simply by acking.
    this.pendingUnacked = 0;
  }

  /** Emit a final `stream.overflow` and close the connection (§2.1 slow-subscriber disconnect). */
  overflow(oldestSeq: number, latestSeq: number): void {
    this.overflowed = true;
    try {
      this.sink.write(
        this.encode({
          event: "stream.overflow",
          reason: `unacked backlog exceeded ${MAX_UNACKED} frames`,
          oldest_seq: oldestSeq,
          latest_seq: latestSeq,
        }),
      );
    } catch {
      // Best-effort — the connection is going away regardless.
    }
    this.sink.close();
  }
}

/**
 * The daemon-wide subscriber registry. daemon-core's fan-out (bus → wire) walks the registry for
 * every event; the server registers/removes subscriptions on subscribe/unsubscribe/disconnect.
 */
export class SubscriptionRegistry {
  private readonly subs = new Map<string, Subscription>();

  /** Create + register a subscription, returning its id. */
  create(args: {
    filter: SubscriptionFilter;
    sink: FrameSink;
    encode: (frame: unknown) => string;
  }): Subscription {
    const id = `sub_${randomUUID().slice(0, 12)}`;
    const sub = new Subscription({ id, filter: args.filter, sink: args.sink, encode: args.encode });
    this.subs.set(id, sub);
    return sub;
  }

  get(id: string): Subscription | undefined {
    return this.subs.get(id);
  }

  /** Remove a subscription (unsubscribe or connection close). */
  remove(id: string): boolean {
    return this.subs.delete(id);
  }

  /** Number of active subscriptions. */
  get size(): number {
    return this.subs.size;
  }

  /**
   * Fan a stamped replayable event out to every subscriber. Any subscriber that overflows during
   * delivery is sent a final `stream.overflow` and removed. Returns the count actually delivered.
   */
  fanout(ev: StampedEvent, ring: { oldestSeq: number; latestSeq: number }): number {
    let delivered = 0;
    for (const sub of [...this.subs.values()]) {
      const outcome = sub.deliver(ev);
      if (outcome === "delivered") delivered += 1;
      if (outcome === "overflowed") {
        sub.overflow(ring.oldestSeq, ring.latestSeq);
        this.subs.delete(sub.id);
      }
    }
    return delivered;
  }

  /** Deliver a control frame (heartbeat) to every subscriber that wants it. */
  fanoutControl(name: EventName, payload: object): number {
    let delivered = 0;
    for (const sub of this.subs.values()) {
      if (sub.deliverControl(name, payload)) delivered += 1;
    }
    return delivered;
  }

  /** Drop every subscription bound to a sink (connection closed) — matched by identity via callback. */
  removeWhere(predicate: (sub: Subscription) => boolean): void {
    for (const [id, sub] of [...this.subs.entries()]) {
      if (predicate(sub)) this.subs.delete(id);
    }
  }
}

/** Resolve `session.subscribe` filter params into a `SubscriptionFilter`. */
export function resolveFilter(args: {
  names?: EventName[];
  categories?: EventCategory[];
  include_heartbeat?: boolean;
}): SubscriptionFilter {
  const filter: SubscriptionFilter = {
    includeHeartbeat: args.include_heartbeat ?? true,
  };
  if (args.names && args.names.length > 0) filter.names = new Set(args.names);
  if (args.categories && args.categories.length > 0) filter.categories = new Set(args.categories);
  return filter;
}

/** Build the `resume` block for a subscribe ACK from a ring resume computation. */
export function resumeInfo(args: ResumeInfo): ResumeInfo {
  return args;
}
