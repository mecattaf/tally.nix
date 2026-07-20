// tally — THE composition root (IMPLEMENTATION-PLAN M-designated `main.ts` seam).
//
// This is the one module that KNOWS every layer-2/3 module and wires them onto the daemon-core mount
// seam, then boots. `src/main.ts` imports this (via the registry seam) so the compiled single-file
// binary ships the fully-composed daemon + CLI — not the layer-0 fallback. It mirrors what
// `test/e2e/helpers.ts` performs as the e2e composition root, but with the PRODUCTION substrate:
// the real Bun-backed `Exec`, the on-disk config, the ledger under `$XDG_DATA_HOME`, and the bundled
// `manifests/*.toml` embedded into the binary via a `type:"text"` import.
//
// What it mounts (disjoint ownership; each module brings its own additive RPC surface):
//   • SessionModel      — the single store, discovery join, `session.list`/`session.register_viewer`,
//                          the SnapshotProvider (session.snapshot), the reconcile loop.
//   • JobsEngine        — `queue.enqueue`/`cancel`/`pause`/`resume`, the `jobs[]` section, recover().
//   • DetectorModule    — the supervised scrape/hook loop, `agent.hook_event`/`agent.explain`, the
//                          `WaitScrapeProvider`, the `agents[]` section.
//   • TriggersModule    — `queue.drain` + the `events/` watcher (the drop-dir ingress).
//   • WatcherIngest     — `kitty.watcher_event` (the kitty sensor edge).
//   • IntakeGh          — the gh poller (config-gated; OFF by default).
//   • handlers.ts       — the nine cross-cutting carriers: pane.*, agent.list/get/read, query.status,
//                          query.render (registered here, over the store + kitty + detector + pls).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import claudeCodeManifestToml from "../manifests/claude-code.toml" with { type: "text" };
import piManifestToml from "../manifests/pi.toml" with { type: "text" };

import { unitExitDir, type PathEnv } from "./contracts/paths";
import type { Clock, Exec } from "./contracts/exec";
import type { TallyConfig } from "./contracts/config";
import { bootDaemon, loadConfigFromDisk, type Daemon } from "./daemon/index";
import { SessionModel } from "./model/index";
import { JobsEngine } from "./jobs/index";
import { DetectorModule, loadManifests } from "./detector/index";
import { TriggersModule } from "./triggers/index";
import { IntakeGh } from "./intake/index";
import { WatcherIngest, sensorEdgeBus } from "./kitty/watcher-ingest";
import { KittyRc } from "./kitty/rc";
import { WitnessLedger } from "./witness/index";
import { JournalEmitter } from "./journal/index";
import { TaskChampion } from "./tw/index";
import { PoolRegistry, PlsBroker, LeaseManager } from "./pls/index";
import {
  agentGet,
  agentList,
  agentRead,
  paneCapture,
  paneFocus,
  paneSend,
  paneSendKey,
  queryRender,
  queryStatus,
  type HandlerDeps,
} from "./daemon/handlers";
import { bunExec } from "./cli/query";

/** Options for the composition root (the daemon entrypoint supplies the env + optional overrides). */
export interface ComposeOptions {
  env: PathEnv;
  /** Overrides the on-disk config (tests). Production reads `config.json` (defaults if absent). */
  config?: TallyConfig;
  /** The subprocess seam. Production uses the Bun-backed `Exec`; tests inject a fake. */
  exec?: Exec;
  clock?: Clock;
  /** Skip the detector mount (e.g. a headless conductor with no kitty). Default: mount it. */
  withoutDetector?: boolean;
}

/** The fully-composed, not-yet-started daemon plus the modules the caller may introspect (tests). */
export interface ComposedDaemon {
  daemon: Daemon;
  model: SessionModel;
  engine: JobsEngine;
  detector: DetectorModule | null;
  triggers: TriggersModule;
  intake: IntakeGh;
  watcher: WatcherIngest;
}

/**
 * Build the fully-composed daemon: construct every module over the production substrate, mount each
 * onto the daemon-core seam, register the nine cross-cutting handlers, and register the snapshot +
 * section providers. Does NOT call `daemon.start()` — the entrypoint does that after `recover()` so
 * boot recovery runs before the socket serves.
 */
export function composeDaemon(opts: ComposeOptions): ComposedDaemon {
  const config = opts.config ?? loadConfigFromDisk(opts.env);
  const exec: Exec = opts.exec ?? bunExec();

  const daemonBootOpts: Parameters<typeof bootDaemon>[0] = { env: opts.env, config };
  if (opts.clock) daemonBootOpts.clock = opts.clock;
  const daemon = bootDaemon(daemonBootOpts);
  const clock = daemon.state.clock;
  const bus = daemon.state.bus;

  // ---- Shared sinks: ledger (ledger-as-truth), journald, TaskChampion. ----
  const ledger = WitnessLedger.open(opts.env);
  const journal = new JournalEmitter();
  const tw = new TaskChampion({ exec });

  // ---- pls: the box governor (pools from config; broker over Exec). ----
  const pools = PoolRegistry.fromConfig(config.pools);
  const broker = new PlsBroker(exec);
  const leases = new LeaseManager(broker, pools);

  // ---- Session model: the single store + discovery + snapshot provider. ----
  const modelOpts: ConstructorParameters<typeof SessionModel>[0] = { bus, exec, config };
  if (opts.clock) modelOpts.clock = opts.clock;
  const model = new SessionModel(modelOpts);

  // ---- Jobs engine: the one execution primitive. ----
  const engine = new JobsEngine({
    exec,
    bus,
    tw,
    leases,
    ledger,
    journal,
    clock,
    // The engine stamps `lease_epoch` from daemon-core's resolved boot epoch until a live pls grant
    // supersedes it (never the witness ledger seq — an unrelated counter). This keeps the engine's
    // lease_epoch on the SAME monotone series as the snapshot header / subscribe ACK (§2.2, §2.5).
    bootEpoch: daemon.state.epoch,
    // Production dispatches heavy units as systemd transient units; the runner falls back to a direct
    // spawn when systemd-run is unavailable (dev rig / non-systemd host), so no forceDirect here.
    costFor: () => 1,
    // The durable per-unit ExecStopPost exit records — the exit-code conjunct recovery gates on
    // after `--collect` unloads a unit that exited during the restart window (issue #3).
    unitExitDir: unitExitDir(opts.env),
  });

  // ---- Detector: the supervised scrape/hook loop (bundled manifests embedded in the binary). ----
  let detector: DetectorModule | null = null;
  if (!opts.withoutDetector) {
    const manifests = loadManifests({ pi: piManifestToml, "claude-code": claudeCodeManifestToml });
    detector = new DetectorModule({ exec, bus, clock, manifests, cadence: config.detector });
  }

  // ---- Triggers: the events/ drop-dir ingress + queue.drain. ----
  const triggers = new TriggersModule({
    env: opts.env,
    enqueue: (params) => engine.enqueue(params),
    represent: async () => {
      const plan = await engine.recover();
      return plan.represent.length;
    },
  });

  // ---- Kitty watcher-event sensor edge. ----
  const watcher = new WatcherIngest(sensorEdgeBus(bus));

  // ---- gh intake (config-gated; OFF by default). ----
  const intake = new IntakeGh({ config: config.intake.gh, exec, tw, clock, journal });

  // ---- Mount everything onto the daemon-core seam. ----
  daemon.registerSnapshotProvider(model.snapshotProvider);
  model.mount(daemon);
  engine.mount(daemon);
  if (detector) detector.mount(daemon);
  triggers.mount(daemon);
  intake.mount(daemon);
  daemon.registerRpc("kitty.watcher_event", watcher.handleRpc);

  // ---- Section providers into the single store (the store reads the legs at assembly). ----
  // The store also mirrors these off the Bus, but the provider read is authoritative after a
  // supervised-loop restart (risk 9 single-store ruling).
  model.store.registerSectionProvider(engine.sectionProvider());
  if (detector) model.store.registerSectionProvider(detector.loop);

  // ---- The nine cross-cutting handlers (pane.*, agent.list/get/read, query.status/render). ----
  const deps: HandlerDeps = {
    store: model.store,
    kitty: new KittyRc(exec),
    detector: detector ? detector.loop : null,
    leases,
    pools,
    broker,
    queueDepth: (pool) => engine.queueDepth(pool),
  };
  daemon.registerRpc("pane.send", (params) => paneSend(deps, params));
  daemon.registerRpc("pane.send_key", (params) => paneSendKey(deps, params));
  daemon.registerRpc("pane.focus", (params) => paneFocus(deps, params));
  daemon.registerRpc("pane.capture", (params) => paneCapture(deps, params));
  daemon.registerRpc("agent.list", (params) => agentList(deps, params));
  daemon.registerRpc("agent.get", (params) => agentGet(deps, params));
  daemon.registerRpc("agent.read", (params) => agentRead(deps, params));
  daemon.registerRpc("query.status", (params) => queryStatus(deps, params));
  daemon.registerRpc("query.render", (params) => queryRender(deps, params));

  return { daemon, model, engine, detector, triggers, intake, watcher };
}

/**
 * The real `daemon` entrypoint (the composition root's boot glue). Boots the fully-composed daemon,
 * runs `recover()` (re-present, never replay) BEFORE serving, then blocks until SIGINT/SIGTERM. The
 * registry in `main.ts` dispatches `tally daemon [run]` here (superseding daemon-core's bare boot).
 */
export async function runComposedDaemon(
  argv: string[],
  env: Record<string, string | undefined> = process.env,
): Promise<number> {
  const sub = argv[0];
  if (sub !== undefined && sub !== "run") {
    process.stderr.write(`tally: unknown daemon subcommand '${sub}' (expected 'run')\n`);
    return 2;
  }

  const composed = composeDaemon({ env: env as PathEnv });
  const { daemon, engine } = composed;

  // Recover BEFORE the socket serves: reconcile the witness head, reclaim fenced leases, re-present
  // undeleted un-acked in-budget rows via resume (a boot after a crash never loses durable work).
  await engine.recover().catch((err: unknown) => {
    process.stderr.write(`tally: recover() failed at boot (continuing): ${err instanceof Error ? err.message : String(err)}\n`);
  });

  await daemon.start();
  process.stderr.write(
    `tally: daemon listening on ${daemon.server.socketPath} (lease_epoch=${daemon.state.epoch}, protocol=${daemon.state.config.daemonVersion}` +
      `${composed.detector ? "" : ", detector OFF"}${composed.intake.enabled ? ", gh intake ON" : ""})\n`,
  );

  await new Promise<void>((resolve) => {
    const shutdown = () => {
      void daemon.stop().finally(resolve);
    };
    process.on("SIGINT", shutdown);
    process.on("SIGTERM", shutdown);
  });
  // Force a clean exit after stop resolves: an in-flight no-timeout `session.wait pane_output` (or any
  // pending timer) would otherwise keep the Bun event loop alive and the process would never exit,
  // forcing systemd to SIGKILL it. The detector's stop() aborts such waits, but a residual timer can
  // still pin the loop; exit explicitly once shutdown is complete.
  process.exit(0);
}
