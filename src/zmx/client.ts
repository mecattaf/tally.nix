// tally — the zmx read surface (IMPLEMENTATION-PLAN M1.6 `src/zmx/client.ts`; CLI-SURFACE §3.2).
//
// tally is ENUMERATE-ONLY over zmx. The whole session lifecycle — create, name, attach, reattach,
// detach, kill, rename — is DOTFILES-OWNED (CLI-SURFACE §3.2 MUST-NOT list). This client reads the
// session universe via `zmx list --short` and NOTHING ELSE. The `persistence_session_id` IS the zmx
// session name (a dotfiles timestamp name like `term-0707-1530`), distinct from `session_ref` (the
// harness JSONL id) — never conflated (CLI-SURFACE §0).
//
// The forbidden verbs (`attach`/`a`/`new`/`kill`/`rename`/`detach`) are NEVER constructed here; the
// M1.6 boundary grep-test asserts they appear nowhere under src/ (no zmx carve-out exists — the only
// carve-out is `kitty @ launch` in src/agents/claude-p-contingency.ts, and `zmx attach/kill` remain
// forbidden EVERYWHERE). This module names them only in comments/asserts, never in an argv.

import type { Exec, ExecOptions } from "../contracts/exec";
import { TallyError } from "../contracts/errors";

/** The zmx binary basename. */
export const ZMX_BIN = "zmx" as const;

/**
 * The zmx verbs tally is FORBIDDEN to invoke — the dotfiles-owned lifecycle (CLI-SURFACE §3.2). Named
 * here so the boundary is explicit and greppable; this module never builds an argv containing any of
 * them, and the enumerate-only client throws if asked to.
 */
export const FORBIDDEN_ZMX_VERBS = ["attach", "a", "new", "kill", "rename", "detach"] as const;

const FORBIDDEN_ZMX_VERB_SET: ReadonlySet<string> = new Set(FORBIDDEN_ZMX_VERBS);

/**
 * One enumerated zmx session. `name` == `persistence_session_id`. `--short` yields only the name; the
 * client models just that, since the name is the sole fact tally keys the session leg on.
 */
export interface ZmxSession {
  /** The zmx session name == persistence_session_id (CLI-SURFACE §0, §3.2). */
  name: string;
}

/**
 * Parse `zmx list --short` output — one session name per line (the exact form the dotfiles
 * `desk-resume` fzf picker consumes). Blank lines are dropped; surrounding whitespace trimmed.
 * Tolerant of a trailing newline or no trailing newline.
 */
export function parseListShort(stdout: string): ZmxSession[] {
  return stdout
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.length > 0)
    .map((name) => ({ name }));
}

/**
 * The enumerate-only zmx client. Takes an injected `Exec` (the only way it shells out) so it is
 * testable against the layer-0 `FakeZmx`. It exposes exactly ONE read verb; there is no method that
 * mutates zmx state, by construction.
 */
export class ZmxClient {
  constructor(
    private readonly exec: Exec,
    private readonly bin: string = ZMX_BIN,
  ) {}

  /**
   * `zmx list --short` — the session universe as `persistence_session_id`s. This is tally's ONLY zmx
   * call; discovery joins the result against `kitty @ ls` and watcher edges (session-model does the
   * join — sensors just enumerates).
   */
  async listShort(opts?: ExecOptions): Promise<ZmxSession[]> {
    const argv = [this.bin, "list", "--short"];
    const res = await this.exec.run(argv, opts);
    if (res.code !== 0) {
      throw new TallyError("internal", `zmx list --short failed (exit ${res.code}): ${res.stderr.trim()}`);
    }
    return parseListShort(res.stdout);
  }

  /** Just the session names — the common projection callers want. */
  async names(opts?: ExecOptions): Promise<string[]> {
    return (await this.listShort(opts)).map((s) => s.name);
  }

  /**
   * Assert a verb is not a forbidden lifecycle verb. The client never calls this internally (it only
   * ever runs `list --short`); it is exported defence-in-depth so a hypothetical future caller that
   * tried to route a lifecycle verb through the sensors surface fails loudly, matching the fake and
   * the grep-test.
   */
  static assertReadOnlyVerb(verb: string): void {
    if (FORBIDDEN_ZMX_VERB_SET.has(verb)) {
      throw new TallyError(
        "unsupported",
        `zmx ${verb} is forbidden (CLI-SURFACE §3.2 MUST-NOT list); the session lifecycle is ` +
          "dotfiles-owned — tally enumerates only via `zmx list --short`.",
      );
    }
  }
}
