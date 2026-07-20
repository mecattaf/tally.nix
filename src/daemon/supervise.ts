// tally daemon-core — the restart-isolation harness for in-daemon supervised loops (PS#15a).
//
// The detector rides this (IMPLEMENTATION-PLAN M2.3), as does the gh intake poller (M2.4). A
// supervised loop that throws or exits is RESTARTED in isolation — a crash in one loop never takes
// the daemon (or a sibling loop) down. Restarts are backoff-bounded so a hard-looping crash does not
// spin the CPU; after a cap the loop is quarantined (stopped, logged) rather than restarted forever.
//
// Timing is driven through the injected `Clock` seam so tests advance backoff deterministically.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Clock } from "../contracts/exec";
import type { SupervisedLoop } from "../contracts/bus";

/** Backoff/quarantine policy for a supervised loop. */
export interface SupervisePolicy {
  /** First restart delay in ms. */
  baseBackoffMs: number;
  /** Backoff ceiling in ms. */
  maxBackoffMs: number;
  /** Restarts allowed within `crashWindowMs` before quarantine. `0` ⇒ never quarantine. */
  maxRestarts: number;
  /** The sliding window over which restarts are counted for the quarantine decision. */
  crashWindowMs: number;
}

export const DEFAULT_SUPERVISE_POLICY: SupervisePolicy = {
  baseBackoffMs: 250,
  maxBackoffMs: 10_000,
  maxRestarts: 20,
  crashWindowMs: 60_000,
};

/** The observable lifecycle state of one supervised loop. */
export type SuperviseState = "stopped" | "running" | "backoff" | "quarantined";

/** A sink for supervisor diagnostics (logged, never proof — PS#21). Defaults to stderr. */
export type SuperviseLog = (line: string) => void;

interface Supervised {
  loop: SupervisedLoop;
  state: SuperviseState;
  restarts: number[];
  attempt: number;
  cancelBackoff: (() => void) | null;
  stopping: boolean;
}

/**
 * The supervisor. `register` adds a loop; `start` boots every registered loop; a loop that
 * settles (its `start()` promise resolves or rejects) is restarted with exponential backoff, unless
 * it exceeds the crash budget, at which point it is quarantined. `stop` tears everything down.
 */
export class Supervisor {
  private readonly loops = new Map<string, Supervised>();

  constructor(
    private readonly clock: Clock,
    private readonly policy: SupervisePolicy = DEFAULT_SUPERVISE_POLICY,
    private readonly log: SuperviseLog = (l) => process.stderr.write(`tally[supervise]: ${l}\n`),
  ) {}

  /** Register a supervised loop (does not start it). Duplicate names replace the prior registration. */
  register(loop: SupervisedLoop): void {
    if (this.loops.has(loop.name)) {
      this.log(`replacing already-registered loop "${loop.name}"`);
    }
    this.loops.set(loop.name, {
      loop,
      state: "stopped",
      restarts: [],
      attempt: 0,
      cancelBackoff: null,
      stopping: false,
    });
  }

  /** The current state of a loop (for tests/diagnostics). */
  stateOf(name: string): SuperviseState | undefined {
    return this.loops.get(name)?.state;
  }

  /** How many times a loop has (re)started within the crash window (for tests). */
  restartCount(name: string): number {
    return this.loops.get(name)?.restarts.length ?? 0;
  }

  /** Boot every registered loop. */
  start(): void {
    for (const s of this.loops.values()) {
      if (s.state === "stopped") this.runLoop(s);
    }
  }

  /** Boot a single named loop (used when a module mounts after the initial `start`). */
  startOne(name: string): void {
    const s = this.loops.get(name);
    if (s && s.state === "stopped") this.runLoop(s);
  }

  private runLoop(s: Supervised): void {
    s.state = "running";
    s.stopping = false;
    // Isolate: never let a synchronous throw in `start()` escape into the caller.
    let started: Promise<void> | void;
    try {
      started = s.loop.start();
    } catch (err) {
      this.onSettled(s, err);
      return;
    }
    if (started && typeof (started as Promise<void>).then === "function") {
      (started as Promise<void>).then(
        () => this.onSettled(s, undefined),
        (err) => this.onSettled(s, err),
      );
    } else {
      // A synchronous `start()` that returned void: treat as a still-running loop that installed its
      // own timers/watchers. It only "settles" if it later throws through its own machinery, which we
      // cannot observe here — so we leave it running. Loops that are truly one-shot should return a
      // promise.
    }
  }

  private onSettled(s: Supervised, err: unknown): void {
    if (s.stopping) {
      s.state = "stopped";
      return;
    }
    if (err !== undefined) {
      this.log(`loop "${s.loop.name}" crashed: ${err instanceof Error ? err.stack ?? err.message : String(err)}`);
    } else {
      this.log(`loop "${s.loop.name}" exited; restarting`);
    }
    // Record the restart within the sliding window.
    const now = this.clock.now();
    s.restarts.push(now);
    s.restarts = s.restarts.filter((t) => now - t <= this.policy.crashWindowMs);
    if (this.policy.maxRestarts > 0 && s.restarts.length > this.policy.maxRestarts) {
      s.state = "quarantined";
      this.log(`loop "${s.loop.name}" exceeded ${this.policy.maxRestarts} restarts in ${this.policy.crashWindowMs}ms; quarantined`);
      return;
    }
    // Exponential backoff, capped.
    const delay = Math.min(this.policy.baseBackoffMs * 2 ** s.attempt, this.policy.maxBackoffMs);
    s.attempt += 1;
    s.state = "backoff";
    s.cancelBackoff = this.clock.setTimer(delay, () => {
      s.cancelBackoff = null;
      if (s.stopping) {
        s.state = "stopped";
        return;
      }
      this.runLoop(s);
    });
  }

  /** Stop one loop (cancel any pending backoff, call its `stop`). */
  async stopOne(name: string): Promise<void> {
    const s = this.loops.get(name);
    if (!s) return;
    s.stopping = true;
    if (s.cancelBackoff) {
      s.cancelBackoff();
      s.cancelBackoff = null;
    }
    try {
      await s.loop.stop?.();
    } catch (err) {
      this.log(`loop "${s.loop.name}" stop() threw: ${err instanceof Error ? err.message : String(err)}`);
    }
    s.state = "stopped";
  }

  /** Stop every loop. */
  async stop(): Promise<void> {
    await Promise.all([...this.loops.keys()].map((n) => this.stopOne(n)));
  }
}
