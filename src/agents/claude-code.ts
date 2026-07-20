// tally — the `claude-code` agent adapter (IMPLEMENTATION-PLAN M2.2 `src/agents/claude-code.ts`;
// CLI-SURFACE §3.1 sidenote; SPEC "resume/recover: claude --resume <id>").
//
// Claude Code is resumed by `claude --resume <id>` (the documented interface). The DEFAULT dispatch
// path runs the leaf under the transient unit like any other kind — headless, no terminal. The
// `claude -p` contingency (`--via-terminal`, DECISIONS Q6; §3.1 sidenote) is the SOLE boundary
// exception and lives entirely in its own dedicated file `src/agents/claude-p-contingency.ts` (the
// ONLY place under src/ where `kitty @ launch` appears — the M1.6 boundary grep-test carve-out).
// This adapter therefore NEVER references `kitty @ launch`; when a job is flagged `--via-terminal`
// the engine hands off to that dedicated function instead of running this adapter's argv. This file
// stays clean of the carve-out so the boundary law holds here by construction.
//
// Model choice is DECLARED (`--model-class`), never re-picked (PS#2).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { normalizeModelId } from "../witness/index";
import type { AdapterContext, AgentAdapter, LeafInvocation, RunExtract } from "./kinds";

/** Claude Code prints its session id on start; parse `session_id: <uuid>` / `session=<id>`. */
const CC_SESSION_MARKER = /(?:session[_ ]?id|session)[=:]\s*([A-Za-z0-9._-]+)/i;
/** The executing model, when Claude Code reports it: `model: <id>` / `model=<id>`. */
const CC_MODEL_MARKER = /model[=:]\s*([^\s]+)/i;

/**
 * The claude-code adapter. Builds `claude [--resume <id>] [--model <m>] -- <command>` for the
 * headless default path, and `claude --resume <id> -- <command>` for a resume. The `--via-terminal`
 * contingency is NOT built here (it is a separate, gated file); this adapter is the non-contingency
 * default only.
 */
export class ClaudeCodeAdapter implements AgentAdapter {
  readonly kind = "claude-code" as const;

  build(ctx: AdapterContext): LeafInvocation {
    const argv = ["claude"];
    const sessionRef = ctx.session;
    // A declared existing session binds a `--resume` (read, never create; CLI-SURFACE §1.2).
    if (sessionRef !== null) {
      argv.push("--resume", sessionRef);
    }
    const model = normalizeModelId(ctx.params.model_class ?? null);
    if (ctx.params.model_class !== undefined) {
      argv.push("--model", ctx.params.model_class);
    }
    argv.push("--", ...ctx.command);
    return {
      argv,
      sessionRef,
      model,
      resumeArgv: sessionRef !== null ? ["claude", "--resume", sessionRef, "--", ...ctx.command] : null,
    };
  }

  resume(ctx: AdapterContext, sessionRef: string | null): LeafInvocation {
    if (sessionRef === null) {
      return this.build(ctx);
    }
    const argv = ["claude", "--resume", sessionRef];
    if (ctx.params.model_class !== undefined) {
      argv.push("--model", ctx.params.model_class);
    }
    argv.push("--", ...ctx.command);
    return {
      argv,
      sessionRef,
      model: normalizeModelId(ctx.params.model_class ?? null),
      resumeArgv: ["claude", "--resume", sessionRef, "--", ...ctx.command],
    };
  }

  extract(stdout: string): RunExtract {
    const out: RunExtract = {};
    const s = CC_SESSION_MARKER.exec(stdout);
    if (s) out.sessionRef = s[1]!;
    const m = CC_MODEL_MARKER.exec(stdout);
    if (m) out.model = normalizeModelId(m[1]!);
    return out;
  }
}

/** The singleton claude-code adapter. */
export const claudeCodeAdapter = new ClaudeCodeAdapter();
