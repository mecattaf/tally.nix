// tally daemon-core — the daemon composition root + `DaemonMount` implementation
// (IMPLEMENTATION-PLAN M1.1, §3 Seams). This is the barrel every daemon-core piece exports through,
// AND the boot glue: it constructs `DaemonState` + `DaemonServer` + `Supervisor`, exposes the
// `DaemonMount` seam (`registerRpc`/`registerWatcher`/`registerSupervised`) the composition root
// (`main.ts`) uses to wire every in-daemon module without those modules importing daemon-core
// internals, and registers the `daemon` entrypoint so `tally daemon run` boots the real daemon.
//
// daemon-core owns the TRANSPORT + the mount seam; the mounted modules (session-model, jobs,
// detector, intake-gh, triggers) bring the DATA and the additive RPC/handler surface.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { existsSync, readFileSync, watch, type FSWatcher } from "node:fs";
import type { PathEnv } from "../contracts/paths";
import { configPath } from "../contracts/paths";
import { loadConfig, type TallyConfig } from "../contracts/config";
import type {
  DaemonMount,
  RpcHandler,
  WatcherHandler,
  SupervisedLoop,
  WaitScrapeProvider,
  JobBarrierProvider,
} from "../contracts/bus";
import type { SnapshotProvider, SnapshotSectionProvider } from "../contracts/snapshot";
import type { Clock } from "../contracts/exec";
import { DaemonState } from "./state";
import { DaemonServer } from "./server";
import { RpcRouter } from "./rpc";
import { Supervisor, type SupervisePolicy } from "./supervise";

// Re-export the daemon-core surface for the composition root and tests.
export { DaemonState, DaemonBus, noWaitScrapeProvider } from "./state";
export { DaemonServer, makeServer } from "./server";
export { RpcRouter, isConnectionBound, negotiateProtocol } from "./rpc";
export { Supervisor, DEFAULT_SUPERVISE_POLICY } from "./supervise";
export { ReplayRing } from "./replay-ring";
export type { StampedEvent, ResumeComputation } from "./replay-ring";
export { SubscriptionRegistry, Subscription, resolveFilter } from "./subscriptions";
export type { FrameSink, SubscriptionFilter } from "./subscriptions";
export { Heartbeat } from "./heartbeat";
export { LineDecoder, encodeFrame, fitsFrameCap } from "./framing";
export { bumpEpoch, readEpochCounter, readPlsGenerationCounter, writeEpochCounter } from "./epoch";
export type { ResolvedEpoch, EpochSource } from "./epoch";
export { runWait } from "./wait";
export type { WaitHost } from "./wait";

/** Options for booting the daemon. */
export interface DaemonBootOptions {
  env: PathEnv;
  /** Overrides the on-disk config (tests). When absent, `config.json` is read (defaults if missing). */
  config?: TallyConfig;
  clock?: Clock;
  plsGeneration?: number;
  supervisePolicy?: SupervisePolicy;
}

/**
 * The live daemon: state + server + supervisor, exposing the `DaemonMount` seam. Constructed by
 * `bootDaemon`; the composition root mounts modules onto it, then calls `start`.
 */
export class Daemon implements DaemonMount {
  readonly state: DaemonState;
  readonly server: DaemonServer;
  readonly router: RpcRouter;
  readonly supervisor: Supervisor;
  private readonly watchers: FSWatcher[] = [];
  private busUnwire: (() => void) | null = null;
  private started = false;

  constructor(opts: DaemonBootOptions) {
    const config = opts.config ?? loadConfigFromDisk(opts.env);
    const stateOpts: ConstructorParameters<typeof DaemonState>[0] = { env: opts.env, config };
    if (opts.clock) stateOpts.clock = opts.clock;
    if (opts.plsGeneration !== undefined) stateOpts.plsGeneration = opts.plsGeneration;
    this.state = new DaemonState(stateOpts);
    this.router = new RpcRouter(this.state);
    this.server = new DaemonServer(this.state, this.router, opts.env);
    this.supervisor = new Supervisor(this.state.clock, opts.supervisePolicy);
  }

  // ---- DaemonMount seam ----------------------------------------------------------------------

  /** Register an additive RPC carrier (queue.*, pane.*, agent.*, sensor edges, session.list, …). */
  registerRpc(method: string, handler: RpcHandler): void {
    this.router.register(method, handler);
  }

  /**
   * Register a directory watcher (triggers' `events/` sweep). The handler is called with each changed
   * path under `path`; a best-effort `fs.watch` drives it. The watcher survives the daemon lifetime
   * and is closed on `stop`.
   */
  registerWatcher(path: string, handler: WatcherHandler): void {
    try {
      const w = watch(path, { persistent: false }, (_event, filename) => {
        if (filename) void Promise.resolve(handler(`${path}/${filename}`)).catch((err) => {
          process.stderr.write(`tally[watcher]: ${path} handler threw: ${err instanceof Error ? err.message : String(err)}\n`);
        });
      });
      this.watchers.push(w);
    } catch (err) {
      process.stderr.write(`tally[watcher]: failed to watch ${path}: ${err instanceof Error ? err.message : String(err)}\n`);
    }
  }

  /** Register a supervised loop (detector, gh poller) on the restart-isolation host. */
  registerSupervised(loop: SupervisedLoop): void {
    this.supervisor.register(loop);
    // If the daemon is already running, boot the newly-mounted loop immediately.
    if (this.started) this.supervisor.startOne(loop.name);
  }

  // ---- Provider registration (delegate to state) --------------------------------------------

  /** Register the session-model's snapshot provider (daemon-core owns transport, model owns data). */
  registerSnapshotProvider(provider: SnapshotProvider): void {
    this.state.registerSnapshotProvider(provider);
  }

  /** Register the detector's `WaitScrapeProvider` for `session.wait pane_output`. */
  registerWaitScrapeProvider(provider: WaitScrapeProvider): void {
    this.state.registerWaitScrapeProvider(provider);
  }

  /** Register the jobs engine's `JobBarrierProvider` for `session.wait {subject:job}`. */
  registerJobBarrierProvider(provider: JobBarrierProvider): void {
    this.state.registerJobBarrierProvider(provider);
  }

  /**
   * Register a snapshot SECTION provider (the single-store ruling, risk 9). daemon-core does not
   * assemble sections — the single `model/store.ts` does — so this is a pass-through convenience the
   * store may use; daemon-core simply exposes it on the mount for symmetry. The section writer (jobs
   * `jobs[]`, detector `agents[]`) writes into the store via the `Bus`; the store reads sections at
   * assembly time. Kept as a typed no-op hook here so a caller has one obvious mount point.
   */
  registerSnapshotSection(_provider: SnapshotSectionProvider): void {
    // Intentionally not stored in daemon-core: the store (layer 2) owns section composition. daemon-
    // core only owns the SnapshotProvider that reads the assembled store. This method exists so the
    // mount surface is complete; the store registers itself as the single SnapshotProvider.
  }

  // ---- Lifecycle ----------------------------------------------------------------------------

  /** Boot: wire the bus to the wire, bind the socket, start the supervisor. */
  async start(): Promise<void> {
    this.busUnwire = this.state.wireBusToWire();
    await this.server.listen();
    this.supervisor.start();
    this.started = true;
  }

  /** Tear down: stop supervised loops, close watchers, stop the server, unwire the bus. */
  async stop(): Promise<void> {
    await this.supervisor.stop();
    for (const w of this.watchers) {
      try {
        w.close();
      } catch {
        // ignore
      }
    }
    this.watchers.length = 0;
    this.server.stop();
    if (this.busUnwire) {
      this.busUnwire();
      this.busUnwire = null;
    }
    this.started = false;
  }
}

/** Read `$XDG_CONFIG_HOME/tally/config.json` into a validated `TallyConfig` (defaults if absent). */
export function loadConfigFromDisk(env: PathEnv): TallyConfig {
  const path = configPath(env);
  if (!existsSync(path)) return loadConfig(undefined);
  try {
    const raw = JSON.parse(readFileSync(path, "utf8")) as unknown;
    return loadConfig(raw);
  } catch (err) {
    process.stderr.write(`tally: config at ${path} is invalid (${err instanceof Error ? err.message : String(err)}); using defaults\n`);
    return loadConfig(undefined);
  }
}

/** Construct a daemon (without starting it) — the composition root mounts modules, then `start`s. */
export function bootDaemon(opts: DaemonBootOptions): Daemon {
  return new Daemon(opts);
}

/**
 * The `tally daemon run` entrypoint. Boots the real daemon and blocks until SIGINT/SIGTERM. The
 * composition root (`main.ts`) registers this via `registerEntrypoint("daemon", …)`; the mounted
 * modules are wired in by that root BEFORE `start` (this bare entry mounts nothing beyond daemon-core
 * itself, so `main.ts`'s real composition root — which knows the layer-2/3 modules — supersedes it).
 */
export async function runDaemonEntrypoint(
  argv: string[],
  env: Record<string, string | undefined> = process.env,
): Promise<number> {
  const sub = argv[0];
  if (sub === "drain") {
    process.stderr.write("tally: `daemon drain` is a socket client served by the CLI module (M3.1), not daemon-core\n");
    return 2;
  }
  if (sub !== undefined && sub !== "run") {
    process.stderr.write(`tally: unknown daemon subcommand '${sub}' (expected 'run')\n`);
    return 2;
  }

  const daemon = bootDaemon({ env: env as PathEnv });
  await daemon.start();
  process.stderr.write(
    `tally: daemon listening on ${daemon.server.socketPath} (lease_epoch=${daemon.state.epoch}, protocol=${daemon.state.config.daemonVersion})\n`,
  );

  await new Promise<void>((resolve) => {
    const shutdown = () => {
      void daemon.stop().finally(resolve);
    };
    process.on("SIGINT", shutdown);
    process.on("SIGTERM", shutdown);
  });
  return 0;
}
