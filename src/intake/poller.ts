// tally — the gh intake poller (IMPLEMENTATION-PLAN M2.4 `poller.ts`; octo.nvim surface scan §5–6).
// A daemon-core-SUPERVISED loop (restart-isolated like the detector) that polls the gh read surface
// in priority order, distills signals, maps them to durable TW rows, and emits a journald line per
// landed row. WIRED but OFF by default (DECISIONS Q8): the loop is a no-op while `intake.gh.enable`
// is false, so it costs nothing until Tom opts a source in.
//
// The two-phase discipline (scan §5.7 / §6): a cheap `/notifications` + search poll surfaces
// candidates; a per-item `updatedAt` probe runs BEFORE any full hydration, and only a delta triggers
// a re-map. Rate-limit headroom is checked before each cycle and a `RateLimitExceeded` mid-cycle
// throw backs the loop off. Mute is respected (scan §5.8) — muted subjects never resurface.
//
// The poller depends on the taskchampion veneer (M1.3, via the mapper) and the journal writer
// (M1.4). It NEVER imports daemon-core internals: it is mounted onto the daemon's supervise host via
// the `DaemonMount` seam by the composition root (`main.ts`). It shells out ONLY through the injected
// `Exec` (inside `GhClient`).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Clock } from "../contracts/exec";
import type { SupervisedLoop } from "../contracts/bus";
import type { IntakeGhConfig } from "../contracts/config";

import { JournalEmitter } from "../journal/index";
import type { TaskChampion } from "../tw/index";

import { GhClient, RateLimitExceeded, type GhClientOptions } from "./gh";
import {
  defaultSignalPolicy,
  signalFromNotification,
  signalFromSearchNode,
  SEARCH_QUALIFIERS,
  type Signal,
  type SignalPolicy,
} from "./signals";
import { SignalMapper, dedupeSignals, type MapOutcome } from "./map";
import { RateLimitGate, decideFromSnapshot } from "./ratelimit";

/** The default poll cadence between cycles when the loop is enabled (ms). */
export const DEFAULT_POLL_INTERVAL_MS = 60_000;

/** The supervised-loop name — the label under which the supervisor tracks the poller. */
export const POLLER_NAME = "intake-gh";

/** Options for constructing the {@link GhPoller}. */
export interface GhPollerOptions {
  /** The gh intake config (from `TallyConfig.intake.gh`). `enable:false` ⇒ the loop is a no-op. */
  config: IntakeGhConfig;
  /** The gh CLI client options (the injected `Exec` lives here). */
  gh: GhClientOptions;
  /** The TaskChampion veneer the mapper lands rows through. */
  tw: TaskChampion;
  /** The journald emitter — one line per landed row (observability). */
  journal: JournalEmitter;
  /** The injected clock (cadence + rate-limit backoff, deterministic under test). */
  clock: Clock;
  /** Signal policy override (reasons + class→priority). Defaults to the scan defaults. */
  policy?: SignalPolicy;
  /** Cycle interval override in ms. */
  intervalMs?: number;
  /** A diagnostics sink (logged, never proof — PS#21). Defaults to stderr. */
  log?: (line: string) => void;
}

/** The result of one poll cycle — for tests and diagnostics. */
export interface CycleResult {
  /** Whether the cycle ran (false when disabled or rate-limited). */
  ran: boolean;
  /** The signals that survived filtering + mute + two-phase probing this cycle. */
  signals: Signal[];
  /** The map outcomes (created vs existing) this cycle. */
  outcomes: MapOutcome[];
  /** True when the cycle was deferred for rate-limit backoff. */
  rateLimited: boolean;
}

/**
 * The gh intake poller. Runs one cycle per {@link intervalMs} while enabled; each cycle:
 *   1. checks rate-limit headroom (defers on exhaustion),
 *   2. polls `/notifications` (reasons filtered, mute respected) + the search qualifiers,
 *   3. de-conflicts signals to the highest-urgency class per node id,
 *   4. two-phase probes each candidate's `updatedAt` and skips unchanged already-landed subjects,
 *   5. maps the survivors to durable TW rows (deduped on node id),
 *   6. emits a journald `enqueued` line per NEWLY-created row.
 *
 * Restart-isolated via the supervise host: a throw restarts the loop, never the daemon.
 */
export class GhPoller implements SupervisedLoop {
  readonly name = POLLER_NAME;

  private readonly config: IntakeGhConfig;
  private readonly gh: GhClient;
  private readonly tw: TaskChampion;
  private readonly journal: JournalEmitter;
  private readonly clock: Clock;
  private readonly policy: SignalPolicy;
  private readonly intervalMs: number;
  private readonly log: (line: string) => void;
  private readonly mapper: SignalMapper;
  private readonly gate: RateLimitGate;

  /** The last-seen `updatedAt` per node id — the two-phase change-detection memory (scan §5.7). */
  private readonly lastSeen = new Map<string, string>();

  private running = false;
  private cancelTimer: (() => void) | null = null;
  /** The pending `start()` promise + its resolver — settled only by `stop()` (a long-running loop). */
  private donePromise: Promise<void> | null = null;
  private resolveDone: (() => void) | null = null;

  constructor(opts: GhPollerOptions) {
    this.config = opts.config;
    this.gh = new GhClient(opts.gh);
    this.tw = opts.tw;
    this.journal = opts.journal;
    this.clock = opts.clock;
    this.policy = opts.policy ?? defaultSignalPolicy();
    this.intervalMs = opts.intervalMs ?? DEFAULT_POLL_INTERVAL_MS;
    this.log = opts.log ?? ((l) => process.stderr.write(`tally[intake-gh]: ${l}\n`));
    this.mapper = new SignalMapper(this.tw, this.policy);
    this.gate = new RateLimitGate(this.clock);
  }

  /** True when gh intake is enabled in config (OFF by default, DECISIONS Q8). */
  get enabled(): boolean {
    return this.config.enable === true;
  }

  /**
   * Start the supervised loop. In BOTH the enabled and the disabled (no-op) case the returned promise
   * stays PENDING until {@link stop} — a long-running loop that only settles on a clean stop, so the
   * supervise host treats it as still-running and never restart-churns it (a `start()` that resolved
   * while the loop is meant to keep running would be seen by the supervisor as "exited; restarting" and
   * re-invoked forever — the daemon's endless-restart trap). When enabled it installs the repeating
   * timer driving {@link runCycle}; when disabled it is inert (idle, consuming nothing) but still a live
   * mounted loop, so an `intake.gh.enable` flip takes effect on the next daemon restart with no rewiring.
   */
  start(): Promise<void> {
    if (this.running) return this.donePromise ?? Promise.resolve();
    this.running = true;

    const done = new Promise<void>((resolve) => {
      this.resolveDone = resolve;
    });
    this.donePromise = done;

    if (!this.enabled) {
      // Inert no-op loop: log once, install NO timer, and stay pending until stop() (never settle —
      // otherwise the supervisor re-invokes start() in a tight backoff-bounded loop).
      this.log("gh intake disabled (intake.gh.enable=false); poller idle");
      return done;
    }

    this.log(`gh intake enabled; polling every ${this.intervalMs}ms (sources: ${this.config.sources.join(", ") || "all"})`);
    const tick = async (): Promise<void> => {
      if (!this.running) return;
      try {
        await this.runCycle();
      } catch (err) {
        // A cycle error is isolated here (the supervisor also catches a start()-rejection, but a
        // per-cycle error should not tear down the loop — it retries next tick).
        this.log(`cycle error: ${err instanceof Error ? err.message : String(err)}`);
      }
    };
    // Drive the first cycle immediately, then on the interval; the wait between cycles honors any
    // active rate-limit backoff via the gate.
    this.cancelTimer = this.clock.setInterval(this.intervalMs, () => void tick());
    void tick();
    return done;
  }

  /** Stop the loop (supervisor shutdown). Resolves the pending `start()` promise. */
  stop(): void {
    this.running = false;
    if (this.cancelTimer) {
      this.cancelTimer();
      this.cancelTimer = null;
    }
    if (this.resolveDone) {
      this.resolveDone();
      this.resolveDone = null;
      this.donePromise = null;
    }
  }

  /**
   * Run one poll cycle. Public so tests drive a single cycle deterministically without the timer.
   * Honors the enable flag and the rate-limit gate; returns the cycle's signals + outcomes.
   */
  async runCycle(): Promise<CycleResult> {
    const empty: CycleResult = { ran: false, signals: [], outcomes: [], rateLimited: false };
    if (!this.enabled) return empty;

    // Respect an active backoff from a prior cycle's rate-limit exhaustion.
    if (!this.gate.isOpen()) {
      return { ...empty, rateLimited: true };
    }

    // Phase 0: rate-limit headroom check (scan §5.7).
    try {
      const snap = await this.gh.rateLimit();
      const decision = decideFromSnapshot(snap, this.clock);
      if (!decision.ok) {
        this.gate.backoff(decision.backoffMs);
        this.log(`rate-limited on ${decision.limiting}; backing off ${decision.backoffMs}ms`);
        return { ...empty, rateLimited: true };
      }
    } catch (err) {
      if (err instanceof RateLimitExceeded) {
        this.gate.onExceeded();
        return { ...empty, rateLimited: true };
      }
      throw err;
    }

    try {
      // Phase 1: collect candidate signals (notifications first, then search).
      const candidates = await this.collectSignals();
      // De-conflict to the highest-urgency class per node id.
      const deduped = dedupeSignals(candidates);
      // Phase 2: two-phase `updatedAt` probe — skip already-landed subjects with no delta.
      const changed = await this.filterByDelta(deduped);
      // Phase 3: map survivors to durable rows (deduped on node id).
      const outcomes = await this.mapper.mapAll(changed);
      // Observability: one journald line per NEWLY-created row.
      for (const outcome of outcomes) {
        if (outcome.status === "created") this.emitEnqueued(outcome);
      }
      return { ran: true, signals: changed, outcomes, rateLimited: false };
    } catch (err) {
      if (err instanceof RateLimitExceeded) {
        this.gate.onExceeded();
        return { ...empty, rateLimited: true };
      }
      throw err;
    }
  }

  /**
   * Collect candidate signals from the poll surface in priority order (scan §6): notifications
   * (reason-filtered, mute-respected) first, then the search qualifiers (paged). Search nodes carry
   * no subscription tier, so mute-respect is applied only at the notification layer; the mapper's
   * dedup keeps a muted-but-search-hit subject from double-landing.
   */
  private async collectSignals(): Promise<Signal[]> {
    const out: Signal[] = [];

    // (1) Notifications — the front door (scan §5.1).
    const notifications = await this.gh.notifications();
    for (const n of notifications) {
      const sig = signalFromNotification(n, this.policy);
      if (sig !== null) out.push(sig);
    }

    // (2) Search qualifiers — follow-ups (scan §6). Each is paged to completion.
    for (const { qualifier, class: cls } of SEARCH_QUALIFIERS) {
      let after: string | undefined;
      // Bounded paging guard — never loop forever on a malformed cursor.
      for (let page = 0; page < 20; page++) {
        const result = await this.gh.search(qualifier, after);
        for (const node of result.nodes) out.push(signalFromSearchNode(node, cls));
        if (!result.hasNextPage || result.endCursor === null) break;
        after = result.endCursor;
      }
    }

    return out;
  }

  /**
   * The two-phase probe (scan §5.7 / §6 primitive 3): for each candidate, compare its poll-provided
   * `updated_at` against the last-seen value; when unchanged AND already landed, skip it — no full
   * hydration. A first-sight subject or a changed `updatedAt` passes through. A notification-origin
   * signal whose key is a derived subject key (not a GraphQL node id) uses its poll `updated_at`
   * directly (the notification payload already carries it — no extra probe needed), while a search
   * node carries its own `updatedAt` from the search response. The dedicated `probeUpdatedAt` node
   * query is reserved for the case a caller tracks an item across cycles with no fresh list entry;
   * within one cycle the list-provided `updatedAt` is authoritative and cheaper.
   */
  private async filterByDelta(signals: Signal[]): Promise<Signal[]> {
    const kept: Signal[] = [];
    for (const s of signals) {
      const prev = this.lastSeen.get(s.node_id);
      const current = s.updated_at;
      if (prev !== undefined && current !== "" && prev === current) {
        // No change since last cycle — skip re-mapping (two-phase: no hydration without delta).
        continue;
      }
      this.lastSeen.set(s.node_id, current);
      kept.push(s);
    }
    return kept;
  }

  /** Emit one journald `enqueued` line for a newly-landed gh row (observability; SPEC journald table). */
  private emitEnqueued(outcome: MapOutcome): void {
    this.journal.emit({
      event: "enqueued",
      task_uuid: outcome.uuid,
      class: outcome.priority,
      source: "gh",
      message: `gh intake landed ${outcome.dedup_key}`,
    });
  }
}
