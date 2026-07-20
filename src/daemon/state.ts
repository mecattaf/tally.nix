// tally daemon-core — the daemon runtime state (CLI-SURFACE §2.2, §2.5; IMPLEMENTATION-PLAN M1.1).
//
// The shared spine every daemon-core piece reads: the resolved `lease_epoch`, the replay ring, the
// subscriber registry, the in-daemon `Bus`, and the two cross-layer provider seams the session-model
// and detector register into daemon-core WITHOUT importing it — `SnapshotProvider` (snapshot
// assembly) and `WaitScrapeProvider` (the `session.wait pane_output` read). daemon-core owns the
// TRANSPORT; the model owns the DATA. Until a provider is registered, daemon-core serves a well-formed
// empty snapshot and rejects pane_output waits with an honest `unsupported` — never a stub lie.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { PathEnv } from "../contracts/paths";
import type { TallyConfig } from "../contracts/config";
import { defaultConfig } from "../contracts/config";
import type { SnapshotProvider, Snapshot } from "../contracts/snapshot";
import { emptySnapshot } from "../contracts/snapshot";
import type { WaitScrapeProvider, JobBarrierProvider } from "../contracts/bus";
import type { Bus, BusEvent, Unsubscribe } from "../contracts/bus";
import type { EventName, EventPayloadMap } from "../contracts/events";
import type { Clock } from "../contracts/exec";
import { systemClock } from "../contracts/exec";
import { TallyError } from "../contracts/errors";
import { ReplayRing } from "./replay-ring";
import { SubscriptionRegistry } from "./subscriptions";
import { bumpEpoch, type ResolvedEpoch } from "./epoch";

/**
 * A minimal synchronous in-daemon bus (IMPLEMENTATION-PLAN §3 Seams `Bus`). daemon-core owns the
 * canonical instance so mounted modules (detector, jobs, session-model) publish through it and the
 * wire fan-out subscribes via `onAny`. Fan-out is synchronous; a throwing handler is isolated so one
 * bad subscriber cannot break the emit.
 */
export class DaemonBus implements Bus {
  private readonly named = new Map<EventName, Set<(payload: unknown) => void>>();
  private readonly any = new Set<(e: BusEvent) => void>();

  emit<N extends EventName>(event: N, payload: EventPayloadMap[N]): void {
    const set = this.named.get(event);
    if (set) {
      for (const h of [...set]) {
        try {
          h(payload);
        } catch (err) {
          process.stderr.write(`tally[bus]: handler for "${event}" threw: ${err instanceof Error ? err.message : String(err)}\n`);
        }
      }
    }
    if (this.any.size > 0) {
      const e: BusEvent<N> = { event, payload };
      for (const h of [...this.any]) {
        try {
          h(e as BusEvent);
        } catch (err) {
          process.stderr.write(`tally[bus]: onAny handler threw: ${err instanceof Error ? err.message : String(err)}\n`);
        }
      }
    }
  }

  on<N extends EventName>(event: N, handler: (payload: EventPayloadMap[N]) => void): Unsubscribe {
    let set = this.named.get(event);
    if (!set) {
      set = new Set();
      this.named.set(event, set);
    }
    const h = handler as (payload: unknown) => void;
    set.add(h);
    return () => {
      set!.delete(h);
    };
  }

  onAny(handler: (e: BusEvent) => void): Unsubscribe {
    this.any.add(handler);
    return () => {
      this.any.delete(handler);
    };
  }
}

/** The daemon's boot options. `env` roots every XDG path; `clock` is injectable for tests. */
export interface DaemonStateOptions {
  env: PathEnv;
  config?: TallyConfig;
  clock?: Clock;
  /** The pls lease generation, the PRIMARY lease-epoch source when available (PS#21). */
  plsGeneration?: number;
}

/**
 * The daemon runtime state. Holds the epoch, ring, subscribers, bus, config, and the two provider
 * seams. Created once per `tally daemon run`; a restart produces a NEW instance with a strictly
 * greater epoch (voiding every client cursor, §2.5).
 */
export class DaemonState {
  readonly env: PathEnv;
  readonly config: TallyConfig;
  readonly clock: Clock;
  readonly bus: DaemonBus;
  readonly ring: ReplayRing;
  readonly subscriptions: SubscriptionRegistry;
  readonly epoch: number;
  readonly epochSource: ResolvedEpoch["source"];

  private snapshotProvider: SnapshotProvider | null = null;
  private waitScrapeProvider: WaitScrapeProvider | null = null;
  private jobBarrierProvider: JobBarrierProvider | null = null;

  constructor(opts: DaemonStateOptions) {
    this.env = opts.env;
    this.config = opts.config ?? defaultConfig();
    this.clock = opts.clock ?? systemClock;
    this.bus = new DaemonBus();
    this.ring = new ReplayRing();
    this.subscriptions = new SubscriptionRegistry();
    const resolved = bumpEpoch(opts.env, opts.plsGeneration);
    this.epoch = resolved.epoch;
    this.epochSource = resolved.source;
  }

  /** The informational binary semver surfaced in the snapshot's `daemon_version`. */
  get daemonVersion(): string {
    return this.config.daemonVersion;
  }

  /**
   * Register the session-model's `SnapshotProvider` (IMPLEMENTATION-PLAN M1.1 — assembly delegates to
   * the model; daemon-core owns transport only). Idempotent-replace.
   */
  registerSnapshotProvider(provider: SnapshotProvider): void {
    this.snapshotProvider = provider;
  }

  /** Register the detector's `WaitScrapeProvider` for `session.wait pane_output` reads (risk 9). */
  registerWaitScrapeProvider(provider: WaitScrapeProvider): void {
    this.waitScrapeProvider = provider;
  }

  /** The registered wait-scrape provider, or null if the detector has not mounted yet. */
  get waitScrape(): WaitScrapeProvider | null {
    return this.waitScrapeProvider;
  }

  /** Register the jobs engine's `JobBarrierProvider` for `session.wait {subject:job}` (drains terminals). */
  registerJobBarrierProvider(provider: JobBarrierProvider): void {
    this.jobBarrierProvider = provider;
  }

  /** The registered job-barrier provider, or null if the jobs engine has not mounted yet. */
  get jobBarrier(): JobBarrierProvider | null {
    return this.jobBarrierProvider;
  }

  /**
   * Assemble the current `session.snapshot` frame. If the model registered a provider, its frame is
   * returned verbatim BUT with daemon-core's authoritative `lease_epoch`/`seq`/`ts`/protocol header
   * stamped in — daemon-core owns those fields (§2.2). Before any provider mounts, a well-formed empty
   * snapshot is served (the daemon answers the ping from boot).
   */
  assembleSnapshot(): Snapshot {
    const seq = this.ring.latestSeq;
    const ts = this.clock.nowIso();
    if (!this.snapshotProvider) {
      return emptySnapshot({
        daemon_version: this.daemonVersion,
        lease_epoch: this.epoch,
        seq,
        ts,
      });
    }
    const base = this.snapshotProvider.snapshot();
    // daemon-core owns the transport header; overwrite whatever the model put there so the fence and
    // cursor are always authoritative.
    return {
      ...base,
      protocol: base.protocol,
      protocol_version: base.protocol_version,
      daemon_version: this.daemonVersion,
      lease_epoch: this.epoch,
      seq,
      ts,
    };
  }

  /**
   * Publish a replayable event: stamp it in the ring and fan it out to subscribers. This is the ONE
   * path a wire event takes. Non-replayable control frames go through `emitControl`.
   */
  publish<N extends EventName>(event: N, payload: EventPayloadMap[N]): void {
    const stamped = this.ring.append(event, payload);
    this.subscriptions.fanout(stamped, {
      oldestSeq: this.ring.oldestSeq,
      latestSeq: this.ring.latestSeq,
    });
  }

  /** Fan a control frame (heartbeat) to subscribers; not replayable, not stamped. */
  emitControl(name: EventName, payload: object): void {
    this.subscriptions.fanoutControl(name, payload);
  }

  /**
   * Wire the in-daemon bus to the wire: every event a mounted module emits on the `Bus` is published
   * through the ring to subscribers. Call once at boot. Control-frame names on the bus (`heartbeat`,
   * `stream.overflow`) are ignored here — those originate in daemon-core itself, not modules.
   */
  wireBusToWire(): Unsubscribe {
    return this.bus.onAny((e) => {
      if (e.event === "heartbeat" || e.event === "stream.overflow") return;
      this.publish(e.event, e.payload as EventPayloadMap[typeof e.event]);
    });
  }
}

/** The error daemon-core returns for a `session.wait pane_output` when no detector has mounted. */
export function noWaitScrapeProvider(): TallyError {
  return new TallyError(
    "unsupported",
    "session.wait pane_output requires the detector (WaitScrapeProvider) to be mounted",
  );
}
