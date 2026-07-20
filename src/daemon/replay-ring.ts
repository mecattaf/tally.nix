// tally daemon-core — the bounded replay ring (CLI-SURFACE §2.1, byte-for-byte).
//
// Every REPLAYABLE event is stamped with a monotonic `seq` (per `lease_epoch`) and a stable event
// uuid `id`, then retained in a bounded in-memory ring of exactly `REPLAY_RING` (4096) events —
// tally's own memory budget. A subscriber that resumes with a `from_seq` older than the ring's oldest
// retained seq gets `gap:true` and must re-snapshot. Control frames (`heartbeat`, `stream.overflow`)
// are NOT replayable — they carry no `seq`/`id` and never enter the ring.
//
// `seq` starts at 1 within an epoch (0 = "no events yet", the value a fresh snapshot reports). The
// ring never mutates a stamped event; it appends and evicts the oldest once full.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { randomUUID } from "node:crypto";
import { REPLAY_RING } from "../contracts/constants";
import type { EventName, EventPayloadMap } from "../contracts/events";
import { isReplayable } from "../contracts/events";

/**
 * A stamped, replayable event as it sits in the ring and goes to the wire. The payload fields are
 * spread flat onto the frame alongside `seq`/`id`/`event` at encode time (§2.3).
 */
export interface StampedEvent<N extends EventName = EventName> {
  seq: number;
  /** The stable event uuid — idempotent dedupe across a resume overlap (§2.1). */
  id: string;
  event: N;
  payload: EventPayloadMap[N];
}

/** The resume block computed for a subscriber's requested `from_seq` (CLI-SURFACE §2.1, §2.4). */
export interface ResumeComputation {
  after_seq: number;
  oldest_seq: number;
  latest_seq: number;
  next_seq: number;
  gap: boolean;
  /** The events to replay to the subscriber (those with `seq > after_seq` still in the ring). */
  replay: StampedEvent[];
}

/**
 * The bounded replay ring. Assigns `seq`/`id` to replayable events, retains the last `REPLAY_RING` of
 * them, and computes the resume window for a reconnecting subscriber.
 */
export class ReplayRing {
  private readonly capacity: number;
  private readonly ring: StampedEvent[] = [];
  /** The last assigned seq; 0 before any event (a fresh snapshot reports seq 0). */
  private lastSeq = 0;

  constructor(capacity: number = REPLAY_RING) {
    this.capacity = capacity;
  }

  /** The latest assigned seq (the snapshot's `seq`, the heartbeat's `latest_seq`). */
  get latestSeq(): number {
    return this.lastSeq;
  }

  /** The oldest seq still retained (0 when the ring is empty). */
  get oldestSeq(): number {
    return this.ring.length === 0 ? 0 : this.ring[0]!.seq;
  }

  /** The seq the NEXT replayable event will receive. */
  get nextSeq(): number {
    return this.lastSeq + 1;
  }

  /** Number of events currently retained. */
  get size(): number {
    return this.ring.length;
  }

  /**
   * Stamp a replayable event with the next `seq` and a fresh uuid, append it (evicting the oldest
   * when full), and return the stamped frame for fan-out. Throws if handed a non-replayable name
   * (control frames must not enter the ring — that is a caller bug).
   */
  append<N extends EventName>(event: N, payload: EventPayloadMap[N]): StampedEvent<N> {
    if (!isReplayable(event)) {
      throw new Error(`replay ring refuses non-replayable event "${event}"`);
    }
    const stamped: StampedEvent<N> = {
      seq: ++this.lastSeq,
      id: randomUUID(),
      event,
      payload,
    };
    this.ring.push(stamped);
    if (this.ring.length > this.capacity) {
      this.ring.shift();
    }
    return stamped;
  }

  /**
   * Compute the resume window for a subscriber resuming after `fromSeq` (the last seq it holds).
   * `undefined` means "subscribe live from `next_seq`" — no replay, no gap. A `from_seq` older than
   * the oldest retained seq (and older than latest) sets `gap:true`: the client MUST re-snapshot.
   */
  resume(fromSeq: number | undefined): ResumeComputation {
    const latest = this.lastSeq;
    const oldest = this.oldestSeq;
    if (fromSeq === undefined) {
      // Live subscription from the tip: no replay, no gap.
      return {
        after_seq: latest,
        oldest_seq: oldest,
        latest_seq: latest,
        next_seq: latest + 1,
        gap: false,
        replay: [],
      };
    }
    // A gap exists when the requested resume point precedes everything the ring still holds AND the
    // ring is non-empty and there is genuinely-missing history (fromSeq < oldest-1). When fromSeq is
    // already >= latest there is nothing to replay and no gap.
    let gap = false;
    let replay: StampedEvent[] = [];
    if (fromSeq >= latest) {
      // Caller is caught up (or ahead, after an epoch reset); nothing to replay.
      replay = [];
      gap = false;
    } else if (this.ring.length === 0) {
      // No retained history but the caller trails the tip ⇒ it cannot be served ⇒ gap.
      gap = fromSeq < latest;
      replay = [];
    } else if (fromSeq < oldest - 1) {
      // The requested continuation point fell out of the ring ⇒ gap.
      gap = true;
      replay = [];
    } else {
      // Serve every retained event strictly after `fromSeq`.
      replay = this.ring.filter((e) => e.seq > fromSeq);
      gap = false;
    }
    return {
      after_seq: fromSeq,
      oldest_seq: oldest,
      latest_seq: latest,
      next_seq: latest + 1,
      gap,
      replay,
    };
  }

  /** All retained events (oldest-first) — for tests/diagnostics. */
  snapshotRing(): readonly StampedEvent[] {
    return this.ring.slice();
  }
}
