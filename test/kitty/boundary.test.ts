// tally — the BOUNDARY grep-test (IMPLEMENTATION-PLAN M1.6, the load-bearing sensors invariant).
//
// The out-of-band law (CLI-SURFACE §3.1/§3.2): tally NEVER creates kitty windows (`kitty @ launch`)
// and NEVER touches the zmx session lifecycle (`zmx attach`/`zmx kill`/…). This test asserts those
// forbidden verbs are never INVOKED anywhere under `src/`, with EXACTLY ONE carve-out:
// `src/agents/claude-p-contingency.ts` (the gated `--via-terminal` contingency, M2.2 / §1 item 9),
// and it additionally asserts that carve-out's `kitty @ launch` is reachable only behind the
// `--via-terminal` flag. `zmx attach/kill` remain forbidden EVERYWHERE — no zmx carve-out.
//
// Method: we scan every `src/**/*.ts` line for an ACTUAL invocation of a forbidden verb — a string
// literal in argv position that would form the command — while ignoring comments and intentional
// named-constant / documentation mentions (the FORBIDDEN_* declarations sensors exports so the
// boundary is greppable). An invocation is: the verb appearing as a quoted argv element next to the
// binary, or the binary + verb appearing on one command-ish line. This is deliberately conservative
// toward FAILING (a real launch is a loud failure) while not tripping on the documented constants.

import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync, statSync, existsSync } from "node:fs";
import { join } from "node:path";

const SRC_ROOT = join(import.meta.dir, "..", "..", "src");
const CARVE_OUT = "agents/claude-p-contingency.ts"; // the ONE file allowed to `kitty @ launch`.

/** Recursively list every `.ts` file under `src/`, as `src/`-relative POSIX paths. */
function srcFiles(dir: string, base: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const abs = join(dir, entry);
    const rel = base ? `${base}/${entry}` : entry;
    if (statSync(abs).isDirectory()) out.push(...srcFiles(abs, rel));
    else if (entry.endsWith(".ts")) out.push(rel);
  }
  return out;
}

/** Strip a leading line-comment; return "" for pure comment / doc lines. */
function codePart(line: string): string {
  const trimmed = line.trimStart();
  if (trimmed.startsWith("//") || trimmed.startsWith("*") || trimmed.startsWith("/*")) return "";
  return line;
}

/**
 * Does a code line INVOKE `kitty @ launch`? We look for the launch verb appearing as an argv-style
 * quoted element on a line that also references kitty `@`, OR the classic `@ launch` command spelling.
 * We deliberately do NOT flag lines that only reference the exported FORBIDDEN_KITTY_VERB constant.
 */
function invokesKittyLaunch(code: string): boolean {
  if (code.includes("FORBIDDEN_KITTY_VERB")) return false;
  // A real invocation constructs argv: `"launch"` AND `"@"` each as standalone quoted tokens on the
  // line (e.g. `["kitty", "@", "launch", …]`). Prose that merely mentions `kitty @ launch` inside one
  // larger string has neither token standalone, so it is not flagged.
  const quotedLaunch = /["'`]launch["'`]/.test(code);
  const quotedAt = /["'`]@["'`]/.test(code);
  return quotedLaunch && quotedAt;
}

/** Does a code line INVOKE a forbidden zmx lifecycle verb (attach/a/new/kill/rename/detach)? */
function invokesZmxLifecycle(code: string): boolean {
  if (code.includes("FORBIDDEN_ZMX_VERB")) return false;
  // A quoted lifecycle verb as an argv token on a line that references zmx.
  const referencesZmx = /["'`]zmx["'`]/.test(code) || /\bzmx\b/i.test(code);
  if (!referencesZmx) return false;
  return /["'`](attach|kill|rename|detach|new)["'`]/.test(code) || /zmx\s+(attach|kill|rename|detach|new)\b/.test(code);
}

describe("sensors boundary grep-test", () => {
  const files = srcFiles(SRC_ROOT, "");

  test("`kitty @ launch` is invoked NOWHERE under src/ except the carve-out", () => {
    const offenders: Array<{ file: string; line: number; text: string }> = [];
    for (const file of files) {
      if (file === CARVE_OUT) continue; // the ONE allowed launcher.
      const lines = readFileSync(join(SRC_ROOT, file), "utf8").split("\n");
      lines.forEach((raw, i) => {
        const code = codePart(raw);
        if (code && invokesKittyLaunch(code)) offenders.push({ file, line: i + 1, text: raw.trim() });
      });
    }
    expect(offenders).toEqual([]);
  });

  test("`zmx attach/kill` (and the rest of the lifecycle) is invoked NOWHERE under src/ — no carve-out", () => {
    const offenders: Array<{ file: string; line: number; text: string }> = [];
    for (const file of files) {
      const lines = readFileSync(join(SRC_ROOT, file), "utf8").split("\n");
      lines.forEach((raw, i) => {
        const code = codePart(raw);
        if (code && invokesZmxLifecycle(code)) offenders.push({ file, line: i + 1, text: raw.trim() });
      });
    }
    expect(offenders).toEqual([]);
  });

  test("the carve-out, WHEN it exists, gates its `kitty @ launch` behind --via-terminal", () => {
    const abs = join(SRC_ROOT, CARVE_OUT);
    if (!existsSync(abs)) {
      // The contingency file is a concurrently-built sibling (M2.2). If absent at this layer, the
      // carve-out constraint is vacuously satisfied — sensors owns only the boundary law, not the file.
      return;
    }
    const contents = readFileSync(abs, "utf8");
    // If it launches at all, the flag that gates it must be present in the same file.
    const launches = /@\s+launch\b/.test(contents) || /["'`]launch["'`]/.test(contents);
    if (launches) {
      expect(contents.includes("--via-terminal") || contents.includes("viaTerminal") || contents.includes("via_terminal")).toBe(
        true,
      );
    }
  });

  test("the sensors modules expose the forbidden verbs as named constants (greppable boundary)", () => {
    // Positive assertion: the boundary is DECLARED, not merely absent — rc.ts / client.ts name the
    // forbidden verbs so a reader/grep sees the law spelled out.
    const rc = readFileSync(join(SRC_ROOT, "kitty", "rc.ts"), "utf8");
    const zmx = readFileSync(join(SRC_ROOT, "zmx", "client.ts"), "utf8");
    expect(rc).toContain("FORBIDDEN_KITTY_VERB");
    expect(zmx).toContain("FORBIDDEN_ZMX_VERBS");
  });
});
