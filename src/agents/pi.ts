// tally — the `pi` agent adapter (IMPLEMENTATION-PLAN M2.2 `src/agents/pi.ts`; CLI-SURFACE §3.4,
// §5 flag 3; SPEC "resume/recover: pi --session <id>").
//
// pi (badlogic/pi) is resumed by `pi --session <id>` (the herdr `session-state.mdx` /
// documented-interface binding — the vendor pin is STALE, so this adapter binds to the DOCUMENTED
// interface, never to a cloned tree, CLI-SURFACE §5 flag 3). The extensions dir (§3.4) is the
// cooperative-hook install target (owned by the hooks module M3.2), not this adapter's concern.
//
// A pi run's session id is the `--resume` join key carried as witness `trace_ref` / journald
// `TALLY_SESSION_REF`; the adapter derives it from a declared `--session` binding up-front, and
// refines it from the run's stdout when pi prints its session id (a `pi:session=<id>` marker on the
// documented interface). Model choice is DECLARED (`--model-class`), never re-picked (PS#2).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { normalizeModelId } from "../witness/index";
import type { AdapterContext, AgentAdapter, LeafInvocation, RunExtract } from "./kinds";

/** The stdout marker pi prints (documented interface) to announce its session id: `pi:session=<id>`. */
const PI_SESSION_MARKER = /pi:session=([A-Za-z0-9._-]+)/;
/** The stdout marker for the executing model, when pi reports it: `pi:model=<id>`. */
const PI_MODEL_MARKER = /pi:model=([^\s]+)/;
/** The stdout marker for a pi-RPC trace pointer, when present: `pi:trace=<ref>`. */
const PI_TRACE_MARKER = /pi:trace=([^\s]+)/;

/**
 * The pi adapter. Builds `pi [--session <id>] [--model-class <m>] -- <command>` for a fresh run, and
 * `pi --session <id> -- <command>` for a resume (recover()/preemption). Never touches the process.
 */
export class PiAdapter implements AgentAdapter {
  readonly kind = "pi" as const;

  build(ctx: AdapterContext): LeafInvocation {
    const argv = ["pi"];
    // A declared `--session` binds pi to an EXISTING session for resume (tally reads, never creates,
    // CLI-SURFACE §1.2). Absent, pi starts a fresh session and announces the id on stdout.
    const sessionRef = ctx.session;
    if (sessionRef !== null) {
      argv.push("--session", sessionRef);
    }
    const model = normalizeModelId(ctx.params.model_class ?? null);
    if (ctx.params.model_class !== undefined) {
      // Carry the DECLARED model class verbatim to pi (never re-picked, PS#2).
      argv.push("--model-class", ctx.params.model_class);
    }
    argv.push("--", ...ctx.command);
    return {
      argv,
      sessionRef,
      model,
      resumeArgv: sessionRef !== null ? ["pi", "--session", sessionRef, "--", ...ctx.command] : null,
    };
  }

  resume(ctx: AdapterContext, sessionRef: string | null): LeafInvocation {
    if (sessionRef === null) {
      // No recorded session — a fresh build is the only honest re-present (recover() re-presents,
      // never replays; a pi run with no session ref cannot be resumed, so it restarts fresh).
      return this.build(ctx);
    }
    const argv = ["pi", "--session", sessionRef];
    if (ctx.params.model_class !== undefined) {
      argv.push("--model-class", ctx.params.model_class);
    }
    argv.push("--", ...ctx.command);
    return {
      argv,
      sessionRef,
      model: normalizeModelId(ctx.params.model_class ?? null),
      resumeArgv: ["pi", "--session", sessionRef, "--", ...ctx.command],
    };
  }

  extract(stdout: string): RunExtract {
    const out: RunExtract = {};
    const s = PI_SESSION_MARKER.exec(stdout);
    if (s) out.sessionRef = s[1]!;
    const m = PI_MODEL_MARKER.exec(stdout);
    if (m) out.model = normalizeModelId(m[1]!);
    const t = PI_TRACE_MARKER.exec(stdout);
    if (t) out.traceRef = t[1]!;
    return out;
  }
}

/** The singleton pi adapter (adapters are stateless). */
export const piAdapter = new PiAdapter();
