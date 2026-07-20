// tally — the agent-adapter surface (IMPLEMENTATION-PLAN M2.2 `src/agents/*`; SPEC "Three planes",
// "The spawn-tracked-agent-job"; CLI-SURFACE §0/§1.1a, §3.4).
//
// The jobs dispatcher speaks to the leaf worker through ONE adapter per `AgentKind` (`pi`,
// `claude-code`, `shell`). An adapter's whole job is to turn a Seam-A enqueue into (a) the leaf
// argv the transient unit runs under the pls lease and (b) the extraction of `session_ref` + `model`
// from the enqueue params / the finished run, so the witness line and the `--resume`/recover() join
// are populated WITHOUT the dispatcher knowing any harness's flags. Model choice is NEVER made here
// (PS#2 — the model is DECLARED, carried from ignition, never re-picked).
//
// No adapter shells out itself: it only BUILDS the argv the dispatcher runs through the injected
// `Exec` transient-unit path. The boundary law (CLI-SURFACE §3.1) is upheld by construction — no
// adapter here calls `kitty @ launch`; the sole `--via-terminal` carve-out lives in its own
// dedicated file (M1.6 boundary grep-test carve-out), never in this set.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { AgentKind, EnqueueParams } from "../contracts/index";

/**
 * The resolved leaf invocation an adapter produces from an enqueue. `argv` is the exact command the
 * transient systemd unit (or the dev-rig `Bun.spawn` fallback) runs under the lease. `sessionRef` is
 * the harness session id when the adapter can derive it up-front (e.g. a `--session <id>` resume
 * binding); it may be refined post-run by {@link AgentAdapter.extract}. `model` is the DECLARED model
 * id (from `--model-class`, normalized to a models.dev id) or null on shell runs.
 */
export interface LeafInvocation {
  /** The exact argv the leaf worker runs (already merged: the harness prefix + the caller command). */
  argv: string[];
  /** The harness session id for the `--resume`/recover() join, or null when not yet known. */
  sessionRef: string | null;
  /** The declared models.dev model id, or null (shell / no model declared). */
  model: string | null;
  /** The resume verb an adapter re-dispatches with on recover() (`pi --session <id>` etc.), or null. */
  resumeArgv: string[] | null;
}

/**
 * What an adapter learns from a completed run's captured output, refining the leaf invocation. A
 * harness may print its session id to stdout on start; the adapter parses it here so recover() and
 * the witness `trace_ref`/`session_ref` join have the real value even when the enqueue did not
 * declare one. Absent fields leave the pre-run values in place.
 */
export interface RunExtract {
  sessionRef?: string | null;
  model?: string | null;
  traceRef?: string | null;
}

/**
 * The context an adapter reads to build a leaf invocation: the validated enqueue params plus the
 * leaf command the caller supplied (already normalized to an argv by the engine — `invocation`
 * shell-split or the explicit `argv`).
 */
export interface AdapterContext {
  params: EnqueueParams;
  /** The caller's leaf command as an argv (from `--invocation` split, or `-- <argv...>`). */
  command: string[];
  /**
   * The zmx session to bind an existing resume onto (`--session`), or null. tally READS this session
   * (never creates it, CLI-SURFACE §1.2) — the adapter only uses it to shape a `--resume` flag.
   */
  session: string | null;
}

/**
 * One agent-kind adapter (IMPLEMENTATION-PLAN M2.2 `src/agents/*`). Pure: it constructs argv and
 * parses output; it never touches the process, the lease, or the socket.
 */
export interface AgentAdapter {
  readonly kind: AgentKind;
  /** Build the leaf invocation for a fresh dispatch. */
  build(ctx: AdapterContext): LeafInvocation;
  /**
   * Build the leaf invocation for a RE-dispatch (recover()/preemption resume): resume the recorded
   * `sessionRef` rather than starting fresh. Falls back to a fresh build when no session ref exists.
   */
  resume(ctx: AdapterContext, sessionRef: string | null): LeafInvocation;
  /** Parse a completed run's captured stdout for the real session ref / model / trace ref. */
  extract(stdout: string): RunExtract;
}

/**
 * Split a `--invocation "<cmd>"` string into an argv with minimal, quote-aware tokenization. tally
 * never runs the command through a shell (the transient unit execs the argv directly), so a caller
 * that needs shell semantics passes an explicit `-- <argv...>` or `sh -c "…"`. This tokenizer honors
 * single and double quotes and backslash-escapes so a quoted path with spaces survives; it is
 * deliberately small (no glob, no env expansion — that is the shell's job when asked for).
 */
export function splitInvocation(invocation: string): string[] {
  const out: string[] = [];
  let cur = "";
  let quote: '"' | "'" | null = null;
  let sawToken = false;
  for (let i = 0; i < invocation.length; i++) {
    const ch = invocation[i]!;
    if (quote) {
      if (ch === "\\" && quote === '"' && i + 1 < invocation.length) {
        cur += invocation[++i]!;
        continue;
      }
      if (ch === quote) {
        quote = null;
        continue;
      }
      cur += ch;
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
      sawToken = true;
      continue;
    }
    if (ch === "\\" && i + 1 < invocation.length) {
      cur += invocation[++i]!;
      sawToken = true;
      continue;
    }
    if (ch === " " || ch === "\t" || ch === "\n") {
      if (sawToken) {
        out.push(cur);
        cur = "";
        sawToken = false;
      }
      continue;
    }
    cur += ch;
    sawToken = true;
  }
  if (quote) throw new Error(`unterminated ${quote} quote in invocation: ${invocation}`);
  if (sawToken) out.push(cur);
  return out;
}

/** Shell metacharacters that mean something to a shell but are inert (literal argv tokens) once
 * `splitInvocation` hands them to a directly-exec'd argv: redirection, pipe, sequencing/background,
 * and command substitution. `findUnquotedShellMetachars` below flags these so the CLI enqueue path
 * (issue #6) can warn a caller who typed `--invocation "cmd > file"` expecting shell semantics. */
const SHELL_METACHARS = new Set(["<", ">", "|", ";", "&"]);

/**
 * Scan a `--invocation` string for shell metacharacters that would be meaningful to a shell but are
 * NOT (they are literal argv tokens, since `splitInvocation`/`resolveCommand` above never runs the
 * command through a shell). Mirrors `splitInvocation`'s quote/backslash-escape tracking exactly —
 * only UNQUOTED occurrences count, since a quoted `>` is plainly intended as a literal argument.
 * Returns the distinct metacharacters/sequences found, in first-seen order; empty when none. This is
 * an enqueue-time WARNING signal (CLI-SURFACE §1.1a), not a validation rule — the evidence gate is
 * the ruled backstop, and a literal `>` argv token is legal.
 */
export function findUnquotedShellMetachars(invocation: string): string[] {
  const found: string[] = [];
  const seen = new Set<string>();
  let quote: '"' | "'" | null = null;
  for (let i = 0; i < invocation.length; i++) {
    const ch = invocation[i]!;
    if (quote) {
      if (ch === "\\" && quote === '"' && i + 1 < invocation.length) {
        i++;
        continue;
      }
      if (ch === quote) quote = null;
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
      continue;
    }
    if (ch === "\\" && i + 1 < invocation.length) {
      i++;
      continue;
    }
    if (ch === "$" && invocation[i + 1] === "(") {
      if (!seen.has("$(")) {
        seen.add("$(");
        found.push("$(");
      }
      i++;
      continue;
    }
    if (SHELL_METACHARS.has(ch)) {
      const token = ch === "&" && invocation[i + 1] === "&" ? "&&" : ch;
      if (!seen.has(token)) {
        seen.add(token);
        found.push(token);
      }
      if (token === "&&") i++;
      continue;
    }
  }
  return found;
}

/**
 * Normalize a Seam-A enqueue's leaf command to an argv: exactly one of `invocation` / `argv` is set
 * (the wire validator guarantees the XOR), so this resolves the present one. `invocation` is
 * tokenized; `argv` is used verbatim.
 */
export function resolveCommand(params: Pick<EnqueueParams, "invocation" | "argv">): string[] {
  if (params.argv !== undefined) {
    if (params.argv.length === 0) throw new Error("enqueue argv must be non-empty");
    return [...params.argv];
  }
  if (params.invocation !== undefined) {
    const argv = splitInvocation(params.invocation);
    if (argv.length === 0) throw new Error("enqueue invocation tokenized to an empty argv");
    return argv;
  }
  throw new Error("enqueue requires exactly one of invocation / argv");
}
