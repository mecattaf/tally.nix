// tally — the triggers module barrel + daemon mount (M2.5; SPEC "Trigger surface"; PS#16b).
//
// The three-ingress trigger surface, ONE queue, no path privileged (PS#16b):
//   (1) the `events/` drop directory — watched + swept (`events-dir.ts`);
//   (2) systemd timers — the `tally daemon drain` oneshot issues the `queue.drain` RPC (`drain.ts`);
//   (3) live socket-enqueue — Seam A itself, owned by the jobs module (not here).
//
// This module mounts IN-DAEMON: at boot the composition root (`main.ts`) calls `mount(daemon)`, which
// registers the `queue.drain` RPC handler and the `events/` directory watcher on the daemon runtime
// via the `DaemonMount` seam. The `queue.drain` handler and every watch edge call the in-daemon
// `Drainer`, which sweeps `events/` and re-presents durable rows through the jobs engine's ordinary
// enqueue / re-present paths (injected seams). `triggers` neither is imported by jobs nor
// re-implements a queue — no filesystem-drain codepath ever runs outside the daemon (PS#1).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { DaemonModule, DaemonMount } from "../contracts/bus";
import type { PathEnv } from "../contracts/paths";
import { EventsDir, type EnqueueFn, type NoticeSink } from "./events-dir";
import { Drainer, QUEUE_DRAIN_METHOD, type RepresentFn, type DrainResult } from "./drain";

export {
  EventsDir,
  stdoutNotice,
  type EnqueueFn,
  type NoticeSink,
  type DropOutcome,
  type SweepResult,
  type EventsDirOptions,
} from "./events-dir";

export {
  Drainer,
  runDrainOneshot,
  drainOverSocket,
  QUEUE_DRAIN_METHOD,
  type RepresentFn,
  type DrainResult,
  type DrainerOptions,
  type DrainOneshotOptions,
} from "./drain";

/**
 * Options for constructing the triggers module. The `enqueue` and (optional) `represent` seams are
 * the jobs-engine paths the composition root injects — `triggers` reaches the jobs engine only
 * through these, never by importing it.
 */
export interface TriggersOptions {
  env: PathEnv;
  /** The ordinary Seam-A enqueue path (jobs engine). */
  enqueue: EnqueueFn;
  /** The jobs-engine re-present path (durable-row recover). Optional; absent ⇒ re-presents zero rows. */
  represent?: RepresentFn;
  /** Diagnostic sink for rejected drop files (default: stdout → journald). */
  notice?: NoticeSink;
}

/**
 * The triggers module. Owns the `events/` sweeper and the in-daemon drainer; mounts the `queue.drain`
 * RPC handler + the `events/` watcher onto the daemon. A single instance is constructed by the
 * composition root and mounted once.
 */
export class TriggersModule implements DaemonModule {
  readonly events: EventsDir;
  readonly drainer: Drainer;

  constructor(opts: TriggersOptions) {
    const eventsDirOpts: ConstructorParameters<typeof EventsDir>[0] = {
      env: opts.env,
      enqueue: opts.enqueue,
    };
    if (opts.notice !== undefined) eventsDirOpts.notice = opts.notice;
    this.events = new EventsDir(eventsDirOpts);

    const drainerOpts: ConstructorParameters<typeof Drainer>[0] = { eventsDir: this.events };
    if (opts.represent !== undefined) drainerOpts.represent = opts.represent;
    this.drainer = new Drainer(drainerOpts);
  }

  /**
   * Run one drain — sweep `events/` + re-present durable rows. The `queue.drain` RPC handler and the
   * `events/` watch edge both funnel here, so a watch-triggered sweep and a timer-triggered drain
   * share the same serialized `EventsDir.sweep` tail.
   */
  drain(): Promise<DrainResult> {
    return this.drainer.drain();
  }

  /**
   * Mount onto the daemon at boot (`DaemonMount` seam): register the `queue.drain` RPC handler and the
   * `events/` directory watcher. The handler makes the DAEMON sweep `events/` + re-present rows; the
   * watcher makes a dropped file trigger the same in-daemon drain edge-driven (a fallback timer drain
   * still catches anything the watch misses).
   */
  mount(daemon: DaemonMount): void {
    // Ensure the drop dir + archive subdirs exist before the watch is registered, so `fs.watch` binds
    // to a real directory and a file dropped immediately after boot is not missed.
    this.events.ensureDirs();

    daemon.registerRpc(QUEUE_DRAIN_METHOD, async () => {
      return await this.drainer.drain();
    });

    daemon.registerWatcher(this.events.path, async () => {
      // Any change under `events/` triggers a full sweep (self-serializing via `EventsDir.sweep`). We
      // ignore the specific changed path and sweep the whole dir so a rename into place, a partial
      // write, and a batch drop all resolve to one deterministic pass.
      await this.events.sweep();
    });
  }
}
