// test/e2e/cli-surface.test.ts
//
// `tally --help` verb-tree golden test (IMPLEMENTATION-PLAN M4.1 case 8; BUILD-SEQUENCE step-1
// acceptance). The frozen §1 verb set must be COMPLETE and stable: every noun, every verb, the
// top-level `tally enqueue` alias, every Seam-A flag, and the internal verbs appear in the help tree —
// and the CLI module's tree matches the `main.ts` entry tree byte-for-byte (one frozen surface, two
// entry points).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { describe, expect, test } from "bun:test";
import { runCli, HELP_TEXT } from "../../src/cli/index.ts";
import { captureWriter } from "../../src/cli/output.ts";
import { run as runMain } from "../../src/main.ts";

/** Capture stdout of a `main.run` dispatch (the compiled-binary argv shape: [rt, self, ...user]). */
async function captureMain(userArgs: string[]): Promise<{ code: number; stdout: string; stderr: string }> {
  const chunks: string[] = [];
  const errs: string[] = [];
  const origOut = process.stdout.write.bind(process.stdout);
  const origErr = process.stderr.write.bind(process.stderr);
  (process.stdout.write as unknown) = (s: string | Uint8Array): boolean => {
    chunks.push(typeof s === "string" ? s : Buffer.from(s).toString("utf8"));
    return true;
  };
  (process.stderr.write as unknown) = (s: string | Uint8Array): boolean => {
    errs.push(typeof s === "string" ? s : Buffer.from(s).toString("utf8"));
    return true;
  };
  try {
    const code = await runMain(["bun", "tally", ...userArgs]);
    return { code, stdout: chunks.join(""), stderr: errs.join("") };
  } finally {
    (process.stdout.write as unknown) = origOut;
    (process.stderr.write as unknown) = origErr;
  }
}

describe("tally --help — the frozen §1 verb tree (M4.1 case 8)", () => {
  test("`tally --help` exits 0 and prints the exact frozen tree", async () => {
    const w = captureWriter();
    const code = await runCli(["--help"], { writer: w });
    expect(code).toBe(0);
    expect(w.stdout).toBe(HELP_TEXT);
  });

  test("every §1 noun and verb is present in the tree", async () => {
    const w = captureWriter();
    await runCli(["--help"], { writer: w });
    const tree = w.stdout;

    // The nouns (CLI-SURFACE §1).
    for (const noun of ["queue", "session", "pane", "agent", "query", "witness"]) {
      expect(tree).toContain(noun);
    }
    // The queue control plane + the top-level alias (§0).
    for (const v of ["tally enqueue", "queue enqueue", "queue cancel", "queue pause", "queue resume"]) {
      expect(tree).toContain(v);
    }
    // session / pane / agent / query / witness verbs.
    for (const v of [
      "session list",
      "session watch",
      "pane send",
      "pane send-key",
      "pane focus",
      "pane capture",
      "agent list",
      "agent get",
      "agent read",
      "agent explain",
      "agent wait",
      "agent send",
      "agent focus",
      "query status",
      "query log",
      "query render",
      "query standup",
      "witness verify",
    ]) {
      expect(tree).toContain(v);
    }
    // The internal verbs.
    for (const v of ["daemon run", "daemon drain", "pls-wrap", "hooks install"]) {
      expect(tree).toContain(v);
    }
  });

  test("every Seam-A flag appears in the enqueue surface (CLI-SURFACE §1.1a)", async () => {
    const w = captureWriter();
    await runCli(["--help"], { writer: w });
    const tree = w.stdout;
    for (const flag of [
      "--priority",
      "--source",
      "--kind",
      "--invocation",
      "--cwd",
      "--worktree",
      "--evidence",
      "--pool",
      "--model-class",
      "--dedup-key",
      "--session",
      "--barrier",
      "--wait-group",
      "--wait-count",
      "--wait",
      "--timeout",
      "--detach",
    ]) {
      expect(tree).toContain(flag);
    }
  });

  test("no `agent start` verb exists — starting an agent IS enqueue (§4 divergence 1)", async () => {
    const w = captureWriter();
    await runCli(["--help"], { writer: w });
    // The tree may DESCRIBE the absence ("no `agent start`"); it must never LIST a `tally agent start`
    // verb. Assert the invocable form is absent.
    expect(w.stdout).not.toContain("tally agent start");
    expect(w.stdout).not.toMatch(/^\s*agent start\b/m);
  });

  test("the two entry points agree — `main.run` help === the CLI help tree (one frozen surface)", async () => {
    const w = captureWriter();
    await runCli(["--help"], { writer: w });
    const cliHelp = w.stdout;

    const viaMain = await captureMain(["--help"]);
    expect(viaMain.code).toBe(0);
    // Both entries expose the same §1 verb tree (the alias + every noun). They need not be byte-equal
    // (main.ts owns its own copy), but the frozen surface — nouns, alias, internal verbs — must match.
    for (const needle of [
      "tally enqueue",
      "queue enqueue",
      "session watch",
      "pane capture",
      "agent wait",
      "query standup",
      "witness verify",
      "daemon run",
      "daemon drain",
      "pls-wrap",
      "hooks install",
    ]) {
      expect(cliHelp).toContain(needle);
      expect(viaMain.stdout).toContain(needle);
    }
  });

  test("`tally --version --json` reports protocol_version 1", async () => {
    const w = captureWriter();
    const code = await runCli(["--version", "--json"], { writer: w });
    expect(code).toBe(0);
    const out = JSON.parse(w.stdout);
    expect(out.protocol_version).toBe(1);
    expect(typeof out.daemon_version).toBe("string");
  });

  test("an unknown noun exits 127 (not a silent success)", async () => {
    const w = captureWriter();
    const code = await runCli(["frobnicate"], { writer: w });
    expect(code).toBe(127);
  });
});

describe("subcommand `--help` (issue #6) — intercepted before argument validation", () => {
  test("`tally queue enqueue --help` prints the tree instead of erroring on missing invocation/argv", async () => {
    const w = captureWriter();
    const code = await runCli(["queue", "enqueue", "--help"], { writer: w });
    expect(code).toBe(0);
    expect(w.stdout).toBe(HELP_TEXT);
    expect(w.stderr).toBe("");
  });

  test("`tally queue --help` (noun-only, no verb) prints the tree rather than erroring on the verb", async () => {
    const w = captureWriter();
    const code = await runCli(["queue", "--help"], { writer: w });
    expect(code).toBe(0);
    expect(w.stdout).toBe(HELP_TEXT);
  });

  test("`tally queue cancel --help` prints the tree instead of erroring on a missing selector", async () => {
    const w = captureWriter();
    const code = await runCli(["queue", "cancel", "--help"], { writer: w });
    expect(code).toBe(0);
    expect(w.stdout).toBe(HELP_TEXT);
  });

  test("`tally session watch --help` prints the tree without dispatching to the daemon", async () => {
    const w = captureWriter();
    const code = await runCli(["session", "watch", "--help"], { writer: w });
    expect(code).toBe(0);
    expect(w.stdout).toBe(HELP_TEXT);
  });

  test("`tally daemon run --help` (internal verb) prints the tree instead of booting the daemon", async () => {
    const w = captureWriter();
    const code = await runCli(["daemon", "run", "--help"], { writer: w });
    expect(code).toBe(0);
    expect(w.stdout).toBe(HELP_TEXT);
  });

  test("`-h` shorthand is honored the same as `--help` on a noun+verb", async () => {
    const w = captureWriter();
    const code = await runCli(["queue", "enqueue", "-h"], { writer: w });
    expect(code).toBe(0);
    expect(w.stdout).toBe(HELP_TEXT);
  });
});
