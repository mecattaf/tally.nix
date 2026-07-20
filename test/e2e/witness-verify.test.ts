// test/e2e/witness-verify.test.ts
//
// `tally witness verify` detects a byte-flipped line by exact seq (IMPLEMENTATION-PLAN M4.1 case 6;
// SPEC "Per-line hash chain"). The verify is DAEMONLESS — it runs on any copy of the ledger.
//
// End-to-end: the REAL jobs engine drains a small batch, producing a genuine chained ledger; the REAL
// CLI (`runCli`) verifies it clean; we then byte-flip one committed line and re-run the CLI, asserting
// a non-zero exit and a report that names the EXACT breaking seq.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { readFileSync, writeFileSync } from "node:fs";
import {
  makeEngineHarness,
  enqueueSettled,
  ocrEnqueue,
  readLedger,
  type EngineHarness,
} from "./helpers.ts";
import { runCli } from "../../src/cli/index.ts";
import { captureWriter } from "../../src/cli/output.ts";

describe("witness verify — byte-flip detection by exact seq (M4.1 case 6)", () => {
  let h: EngineHarness;
  beforeEach(() => {
    h = makeEngineHarness();
  });
  afterEach(() => h.cleanup());

  /** Drain a small batch through the real engine, producing a genuine chained ledger. */
  async function drainBatch(n: number): Promise<void> {
    for (let i = 0; i < n; i++) {
      const res = await enqueueSettled(h, 
        ocrEnqueue(h.artifactPath(`w-${i}.txt`), { dedup_key: `w-${i}`, source: "r2" }),
      );
      expect(res.status).toBe("completed");
    }
  }

  test("a genuine engine-produced ledger verifies clean (exit 0)", async () => {
    await drainBatch(4);
    const w = captureWriter();
    const code = await runCli(["witness", "verify", "--ledger", h.ledger.filePath, "--json"], { writer: w });
    expect(code).toBe(0);
    const report = JSON.parse(w.stdout);
    expect(report.ok).toBe(true);
    expect(report.records).toBe(4);
    expect(report.problems).toEqual([]);
  });

  test("byte-flipping a committed line makes verify fail (non-zero) at the exact breaking seq", async () => {
    await drainBatch(5);
    const lines = readLedger(h.ledger.filePath);
    expect(lines.length).toBe(5);

    // Tamper the line at seq 3: flip a value WITHOUT recomputing its hash, so the recomputed hash no
    // longer matches the stored one (a byte-flip in the middle of the chain).
    const targetSeq = 3;
    const raw = readFileSync(h.ledger.filePath, "utf8").split("\n").filter((l) => l.trim().length > 0);
    const idx = lines.findIndex((l) => l.seq === targetSeq);
    const parsed = JSON.parse(raw[idx]!) as Record<string, unknown>;
    parsed.exit_code = (parsed.exit_code as number) + 999; // flip a byte-bearing field, keep the stored hash
    raw[idx] = JSON.stringify(parsed);
    writeFileSync(h.ledger.filePath, raw.map((l) => l + "\n").join(""), "utf8");

    const w = captureWriter();
    const code = await runCli(["witness", "verify", "--ledger", h.ledger.filePath, "--json"], { writer: w });
    expect(code).toBe(1);
    const report = JSON.parse(w.stdout);
    expect(report.ok).toBe(false);
    // The report names a concrete breaking seq — and it is the tampered one (or the link that follows,
    // whose prev_hash no longer matches). Assert the tampered seq is implicated.
    const brokenSeqs = report.problems
      .map((p: { seq: number | null }) => p.seq)
      .filter((s: number | null): s is number => typeof s === "number");
    expect(brokenSeqs.length).toBeGreaterThan(0);
    expect(brokenSeqs).toContain(targetSeq);
  });

  test("text output surfaces the FAILED banner on a tampered engine ledger", async () => {
    await drainBatch(3);
    // Flip the tail line's content.
    const raw = readFileSync(h.ledger.filePath, "utf8").split("\n").filter((l) => l.trim().length > 0);
    const last = JSON.parse(raw[raw.length - 1]!) as Record<string, unknown>;
    last.attempt = (last.attempt as number) + 1;
    raw[raw.length - 1] = JSON.stringify(last);
    writeFileSync(h.ledger.filePath, raw.map((l) => l + "\n").join(""), "utf8");

    const w = captureWriter();
    const code = await runCli(["witness", "verify", "--ledger", h.ledger.filePath], { writer: w });
    expect(code).toBe(1);
    expect(w.stdout).toContain("FAILED");
  });
});
