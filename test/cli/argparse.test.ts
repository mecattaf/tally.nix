// test/cli/argparse.test.ts
//
// The CLI arg-parse table for every verb (IMPLEMENTATION-PLAN M3.1 tests: "arg-parse table for every
// verb"). Exercises the hand-rolled tokenizer + the Seam-A enqueue param builder + the duration
// parser + the verdict→exit-code mapping, all pure and daemonless.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { describe, expect, test } from "bun:test";
import { parseArgs, flag, flagAll, hasFlag, wantsJson } from "../../src/cli/index.ts";
import { buildEnqueueParams, parseDuration, verdictExitCode } from "../../src/cli/queue.ts";
import { captureWriter } from "../../src/cli/output.ts";
import type { CliContext } from "../../src/cli/index.ts";

function ctxFor(rest: string[], noun = "queue", verb = "enqueue"): CliContext {
  return { noun, verb, args: parseArgs(rest), writer: captureWriter(), env: {} };
}

describe("parseArgs — the hand-rolled tokenizer", () => {
  test("splits positionals, value flags, boolean flags", () => {
    const p = parseArgs(["term-0707:p2", "--source", "gh", "--json", "--priority", "high"]);
    expect(p.positionals).toEqual(["term-0707:p2"]);
    expect(flag(p, "--source")).toBe("gh");
    expect(flag(p, "--priority")).toBe("high");
    expect(hasFlag(p, "--json")).toBe(true);
    expect(wantsJson(p)).toBe(true);
  });

  test("--flag=value form", () => {
    const p = parseArgs(["--pool=worker-gpu", "--kind=shell"]);
    expect(flag(p, "--pool")).toBe("worker-gpu");
    expect(flag(p, "--kind")).toBe("shell");
  });

  test("repeatable flags accumulate (--evidence)", () => {
    const p = parseArgs(["--evidence", "artifact:/out.txt", "--evidence", "exit:0"]);
    expect(flagAll(p, "--evidence")).toEqual(["artifact:/out.txt", "exit:0"]);
  });

  test("`--` captures the leaf-worker argv as passthrough", () => {
    const p = parseArgs(["--kind", "shell", "--", "ocr", "--in", "paper.pdf"]);
    expect(p.passthrough).toEqual(["ocr", "--in", "paper.pdf"]);
    expect(flag(p, "--kind")).toBe("shell");
  });

  test("boolean flags never swallow the next token", () => {
    const p = parseArgs(["--wait", "term-0707:p2"]);
    expect(hasFlag(p, "--wait")).toBe(true);
    expect(p.positionals).toEqual(["term-0707:p2"]);
  });
});

describe("buildEnqueueParams — Seam A", () => {
  test("full flag set validates through the shared validator", () => {
    const ctx = ctxFor([
      "--priority", "high", "--source", "gh", "--kind", "claude-code",
      "--invocation", "claude --resume abc",
      "--cwd", "/home/tom/work",
      "--evidence", "artifact:/out.md", "--evidence", "exit:0",
      "--pool", "worker-gpu", "--model-class", "opus", "--dedup-key", "k1",
    ]);
    const p = buildEnqueueParams(ctx);
    expect(p.priority).toBe("high");
    expect(p.source).toBe("gh");
    expect(p.kind).toBe("claude-code");
    expect(p.invocation).toBe("claude --resume abc");
    expect(p.cwd).toBe("/home/tom/work");
    expect(p.evidence).toEqual([
      { kind: "artifact", path: "/out.md" },
      { kind: "exit", code: 0 },
    ]);
    expect(p.pool).toBe("worker-gpu");
    expect(p.dedup_key).toBe("k1");
  });

  test("`-- <argv>` becomes argv (XOR with invocation)", () => {
    const ctx = ctxFor(["--kind", "shell", "--source", "manual", "--", "ocr", "paper.pdf"]);
    const p = buildEnqueueParams(ctx);
    expect(p.argv).toEqual(["ocr", "paper.pdf"]);
    expect(p.invocation).toBeUndefined();
  });

  test("defaults: priority=medium, source=manual, kind=shell when omitted", () => {
    const ctx = ctxFor(["--", "true"]);
    const p = buildEnqueueParams(ctx);
    expect(p.priority).toBe("medium");
    expect(p.source).toBe("manual");
    expect(p.kind).toBe("shell");
  });

  test("both --invocation and -- <argv> is rejected (XOR)", () => {
    const ctx = ctxFor(["--invocation", "x", "--", "y"]);
    expect(() => buildEnqueueParams(ctx)).toThrow();
  });

  test("barrier flags parse", () => {
    const ctx = ctxFor(["--kind", "shell", "--wait-group", "g1", "--wait-count", "3", "--", "true"]);
    const p = buildEnqueueParams(ctx);
    expect(p.wait_group).toBe("g1");
    expect(p.wait_count).toBe(3);
  });

  test("malformed --wait-count throws a clear error", () => {
    const ctx = ctxFor(["--kind", "shell", "--wait-count", "abc", "--", "true"]);
    expect(() => buildEnqueueParams(ctx)).toThrow(/wait-count/);
  });
});

describe("--invocation unquoted shell metachars — enqueue-time warning (issue #6)", () => {
  test("an unquoted '>' warns on stderr but still builds params (warn, not error)", () => {
    const ctx = ctxFor(["--kind", "shell", "--invocation", "printf x > out"]);
    const p = buildEnqueueParams(ctx);
    expect(p.invocation).toBe("printf x > out");
    const w = ctx.writer as ReturnType<typeof captureWriter>;
    expect(w.stderr).toContain(">");
    expect(w.stderr).toContain("sh -c");
  });

  test("pipe, semicolon, &&, and $( are all detected", () => {
    const ctx = ctxFor(["--kind", "shell", "--invocation", "a | b; c && d $(e)"]);
    buildEnqueueParams(ctx);
    const w = ctx.writer as ReturnType<typeof captureWriter>;
    expect(w.stderr).toContain("|");
    expect(w.stderr).toContain(";");
    expect(w.stderr).toContain("&&");
    expect(w.stderr).toContain("$(");
  });

  test("no warning when the invocation has no shell metacharacters", () => {
    const ctx = ctxFor(["--kind", "claude-code", "--invocation", "claude --resume abc"]);
    buildEnqueueParams(ctx);
    const w = ctx.writer as ReturnType<typeof captureWriter>;
    expect(w.stderr).toBe("");
  });

  test("a quoted metacharacter is not flagged — it's a deliberate literal argv token", () => {
    const ctx = ctxFor(["--kind", "shell", "--invocation", 'echo ">"']);
    buildEnqueueParams(ctx);
    const w = ctx.writer as ReturnType<typeof captureWriter>;
    expect(w.stderr).toBe("");
  });

  test("no --invocation (the `-- <argv>` form) never warns", () => {
    const ctx = ctxFor(["--kind", "shell", "--", "sh", "-c", "echo hi > out"]);
    buildEnqueueParams(ctx);
    const w = ctx.writer as ReturnType<typeof captureWriter>;
    expect(w.stderr).toBe("");
  });
});

describe("parseDuration", () => {
  test("bare number is seconds", () => {
    expect(parseDuration("30")).toBe(30_000);
  });
  test("suffixed forms", () => {
    expect(parseDuration("500ms")).toBe(500);
    expect(parseDuration("2s")).toBe(2000);
    expect(parseDuration("5m")).toBe(300_000);
    expect(parseDuration("1h")).toBe(3_600_000);
  });
  test("malformed throws", () => {
    expect(() => parseDuration("later")).toThrow();
  });
});

describe("verdictExitCode — the barrier exit mirrors the verdict (§1.1a)", () => {
  test("pass / reused ⇒ 0", () => {
    expect(verdictExitCode("pass")).toBe(0);
    expect(verdictExitCode("reused")).toBe(0);
  });
  test("clean-exit-no-artifact ⇒ distinguished forensic code 3 (matches barrier.ts)", () => {
    expect(verdictExitCode("clean-exit-no-artifact")).toBe(3);
  });
  test("failed ⇒ non-zero", () => {
    expect(verdictExitCode("failed")).toBe(1);
  });
  test("cancelled ⇒ 4 (matches barrier.ts — the CLI and daemon barriers agree)", () => {
    expect(verdictExitCode("cancelled")).toBe(4);
  });
});
