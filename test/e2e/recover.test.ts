// test/e2e/recover.test.ts
//
// Crash recovery (IMPLEMENTATION-PLAN M4.1 case 3): kill the daemon mid-flight ⇒ recover()
// re-presents (never replays), the witness chain HEAD survives the restart, and a reconnecting
// subscriber observes the cursor-voiding lease-epoch bump (§2.5).
//
// Two halves, both real:
//   • the jobs engine's recover() re-presents an undeleted un-acked pending TW row via resume
//     (labor_class:recovered, bumped attempt) and runs it to completion — proving "re-present, not
//     replay" and the surviving witness chain across the boundary;
//   • a live daemon booted, torn down, and re-booted against the SAME epoch-counter file bumps
//     lease_epoch, so a §2 client that reconnects sees the new epoch in its snapshot + subscribe ACK
//     — its old cursor is void.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import {
  bootDaemonHarness,
  makeEngineHarness,
  enqueueSettled,
  ocrEnqueue,
  readLedger,
  tick,
  type DaemonHarness,
  type EngineHarness,
} from "./helpers.ts";
import { ok, type ExecResult } from "../helpers/exec-fakes.ts";
import { bootDaemon, type Daemon } from "../../src/daemon/index.ts";
import { defaultConfig, unitExitPath, type EnqueueParams } from "../../src/contracts/index.ts";
import { rowSeedFor } from "../../src/jobs/index.ts";
import { verifyLedgerFile } from "../../src/witness/index.ts";

describe("recover() — re-present, never replay; the chain head survives (M4.1 case 3)", () => {
  let h: EngineHarness;
  beforeEach(() => {
    h = makeEngineHarness();
  });
  afterEach(() => h.cleanup());

  test("a mid-flight crash leaves a pending row; recover() re-presents it via resume and completes it", async () => {
    // Simulate the state a crash leaves behind: one batch job already witnessed + completed, and one
    // job still 'pending' in TaskChampion (dispatched-but-not-terminal when the daemon died).
    const donePath = h.artifactPath("done-before-crash.txt");
    const doneRes = await enqueueSettled(h, ocrEnqueue(donePath, { dedup_key: "done-1", source: "r2" }));
    expect(doneRes.status).toBe("completed");
    const chainHeadBefore = h.ledger.chainHead;
    expect(chainHeadBefore.seq).toBe(1);

    // The in-flight row the crash orphaned (a runnable OCR command, un-acked, in budget).
    const recoverPath = h.artifactPath("recovered.txt");
    h.task.seed({
      uuid: "u-inflight",
      description: `ocr ${recoverPath}`,
      status: "pending",
      agent: "shell",
      source: "r2",
      priority_class: "low",
      attempt: 1,
    });

    // Boot-time recovery: re-present the undeleted un-acked row and drive it to terminal.
    const plan = await h.engine.recover();

    // It was re-presented (not replayed): a NEW run with labor_class recovered + bumped attempt.
    expect(plan.represent.map((r) => r.task_uuid)).toContain("u-inflight");
    expect(existsSync(recoverPath)).toBe(true);

    // The witness chain SURVIVED across the boundary: the pre-crash line is intact and the recovered
    // line chains onto it (one unbroken ledger-wide chain).
    const lines = readLedger(h.ledger.filePath);
    expect(lines.length).toBe(2);
    expect(lines[0]!.hash).toBe(chainHeadBefore.hash);
    expect(lines[1]!.prev_hash).toBe(lines[0]!.hash);
    expect(lines[1]!.labor_class).toBe("recovered");
    expect(lines[1]!.attempt).toBe(2);
  });

  test("recovery re-presents the PERSISTED argv verbatim — a quoted multi-word argument survives (issue #2)", async () => {
    // A worker that records the argv it was actually invoked with.
    const out = h.artifactPath("argv-echo.json");
    h.exec.register("argecho", (args): ExecResult => {
      writeFileSync(args[1]!, JSON.stringify(args), "utf8");
      return ok("");
    });
    // The crash leftover, written by the REAL enqueue-time seed path (rowSeedFor → tw.createRow):
    // an argv carrying a quoted multi-word argument the whitespace-joined description CANNOT
    // represent (`argecho sleep $1; touch done <out>` re-tokenizes into six bogus words).
    const params: EnqueueParams = {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["argecho", "sleep $1; touch done", out],
      evidence: [{ kind: "artifact", path: out }, { kind: "exit", code: 0 }],
    };
    await h.tw.createRow(rowSeedFor(params, "u-quoted", 1));

    const plan = await h.engine.recover();
    expect(plan.represent.map((r) => r.task_uuid)).toContain("u-quoted");

    // The recovered run received the ORIGINAL argv: the multi-word argument arrived as ONE token,
    // never re-tokenized shards of the cosmetic description.
    expect(existsSync(out)).toBe(true);
    expect(JSON.parse(readFileSync(out, "utf8"))).toEqual(["sleep $1; touch done", out]);

    // ...and the ORIGINAL evidence spec was re-armed: the recovered attempt's job.enqueued delta
    // carries the enqueue-time gates, and the witness line hashed the declared artifact (the hash
    // is only computed when the artifact gate is present — pre-fix it was silently dropped).
    const enq = h.bus.ofType("job.enqueued").at(-1) as { evidence_spec: string[] };
    expect(enq.evidence_spec).toContain(`artifact:${out}`);
    expect(enq.evidence_spec).toContain("exit:0");
    const lines = readLedger(h.ledger.filePath);
    const line = lines.at(-1)!;
    expect(line.labor_class).toBe("recovered");
    expect(line.verdict).toBe("pass");
    expect(line.artifact_content_hash).toMatch(/^sha256:/);
  });

  test("a recovered job KEEPS its evidence gates — a clean exit with no artifact still gate-fails (issue #2)", async () => {
    // Pre-fix, paramsFromRow reconstructed NO evidence at all: a recovered `noop` (clean exit,
    // writes nothing) was witnessed `pass` without its enqueue-time artifact gate. With the
    // persisted evidence_json the gate survives recovery and the run records the PS#21 forensic.
    const missing = h.artifactPath("never-written.txt");
    const params: EnqueueParams = {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["noop"],
      evidence: [{ kind: "artifact", path: missing }],
    };
    await h.tw.createRow(rowSeedFor(params, "u-gated", 1));

    await h.engine.recover();
    const lines = readLedger(h.ledger.filePath);
    expect(lines.length).toBe(1);
    expect(lines[0]!.labor_class).toBe("recovered");
    expect(lines[0]!.verdict).toBe("clean-exit-no-artifact");
  });

  test("an acked in-flight row is NOT re-presented (invariant 2 — no double execution)", async () => {
    h.task.seed({
      uuid: "u-acked",
      description: "ocr /tmp/x",
      status: "pending",
      agent: "shell",
      source: "r2",
      priority_class: "low",
      attempt: 1,
    });
    const plan = await h.engine.recover({ ackedTaskUuids: new Set(["u-acked"]) });
    expect(plan.represent.find((r) => r.task_uuid === "u-acked")).toBeUndefined();
  });

  test("the surviving ledger re-verifies clean after recovery (daemonless chain verify)", async () => {
    await enqueueSettled(h, ocrEnqueue(h.artifactPath("a.txt"), { dedup_key: "a", source: "r2" }));
    h.task.seed({
      uuid: "u-b",
      description: `ocr ${h.artifactPath("b.txt")}`,
      status: "pending",
      agent: "shell",
      source: "r2",
      priority_class: "low",
      attempt: 1,
    });
    await h.engine.recover();

    const report = await verifyLedgerFile(h.ledger.filePath);
    expect(report.ok).toBe(true);
    expect(report.problems).toEqual([]);
  });
});

describe("recovery reconciles the restart window — surviving units adopted, exits witnessed, never re-run (issue #3)", () => {
  let h: EngineHarness;
  beforeEach(() => {
    h = makeEngineHarness();
  });
  afterEach(() => h.cleanup());

  test("a rowed job's transient unit is named by its task_uuid — findable across a daemon restart", async () => {
    // Pre-fix the unit was tally-job-<random job_id>: the job_id dies with the daemon that minted
    // it, so a rebooted daemon could never locate a row's surviving unit at all.
    const out = h.artifactPath("unit-name.txt");
    const res = await enqueueSettled(h, ocrEnqueue(out, { source: "r2" }));
    expect(res.status).toBe("completed");
    const dispatched = h.bus.ofType("job.dispatched").at(-1) as { unit: string };
    expect(dispatched.unit).toBe(`tally-job-${res.task_uuid}`);
  });

  test("an unobserved EXITED unit's work is witnessed as attempt 1 — never re-run as attempt 2", async () => {
    // The issue's exact window (val-jul11-slow-3): attempt-1's transient unit outlived the old
    // daemon, wrote its artifact, and exited clean — but no daemon was watching. The new daemon
    // boots onto a still-pending row. REALISTIC systemd shape: `systemd-run --collect` unloaded
    // the exited unit within moments (LoadState=not-found — the unit is deliberately NOT seeded
    // in the fake), so the only surviving exit evidence is the durable ExecStopPost exit record.
    const out = h.artifactPath("survivor.txt");
    const params: EnqueueParams = {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["ocr", out],
      evidence: [{ kind: "artifact", path: out }, { kind: "exit", code: 0 }],
    };
    await h.tw.createRow(rowSeedFor(params, "u-survivor", 1));
    writeFileSync(out, "WRITTEN-BY-THE-SURVIVING-UNIT", "utf8");
    const record = unitExitPath(h.env.env, "tally-job-u-survivor");
    mkdirSync(dirname(record), { recursive: true });
    writeFileSync(record, "0", "utf8");

    await h.engine.recover();
    await h.engine.settle();

    // The leaf was NEVER re-executed — the artifact is the surviving unit's, untouched.
    expect(h.exec.callsFor("ocr").length).toBe(0);
    expect(readFileSync(out, "utf8")).toBe("WRITTEN-BY-THE-SURVIVING-UNIT");

    // ONE witness line: the attempt-1 completion — exit conjunct from the ExecStopPost record,
    // artifact/hash conjuncts from disk, gpu_seconds null (the span was unobserved — the PS#9
    // cloud-run treatment).
    const lines = readLedger(h.ledger.filePath);
    expect(lines.length).toBe(1);
    expect(lines[0]!.task_uuid).toBe("u-survivor");
    expect(lines[0]!.verdict).toBe("pass");
    expect(lines[0]!.attempt).toBe(1);
    expect(lines[0]!.gpu_seconds).toBeNull();
    expect(lines[0]!.artifact_content_hash).toMatch(/^sha256:/);

    // The ledger records the success — the row completed, no phantom attempt-2 failure — and the
    // exit record was CONSUMED (read once, then deleted).
    expect((await h.tw.getRow("u-survivor"))?.status).toBe("completed");
    expect(existsSync(record)).toBe(false);
  });

  test("the narrow pre-collection race: a still-LOADED exited unit is gated on its ExecMainStatus", async () => {
    // Between the unit stopping and systemd's GC there is a moments-wide window where the unit is
    // still loaded and ExecMainStatus is live — the probe path must keep working there too.
    const out = h.artifactPath("survivor-loaded.txt");
    const params: EnqueueParams = {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["ocr", out],
      evidence: [{ kind: "artifact", path: out }, { kind: "exit", code: 0 }],
    };
    await h.tw.createRow(rowSeedFor(params, "u-loaded", 1));
    writeFileSync(out, "WRITTEN-BY-THE-SURVIVING-UNIT", "utf8");
    h.systemd.setUnitStates("tally-job-u-loaded", [{ activeState: "inactive", execMainStatus: 0 }]);

    await h.engine.recover();
    await h.engine.settle();

    expect(h.exec.callsFor("ocr").length).toBe(0);
    const lines = readLedger(h.ledger.filePath);
    expect(lines.length).toBe(1);
    expect(lines[0]!.task_uuid).toBe("u-loaded");
    expect(lines[0]!.verdict).toBe("pass");
    expect(lines[0]!.attempt).toBe(1);
  });

  test("a COLLECTED unit whose exit record says non-zero falls through to the planned re-present", async () => {
    // The collected analogue of the failing-survivor case: the record says exit 7 and no artifact
    // exists — nothing witnessable, so the recovery plan's re-present (attempt 2) proceeds.
    const out = h.artifactPath("collected-failed.txt");
    const params: EnqueueParams = {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["ocr", out],
      evidence: [{ kind: "artifact", path: out }, { kind: "exit", code: 0 }],
    };
    await h.tw.createRow(rowSeedFor(params, "u-collected-fail", 1));
    const record = unitExitPath(h.env.env, "tally-job-u-collected-fail");
    mkdirSync(dirname(record), { recursive: true });
    writeFileSync(record, "7", "utf8");

    await h.engine.recover();
    await h.engine.settle();

    expect(h.exec.callsFor("ocr").length).toBe(1);
    const lines = readLedger(h.ledger.filePath);
    expect(lines.length).toBe(1);
    expect(lines[0]!.attempt).toBe(2);
    expect(lines[0]!.labor_class).toBe("recovered");
    expect(lines[0]!.verdict).toBe("pass");
  });

  test("an ADOPTED unit COLLECTED between watch polls is still gated on its exit record (no re-run)", async () => {
    // The adoption watcher's poll can race systemd's GC: the unit answers `active` at recover()
    // time, then exits AND is collected before the next poll — the probe reads not-found with no
    // ExecMainStatus. The ExecStopPost record carries the status across that gap.
    const out = h.artifactPath("adopted-collected.txt");
    const params: EnqueueParams = {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["ocr", out],
      evidence: [{ kind: "artifact", path: out }, { kind: "exit", code: 0 }],
    };
    await h.tw.createRow(rowSeedFor(params, "u-adopt-gc", 1));
    writeFileSync(out, "WRITTEN-BY-THE-ADOPTED-UNIT", "utf8");
    const record = unitExitPath(h.env.env, "tally-job-u-adopt-gc");
    mkdirSync(dirname(record), { recursive: true });
    writeFileSync(record, "0", "utf8");
    // active at the recover()-time probe, then collected (not-found) at the watcher's next poll.
    h.systemd.setUnitStates("tally-job-u-adopt-gc", [
      { activeState: "active" },
      { loadState: "not-found", activeState: "inactive" },
    ]);

    await h.engine.recover();
    await h.engine.adoptionsSettled();
    await h.engine.settle();

    expect(h.exec.callsFor("ocr").length).toBe(0);
    const lines = readLedger(h.ledger.filePath);
    expect(lines.length).toBe(1);
    expect(lines[0]!.task_uuid).toBe("u-adopt-gc");
    expect(lines[0]!.verdict).toBe("pass");
    expect(lines[0]!.attempt).toBe(1);
    expect(existsSync(record)).toBe(false);
  });

  test("a STILL-RUNNING surviving unit is ADOPTED — parked, watched to exit, then gated (no duplicate run)", async () => {
    const out = h.artifactPath("adopted.txt");
    const params: EnqueueParams = {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["ocr", out],
      evidence: [{ kind: "artifact", path: out }, { kind: "exit", code: 0 }],
    };
    await h.tw.createRow(rowSeedFor(params, "u-adopt", 1));
    writeFileSync(out, "WRITTEN-BY-THE-ADOPTED-UNIT", "utf8");
    // Script the surviving unit across the adoption polls: the recover()-time probe sees it
    // running, the watcher's first poll still running, then it exits clean.
    h.systemd.setUnitStates("tally-job-u-adopt", [
      { activeState: "active" },
      { activeState: "active" },
      { activeState: "inactive", execMainStatus: 0 },
    ]);

    const plan = await h.engine.recover();
    // The plan listed the row, but it was ADOPTED instead of re-presented — no duplicate
    // concurrent run exists while the surviving unit still holds the work.
    expect(plan.represent.map((r) => r.task_uuid)).toContain("u-adopt");
    expect(h.engine.getJobByTask("u-adopt")).toBeUndefined();

    await h.engine.adoptionsSettled();
    await h.engine.settle();

    expect(h.exec.callsFor("ocr").length).toBe(0);
    const lines = readLedger(h.ledger.filePath);
    expect(lines.length).toBe(1);
    expect(lines[0]!.task_uuid).toBe("u-adopt");
    expect(lines[0]!.verdict).toBe("pass");
    expect(lines[0]!.attempt).toBe(1);
    expect((await h.tw.getRow("u-adopt"))?.status).toBe("completed");
  });

  test("a surviving unit that FAILS its evidence gate falls through to the planned re-present (attempt 2)", async () => {
    const out = h.artifactPath("failed-survivor.txt");
    const params: EnqueueParams = {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["ocr", out],
      evidence: [{ kind: "artifact", path: out }, { kind: "exit", code: 0 }],
    };
    await h.tw.createRow(rowSeedFor(params, "u-fell", 1));
    // The surviving unit exited 7 and never wrote its artifact — nothing to witness; recover it.
    // (Still-loaded shape: the narrow pre-collection race; the collected analogue is covered by
    // the exit-record test above.)
    h.systemd.setUnitStates("tally-job-u-fell", [{ activeState: "failed", result: "exit-code", execMainStatus: 7 }]);

    await h.engine.recover();
    await h.engine.settle();

    // The re-present ran the leaf and completed attempt 2 as usual (labor_class recovered).
    expect(h.exec.callsFor("ocr").length).toBe(1);
    const lines = readLedger(h.ledger.filePath);
    expect(lines.length).toBe(1);
    expect(lines[0]!.attempt).toBe(2);
    expect(lines[0]!.labor_class).toBe("recovered");
    expect(lines[0]!.verdict).toBe("pass");
  });

  test("dedup-by-existence completes a recovered row whose output is already witnessed — reused, no attempt 2", async () => {
    // A witnessed success already exists under this dedup key (an earlier identical run).
    const out = h.artifactPath("dedup.txt");
    const first = await enqueueSettled(h, ocrEnqueue(out, { dedup_key: "dk-recover", source: "r2" }));
    expect(first.status).toBe("completed");
    expect(h.exec.callsFor("ocr").length).toBe(1);

    // The crash leftover: a pending row for the SAME work (same key, same artifact), whose re-run
    // would overwrite the artifact with different content.
    const params: EnqueueParams = {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["ocr", out, "A-DIFFERENT-RERUN-OUTPUT"],
      evidence: [{ kind: "artifact", path: out }, { kind: "exit", code: 0 }],
      dedup_key: "dk-recover",
    };
    await h.tw.createRow(rowSeedFor(params, "u-dedup", 1));

    await h.engine.recover();
    await h.engine.settle();

    // The gate hit (artifact-exists ∧ hash pass): the leaf never re-ran, and the row completed with
    // an attempt-1 `reused` line tagged out of canonical GPU-seconds (DECISIONS appendix).
    expect(h.exec.callsFor("ocr").length).toBe(1);
    const lines = readLedger(h.ledger.filePath);
    expect(lines.length).toBe(2);
    expect(lines[1]!.task_uuid).toBe("u-dedup");
    expect(lines[1]!.labor_class).toBe("reused");
    expect(lines[1]!.verdict).toBe("reused");
    expect(lines[1]!.gpu_seconds).toBeNull();
    expect(lines[1]!.attempt).toBe(1);
    expect(lines[1]!.dedup_key).toBe("dk-recover");
    expect((await h.tw.getRow("u-dedup"))?.status).toBe("completed");

    // The chain stays verifiable across the reconciled line.
    const report = await verifyLedgerFile(h.ledger.filePath);
    expect(report.ok).toBe(true);
  });
});

describe("cursor-voiding epoch bump across a daemon restart (§2.5, M4.1 case 3)", () => {
  let dh: DaemonHarness;
  beforeEach(async () => {
    dh = await bootDaemonHarness();
  });
  afterEach(async () => {
    await dh.stop();
    dh.cleanup();
  });

  test("a reconnecting subscriber sees a strictly-greater lease_epoch ⇒ its old cursor is void", async () => {
    const c1 = await dh.client();
    const snap1 = await c1.call<Record<string, unknown>>("session.snapshot");
    const firstEpoch = snap1.lease_epoch as number;
    const ack1 = await c1.call<Record<string, unknown>>("session.subscribe", {});
    expect(ack1.epoch).toBe(firstEpoch);

    // Kill the daemon mid-flight — the subscriber's connection dies with it.
    await dh.daemon.stop();
    c1.close();

    // Re-boot against the SAME env (same epoch-counter file). lease_epoch changes ONLY on
    // (re)start (§2.5), sourced from the daemon-bumped counter file so it stays monotone
    // (the daemon is the counter's sole incrementer — issue #9).
    const daemon2: Daemon = bootDaemon({
      env: dh.env.env,
      config: { ...defaultConfig(), heartbeatMs: 100000 },
    });
    await daemon2.start();
    try {
      expect(daemon2.state.epoch).toBeGreaterThan(firstEpoch);

      // A reconnecting client observes the new epoch in BOTH the snapshot and the subscribe ACK — its
      // pre-restart cursor (bound to firstEpoch) is therefore void.
      const c2 = await (async () => {
        const { connectClient } = await import("../helpers/socket-client.ts");
        return connectClient(daemon2.server.socketPath);
      })();
      try {
        const snap2 = await c2.call<Record<string, unknown>>("session.snapshot");
        expect(snap2.lease_epoch).toBe(daemon2.state.epoch);
        // seq resets within the new epoch.
        expect(snap2.seq).toBe(0);
        const ack2 = await c2.call<Record<string, unknown>>("session.subscribe", {});
        expect(ack2.epoch).toBe(daemon2.state.epoch);
        expect(ack2.epoch as number).toBeGreaterThan(firstEpoch);
      } finally {
        c2.close();
      }
    } finally {
      await daemon2.stop();
    }
    await tick(5);
  });
});
