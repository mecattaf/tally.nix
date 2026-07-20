// test/cli/witness-verify.test.ts
//
// `tally witness verify [--ledger <path>]` — the DAEMONLESS hash-chain verify CLI on a tampered
// fixture ledger (IMPLEMENTATION-PLAN M3.1 tests: "witness verify CLI on a tampered fixture ledger").
// Exercises the exit code (0 intact / non-zero broken), the --ledger override, and the JSON report
// shape (exact breaking seq).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { describe, expect, test } from "bun:test";
import { join } from "node:path";
import { runCli } from "../../src/cli/index.ts";
import { captureWriter } from "../../src/cli/output.ts";

const FIXTURE_DIR = join(import.meta.dir, "..", "fixtures", "ledger");

describe("tally witness verify", () => {
  test("a valid ledger verifies ok (exit 0) with the record count", async () => {
    const w = captureWriter();
    const code = await runCli(["witness", "verify", "--ledger", join(FIXTURE_DIR, "valid.jsonl"), "--json"], { writer: w });
    expect(code).toBe(0);
    const report = JSON.parse(w.stdout);
    expect(report.ok).toBe(true);
    expect(report.records).toBeGreaterThan(0);
    expect(report.problems).toEqual([]);
  });

  test("a tampered ledger fails (non-zero) and names the breaking seq", async () => {
    const w = captureWriter();
    const code = await runCli(["witness", "verify", "--ledger", join(FIXTURE_DIR, "tampered.jsonl"), "--json"], { writer: w });
    expect(code).toBe(1);
    const report = JSON.parse(w.stdout);
    expect(report.ok).toBe(false);
    expect(report.problems.length).toBeGreaterThan(0);
    // At least one problem carries a concrete breaking seq.
    expect(report.problems.some((p: { seq: number | null }) => typeof p.seq === "number")).toBe(true);
  });

  test("text output surfaces the FAILED banner + problem lines on tamper", async () => {
    const w = captureWriter();
    const code = await runCli(["witness", "verify", "--ledger", join(FIXTURE_DIR, "tampered.jsonl")], { writer: w });
    expect(code).toBe(1);
    expect(w.stdout).toContain("FAILED");
  });

  test("a missing ledger verifies as empty+ok (exit 0)", async () => {
    const w = captureWriter();
    const code = await runCli(["witness", "verify", "--ledger", "/nonexistent/witness.jsonl", "--json"], { writer: w });
    expect(code).toBe(0);
    const report = JSON.parse(w.stdout);
    expect(report.ok).toBe(true);
    expect(report.records).toBe(0);
  });

  test("unknown witness verb is a usage error (exit 2)", async () => {
    const w = captureWriter();
    const code = await runCli(["witness", "bogus"], { writer: w });
    expect(code).toBe(2);
    expect(w.stderr).toContain("unknown witness verb");
  });
});
