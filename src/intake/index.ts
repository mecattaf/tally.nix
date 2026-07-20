// tally — the gh intake module barrel + daemon mount (IMPLEMENTATION-PLAN M2.4).
//
// Direct `gh` CLI intake (PS#21, bugwarrior replaced) — WIRED but OFF by default, opt-in per-source
// (DECISIONS Q8). Composes the typed gh client (gh.ts), the signal classifier + mute-respect
// (signals.ts), the rate-limit headroom/backoff (ratelimit.ts), the signal→row mapper (map.ts), and
// the supervised two-phase poll loop (poller.ts) into one `DaemonModule` the composition root mounts.
//
// Daemon mount (mechanism named, not guessed — IMPLEMENTATION-PLAN M2.4, §3 Seams `DaemonMount`):
// the daemon is the only persistent process (the nix module ships no intake timer unit), so intake-gh
// registers its `poller.ts` on daemon-core's `supervise.ts` cadence host at boot via the
// `DaemonMount.registerSupervised` seam. It runs restart-isolated like the detector loop and is a
// no-op while OFF by default. `main.ts` calls `mount(daemon)` — intake-gh never imports daemon-core
// internals nor is imported by a sibling to get wired.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { DaemonModule, DaemonMount } from "../contracts/bus";
import type { Clock } from "../contracts/exec";
import type { Exec } from "../contracts/exec";
import type { IntakeGhConfig } from "../contracts/config";

import { JournalEmitter } from "../journal/index";
import type { TaskChampion } from "../tw/index";

import { GhPoller, type GhPollerOptions } from "./poller";
import type { SignalPolicy } from "./signals";

export * from "./gh";
export * from "./signals";
export * from "./ratelimit";
export * from "./map";
export * from "./poller";

/** Options for constructing the {@link IntakeGh} module facade. */
export interface IntakeGhOptions {
  /** The gh intake config (`TallyConfig.intake.gh`). `enable:false` ⇒ the poller is a no-op. */
  config: IntakeGhConfig;
  /** The injectable subprocess seam (the ONLY way the module shells out to `gh`). */
  exec: Exec;
  /** The TaskChampion veneer the mapper lands rows through. */
  tw: TaskChampion;
  /** The injected clock (cadence + rate-limit backoff). */
  clock: Clock;
  /** The journald emitter (one line per landed row); defaults to a fresh stdout-sink emitter. */
  journal?: JournalEmitter;
  /** Signal policy override (reasons + class→priority). */
  policy?: SignalPolicy;
  /** Poll cycle interval override in ms. */
  intervalMs?: number;
  /** The `gh` binary name/path (auth ambient, Q8). Defaults to `"gh"`. */
  ghBin?: string;
  /** A diagnostics sink (logged, never proof). */
  log?: (line: string) => void;
}

/**
 * The gh intake module facade — a {@link DaemonModule}. Holds the composed poller and mounts it onto
 * the daemon's supervise host at boot. The module is inert until `intake.gh.enable` is true; mounting
 * an inert poller is intentional (an enable takes effect on the next daemon restart with no wiring
 * change).
 */
export class IntakeGh implements DaemonModule {
  readonly poller: GhPoller;

  constructor(opts: IntakeGhOptions) {
    const journal = opts.journal ?? new JournalEmitter();
    const pollerOpts: GhPollerOptions = {
      config: opts.config,
      gh: { exec: opts.exec, ...(opts.ghBin !== undefined ? { bin: opts.ghBin } : {}) },
      tw: opts.tw,
      journal,
      clock: opts.clock,
      ...(opts.policy !== undefined ? { policy: opts.policy } : {}),
      ...(opts.intervalMs !== undefined ? { intervalMs: opts.intervalMs } : {}),
      ...(opts.log !== undefined ? { log: opts.log } : {}),
    };
    this.poller = new GhPoller(pollerOpts);
  }

  /** True when gh intake is enabled (OFF by default). */
  get enabled(): boolean {
    return this.poller.enabled;
  }

  /**
   * Mount onto the daemon at boot (IMPLEMENTATION-PLAN §3 `DaemonMount`). Registers the poller as a
   * supervised loop on daemon-core's `supervise.ts` cadence host — restart-isolated, no-op while OFF.
   * The composition root (`main.ts`) is the single caller.
   */
  mount(daemon: DaemonMount): void {
    daemon.registerSupervised(this.poller);
  }
}
