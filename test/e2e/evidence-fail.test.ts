// test/e2e/evidence-fail.test.ts
//
// Evidence-fail forensics (IMPLEMENTATION-PLAN M4.1 case 2; PS#21; SPEC "Evidence gate"). A clean
// exit (code 0) with a MISSING declared artifact is not success — the terminal commit gates on
// artifact-exists ∧ content-hash ∧ exit-code-ok ∧ witness-span, never self-report. The forensic
// verdict is `clean-exit-no-artifact`, the delta is `job.evidence_fail`, journald records
// `evidence_fail`, and the line is EXCLUDED from canonical GPU-seconds.
//
// Runs the REAL jobs engine end-to-end over the layer-0 fakes. Authored fresh for tally; no vendor/
// code (clean-room, CLI-SURFACE §4).

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { existsSync } from "node:fs";
import { makeEngineHarness, enqueueSettled, readLedger, type EngineHarness } from "./helpers.ts";
import { canonicalGpuSeconds, parseRecord } from "../../src/witness/index.ts";
import type { WitnessRecord } from "../../src/contracts/index.ts";

function witnessRecords(ledgerPath: string): WitnessRecord[] {
  const out: WitnessRecord[] = [];
  for (const raw of readLedger(ledgerPath)) {
    const res = parseRecord(raw);
    if (res.ok) out.push(res.record);
  }
  return out;
}

describe("evidence-fail forensics — clean exit, no artifact (PS#21)", () => {
  let h: EngineHarness;
  beforeEach(() => {
    h = makeEngineHarness();
  });
  afterEach(() => h.cleanup());

  test("a clean-exit run with a missing declared artifact ⇒ clean-exit-no-artifact + evidence_fail", async () => {
    const missing = h.artifactPath("never-written.txt");
    const res = await enqueueSettled(h, {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["noop"], // exits 0 but writes NOTHING
      evidence: [{ kind: "artifact", path: missing }, { kind: "exit", code: 0 }],
    });

    // The run did not fabricate success from a clean exit.
    expect(res.status).toBe("failed");
    expect(res.verdict).toBe("clean-exit-no-artifact");
    expect(existsSync(missing)).toBe(false);

    // The forensic delta fired; no pass delta.
    expect(h.bus.ofType("job.evidence_fail").length).toBe(1);
    expect(h.bus.ofType("job.evidence_pass").length).toBe(0);
    // The terminal delta is a failure, never a completion.
    expect(h.bus.ofType("job.failed").length).toBe(1);
    expect(h.bus.ofType("job.completed").length).toBe(0);

    // A witness line still exists (the ledger is broader than the pass set) with the forensic verdict.
    const lines = readLedger(h.ledger.filePath);
    expect(lines.length).toBe(1);
    expect(lines[0]!.verdict).toBe("clean-exit-no-artifact");
    expect(lines[0]!.artifact_content_hash).toBeNull();

    // journald mirrored the one vocabulary: an `evidence_fail` event, never `completed`.
    const journalEvents = h.journalLines.map((l) => JSON.parse(l).TALLY_EVENT);
    expect(journalEvents).toContain("evidence_fail");
    expect(journalEvents).not.toContain("completed");
  });

  test("the forensic line is excluded from canonical GPU-seconds", async () => {
    await enqueueSettled(h, {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["noop"],
      pool: "worker-gpu",
      evidence: [{ kind: "artifact", path: h.artifactPath("gone.txt") }, { kind: "exit", code: 0 }],
    });
    const records = witnessRecords(h.ledger.filePath);
    expect(records.length).toBe(1);
    expect(records[0]!.verdict).toBe("clean-exit-no-artifact");
    // A clean-exit-no-artifact line never counts toward the verifiable GPU-seconds aggregate.
    expect(canonicalGpuSeconds(records)).toBe(0);
  });

  test("a non-zero exit is a PLAIN failure, not the clean-exit forensic", async () => {
    const res = await enqueueSettled(h, {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["boom"], // exits 7
      evidence: [{ kind: "exit", code: 0 }],
    });
    expect(res.status).toBe("failed");
    expect(res.verdict).toBe("failed");
    expect(res.verdict).not.toBe("clean-exit-no-artifact");
    const lines = readLedger(h.ledger.filePath);
    expect(lines[0]!.verdict).toBe("failed");
    expect(lines[0]!.exit_code).toBe(7);
  });

  test("a durable row for a forensic failure is completed (not left pending) — the run terminated", async () => {
    const res = await enqueueSettled(h, {
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["noop"],
      evidence: [{ kind: "artifact", path: h.artifactPath("absent.txt") }, { kind: "exit", code: 0 }],
    });
    expect(res.task_uuid).not.toBeNull();
    const row = h.task.get(res.task_uuid!);
    // The row is no longer pending — a terminated (failed) run does not leave a runnable ghost for
    // recover() to re-present.
    expect(row?.status).not.toBe("pending");
  });
});
