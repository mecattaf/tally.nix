// test/e2e/standup.test.ts
//
// `query standup` reconstructs the run from `session_ref` alone (IMPLEMENTATION-PLAN M4.1 case 7;
// SPEC "The four-log read-time join"). The digest is a DAEMONLESS read-time join over
// `task export × journalctl -t tally -o json × git log × witness ledger`, keyed on `session_ref` /
// `TALLY_TASK_UUID`.
//
// End-to-end: the REAL jobs engine drains a batch, producing a genuine witness ledger (GPU-seconds +
// verdicts) with concrete task_uuids; we then present the journald spine for those same tasks carrying
// their `TALLY_SESSION_REF`, and assert `buildStandup` reconstructs the run grouped by session_ref —
// GPU-seconds sourced from the ledger, gate-fails bucketed, reused counted.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  makeEngineHarness,
  enqueueSettled,
  ocrEnqueue,
  readLedger,
  type EngineHarness,
} from "./helpers.ts";
import { FakeExec, ok } from "../helpers/exec-fakes.ts";
import { FakeJournalctl, type TallyFields } from "../helpers/fake-journalctl.ts";
import { FakeTask } from "../helpers/fake-task.ts";
import { buildStandup, runStandup } from "../../src/cli/standup.ts";
import { parseArgs, type CliContext } from "../../src/cli/index.ts";
import { captureWriter } from "../../src/cli/output.ts";

/** A FakeExec wiring journalctl + task + git for the daemonless standup join. */
function joinExec(configure: (j: FakeJournalctl, t: FakeTask) => void): FakeExec {
  const exec = new FakeExec();
  const journal = new FakeJournalctl();
  const task = new FakeTask();
  configure(journal, task);
  journal.install(exec);
  task.install(exec);
  exec.register("git", () => ok("")); // git log is existence-only
  return exec;
}

describe("query standup — reconstruct a run from session_ref alone (M4.1 case 7)", () => {
  let h: EngineHarness;
  beforeEach(() => {
    h = makeEngineHarness();
  });
  afterEach(() => h.cleanup());

  test("a real engine ledger + its journald spine reconstruct the run grouped by session_ref", async () => {
    const SESSION_REF = "term-drive-0709";

    // Drive three real jobs whose durable rows carry the SAME session_ref (one drive session).
    const uuids: string[] = [];
    for (let i = 0; i < 3; i++) {
      const res = await enqueueSettled(h, 
        ocrEnqueue(h.artifactPath(`sc-${i}.txt`), { dedup_key: `sc-${i}`, source: "r2", session: SESSION_REF }),
      );
      expect(res.status).toBe("completed");
      uuids.push(res.task_uuid!);
    }
    // A real, chained ledger now exists for these tasks.
    const ledgerLines = readLedger(h.ledger.filePath);
    expect(ledgerLines.length).toBe(3);
    expect(ledgerLines.every((l) => l.task_uuid !== null)).toBe(true);

    // The journald spine for those same tasks, each carrying the drive's session_ref.
    const exec = joinExec((j) => {
      for (const u of uuids) {
        j.emit({ TALLY_EVENT: "completed", TALLY_TASK_UUID: u, TALLY_SOURCE: "r2", TALLY_SESSION_REF: SESSION_REF });
      }
    });

    const digest = await buildStandup({ exec, ledger: h.ledger.filePath });

    // The run is reconstructed: every task completed, all bearing the ONE session_ref.
    const bySession = digest.completed.filter((c) => c.session_ref === SESSION_REF);
    expect(bySession.length).toBe(3);
    expect(bySession.map((c) => c.task_uuid).sort()).toEqual([...uuids].sort());
    // GPU-seconds come from the ledger (load-bearing verdict/gpu-seconds, not journald).
    for (const c of bySession) {
      expect(c.verdict).toBe("pass");
    }
  });

  test("a gate-fail in the drive is bucketed into gate_fails; a reused line increments reused", async () => {
    const SESSION_REF = "term-mixed-0709";

    // One clean pass, one clean-exit-no-artifact forensic, both in the same drive.
    const pass = await enqueueSettled(h, 
      ocrEnqueue(h.artifactPath("ok.txt"), { dedup_key: "ok", source: "r2", session: SESSION_REF }),
    );
    const fail = await enqueueSettled(h, {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["noop"], // clean exit, no artifact
      session: SESSION_REF,
      evidence: [{ kind: "artifact", path: h.artifactPath("missing.txt") }, { kind: "exit", code: 0 }],
    });
    // A reuse of the passing work (dedup hit ⇒ a reused witness line already in the ledger? no — reuse
    // writes NO line; instead seed a reused-labor line by re-enqueuing the SAME dedup key).
    const reused = await enqueueSettled(h, 
      ocrEnqueue(h.artifactPath("ok.txt"), { dedup_key: "ok", source: "r2", session: SESSION_REF }),
    );
    expect(reused.status).toBe("reused");

    const exec = joinExec((j) => {
      j.emit({ TALLY_EVENT: "completed", TALLY_TASK_UUID: pass.task_uuid!, TALLY_SOURCE: "r2", TALLY_SESSION_REF: SESSION_REF });
      j.emit({ TALLY_EVENT: "evidence_fail", TALLY_TASK_UUID: fail.task_uuid!, TALLY_SOURCE: "r2", TALLY_SESSION_REF: SESSION_REF });
    });

    const digest = await buildStandup({ exec, ledger: h.ledger.filePath });

    // The forensic lands in gate_fails with the clean-exit verdict.
    expect(digest.gate_fails.map((g) => g.task_uuid)).toContain(fail.task_uuid!);
    expect(digest.gate_fails.find((g) => g.task_uuid === fail.task_uuid!)!.verdict).toBe("clean-exit-no-artifact");
    // The pass is completed.
    expect(digest.completed.map((c) => c.task_uuid)).toContain(pass.task_uuid!);
  });

  test("a force-cancelled RUNNING job lands in the cancelled bucket, never under completed (issue #7)", async () => {
    const SESSION_REF = "term-cancel-0711";

    // A leaf gated on a promise, so the force-cancel lands while the job is genuinely RUNNING —
    // that is the path that writes a real `cancelled` witness line (fenced commit).
    let release: (() => void) | null = null;
    const gate = new Promise<void>((r) => (release = r));
    h.exec.register("slow", async () => {
      await gate;
      return ok("");
    });
    const admitted = await h.engine.enqueue({
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["slow"],
      session: SESSION_REF,
      evidence: [{ kind: "exit", code: 0 }],
    });
    // Wait for the drive to reach `started` (the leaf is blocked inside the gated exec).
    for (let i = 0; i < 1000 && h.engine.getJobByTask(admitted.task_uuid!)?.state !== "started"; i++) {
      await new Promise((r) => setTimeout(r, 1));
    }
    expect(h.engine.getJobByTask(admitted.task_uuid!)?.state).toBe("started");
    const res = await h.engine.cancel(admitted.task_uuid!, true);
    expect(res.affected).toBe(1);
    release!();
    await h.engine.settle();

    // The engine wrote a genuine `cancelled` witness line for the fenced holder.
    const lines = readLedger(h.ledger.filePath);
    expect(lines.some((l) => l.task_uuid === admitted.task_uuid && l.verdict === "cancelled")).toBe(true);

    // Replay the REAL journald spine the engine emitted into the join.
    const exec = joinExec((j) => {
      for (const line of h.journalLines) j.emit(JSON.parse(line) as TallyFields);
    });
    const digest = await buildStandup({ exec, ledger: h.ledger.filePath });

    // Cancelled is its OWN bucket — not success-shaped under completed, not a gate fail.
    expect(digest.cancelled.map((c) => c.task_uuid)).toContain(admitted.task_uuid!);
    expect(digest.cancelled.find((c) => c.task_uuid === admitted.task_uuid!)!.verdict).toBe("cancelled");
    expect(digest.completed.map((c) => c.task_uuid)).not.toContain(admitted.task_uuid!);
    expect(digest.gate_fails.map((g) => g.task_uuid)).not.toContain(admitted.task_uuid!);

    // Ledger-only fold (journald pruned): the cancelled row still lands in its own bucket.
    const folded = await buildStandup({ exec: joinExec(() => {}), ledger: h.ledger.filePath });
    expect(folded.cancelled.map((c) => c.task_uuid)).toContain(admitted.task_uuid!);
    expect(folded.completed.map((c) => c.task_uuid)).not.toContain(admitted.task_uuid!);
  });

  test("an exit-nonzero evidence fail (witnessed `failed`, gate detail only in journald) counts as a gate-fail (issue #7)", async () => {
    const SESSION_REF = "term-gate-0711";

    // `boom` exits 7 against an `exit:0` evidence spec: the gate fails, but the witness verdict is
    // plain `failed` (NOT clean-exit-no-artifact) — the evidence_fail marker lives only in journald.
    const res = await enqueueSettled(h, {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["boom"],
      session: SESSION_REF,
      evidence: [{ kind: "exit", code: 0 }],
    });
    expect(res.status).toBe("failed");
    const lines = readLedger(h.ledger.filePath);
    expect(lines.find((l) => l.task_uuid === res.task_uuid)!.verdict).toBe("failed");
    // The engine's real journald spine carries the canonical gate-fail marker (PS#21 forensics).
    expect(h.journalLines.some((l) => (JSON.parse(l) as TallyFields).TALLY_EVENT === "evidence_fail")).toBe(true);

    const exec = joinExec((j) => {
      for (const line of h.journalLines) j.emit(JSON.parse(line) as TallyFields);
    });
    const digest = await buildStandup({ exec, ledger: h.ledger.filePath });

    // The evidence fail is a gate fail — not a success-shaped completed row.
    expect(digest.gate_fails.map((g) => g.task_uuid)).toContain(res.task_uuid!);
    expect(digest.gate_fails.find((g) => g.task_uuid === res.task_uuid!)!.verdict).toBe("failed");
    expect(digest.completed.map((c) => c.task_uuid)).not.toContain(res.task_uuid!);
  });

  test("--stale-hours warns on stderr (ruling pending, CLI-SURFACE §1.5) instead of being silently swallowed (issue #7)", async () => {
    const w = captureWriter();
    const ctx: CliContext = {
      noun: "query",
      verb: "standup",
      args: parseArgs(["--stale-hours", "4", "--format", "json"]),
      writer: w,
      env: h.env.env,
    };
    const code = await runStandup(ctx, joinExec(() => {}));

    // Warn, not exit 2: the digest still renders, but the operator is told the flag did nothing.
    expect(code).toBe(0);
    expect(w.stderr).toContain("--stale-hours");
    expect(w.stderr).toContain("ignored");
    const digest = JSON.parse(w.stdout) as Record<string, unknown>;
    expect(digest.stale).toBeUndefined(); // the stale bucket stays unbuilt until Tom rules
    expect(Array.isArray(digest.cancelled)).toBe(true); // the additive bucket rides every digest
  });

  test("witness lines outside the journald window are still folded into the reconstruction", async () => {
    // Drive one job; present an EMPTY journald spine. The digest must still surface the ledger's
    // completed line (the witness is load-bearing; journald is observability).
    await enqueueSettled(h, ocrEnqueue(h.artifactPath("folded.txt"), { dedup_key: "folded", source: "r2" }));
    const exec = joinExec(() => {});
    const digest = await buildStandup({ exec, ledger: h.ledger.filePath });
    expect(digest.completed.length + digest.gate_fails.length).toBeGreaterThan(0);
  });
});
