// tally CLI — `tally witness verify [--ledger <path>]` (IMPLEMENTATION-PLAN M3.1; CLI-SURFACE §1.5
// witness; SPEC "Per-line hash chain" / "Independently verifiable").
//
// DAEMONLESS: runs the hash-chain verifier (M1.2 `verify.ts`) on ANY copy of the ledger with NO
// daemon — walks `seq` order, recomputes each `hash`, checks each `prev_hash`, reports the EXACT
// breaking `seq` + reason, and runs the separate sequence-gap completeness pass. Exit code 0 when the
// chain verifies, non-zero when it does not (so a CI/audit script gates on the exit).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { verifyLedgerFile, type VerifyReport } from "../witness/verify";
import { ledgerPath } from "../contracts/paths";
import { printJson, printLine, printError, type Writer } from "./output";
import { flag, wantsJson, type CliContext } from "./index";

/** Route the `witness` noun. */
export async function runWitnessCmd(ctx: CliContext): Promise<number> {
  switch (ctx.verb) {
    case "verify":
      return doVerify(ctx);
    default:
      printError(ctx.writer, `unknown witness verb '${ctx.verb ?? "(none)"}' (expected verify)`);
      return 2;
  }
}

function doVerify(ctx: CliContext): number {
  // `--ledger` overrides the XDG-resolved `$XDG_DATA_HOME/tally/witness.jsonl`; verify runs on any copy.
  const explicit = flag(ctx.args, "--ledger");
  const path = explicit ?? ledgerPath(ctx.env);

  const report = verifyLedgerFile(path);
  emitReport(ctx.writer, path, report, wantsJson(ctx.args));
  // The exit code is the audit gate: 0 ⇒ chain intact; non-zero ⇒ tampered/torn/reordered/gapped.
  return report.ok ? 0 : 1;
}

function emitReport(w: Writer, path: string, report: VerifyReport, json: boolean): void {
  if (json) {
    printJson(w, {
      ledger: path,
      ok: report.ok,
      records: report.records,
      first_seq: report.firstSeq,
      last_seq: report.lastSeq,
      problems: report.problems,
    });
    return;
  }
  if (report.ok) {
    printLine(w, `ok — ${report.records} record(s), seq ${report.firstSeq ?? 0}..${report.lastSeq ?? 0} (${path})`);
    return;
  }
  printLine(w, `FAILED — ${report.problems.length} problem(s) in ${path}`);
  for (const p of report.problems) {
    const seq = p.seq !== null ? `seq ${p.seq}` : `line ${p.line}`;
    printLine(w, `  ${p.kind} @ ${seq}: ${p.reason}`);
  }
}
