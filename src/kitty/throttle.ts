// tally — centralized kitty read throttling (IMPLEMENTATION-PLAN M1.6 `throttle.ts`).
//
// The detector scrape loop and the operator-facing `pane capture` both read the emulated grid via
// `kitty @ get-text` (CLI-SURFACE §3.1, the ONE out-of-band read path). This module centralizes the
// read budget so those two consumers SHARE ONE budget per window — a single place that (a) coalesces
// concurrent reads of the same window into one in-flight `get-text` (so a `pane capture` racing the
// detector's poll does not double-hit kitty), and (b) enforces a minimum interval between reads of a
// window, adapting the cadence to the pane's agent status (flag 4 provisional default: 2s while the
// agent is `working`, 10s otherwise — CLI-SURFACE §5 flag 4, IMPLEMENTATION-PLAN §1).
//
// It takes a `Clock` seam so tests drive the cadence deterministically, and produces a monotonically
// increasing `revision` per window per successful read — the `read.revision` field carried by
// `pane.output_matched` (CLI-SURFACE §2.3) so consumers can dedupe overlapping reads.

import type { Clock } from "../contracts/exec";
import type { AgentStatus } from "../contracts/agent";
import type { DetectorConfig } from "../contracts/config";

/** The result of a throttled read: the grid text + the per-window read `revision`. */
export interface ThrottledRead {
  text: string;
  /** Monotonic per-window read counter — the `read.revision` on `pane.output_matched`. */
  revision: number;
  /** True when this call reused an in-flight/last read rather than issuing a fresh `get-text`. */
  coalesced: boolean;
}

/** A grid reader — the throttle calls this to actually fetch text (normally `KittyRc.getText`). */
export type GridReader = (windowId: number) => Promise<string>;

/** Per-window throttle bookkeeping. */
interface WindowState {
  /** Monotonic read counter (the `revision`). */
  revision: number;
  /** `Clock.now()` at the last completed read. */
  lastReadAt: number;
  /** The text of the last completed read (returned when a call is coalesced under the min interval). */
  lastText: string;
  /** Whether any read has completed yet. */
  primed: boolean;
  /** An in-flight read, if one is running — concurrent callers await it instead of racing. */
  inFlight?: Promise<string>;
  /** The pane's current agent status — selects the cadence (working vs idle). */
  status: AgentStatus;
}

/** Default detector cadence when no config override is supplied (flag 4 provisional; §1). */
export const DEFAULT_DETECTOR_CADENCE: DetectorConfig = {
  working_poll_ms: 2000,
  idle_poll_ms: 10000,
};

/**
 * The shared kitty-read budget. One instance per daemon; the detector loop and `pane.capture` both
 * route their `get-text` reads through it so they never double-poll a window and always agree on the
 * `revision`. Construct with the real `KittyRc.getText` bound as the `GridReader`.
 */
export class ReadThrottle {
  private readonly windows = new Map<number, WindowState>();

  constructor(
    private readonly reader: GridReader,
    private readonly clock: Clock,
    private readonly cadence: DetectorConfig = DEFAULT_DETECTOR_CADENCE,
  ) {}

  private state(windowId: number): WindowState {
    let s = this.windows.get(windowId);
    if (!s) {
      s = { revision: 0, lastReadAt: -Infinity, lastText: "", primed: false, status: "idle" };
      this.windows.set(windowId, s);
    }
    return s;
  }

  /** The minimum interval (ms) between reads of a window, given its current agent status. */
  intervalFor(status: AgentStatus): number {
    return status === "working" ? this.cadence.working_poll_ms : this.cadence.idle_poll_ms;
  }

  /**
   * Record a window's current agent status so the NEXT throttle decision uses the right cadence
   * (the detector calls this as it classifies; a `working` pane polls faster than an `idle` one).
   */
  setStatus(windowId: number, status: AgentStatus): void {
    this.state(windowId).status = status;
  }

  /** The current read revision for a window (0 before the first read). */
  revisionOf(windowId: number): number {
    return this.windows.get(windowId)?.revision ?? 0;
  }

  /** Whether a fresh read of the window is due under the current cadence (used by the poll loop). */
  isDue(windowId: number): boolean {
    const s = this.state(windowId);
    if (!s.primed) return true;
    return this.clock.now() - s.lastReadAt >= this.intervalFor(s.status);
  }

  /**
   * A throttled read that respects the min interval: if the window was read within its cadence
   * window, return the last text WITHOUT issuing a new `get-text` (coalesced). Otherwise perform a
   * fresh read. Concurrent callers of the same window await the single in-flight read.
   *
   * This is the `pane capture` / detector-poll entry point — one budget, no double-polling.
   */
  async read(windowId: number): Promise<ThrottledRead> {
    const s = this.state(windowId);
    // Under the min interval and already primed ⇒ serve the cached read (coalesce).
    if (s.primed && !s.inFlight && this.clock.now() - s.lastReadAt < this.intervalFor(s.status)) {
      return { text: s.lastText, revision: s.revision, coalesced: true };
    }
    return this.forceRead(windowId, true);
  }

  /**
   * An UNTHROTTLED read that still coalesces concurrent in-flight reads and still bumps the revision.
   * `pane capture` (an explicit operator request) uses this — the operator asked for fresh text, so
   * the min-interval gate is bypassed, but a read racing the detector's poll is still deduped.
   */
  async forceRead(windowId: number, _internal = false): Promise<ThrottledRead> {
    const s = this.state(windowId);
    if (s.inFlight) {
      // Another read of this window is already running — join it, don't double-hit kitty.
      const text = await s.inFlight;
      return { text, revision: s.revision, coalesced: true };
    }
    const p = this.reader(windowId);
    s.inFlight = p;
    try {
      const text = await p;
      s.revision += 1;
      s.lastReadAt = this.clock.now();
      s.lastText = text;
      s.primed = true;
      return { text, revision: s.revision, coalesced: false };
    } finally {
      // Only clear if it is still OUR in-flight promise (a later read may have replaced it).
      if (s.inFlight === p) delete s.inFlight;
    }
  }

  /** Forget a window's throttle state (its pane closed). */
  forget(windowId: number): void {
    this.windows.delete(windowId);
  }

  /** Drop all state (daemon reset / test teardown). */
  clear(): void {
    this.windows.clear();
  }
}
