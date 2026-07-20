// test/cli/standup.test.ts
//
// `tally query standup` — the four-log read-time join keyed on session_ref / TALLY_TASK_UUID
// (IMPLEMENTATION-PLAN M3.1 tests: "standup join against fixture logs"; SPEC "The four-log read-time
// join"). Joins the journald TALLY_* feed + the TaskChampion rows + the git log (existence-only) +
// the witness ledger (load-bearing verdict/GPU-seconds), DAEMONLESS, against fixture logs.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { describe, expect, test } from "bun:test";
import { join } from "node:path";
import { FakeExec, ok } from "../helpers/exec-fakes.ts";
import { FakeJournalctl } from "../helpers/fake-journalctl.ts";
import { FakeTask } from "../helpers/fake-task.ts";
import { buildStandup } from "../../src/cli/standup.ts";

const LEDGER = join(import.meta.dir, "..", "fixtures", "ledger", "valid.jsonl");

/** A FakeExec with journalctl + task + git wired for the standup join. */
function joinExec(configure: (j: FakeJournalctl, t: FakeTask) => void): FakeExec {
  const exec = new FakeExec();
  const journal = new FakeJournalctl();
  const task = new FakeTask();
  configure(journal, task);
  journal.install(exec);
  task.install(exec);
  // git log is existence-only; a clean empty log is fine for the join.
  exec.register("git", () => ok(""));
  return exec;
}

describe("buildStandup — the four-log read-time join", () => {
  test("joins journald + witness ledger into completed / gate_fails / reused", async () => {
    // The valid fixture ledger carries 4 lines: 2 pass (fresh), 1 clean-exit-no-artifact (gate fail),
    // 1 reused. Journald reports the two fresh tasks completing + the gate-fail task.
    const exec = joinExec((j) => {
      j.emit({ TALLY_EVENT: "completed", TALLY_TASK_UUID: "b2c40001-0000-4000-8000-000000000001", TALLY_SOURCE: "r2", TALLY_SESSION_REF: "ref-1" });
      j.emit({ TALLY_EVENT: "completed", TALLY_TASK_UUID: "b2c40001-0000-4000-8000-000000000002", TALLY_SOURCE: "r2", TALLY_SESSION_REF: "ref-2" });
      j.emit({ TALLY_EVENT: "evidence_fail", TALLY_TASK_UUID: "b2c40001-0000-4000-8000-000000000003", TALLY_SOURCE: "r2" });
    });

    const digest = await buildStandup({ exec, ledger: LEDGER });

    // The gate-fail task lands in gate_fails (verdict clean-exit-no-artifact).
    expect(digest.gate_fails.map((g) => g.task_uuid)).toContain("b2c40001-0000-4000-8000-000000000003");
    expect(digest.gate_fails[0]!.verdict).toBe("clean-exit-no-artifact");

    // The reused witness line increments the reused counter.
    expect(digest.reused).toBe(1);

    // The two fresh passes are completed with their GPU-seconds from the ledger.
    const completedUuids = digest.completed.map((c) => c.task_uuid);
    expect(completedUuids).toContain("b2c40001-0000-4000-8000-000000000001");
    const first = digest.completed.find((c) => c.task_uuid === "b2c40001-0000-4000-8000-000000000001")!;
    expect(first.gpu_seconds).toBe(42.5);
    expect(first.session_ref).toBe("ref-1");
  });

  test("an in-flight task (dispatched, no terminal) is bucketed in_flight", async () => {
    const exec = joinExec((j) => {
      j.emit({ TALLY_EVENT: "dispatched", TALLY_TASK_UUID: "aaaa0000-0000-4000-8000-000000000009", TALLY_SOURCE: "orchestrator", TALLY_SESSION_REF: "live-9" });
      j.emit({ TALLY_EVENT: "started", TALLY_TASK_UUID: "aaaa0000-0000-4000-8000-000000000009", TALLY_SOURCE: "orchestrator" });
    });
    // Point the ledger at a nonexistent path so only the journald spine drives the join.
    const digest = await buildStandup({ exec, ledger: "/nonexistent/witness.jsonl" });
    expect(digest.in_flight.map((f) => f.task_uuid)).toContain("aaaa0000-0000-4000-8000-000000000009");
    expect(digest.in_flight[0]!.state).toBe("started");
    expect(digest.in_flight[0]!.session_ref).toBe("live-9");
    expect(digest.completed).toEqual([]);
  });

  test("--source filter scopes the journald spine", async () => {
    const exec = joinExec((j) => {
      j.emit({ TALLY_EVENT: "dispatched", TALLY_TASK_UUID: "t-gh", TALLY_SOURCE: "gh" });
      j.emit({ TALLY_EVENT: "dispatched", TALLY_TASK_UUID: "t-r2", TALLY_SOURCE: "r2" });
    });
    const digest = await buildStandup({ exec, ledger: "/nonexistent", source: "gh" });
    const uuids = [...digest.in_flight, ...digest.completed].map((e) => e.task_uuid);
    expect(uuids).toContain("t-gh");
    expect(uuids).not.toContain("t-r2");
  });

  test("witness lines absent from the journald window are still folded in", async () => {
    // No journald entries; the digest must still surface the ledger's completed/reused/gate lines.
    const exec = joinExec(() => {});
    const digest = await buildStandup({ exec, ledger: LEDGER });
    expect(digest.completed.length + digest.gate_fails.length).toBeGreaterThan(0);
    expect(digest.reused).toBe(1);
  });
});
