// test/cli/output-help.test.ts
//
// The output rendering helpers + `tally --help` verb tree + `tally --version` + `hooks install
// --dry-run` (the composition-seam internal verbs the CLI dispatches). Pure/daemonless.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runCli, HELP_TEXT } from "../../src/cli/index.ts";
import { captureWriter, renderTree, statusDot, printJsonLines } from "../../src/cli/output.ts";

describe("--help / --version", () => {
  test("`tally --help` prints the frozen §1 verb tree", async () => {
    const w = captureWriter();
    const code = await runCli(["--help"], { writer: w });
    expect(code).toBe(0);
    // Every noun group + the top-level alias must appear (step-1 acceptance).
    for (const needle of ["queue", "session", "pane", "agent", "query", "tally enqueue", "witness verify", "pls-wrap", "hooks install"]) {
      expect(w.stdout).toContain(needle);
    }
    expect(w.stdout).toBe(HELP_TEXT);
  });

  test("no args prints help too", async () => {
    const w = captureWriter();
    const code = await runCli([], { writer: w });
    expect(code).toBe(0);
    expect(w.stdout).toContain("Usage: tally");
  });

  test("`tally --version --json` prints the version record", async () => {
    const w = captureWriter();
    const code = await runCli(["--version", "--json"], { writer: w });
    expect(code).toBe(0);
    const out = JSON.parse(w.stdout);
    expect(out.protocol_version).toBe(1);
    expect(typeof out.daemon_version).toBe("string");
  });
});

describe("output helpers", () => {
  test("statusDot maps the four wire statuses", () => {
    expect(statusDot("blocked")).toBe("!");
    expect(statusDot("working")).toBe("*");
    expect(statusDot("done")).toBe("+");
    expect(statusDot("idle")).toBe(".");
    expect(statusDot(null)).toBe(" ");
  });

  test("renderTree indents Workspace→Session→Pane", () => {
    const text = renderTree([
      { workspace: "ws", sessions: [{ session: "s1", status_rollup: { blocked: 0, working: 1, done: 0, idle: 0 }, panes: [{ pane: "s1:p1", kind: "pi", status: "working" }] }] },
    ]);
    expect(text).toContain("ws");
    expect(text).toContain("  s1");
    expect(text).toContain("    * s1:p1 pi");
  });

  test("printJsonLines emits one JSON object per line", () => {
    const w = captureWriter();
    printJsonLines(w, [{ a: 1 }, { a: 2 }]);
    expect(w.stdout).toBe('{"a":1}\n{"a":2}\n');
  });
});

describe("hooks install (dispatched internal verb)", () => {
  test("--dry-run computes a plan without writing (JSON)", async () => {
    const home = mkdtempSync(join(tmpdir(), "tally-hooks-"));
    try {
      const w = captureWriter();
      const code = await runCli(["hooks", "install", "--kind", "claude-code", "--dry-run", "--json"], {
        writer: w,
        env: { HOME: home, CLAUDE_CONFIG_DIR: join(home, ".claude") },
      });
      expect(code).toBe(0);
      const result = JSON.parse(w.stdout);
      expect(result.dryRun).toBe(true);
      expect(Array.isArray(result.actions)).toBe(true);
    } finally {
      rmSync(home, { recursive: true, force: true });
    }
  });

  test("an invalid --kind is a usage error", async () => {
    const w = captureWriter();
    const code = await runCli(["hooks", "install", "--kind", "emacs"], { writer: w, env: { HOME: "/tmp" } });
    expect(code).toBe(2);
    expect(w.stderr).toContain("--kind");
  });
});
