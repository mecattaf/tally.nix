// test/helpers/fake-zmx.ts
//
// A fake of the zmx read surface. tally is ENUMERATE-ONLY over zmx
// (CLI-SURFACE §3.2): it reads `zmx list --short` for the session universe and
// NEVER creates/names/attaches/kills. This fake therefore serves `list` and
// FORBIDS `attach`/`kill`/`new`/`a` (throws loudly) so an accidental lifecycle
// call surfaces as a test failure, mirroring the boundary law.
//
// `persistence_session_id` == the zmx session name (a dotfiles timestamp name
// like `term-0707-1530`). The `--short` output is one session name per line,
// which is the exact form the dotfiles `desk-resume` fzf picker consumes.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { type FakeExec, type ExecResult, ok, fail, parseArgs } from "./exec-fakes.ts";

/** One zmx session as the fake models it. */
export interface FakeZmxSession {
  /** The zmx session name == persistence_session_id. */
  name: string;
  /** Whether a client is currently attached (informational; `list` ignores it). */
  attached?: boolean;
  /** Optional created timestamp for the long-form listing. */
  created?: string;
}

/**
 * A programmable zmx session universe. Add sessions, then `install(exec)`.
 */
export class FakeZmx {
  private readonly sessions: FakeZmxSession[] = [];
  /** Forbidden lifecycle attempts, recorded for assertion (should stay empty). */
  readonly forbiddenAttempts: string[] = [];

  /** Add a session (or list of names) to the universe. */
  add(...names: Array<string | FakeZmxSession>): this {
    for (const n of names) {
      this.sessions.push(typeof n === "string" ? { name: n } : n);
    }
    return this;
  }

  /** Remove a session by name (simulate it ending). */
  remove(name: string): void {
    const i = this.sessions.findIndex((s) => s.name === name);
    if (i !== -1) this.sessions.splice(i, 1);
  }

  /** Current session names. */
  names(): string[] {
    return this.sessions.map((s) => s.name);
  }

  install(exec: FakeExec): this {
    exec.register("zmx", (args): ExecResult => {
      const verb = args[0];
      const parsed = parseArgs(args.slice(1));
      switch (verb) {
        case "list":
        case "ls": {
          if (parsed.has("short") || parsed.bools.has("short")) {
            // One session name per line (the `--short` contract).
            return ok(this.sessions.map((s) => s.name).join("\n") + (this.sessions.length ? "\n" : ""));
          }
          // Long form: name plus a marker column (still read-only).
          const lines = this.sessions.map(
            (s) => `${s.name}\t${s.attached ? "attached" : "detached"}\t${s.created ?? ""}`,
          );
          return ok(lines.join("\n") + (lines.length ? "\n" : ""));
        }
        case "attach":
        case "a":
        case "new":
        case "kill":
        case "rename": {
          // Lifecycle is dotfiles-owned; tally must never call these.
          this.forbiddenAttempts.push([verb, ...args.slice(1)].join(" "));
          throw new Error(
            `fake-zmx: \`zmx ${verb}\` is forbidden by the tally boundary ` +
              `(CLI-SURFACE §3.2 MUST-NOT list). tally enumerates only.`,
          );
        }
        default:
          return fail(2, `fake-zmx: unsupported verb '${verb}'`);
      }
    });
    return this;
  }
}
