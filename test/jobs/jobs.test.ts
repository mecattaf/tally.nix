// test/jobs/jobs.test.ts
//
// Tests for the jobs module (M2.2) — the spawn-tracked-agent-job, the one execution primitive. The
// brief's required cases:
//   - the full happy path against fakes (enqueue → lease → dispatch → evidence → witness → complete);
//   - dedup skip + re-hash-on-mismatch (dedup-by-existence);
//   - evidence-fail forensics (clean exit, no artifact ⇒ clean-exit-no-artifact + evidence_fail);
//   - recover() torn/ACK/fence/bounded matrices (re-present, never replay; five invariants);
//   - preempt-yield-resume (cooperative yield above the non-preemptible lease);
//   - barrier N-of-M (enqueue-N-await-N = wait_for_subagents);
//   - row-vs-no-row admission (durable-row admission);
//   - the OCR-shaped batch (many enqueues, one lease, artifacts + witness lines end-to-end).
//
// All subprocess access is via the layer-0 fakes (FakeExec + FakeTask + FakePls + FakeSystemdRun);
// the in-daemon Bus is daemon-core's DaemonBus; the witness ledger + journald emitter write to a tmp
// tree. Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { FakeExec, type ExecResult, ok } from "../helpers/exec-fakes.ts";
import { FakePls } from "../helpers/fake-pls.ts";
import { FakeTask } from "../helpers/fake-task.ts";
import { FakeSystemdRun } from "../helpers/fake-systemd.ts";
import { makeTmpEnv, readLedger, type TmpEnv } from "../helpers/tmp.ts";

import { DaemonBus } from "../../src/daemon/state.ts";
import { systemClock, type EnqueueParams, type EnqueueResult, type EventName } from "../../src/contracts/index.ts";
import { PoolRegistry, PlsBroker, LeaseManager } from "../../src/pls/index.ts";
import { WitnessLedger } from "../../src/witness/index.ts";
import { JournalEmitter } from "../../src/journal/index.ts";
import { TaskChampion } from "../../src/tw/index.ts";
import {
  JobsEngine,
  TransientRunner,
  runEvidenceGate,
  probeDedup,
  ArrayWitnessSource,
  splitInvocation,
  shouldPreempt,
  planRecovery,
  admit,
  rowSeedFor,
} from "../../src/jobs/index.ts";

// ---------------------------------------------------------------------------------------------
// Harness.
// ---------------------------------------------------------------------------------------------

/** A bus that records every emitted event for assertions. */
class RecordingBus extends DaemonBus {
  readonly events: Array<{ event: EventName; payload: unknown }> = [];
  constructor() {
    super();
    this.onAny((e) => this.events.push({ event: e.event, payload: e.payload }));
  }
  ofType(name: EventName): unknown[] {
    return this.events.filter((e) => e.event === name).map((e) => e.payload);
  }
}

/**
 * A manual clock: `now()` is fixed unless advanced; `setTimer` records the pending callbacks so a test
 * can fire them deterministically (used to drive the throttled `job.heartbeat` cadence without waiting
 * 15 real seconds). `setInterval` is unused by these tests.
 */
class ManualClock {
  private t = 0;
  readonly timers: Array<{ at: number; fn: () => void; cancelled: boolean }> = [];
  now(): number {
    return this.t;
  }
  nowIso(): string {
    return new Date(this.t).toISOString();
  }
  setTimer(ms: number, fn: () => void): () => void {
    const entry = { at: this.t + ms, fn, cancelled: false };
    this.timers.push(entry);
    return () => {
      entry.cancelled = true;
    };
  }
  setInterval(_ms: number, _fn: () => void): () => void {
    return () => {};
  }
  sleep(_ms: number): Promise<void> {
    return Promise.resolve();
  }
  /** Fire every pending (non-cancelled) timer whose deadline is <= t+ms, advancing time to t+ms. */
  advance(ms: number): void {
    this.t += ms;
    const due = this.timers.filter((e) => !e.cancelled && e.at <= this.t);
    for (const e of due) {
      e.cancelled = true;
      e.fn();
    }
  }
}

interface Harness {
  env: TmpEnv;
  exec: FakeExec;
  pls: FakePls;
  task: FakeTask;
  systemd: FakeSystemdRun;
  bus: RecordingBus;
  ledger: WitnessLedger;
  journal: JournalEmitter;
  tw: TaskChampion;
  leases: LeaseManager;
  journalLines: string[];
  engine: JobsEngine;
  clock: { now(): number; nowIso(): string; advance?(ms: number): void };
  /** Absolute path under the tmp tree for a per-test artifact. */
  artifactPath(name: string): string;
}

function makeHarness(opts: { forceDirect?: boolean; cost?: number; clock?: ManualClock } = {}): Harness {
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

  // A mock OCR-shaped worker: `ocr <out-path> [<content>]` writes the artifact and exits 0.
  exec.register("ocr", (args): ExecResult => {
    const out = args[0]!;
    const content = args[1] ?? "OCR-OUTPUT";
    mkdirSync(join(out, ".."), { recursive: true });
    writeFileSync(out, content, "utf8");
    return ok("pi:session=sess-ocr\npi:model=claude-sonnet\nwrote " + out);
  });
  // A worker that exits clean but writes NOTHING (evidence-fail forensic).
  exec.register("noop", (): ExecResult => ok("did nothing"));
  // A worker that exits non-zero (plain failure).
  exec.register("boom", (): ExecResult => ({ code: 7, stdout: "", stderr: "boom" }));

  const bus = new RecordingBus();
  const ledger = WitnessLedger.open(env.env);
  const journalLines: string[] = [];
  const journal = new JournalEmitter((line) => journalLines.push(line));
  const tw = new TaskChampion({ exec });
  const registry = PoolRegistry.default();
  const leases = new LeaseManager(new PlsBroker(exec), registry);

  const clock = opts.clock ?? systemClock;
  const engine = new JobsEngine({
    exec,
    bus,
    tw,
    leases,
    ledger,
    journal,
    clock,
    forceDirect: opts.forceDirect ?? false,
    costFor: () => opts.cost ?? 1,
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
    tw,
    leases,
    journalLines,
    engine,
    clock: clock as Harness["clock"],
    artifactPath: (name: string) => {
      const dir = join(env.root, "artifacts");
      mkdirSync(dir, { recursive: true });
      return join(dir, name);
    },
  };
}

/**
 * Enqueue and SETTLE the drive, returning a terminal view of the result. Under the `--detach` default
 * (§1.1a) `engine.enqueue` returns immediately with status `queued`/`dispatched`; a caller wanting the
 * completion settles the fire-and-forget drive and reads the terminal state. This helper does exactly
 * that so a test can assert the terminal verdict/status/witness_lsn without re-implementing the wait.
 */
async function enqueueSettled(
  h: Harness,
  params: EnqueueParams,
): Promise<EnqueueResult & { status: string }> {
  const admitted = await h.engine.enqueue(params);
  if (admitted.status === "reused") return admitted; // dedup hit — no run to settle
  await h.engine.settle();
  // Read the terminal entry (by task_uuid when durable, else the sole non-reused job just admitted).
  const entry =
    admitted.task_uuid !== null
      ? h.engine.getJobByTask(admitted.task_uuid)
      : h.engine.allJobs().find((j) => j.argv.join(" ") === params.argv?.join(" "));
  if (entry === undefined) return admitted;
  const status = entry.state === "completed" ? "completed" : entry.state === "failed" ? "failed" : admitted.status;
  return { ...admitted, status, verdict: entry.verdict, witness_lsn: entry.witness_lsn, lease_epoch: entry.lease_epoch };
}

/** A minimal shell-kind enqueue running the mock `ocr` worker, writing an artifact + declaring it. */
function ocrEnqueue(_h: Harness, artifact: string, extra: Partial<EnqueueParams> = {}): EnqueueParams {
  return {
    priority: "low",
    source: "r2",
    kind: "shell",
    argv: ["ocr", artifact],
    evidence: [{ kind: "artifact", path: artifact }, { kind: "exit", code: 0 }],
    ...extra,
  };
}

// ---------------------------------------------------------------------------------------------
// Pure-unit tests (adapters, dedup, evidence, preempt, recover planning).
// ---------------------------------------------------------------------------------------------

describe("agent adapters — leaf invocation build (PS#2: model declared, never re-picked)", () => {
  test("shell adapter is the identity leaf (no model, no session_ref)", () => {
    const a = admit({
      params: { priority: "low", source: "r2", kind: "shell", argv: ["ocr", "/tmp/x"] },
      taskUuid: null,
      leaseEpoch: 1,
    });
    expect(a.leaf.argv).toEqual(["ocr", "/tmp/x"]);
    expect(a.leaf.model).toBeNull();
    expect(a.leaf.sessionRef).toBeNull();
    expect(a.hasRow).toBe(false);
  });

  test("pi adapter binds --session for a resume and carries --model-class verbatim", () => {
    const a = admit({
      params: {
        priority: "high",
        source: "orchestrator",
        kind: "pi",
        invocation: "run the batch",
        session: "term-0707",
        model_class: "claude-sonnet",
      },
      taskUuid: null,
      leaseEpoch: 3,
    });
    expect(a.leaf.argv).toContain("--session");
    expect(a.leaf.argv).toContain("term-0707");
    expect(a.leaf.argv).toContain("--model-class");
    expect(a.leaf.argv).toContain("claude-sonnet");
    // Model normalized to a models.dev id.
    expect(a.leaf.model).toBe("anthropic/claude-sonnet");
    expect(a.leaf.sessionRef).toBe("term-0707");
  });

  test("claude-code adapter uses --resume; splitInvocation is quote-aware", () => {
    const a = admit({
      params: { priority: "medium", source: "manual", kind: "claude-code", invocation: `claude-cmd --flag "a b"`, session: "cc-1" },
      taskUuid: null,
      leaseEpoch: 1,
    });
    expect(a.leaf.argv[0]).toBe("claude");
    expect(a.leaf.argv).toContain("--resume");
    expect(a.leaf.argv).toContain("cc-1");
    expect(splitInvocation(`a "b c" d`)).toEqual(["a", "b c", "d"]);
  });
});

describe("transient-unit argv — the durable ExecStopPost exit record (issue #3)", () => {
  test("exitFile adds an ExecStopPost persisting $EXIT_STATUS past the `--collect` unload", () => {
    const runner = new TransientRunner(new FakeExec());
    const argv = runner.buildSystemdRunArgv(["ocr", "/tmp/x"], {
      unit: "tally-job-u1",
      env: {},
      exitFile: "/state/unit-exit/tally-job-u1.exit",
    });
    // `--collect` GC's the unit the moment it stops — the record is what survives for recovery.
    expect(argv).toContain("--collect");
    const propIdx = argv.indexOf("--property");
    expect(propIdx).toBeGreaterThan(-1);
    const prop = argv[propIdx + 1]!;
    expect(prop.startsWith("ExecStopPost=/bin/sh -c ")).toBe(true);
    // `$$`, not `$`: systemd must NOT expand the variable at Exec-line parse time — the shell
    // reads $EXIT_STATUS from the env systemd sets for stop-post commands.
    expect(prop).toContain('"$$EXIT_STATUS"');
    // Temp-then-rename so a torn write is never read as a status (PS#10 discipline).
    expect(prop).toContain('"/state/unit-exit/tally-job-u1.exit.tmp"');
    expect(prop).toContain('> "/state/unit-exit/tally-job-u1.exit.tmp" && mv');
  });

  test("no exitFile ⇒ no ExecStopPost property (an engine without a unit-exit dir)", () => {
    const runner = new TransientRunner(new FakeExec());
    const argv = runner.buildSystemdRunArgv(["ocr", "/tmp/x"], { unit: "tally-job-u1", env: {} });
    expect(argv).not.toContain("--property");
  });
});

describe("evidence gate — artifact ∧ hash ∧ exit ∧ span (SPEC 'Evidence gate')", () => {
  const tmp = makeTmpEnv();
  afterEach(() => {
    /* keep tmp across the describe; cleaned at process exit */
  });

  test("pass: artifact exists, exit ok", () => {
    const p = join(tmp.root, "art-pass.txt");
    writeFileSync(p, "content");
    const g = runEvidenceGate({ exitCode: 0, wallClockSeconds: 1, evidence: [{ kind: "artifact", path: p }, { kind: "exit", code: 0 }] });
    expect(g.passed).toBe(true);
    expect(g.verdict).toBe("pass");
    expect(g.artifactHash).toMatch(/^sha256:/);
    expect(g.cleanExitNoArtifact).toBe(false);
  });

  test("clean-exit-no-artifact forensic: exit 0 but declared artifact missing", () => {
    const missing = join(tmp.root, "does-not-exist.txt");
    const g = runEvidenceGate({ exitCode: 0, wallClockSeconds: 1, evidence: [{ kind: "artifact", path: missing }, { kind: "exit", code: 0 }] });
    expect(g.passed).toBe(false);
    expect(g.verdict).toBe("clean-exit-no-artifact");
    expect(g.cleanExitNoArtifact).toBe(true);
  });

  test("failed: non-zero exit is a plain failure, not the forensic", () => {
    const g = runEvidenceGate({ exitCode: 7, wallClockSeconds: 1, evidence: [{ kind: "exit", code: 0 }] });
    expect(g.verdict).toBe("failed");
    expect(g.cleanExitNoArtifact).toBe(false);
  });

  test("hash mismatch against a declared value fails the gate", () => {
    const p = join(tmp.root, "art-hash.txt");
    writeFileSync(p, "abc");
    const g = runEvidenceGate({
      exitCode: 0,
      wallClockSeconds: 1,
      evidence: [{ kind: "artifact", path: p }, { kind: "hash", algo: "sha256", value: "deadbeef" }, { kind: "exit", code: 0 }],
    });
    expect(g.passed).toBe(false);
  });
});

describe("preemption-as-policy — cooperative yield above the non-preemptible lease", () => {
  test("shouldPreempt yields a running lower-priority holder for a higher-priority requester", () => {
    const requester = admit({ params: { priority: "high", source: "orchestrator", kind: "pi", argv: ["x"] }, taskUuid: null, leaseEpoch: 1 }).entry;
    const holder = admit({ params: { priority: "low", source: "r2", kind: "shell", argv: ["ocr", "/tmp/y"] }, taskUuid: null, leaseEpoch: 1 }).entry;
    holder.state = "started";
    const d = shouldPreempt(requester, [holder]);
    expect(d).not.toBeNull();
    expect(d!.holder.job_id).toBe(holder.job_id);
  });

  test("no preemption when no strictly-lower-priority holder exists on the pool", () => {
    const requester = admit({ params: { priority: "low", source: "r2", kind: "shell", argv: ["x"] }, taskUuid: null, leaseEpoch: 1 }).entry;
    const holder = admit({ params: { priority: "low", source: "r2", kind: "shell", argv: ["y"] }, taskUuid: null, leaseEpoch: 1 }).entry;
    holder.state = "started";
    expect(shouldPreempt(requester, [holder])).toBeNull();
  });

  test("engine.maybePreempt signals SIGUSR1 to the holder and marks it yield-requested (never transitioning the still-running holder)", async () => {
    const h = makeHarness({ forceDirect: true });
    try {
      // Record systemctl/kill signals sent.
      const signals: string[][] = [];
      h.exec.register("systemctl", (args): ExecResult => {
        signals.push([...args]);
        return ok(); // unit signalled
      });

      // Pause admission so we can stage a running low-priority holder + a high-priority requester.
      const handlers = mountHandlers(h.engine);
      await handlers.get("queue.pause")!({});
      const holderRes = (await handlers.get("queue.enqueue")!(
        ocrEnqueue(h, h.artifactPath("held.txt"), { source: "r2", priority: "low" }),
      )) as { task_uuid: string };
      const holder = h.engine.getJobByTask(holderRes.task_uuid)!;
      // Stage the holder as a running lease holder on worker-gpu.
      holder.state = "started";
      holder.unit = "tally-job-held";
      holder.session_ref = "sess-ocr";
      holder.lease_id = "lease-held";

      // A high-priority requester on the same pool.
      const requester = admit({ params: { priority: "high", source: "orchestrator", kind: "pi", argv: ["x"] }, taskUuid: null, leaseEpoch: 1 }).entry;

      const preempted = await h.engine.maybePreempt(requester);
      expect(preempted).toBe(true);
      // A cooperative yield signal (SIGUSR1) was sent to the holder's unit — never a KILL.
      expect(signals.length).toBe(1);
      expect(signals[0]!.join(" ")).toContain("SIGUSR1");
      // The holder is MARKED yield-requested but is NOT transitioned here: its own in-flight runJob
      // owns the preempted terminal (transitioning a still-executing holder to `preempted` here is
      // exactly what made its later commit() throw an illegal transition and abort the drive loop).
      const after = h.engine.getJobByTask(holderRes.task_uuid)!;
      expect((after as { yieldRequested?: boolean }).yieldRequested).toBe(true);
      expect(after.state).toBe("started");
      // A second maybePreempt for the same holder does not re-signal (already asked to yield).
      expect(await h.engine.maybePreempt(requester)).toBe(false);
      expect(signals.length).toBe(1);
    } finally {
      h.env.cleanup();
    }
  });

  test("a high-priority enqueue preempts a running lower-priority holder end-to-end (yield → resume)", async () => {
    const h = makeHarness({ forceDirect: true });
    try {
      // A low-priority holder whose `slow-ocr` worker blocks until it is signalled to yield (the
      // systemctl SIGUSR1 resolves the gate), then writes its artifact and exits — modelling a
      // cooperative-checkpoint OCR page boundary.
      let releaseHolder: (() => void) | null = null;
      const held = new Promise<void>((resolve) => (releaseHolder = resolve));
      h.exec.register("systemctl", (args): ExecResult => {
        if (args.includes("kill") && releaseHolder) {
          releaseHolder();
          releaseHolder = null;
        }
        return ok();
      });
      const holderArtifact = h.artifactPath("slow-held.txt");
      let holderRuns = 0;
      h.exec.register("slow-ocr", async (args): Promise<ExecResult> => {
        holderRuns++;
        if (holderRuns === 1) await held; // first run blocks until the yield signal arrives
        writeFileSync(args[0]!, "HELD", "utf8");
        return ok("done");
      });

      const holderRes = await h.engine.enqueue({
        priority: "low",
        source: "r2",
        kind: "shell",
        argv: ["slow-ocr", holderArtifact],
        evidence: [{ kind: "artifact", path: holderArtifact }],
      });
      // Wait until the holder has dispatched + started (it then blocks in execute()); preemption only
      // fires against a RUNNING holder, so the high-priority enqueue must land after this point.
      for (let i = 0; i < 200 && h.engine.getJobByTask(holderRes.task_uuid!)?.state !== "started"; i++) {
        await new Promise((r) => setTimeout(r, 5));
      }
      expect(h.engine.getJobByTask(holderRes.task_uuid!)?.state).toBe("started");

      // A high-priority enqueue on the same pool signals the holder to yield (preemption-as-policy).
      const hiArtifact = h.artifactPath("hi.txt");
      await h.engine.enqueue({
        priority: "high",
        source: "orchestrator",
        kind: "shell",
        argv: ["ocr", hiArtifact],
        evidence: [{ kind: "artifact", path: hiArtifact }],
      });
      await h.engine.settle();

      // The high-priority job ran, and the preempted holder was re-presented (job.preempted fired) and
      // resumed to completion (labor_class:recovered).
      expect(existsSync(hiArtifact)).toBe(true);
      expect(h.bus.ofType("job.preempted").length).toBeGreaterThanOrEqual(1);
      const holderEntry = h.engine.getJobByTask(holderRes.task_uuid!)!;
      expect(holderEntry.labor_class).toBe("recovered");
    } finally {
      h.env.cleanup();
    }
  });
});

// ---------------------------------------------------------------------------------------------
// Engine end-to-end tests.
// ---------------------------------------------------------------------------------------------

describe("happy path — enqueue → lease → dispatch → evidence → witness → complete", () => {
  let h: Harness;
  beforeEach(() => {
    h = makeHarness({ forceDirect: true });
  });
  afterEach(() => h.env.cleanup());

  test("a shell OCR job runs to completed, writes the artifact, and chains a witness line", async () => {
    const artifact = h.artifactPath("page-1.txt");
    const res = await enqueueSettled(h, ocrEnqueue(h, artifact));

    expect(existsSync(artifact)).toBe(true);
    expect(res.status).toBe("completed");
    expect(res.verdict).toBe("pass");
    expect(res.witness_lsn).toBe(1);
    expect(res.task_uuid).not.toBeNull(); // r2 source is autonomous ⇒ durable row admitted

    // Witness line chained + verifiable.
    const lines = readLedger(h.ledger.filePath);
    expect(lines.length).toBe(1);
    expect(lines[0]!.verdict).toBe("pass");
    expect(lines[0]!.labor_class).toBe("fresh");
    expect(lines[0]!.artifact_content_hash).toMatch(/^sha256:/);
    // shell run ⇒ no model on the witness line.
    expect(lines[0]!.model).toBeNull();

    // The one-vocabulary lifecycle deltas all fired.
    expect(h.bus.ofType("job.enqueued").length).toBe(1);
    expect(h.bus.ofType("job.dispatched").length).toBe(1);
    expect(h.bus.ofType("job.started").length).toBe(1);
    expect(h.bus.ofType("job.evidence_pass").length).toBe(1);
    expect(h.bus.ofType("job.witness_emitted").length).toBe(1);
    expect(h.bus.ofType("job.completed").length).toBe(1);

    // journald mirrored the same vocabulary.
    const events = h.journalLines.map((l) => JSON.parse(l).TALLY_EVENT);
    expect(events).toContain("enqueued");
    expect(events).toContain("completed");

    // The TW row completed with trust:unreviewed.
    const row = h.task.get(res.task_uuid!);
    expect(row?.status).toBe("completed");
    expect(row?.trust).toBe("unreviewed");
  });

  test("a running job emits the throttled job.heartbeat wire event + journald twin (§2.3)", async () => {
    // A manual clock lets us fire the 15s heartbeat cadence deterministically while a worker blocks.
    const clock = new ManualClock();
    const hb = makeHarness({ forceDirect: true, clock });
    try {
      let releaseWorker: (() => void) | null = null;
      const running = new Promise<void>((r) => (releaseWorker = r));
      hb.exec.register("hb-worker", async (args): Promise<ExecResult> => {
        await running; // block so the job stays 'started' while we advance the clock
        writeFileSync(args[0]!, "DONE", "utf8");
        return ok();
      });
      const artifact = hb.artifactPath("hb.txt");
      const res = await hb.engine.enqueue({
        priority: "low",
        source: "r2",
        kind: "shell",
        argv: ["hb-worker", artifact],
        evidence: [{ kind: "artifact", path: artifact }],
      });
      // Let the job reach 'started' (it then blocks in execute()).
      for (let i = 0; i < 5 && hb.engine.getJobByTask(res.task_uuid!)?.state !== "started"; i++) {
        await new Promise((r) => setTimeout(r, 5));
      }
      expect(hb.engine.getJobByTask(res.task_uuid!)?.state).toBe("started");
      // No heartbeat before the first cadence tick.
      expect(hb.bus.ofType("job.heartbeat").length).toBe(0);
      // Fire two heartbeat cadences (15s each).
      clock.advance(15_000);
      clock.advance(15_000);
      const beats = hb.bus.ofType("job.heartbeat") as Array<{ job_id: string; gpu_seconds: number }>;
      expect(beats.length).toBe(2);
      expect(typeof beats[0]!.job_id).toBe("string");
      expect(typeof beats[0]!.gpu_seconds).toBe("number");
      // The journald twin fired too.
      const events = hb.journalLines.map((l) => JSON.parse(l).TALLY_EVENT);
      expect(events).toContain("heartbeat");
      // Release the worker + settle; the heartbeat timer is cancelled at terminal (no further beats).
      releaseWorker!();
      await hb.engine.settle();
      clock.advance(15_000);
      expect(hb.bus.ofType("job.heartbeat").length).toBe(2);
    } finally {
      hb.env.cleanup();
    }
  });

  test("the pls lease was acquired before the GPU and released after (single release path)", async () => {
    const artifact = h.artifactPath("page-lease.txt");
    await enqueueSettled(h, ocrEnqueue(h, artifact));
    // After the run, no lease is held (RAII release fired exactly once).
    expect(h.pls.holders("worker-gpu")).toEqual([]);
    // A grant + a release were logged in order.
    const ops = h.pls.log.map((l) => l.op);
    expect(ops).toContain("grant");
    expect(ops).toContain("release");
    expect(ops.indexOf("grant")).toBeLessThan(ops.indexOf("release"));
  });
});

describe("row-vs-no-row admission (durable-row admission appendix)", () => {
  let h: Harness;
  beforeEach(() => {
    h = makeHarness({ forceDirect: true });
  });
  afterEach(() => h.env.cleanup());

  test("an orchestrator-spawned unit gets NO row (task_uuid null) but still a witness line", async () => {
    const artifact = h.artifactPath("live.txt");
    const res = await enqueueSettled(h, ocrEnqueue(h, artifact, { source: "orchestrator" }));
    expect(res.task_uuid).toBeNull();
    // Still emitted a witness line (the ledger is broader than the row set).
    expect(readLedger(h.ledger.filePath).length).toBe(1);
    // No TW row was imported for a rowless unit.
    expect(h.task.tasks().length).toBe(0);
  });

  test("an autonomous (r2) unit gets a durable row", async () => {
    const artifact = h.artifactPath("batch.txt");
    const res = await h.engine.enqueue(ocrEnqueue(h, artifact, { source: "r2" }));
    expect(res.task_uuid).not.toBeNull();
    expect(h.task.tasks().length).toBe(1);
  });

  test("a rowless --wait resolves on ITS OWN job_id, never a stranger's terminal delta (issue #4)", async () => {
    // The enqueue result carries the job_id — the BarrierTracker key every terminal delta is
    // recorded under — because a rowless unit has no task_uuid to wait by. The real engine's
    // terminal payloads carry NO lease_epoch (contracts/events.ts), so the old stream-side epoch
    // fence matched the first terminal delta of ANY job; the exact job_id barrier must not.
    const stranger = await enqueueSettled(h, {
      priority: "low",
      source: "orchestrator",
      kind: "shell",
      argv: ["boom"],
      evidence: [{ kind: "exit", code: 0 }],
    });
    expect(stranger.status).toBe("failed"); // the stranger's terminal delta is already recorded

    const artifact = h.artifactPath("rowless-wait.txt");
    const res = await enqueueSettled(h, ocrEnqueue(h, artifact, { source: "orchestrator" }));
    expect(res.task_uuid).toBeNull();
    expect(typeof res.job_id).toBe("string");

    // The rowless job's own wait mirrors ITS verdict (pass ⇒ exit 0), not the stranger's failure —
    // and it resolves from the drained already-terminal delta (no subscribe race).
    const own = await h.engine.waitForJobId(res.job_id!, 1000);
    expect(own.timedOut).toBe(false);
    expect(own.verdict).toBe("pass");
    expect(own.exitCode).toBe(0);

    // A wait keyed on an id nothing terminal matches TIMES OUT instead of grabbing a stranger.
    const nobody = await h.engine.waitForJobId("job-never-admitted", 50);
    expect(nobody.timedOut).toBe(true);
  });

  test("the durable row persists the verbatim argv + evidence spec as JSON UDAs (issue #2)", async () => {
    const artifact = h.artifactPath("uda.txt");
    const res = await enqueueSettled(h, ocrEnqueue(h, artifact));
    const row = h.task.get(res.task_uuid!)!;
    // The row can reconstruct the job's TRUE identity after a crash — verbatim, never a lossy join.
    expect(JSON.parse(row.argv_json as string)).toEqual(["ocr", artifact]);
    expect(JSON.parse(row.evidence_json as string)).toEqual([
      { kind: "artifact", path: artifact },
      { kind: "exit", code: 0 },
    ]);
  });
});

describe("evidence-fail forensics (clean exit, no artifact)", () => {
  let h: Harness;
  beforeEach(() => {
    h = makeHarness({ forceDirect: true });
  });
  afterEach(() => h.env.cleanup());

  test("a clean-exit run with a missing declared artifact ⇒ clean-exit-no-artifact + evidence_fail", async () => {
    const missing = h.artifactPath("never-written.txt");
    const res = await enqueueSettled(h, {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["noop"],
      evidence: [{ kind: "artifact", path: missing }, { kind: "exit", code: 0 }],
    });
    expect(res.status).toBe("failed");
    expect(res.verdict).toBe("clean-exit-no-artifact");
    expect(h.bus.ofType("job.evidence_fail").length).toBe(1);
    expect(h.bus.ofType("job.evidence_pass").length).toBe(0);

    const line = readLedger(h.ledger.filePath)[0]!;
    expect(line.verdict).toBe("clean-exit-no-artifact");
    // The forensic line is EXCLUDED from canonical GPU-seconds (labor_class fresh but verdict fails).
    const journalEvents = h.journalLines.map((l) => JSON.parse(l).TALLY_EVENT);
    expect(journalEvents).toContain("evidence_fail");
  });
});

describe("dedup-by-existence (SPEC 'Dedup-by-existence')", () => {
  let h: Harness;
  beforeEach(() => {
    h = makeHarness({ forceDirect: true });
  });
  afterEach(() => h.env.cleanup());

  test("second enqueue with the same dedup-key + present artifact SKIPS the GPU run (status reused)", async () => {
    const artifact = h.artifactPath("ocr-dedup.txt");
    const first = await enqueueSettled(h, ocrEnqueue(h, artifact, { dedup_key: "sidecar-42" }));
    expect(first.status).toBe("completed");
    const witnessCountAfterFirst = readLedger(h.ledger.filePath).length;

    // Re-enqueue the SAME work: the artifact + success witness exist for the key ⇒ reuse.
    const second = await enqueueSettled(h, ocrEnqueue(h, artifact, { dedup_key: "sidecar-42" }));
    expect(second.status).toBe("reused");
    expect(second.verdict).toBe("reused");
    // No new witness line, no new lease grant for the skipped run.
    expect(readLedger(h.ledger.filePath).length).toBe(witnessCountAfterFirst);
  });

  test("MULTI-ARTIFACT dedup: a 2-artifact witnessed pass dedups on re-enqueue (gate + probe agree on the combined hash)", async () => {
    // A worker that writes TWO artifacts (the multi-page OCR shape). Both are declared as evidence.
    const a1 = h.artifactPath("page-a.txt");
    const a2 = h.artifactPath("page-b.txt");
    h.exec.register("ocr2", (args): ExecResult => {
      mkdirSync(join(args[0]!, ".."), { recursive: true });
      writeFileSync(args[0]!, "AAA", "utf8");
      writeFileSync(args[1]!, "BBB", "utf8");
      return ok("wrote 2");
    });
    const params: EnqueueParams = {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["ocr2", a1, a2],
      dedup_key: "multi-1",
      evidence: [
        { kind: "artifact", path: a1 },
        { kind: "artifact", path: a2 },
        { kind: "exit", code: 0 },
      ],
    };
    const first = await enqueueSettled(h, params);
    expect(first.status).toBe("completed");
    const witnessAfterFirst = readLedger(h.ledger.filePath).length;
    expect(witnessAfterFirst).toBe(1);

    // Re-enqueue the SAME 2-artifact work: the combined hash the dedup probe recomputes MUST equal the
    // one the evidence gate witnessed (hash-of-per-file-hashes) — otherwise a multi-artifact dedup can
    // never hit and the expensive GPU run silently re-executes. This is the defect being guarded.
    const second = await enqueueSettled(h, params);
    expect(second.status).toBe("reused");
    expect(second.verdict).toBe("reused");
    // No new witness line: the run was genuinely skipped.
    expect(readLedger(h.ledger.filePath).length).toBe(witnessAfterFirst);
  });

  test("re-hash-on-mismatch: a changed artifact is NOT a hit (the prior proof no longer describes it)", async () => {
    const artifact = h.artifactPath("ocr-changed.txt");
    await enqueueSettled(h, ocrEnqueue(h, artifact, { dedup_key: "sidecar-99" }));

    // Mutate the artifact after it was witnessed — its content hash no longer matches.
    writeFileSync(artifact, "TAMPERED CONTENT");

    const source = new ArrayWitnessSource(
      readLedger(h.ledger.filePath).map((r) => r as unknown as import("../../src/contracts/index.ts").WitnessRecord),
    );
    const probe = probeDedup({ dedupKey: "sidecar-99", evidence: [{ kind: "artifact", path: artifact }], witness: source });
    expect(probe.hit).toBe(false);
    expect(probe.rehashed).toBe(true);
  });

  test("no dedup key ⇒ never a hit (nothing to match)", async () => {
    const artifact = h.artifactPath("no-key.txt");
    writeFileSync(artifact, "x");
    const probe = probeDedup({ dedupKey: null, evidence: [{ kind: "artifact", path: artifact }], witness: new ArrayWitnessSource([]) });
    expect(probe.hit).toBe(false);
  });
});

describe("barrier N-of-M (enqueue-N-await-N = wait_for_subagents)", () => {
  let h: Harness;
  beforeEach(() => {
    h = makeHarness({ forceDirect: true });
  });
  afterEach(() => h.env.cleanup());

  test("enqueue 3 barrier jobs then await 3 terminal deltas off one stream", async () => {
    const group = "crew-1";
    for (let i = 0; i < 3; i++) {
      await enqueueSettled(h, ocrEnqueue(h, h.artifactPath(`barrier-${i}.txt`), { barrier: group }));
    }
    // All three terminal after the drives settle ⇒ the wait resolves at once (drains prior terminals).
    const r = await h.engine.waitForBarrier(group, 3, 1000);
    expect(r.timedOut).toBe(false);
    expect(r.satisfied).toBe(3);
    expect(r.exitCode).toBe(0);
  });

  test("--wait on a single job mirrors the verdict as the exit code", async () => {
    const res = await h.engine.enqueue(ocrEnqueue(h, h.artifactPath("wait-1.txt"), { source: "r2", wait: true }));
    await h.engine.settle();
    const w = await h.engine.waitForJob(res.task_uuid!, 1000);
    expect(w.timedOut).toBe(false);
    expect(w.verdict).toBe("pass");
    expect(w.exitCode).toBe(0);
  });
});

describe("the OCR-shaped batch — many enqueues, one lease, artifacts + witness lines end-to-end", () => {
  let h: Harness;
  beforeEach(() => {
    h = makeHarness({ forceDirect: true });
  });
  afterEach(() => h.env.cleanup());

  test("N sidecars run serialized on the single worker-gpu lease, each witnessed and deduped on re-run", async () => {
    const N = 12;
    const artifacts: string[] = [];
    for (let i = 0; i < N; i++) {
      const a = h.artifactPath(`sidecar-${i}.txt`);
      artifacts.push(a);
      const res = await enqueueSettled(h, ocrEnqueue(h, a, { dedup_key: `sidecar-${i}` }));
      expect(res.status).toBe("completed");
    }
    // Every artifact exists; exactly N witness lines, chained.
    for (const a of artifacts) expect(existsSync(a)).toBe(true);
    const lines = readLedger(h.ledger.filePath);
    expect(lines.length).toBe(N);
    // seq is monotone 1..N.
    expect(lines.map((l) => l.seq)).toEqual(Array.from({ length: N }, (_, i) => i + 1));
    // Each line's prev_hash chains to the prior line's hash.
    for (let i = 1; i < lines.length; i++) {
      expect(lines[i]!.prev_hash).toBe(lines[i - 1]!.hash);
    }
    // At no point were two leases held at once (single-lease pool serialized the batch).
    expect(h.pls.holders("worker-gpu")).toEqual([]);

    // Re-run the whole batch: every sidecar is a dedup hit (reused), no new witness lines.
    for (let i = 0; i < N; i++) {
      const res = await enqueueSettled(h, ocrEnqueue(h, artifacts[i]!, { dedup_key: `sidecar-${i}` }));
      expect(res.status).toBe("reused");
    }
    expect(readLedger(h.ledger.filePath).length).toBe(N); // no growth
  });
});

describe("recover() — re-present, never replay (five invariants)", () => {
  let h: Harness;
  beforeEach(() => {
    h = makeHarness({ forceDirect: true });
  });
  afterEach(() => h.env.cleanup());

  test("invariant 1 — witness head reconciles cleanly when the applied lsn matches the head", async () => {
    const plan = await planRecovery({
      tw: h.tw,
      witnessHead: { seq: 5, hash: "sha256:abc" },
      lastAppliedLsn: 5,
      currentEpoch: 10,
      liveLeases: [],
      ackedTaskUuids: new Set(),
    });
    expect(plan.witnessReconciled).toBe(true);
    expect(plan.represent).toEqual([]);
  });

  test("invariant 3 — a lease from a PRIOR epoch is fenced (reclaimed); a current-epoch lease is left held", async () => {
    const plan = await planRecovery({
      tw: h.tw,
      witnessHead: { seq: 0, hash: "sha256:0" },
      lastAppliedLsn: 0,
      currentEpoch: 10,
      liveLeases: [
        { pool: "worker-gpu", lease_id: "old", lease_epoch: 4, holderless: false },
        { pool: "worker-gpu", lease_id: "cur", lease_epoch: 10, holderless: false },
      ],
      ackedTaskUuids: new Set(),
    });
    expect(plan.reclaim.map((r) => r.lease_id)).toEqual(["old"]);
  });

  test("invariant 2 — an acked in-flight row is NOT re-presented", async () => {
    // Seed an unfinished pending row.
    h.task.seed({ uuid: "u-acked", description: "batch", status: "pending", agent: "shell", source: "r2", priority_class: "low", attempt: 1 });
    const plan = await planRecovery({
      tw: h.tw,
      witnessHead: { seq: 0, hash: "sha256:0" },
      lastAppliedLsn: 0,
      currentEpoch: 1,
      liveLeases: [],
      ackedTaskUuids: new Set(["u-acked"]),
    });
    expect(plan.represent.find((r) => r.task_uuid === "u-acked")).toBeUndefined();
  });

  test("invariant 4 + 5 — an undeleted un-acked in-budget row is re-presented (recovered); an over-cap row is abandoned", async () => {
    h.task.seed({ uuid: "u-fresh", description: "resume me", status: "pending", agent: "pi", source: "orchestrator", priority_class: "high", attempt: 1, session_ref: "sess-x" });
    h.task.seed({ uuid: "u-cap", description: "too many tries", status: "pending", agent: "shell", source: "r2", priority_class: "low", attempt: 5 });
    const plan = await planRecovery({
      tw: h.tw,
      witnessHead: { seq: 0, hash: "sha256:0" },
      lastAppliedLsn: 0,
      currentEpoch: 1,
      liveLeases: [],
      ackedTaskUuids: new Set(),
      attemptCap: 5,
    });
    const rep = plan.represent.find((r) => r.task_uuid === "u-fresh");
    expect(rep).toBeDefined();
    expect(rep!.labor_class).toBe("recovered");
    expect(rep!.session_ref).toBe("sess-x");
    expect(rep!.attempt).toBe(2);
    // u-cap would be attempt 6 > cap 5 ⇒ abandoned.
    expect(plan.abandon.map((a) => a.task_uuid)).toContain("u-cap");
  });

  test("rowSeedFor persists the verbatim argv + evidence spec; description stays a cosmetic label", () => {
    const params: EnqueueParams = {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["bash", "-c", "sleep $1; touch done", "tally-val", "25"],
      evidence: [{ kind: "artifact", path: "/tmp/out.txt" }, { kind: "exit", code: 0 }],
    };
    const seed = rowSeedFor(params, "u-seed", 3);
    // The true argv survives JSON-encoded — NEVER round-tripped through the joined description
    // (which destroys quoting: `bash -c sleep $1; touch done ...` re-tokenizes as `bash -c sleep`).
    expect(JSON.parse(seed.argv_json)).toEqual(params.argv);
    expect(JSON.parse(seed.evidence_json!)).toEqual(params.evidence);
    // description remains the lossy human-facing join — display only, never re-parsed.
    expect(seed.description).toBe("bash -c sleep $1; touch done tally-val 25");
  });

  test("engine.recover() re-presents a pending row and runs it to completion (labor_class recovered)", async () => {
    const artifact = h.artifactPath("recovered.txt");
    // A pending row describing a runnable OCR command.
    h.task.seed({
      uuid: "u-run",
      description: "ocr " + artifact,
      status: "pending",
      agent: "shell",
      source: "r2",
      priority_class: "low",
      attempt: 1,
    });
    await h.engine.recover();
    expect(existsSync(artifact)).toBe(true);
    const lines = readLedger(h.ledger.filePath);
    expect(lines.length).toBe(1);
    expect(lines[0]!.labor_class).toBe("recovered");
    expect(lines[0]!.attempt).toBe(2);
  });
});

/** Capture the RPC handlers a JobsEngine registers on mount (the queue.* carriers). */
function mountHandlers(engine: JobsEngine): Map<string, (params: unknown) => Promise<unknown> | unknown> {
  const handlers = new Map<string, (params: unknown) => Promise<unknown> | unknown>();
  engine.mount({
    registerRpc: (method, handler) => handlers.set(method, handler),
    registerWatcher: () => {},
    registerSupervised: () => {},
  });
  return handlers;
}

describe("queue cancel / pause / resume — the admission drain gate (CLI-SURFACE §1.1)", () => {
  let h: Harness;
  beforeEach(() => {
    h = makeHarness({ forceDirect: true });
  });
  afterEach(() => h.env.cleanup());

  test("mount registers the queue.* RPC carriers", () => {
    const handlers = mountHandlers(h.engine);
    expect([...handlers.keys()].sort()).toEqual([
      "queue.await_barrier",
      "queue.await_job",
      "queue.cancel",
      "queue.enqueue",
      "queue.pause",
      "queue.resume",
    ]);
  });

  test("pause drains admission: a job enqueued while paused stays queued until resume runs it", async () => {
    const handlers = mountHandlers(h.engine);
    // Pause the worker pool.
    await handlers.get("queue.pause")!({ pool: "worker-gpu" });

    const artifact = h.artifactPath("gated.txt");
    // Enqueue while paused: admission is drained, so the run does NOT execute and the artifact is absent.
    const res = (await handlers.get("queue.enqueue")!(ocrEnqueue(h, artifact))) as { status: string; task_uuid: string | null };
    expect(res.status).toBe("queued");
    expect(existsSync(artifact)).toBe(false);
    expect(h.engine.queueDepth("worker-gpu")).toBeGreaterThan(0);

    // Resume: the drained job now admits and runs to completion.
    await handlers.get("queue.resume")!({ pool: "worker-gpu" });
    expect(existsSync(artifact)).toBe(true);
    expect(readLedger(h.ledger.filePath).length).toBe(1);
  });

  test("cancel removes a queued job and marks its row cancelled (a queued job, not force-evicted)", async () => {
    const handlers = mountHandlers(h.engine);
    await handlers.get("queue.pause")!({}); // pause ALL pools
    const artifact = h.artifactPath("to-cancel.txt");
    const res = (await handlers.get("queue.enqueue")!(ocrEnqueue(h, artifact))) as { status: string; task_uuid: string };
    expect(res.status).toBe("queued");

    const cancelled = (await handlers.get("queue.cancel")!({ task_uuid: res.task_uuid })) as { ok: boolean; affected: number };
    expect(cancelled.affected).toBe(1);
    // A cancelled queued job never ran (no artifact, no witness line).
    expect(existsSync(artifact)).toBe(false);
    expect(readLedger(h.ledger.filePath).length).toBe(0);
    // Terminal delta recorded with the cancelled verdict.
    expect(h.bus.ofType("job.failed").length).toBe(1);
  });

  test("cancel racing the lease-acquire window: the job is cancelled and NEVER dispatched, the drive loop survives", async () => {
    const handlers = mountHandlers(h.engine);
    // Gate the lease acquire so a cancel can land while runJob is between pop and dispatch (state
    // still 'enqueued'). This is the post-pop acquire window the race exploited (queue.remove returns
    // undefined; the entry is still 'enqueued').
    let releaseAcquire: (() => void) | null = null;
    const acquireGate = new Promise<void>((r) => (releaseAcquire = r));
    const origAcquire = h.leases.acquire.bind(h.leases);
    let gated = false;
    h.leases.acquire = (async (pool: Parameters<typeof origAcquire>[0], opts: Parameters<typeof origAcquire>[1]) => {
      if (!gated) {
        gated = true;
        await acquireGate;
      }
      return origAcquire(pool, opts);
    }) as typeof h.leases.acquire;

    const artifact = h.artifactPath("race.txt");
    const res = (await handlers.get("queue.enqueue")!(ocrEnqueue(h, artifact))) as { status: string; task_uuid: string };
    // The drive is in flight, blocked in the gated acquire. Cancel now (the entry is 'enqueued').
    const cancelled = (await handlers.get("queue.cancel")!({ task_uuid: res.task_uuid })) as { ok: boolean; affected: number };
    expect(cancelled.affected).toBe(1);
    // Release the acquire: runJob resumes, sees the terminal entry, releases the lease, and returns —
    // it does NOT throw an illegal transition (failed → dispatched) or abort the drive loop.
    releaseAcquire!();
    await h.engine.settle();

    // The cancelled job never dispatched: no artifact, no witness line, and a cancelled delta fired.
    expect(existsSync(artifact)).toBe(false);
    expect(readLedger(h.ledger.filePath).length).toBe(0);
    const entry = h.engine.getJobByTask(res.task_uuid)!;
    expect(entry.state).toBe("failed");
    expect(entry.verdict).toBe("cancelled");
    // No held lease leaked.
    expect(h.pls.holders("worker-gpu")).toEqual([]);
  });

  test("cancel --force fences a RUNNING holder: it commits CANCELLED (not a phantom completed) and does not resurrect the row", async () => {
    const handlers = mountHandlers(h.engine);
    // Record systemctl stop calls (the fence stops the transient unit).
    const stops: string[][] = [];
    h.exec.register("systemctl", (args): ExecResult => {
      stops.push([...args]);
      return ok();
    });
    // A worker that blocks until we let it exit (modelling a still-running holder).
    let releaseWorker: (() => void) | null = null;
    const running = new Promise<void>((r) => (releaseWorker = r));
    const artifact = h.artifactPath("force.txt");
    h.exec.register("force-worker", async (args): Promise<ExecResult> => {
      await running;
      writeFileSync(args[0]!, "DONE", "utf8"); // it WOULD write its artifact + exit 0
      return ok();
    });

    const res = await h.engine.enqueue({
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["force-worker", artifact],
      evidence: [{ kind: "artifact", path: artifact }],
    });
    // Wait until the holder is running.
    for (let i = 0; i < 200 && h.engine.getJobByTask(res.task_uuid!)?.state !== "started"; i++) {
      await new Promise((r) => setTimeout(r, 5));
    }
    expect(h.engine.getJobByTask(res.task_uuid!)?.state).toBe("started");

    // Force-cancel the running holder: it stops the unit + marks the entry fenced.
    const cancelled = (await handlers.get("queue.cancel")!({ task_uuid: res.task_uuid, force: true })) as {
      ok: boolean;
      affected: number;
      was: string;
    };
    expect(cancelled.affected).toBe(1);
    expect(cancelled.was).toBe("started");
    expect(stops.some((s) => s.includes("stop"))).toBe(true);

    // Let the worker's leaf exit; runJob commits a CANCELLED terminal, NOT a phantom completed.
    releaseWorker!();
    await h.engine.settle();
    const entry = h.engine.getJobByTask(res.task_uuid!)!;
    expect(entry.verdict).toBe("cancelled");
    expect(entry.state).toBe("failed");
    // The witness line records the cancellation; no job.completed was emitted.
    expect(h.bus.ofType("job.completed").length).toBe(0);
    // The deleted TW row was NOT resurrected as completed.
    const row = h.task.get(res.task_uuid!);
    expect(row?.status).not.toBe("completed");
  });
});
