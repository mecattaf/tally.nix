// tally — the `shell` agent adapter (IMPLEMENTATION-PLAN M2.2 `src/agents/shell.ts`, §1 flag 5;
// CLI-SURFACE §0/§1.1a).
//
// A `shell`-kind unit is a plain leaf command with NO harness, NO model, and NO session_ref (SPEC
// "The spawn-tracked-agent-job" — the three kinds; §1 flag 5: shell status derives from process
// state, not a manifest). This is the kind Tom's OCR firehose runs as: a batch worker command per
// sidecar, gated by the pls lease, gated by the evidence gate — the model/session fields stay null
// and the witness `model` is absent (SPEC "Record schema": `model` absent on shell runs).
//
// The adapter is the trivial identity: the leaf argv IS the caller's command; a resume re-runs the
// same command (a shell run is idempotent-by-dedup, so recover() re-presents it verbatim). Never
// touches the process.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { AdapterContext, AgentAdapter, LeafInvocation, RunExtract } from "./kinds";

/** The shell adapter: the leaf argv is the caller's command; no model, no session ref. */
export class ShellAdapter implements AgentAdapter {
  readonly kind = "shell" as const;

  build(ctx: AdapterContext): LeafInvocation {
    return {
      argv: [...ctx.command],
      sessionRef: null,
      model: null,
      // A shell run has no session to resume; recover() re-presents the same command verbatim.
      resumeArgv: [...ctx.command],
    };
  }

  resume(ctx: AdapterContext, _sessionRef: string | null): LeafInvocation {
    // Shell runs carry no session ref — a re-present re-runs the same command (dedup skips it if the
    // artifact already exists; SPEC "Dedup-by-existence").
    return this.build(ctx);
  }

  extract(_stdout: string): RunExtract {
    // Shell runs have no harness-reported session/model/trace to parse.
    return {};
  }
}

/** The singleton shell adapter. */
export const shellAdapter = new ShellAdapter();
