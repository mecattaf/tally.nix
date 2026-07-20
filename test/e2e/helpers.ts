// test/e2e/helpers.ts
//
// The layer-4 e2e harness (IMPLEMENTATION-PLAN M4.1). Full-path integration: this composes the REAL
// modules — daemon-core transport, the jobs engine, the detector loop, the witness ledger, the
// journald emitter, the TaskChampion veneer — over the layer-0 fakes (FakeExec/FakePls/FakeTask/
// FakeSystemdRun/FakeKitty) and, where the brief demands a real socket, a live Bun Unix-socket daemon
// driven by the frozen §2 NDJSON client.
//
// There is no single `composeDaemon` helper in `src/` (the composition root wires each module's
// `mount(daemon)` at boot; `main.ts` owns that seam per module). This harness is the e2e composition
// root: it boots a `Daemon`, mounts a `JobsEngine` (+ optionally a `DetectorLoop`), registers the
// snapshot/wait providers, and exposes the same primitives the CLI reaches over the socket — so the
// enqueue → lease → dispatch → evidence → witness → witness-verify path runs genuinely end-to-end.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { FakeExec, ok, type ExecResult } from "../helpers/exec-fakes.ts";
import { FakePls } from "../helpers/fake-pls.ts";
import { FakeTask } from "../helpers/fake-task.ts";
import { FakeSystemdRun } from "../helpers/fake-systemd.ts";
import { FakeKitty } from "../helpers/fake-kitty.ts";
import { makeTmpEnv, type TmpEnv } from "../helpers/tmp.ts";
import { connectClient, type SocketClient } from "../helpers/socket-client.ts";

import { DaemonBus } from "../../src/daemon/state.ts";
import { bootDaemon, type Daemon } from "../../src/daemon/index.ts";
import {
  systemClock,
  defaultConfig,
  unitExitDir,
  type Clock,
  type EnqueueParams,
  type EnqueueResult,
  type EventName,
} from "../../src/contracts/index.ts";
import { PoolRegistry, PlsBroker, LeaseManager } from "../../src/pls/index.ts";
import { WitnessLedger } from "../../src/witness/index.ts";
import { JournalEmitter } from "../../src/journal/index.ts";
import { TaskChampion } from "../../src/tw/index.ts";
import { JobsEngine } from "../../src/jobs/index.ts";
import { DetectorLoop, type ManifestSet } from "../../src/detector/loop.ts";

export { ok } from "../helpers/exec-fakes.ts";
export type { ExecResult } from "../helpers/exec-fakes.ts";
export { readLedger } from "../helpers/tmp.ts";
export type { SocketClient } from "../helpers/socket-client.ts";
export type { TmpEnv } from "../helpers/tmp.ts";

// ---------------------------------------------------------------------------------------------
// A bus that records every event for assertions (mirrors the jobs-module test harness).
// ---------------------------------------------------------------------------------------------

export class RecordingBus extends DaemonBus {
  readonly events: Array<{ event: EventName; payload: unknown }> = [];
  constructor() {
    super();
    this.onAny((e) => this.events.push({ event: e.event, payload: e.payload }));
  }
  ofType(name: EventName): unknown[] {
    return this.events.filter((e) => e.event === name).map((e) => e.payload);
  }
}

// ---------------------------------------------------------------------------------------------
// Mock workers (the OCR-shaped leaf the e2e suites drive through the real engine).
// ---------------------------------------------------------------------------------------------

/**
 * Register the standard mock workers on a `FakeExec`:
 *   `ocr <out> [<content>]` — writes the artifact + exits 0 (the happy OCR leaf);
 *   `noop`                  — exits 0 but writes NOTHING (the clean-exit-no-artifact forensic);
 *   `boom`                  — exits non-zero (a plain failure).
 */
export function registerWorkers(exec: FakeExec): void {
  exec.register("ocr", (args): ExecResult => {
    const out = args[0]!;
    const content = args[1] ?? "OCR-OUTPUT";
    mkdirSync(join(out, ".."), { recursive: true });
    writeFileSync(out, content, "utf8");
    return ok(`pi:session=sess-ocr\npi:model=claude-sonnet\nwrote ${out}`);
  });
  exec.register("noop", (): ExecResult => ok("did nothing"));
  exec.register("boom", (): ExecResult => ({ code: 7, stdout: "", stderr: "boom" }));
}

// ---------------------------------------------------------------------------------------------
// The engine harness — the REAL jobs engine over the fakes, sharing one witness ledger + journal.
// ---------------------------------------------------------------------------------------------

export interface EngineHarness {
  env: TmpEnv;
  exec: FakeExec;
  pls: FakePls;
  task: FakeTask;
  systemd: FakeSystemdRun;
  bus: RecordingBus;
  ledger: WitnessLedger;
  journal: JournalEmitter;
  journalLines: string[];
  tw: TaskChampion;
  leases: LeaseManager;
  engine: JobsEngine;
  clock: Clock;
  /** A per-test artifact path under the tmp tree. */
  artifactPath(name: string): string;
  cleanup(): void;
}

/**
 * Build the engine harness: the REAL `JobsEngine` composed over the fakes, writing to the tmp-tree
 * witness ledger and an in-memory journald sink. `forceDirect` uses the direct-spawn fallback so the
 * fake `ocr`/`noop`/`boom` workers run synchronously (no systemd on the test host).
 */
export function makeEngineHarness(opts: { clock?: Clock } = {}): EngineHarness {
  const env = makeTmpEnv();
  const exec = new FakeExec();
  const pls = new FakePls();
  pls.addPool("worker-gpu", { capacity: 1, budget: 128 });
  pls.addPool("controller-gpu", { capacity: 1, budget: 128 });
  pls.install(exec);
  const task = new FakeTask();
  task.install(exec);
  const systemd = new FakeSystemdRun();
  systemd.install(exec);
  registerWorkers(exec);

  const bus = new RecordingBus();
  const ledger = WitnessLedger.open(env.env);
  const journalLines: string[] = [];
  const journal = new JournalEmitter((line) => journalLines.push(line));
  const tw = new TaskChampion({ exec });
  const leases = new LeaseManager(new PlsBroker(exec), PoolRegistry.default());
  const clock = opts.clock ?? systemClock;

  const engine = new JobsEngine({
    exec,
    bus,
    tw,
    leases,
    ledger,
    journal,
    clock,
    forceDirect: true,
    costFor: () => 1,
    // The durable ExecStopPost exit-record dir (issue #3): recovery tests seed records here to
    // stand in for what a real unit persisted before `--collect` unloaded it.
    unitExitDir: unitExitDir(env.env),
  });

  return {
    env,
    exec,
    pls,
    task,
    systemd,
    bus,
    ledger,
    journal,
    journalLines,
    tw,
    leases,
    engine,
    clock,
    artifactPath(name: string): string {
      const dir = join(env.root, "artifacts");
      mkdirSync(dir, { recursive: true });
      return join(dir, name);
    },
    cleanup(): void {
      env.cleanup();
    },
  };
}

/** A minimal shell-kind enqueue running the mock `ocr` worker, writing + declaring an artifact. */
export function ocrEnqueue(artifact: string, extra: Partial<EnqueueParams> = {}): EnqueueParams {
  return {
    priority: "low",
    source: "r2",
    kind: "shell",
    argv: ["ocr", artifact],
    evidence: [{ kind: "artifact", path: artifact }, { kind: "exit", code: 0 }],
    ...extra,
  };
}

/**
 * Enqueue and SETTLE the fire-and-forget drive, returning a terminal view of the result. Under the
 * `--detach` default (§1.1a) `engine.enqueue` returns immediately with status `queued`/`dispatched`;
 * this settles the drive and reads the terminal state so a test can assert the terminal verdict/status.
 */
export async function enqueueSettled(h: EngineHarness, params: EnqueueParams): Promise<EnqueueResult & { status: string }> {
  const admitted = await h.engine.enqueue(params);
  if (admitted.status === "reused") return admitted;
  await h.engine.settle();
  const entry =
    admitted.task_uuid !== null
      ? h.engine.getJobByTask(admitted.task_uuid)
      : h.engine.allJobs().find((j) => j.argv.join(" ") === params.argv?.join(" "));
  if (entry === undefined) return admitted;
  const status = entry.state === "completed" ? "completed" : entry.state === "failed" ? "failed" : admitted.status;
  return { ...admitted, status, verdict: entry.verdict, witness_lsn: entry.witness_lsn, lease_epoch: entry.lease_epoch };
}

// ---------------------------------------------------------------------------------------------
// The full daemon harness — a live Bun Unix-socket daemon with the jobs engine mounted, driven by
// the frozen §2 NDJSON client. Used where the brief demands the socket path (recover/barrier over
// the wire, the detector stream a `session watch` client consumes, the cursor-voiding epoch bump).
// ---------------------------------------------------------------------------------------------

export interface DaemonHarness {
  env: TmpEnv;
  exec: FakeExec;
  pls: FakePls;
  task: FakeTask;
  systemd: FakeSystemdRun;
  kitty: FakeKitty;
  ledger: WitnessLedger;
  journal: JournalEmitter;
  journalLines: string[];
  tw: TaskChampion;
  leases: LeaseManager;
  engine: JobsEngine;
  detector: DetectorLoop | null;
  daemon: Daemon;
  socketPath: string;
  /** Connect a fresh §2 NDJSON client to the daemon socket. */
  client(): Promise<SocketClient>;
  artifactPath(name: string): string;
  /** Stop the daemon + close every client (idempotent). */
  stop(): Promise<void>;
  cleanup(): void;
}

/** Options for the full daemon harness. */
export interface DaemonHarnessOptions {
  /** Mount the real detector loop + register it as the WaitScrapeProvider (detector-stream e2e). */
  withDetector?: boolean;
  manifests?: ManifestSet;
  clock?: Clock;
}

/**
 * Boot a live daemon with the jobs engine mounted (and optionally the detector). The daemon uses its
 * OWN `state.bus` — the engine + detector publish onto that bus, and `wireBusToWire` lifts every event
 * to the wire, so a subscribed §2 client sees the same `job.*` / `agent.*` stream the CLI consumes.
 */
export async function bootDaemonHarness(opts: DaemonHarnessOptions = {}): Promise<DaemonHarness> {
  const env = makeTmpEnv();
  const exec = new FakeExec();
  const pls = new FakePls();
  pls.addPool("worker-gpu", { capacity: 1, budget: 128 });
  pls.addPool("controller-gpu", { capacity: 1, budget: 128 });
  pls.install(exec);
  const task = new FakeTask();
  task.install(exec);
  const systemd = new FakeSystemdRun();
  systemd.install(exec);
  const kitty = new FakeKitty();
  kitty.install(exec);
  registerWorkers(exec);

  const clock = opts.clock ?? systemClock;
  // A large heartbeat interval keeps the stream free of heartbeat noise during assertions.
  const daemon = bootDaemon({ env: env.env, config: { ...defaultConfig(), heartbeatMs: 100000 }, clock });

  const ledger = WitnessLedger.open(env.env);
  const journalLines: string[] = [];
  const journal = new JournalEmitter((line) => journalLines.push(line));
  const tw = new TaskChampion({ exec });
  const leases = new LeaseManager(new PlsBroker(exec), PoolRegistry.default());

  const engine = new JobsEngine({
    exec,
    bus: daemon.state.bus,
    tw,
    leases,
    ledger,
    journal,
    clock,
    forceDirect: true,
    costFor: () => 1,
    unitExitDir: unitExitDir(env.env),
  });
  engine.mount(daemon);

  let detector: DetectorLoop | null = null;
  if (opts.withDetector) {
    if (!opts.manifests) throw new Error("bootDaemonHarness: withDetector requires manifests");
    detector = new DetectorLoop({
      exec,
      bus: daemon.state.bus,
      clock,
      manifests: opts.manifests,
      cadence: defaultConfig().detector,
    });
    // The detector is the WaitScrapeProvider daemon-core/wait.ts consults for `session.wait pane_output`.
    daemon.registerWaitScrapeProvider(detector);
    // Ride the supervise host (restart isolation, PS#15a). start() wires its bus subscriptions.
    daemon.registerSupervised(detector);
  }

  await daemon.start();

  const clients: SocketClient[] = [];

  return {
    env,
    exec,
    pls,
    task,
    systemd,
    kitty,
    ledger,
    journal,
    journalLines,
    tw,
    leases,
    engine,
    detector,
    daemon,
    socketPath: daemon.server.socketPath,
    async client(): Promise<SocketClient> {
      const c = await connectClient(daemon.server.socketPath);
      clients.push(c);
      return c;
    },
    artifactPath(name: string): string {
      const dir = join(env.root, "artifacts");
      mkdirSync(dir, { recursive: true });
      return join(dir, name);
    },
    async stop(): Promise<void> {
      for (const c of clients) c.close();
      clients.length = 0;
      await daemon.stop();
    },
    cleanup(): void {
      env.cleanup();
    },
  };
}

/** Announce a pane onto a bus the way session-model would (so the detector learns it). */
export function announcePane(
  bus: DaemonBus,
  opts: { pane_id: string; session_id: string; kitty_window_id: number; is_viewer?: boolean },
): void {
  bus.emit("session.observed", {
    session_id: opts.session_id,
    workspace_id: "ws1",
    persistence_session_id: `term-${opts.session_id}`,
    backend: "zmx",
    observed_at: "2026-07-09T00:00:00.000Z",
  });
  bus.emit("pane.created", {
    pane_id: opts.pane_id,
    session_id: opts.session_id,
    kitty_window_id: opts.kitty_window_id,
    cwd: "/home/tom",
    is_viewer: opts.is_viewer ?? false,
  });
}

/** Sleep a few real event-loop ticks (for async socket round-trips / bus fan-out). */
export function tick(ms = 15): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
