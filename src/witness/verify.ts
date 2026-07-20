// tally — the daemonless witness verifier (SPEC "Per-line hash chain" / "Independently verifiable";
// IMPLEMENTATION-PLAN M1.2). Backs `tally witness verify [--ledger <path>]`. Runs on ANY copy of
// the ledger with NO daemon: reads the JSONL, walks records in `seq` order, recomputes each `hash`,
// checks each `prev_hash` against its predecessor, and reports the exact breaking `seq` + reason.
// Sequence-gap (completeness) checking is a SEPARATE pass.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { existsSync, readFileSync } from "node:fs";
import { GENESIS_PREV_HASH, type WitnessRecord } from "../contracts/index";
import { computeHash } from "./chain";
import { parseRecord } from "./record";

/** A single verification failure with the exact breaking `seq` and machine-readable kind + reason. */
export interface VerifyProblem {
  /** The seq of the offending line. For a parse failure the seq may be unknown (`null`). */
  seq: number | null;
  /** 1-based physical line number in the ledger file, for operator forensics. */
  line: number;
  kind:
    | "parse-error"
    | "invalid-record"
    | "hash-mismatch"
    | "prev-hash-mismatch"
    | "seq-order"
    | "seq-gap"
    | "seq-duplicate";
  reason: string;
}

/** The result of a full verify pass. */
export interface VerifyReport {
  ok: boolean;
  /** Total records that parsed AND validated. */
  records: number;
  /** The seq of the first line, when any valid record exists. */
  firstSeq: number | null;
  /** The seq of the last line, when any valid record exists. */
  lastSeq: number | null;
  /** Every problem found, in physical-line order; empty ⇒ `ok:true`. */
  problems: VerifyProblem[];
}

/**
 * Verify an already-loaded array of raw parsed ledger objects (each is one JSONL line's `JSON.parse`
 * result, in physical file order). `parseErrors` carries lines that failed `JSON.parse` upstream
 * (physical line number → the raw text) so the report is complete even for un-parseable lines.
 *
 * The chain walk: for each valid record in physical order it (1) validates the record shape,
 * (2) recomputes `hash` over the cleared form and compares to the stored `hash`, (3) checks
 * `prev_hash` equals the predecessor's stored `hash` (genesis ⇒ `GENESIS_PREV_HASH`), (4) checks
 * `seq` is strictly increasing by physical order. A separate completeness pass then checks the seq
 * set is a gapless 1..N run.
 */
export function verifyRecords(
  rawLines: readonly unknown[],
  parseErrors: ReadonlyMap<number, string> = new Map(),
): VerifyReport {
  const problems: VerifyProblem[] = [];

  // Fold parse errors in at their physical line numbers.
  for (const [lineNo, _text] of parseErrors) {
    problems.push({
      seq: null,
      line: lineNo,
      kind: "parse-error",
      reason: "line is not valid JSON",
    });
  }

  const valid: { record: WitnessRecord; line: number }[] = [];
  rawLines.forEach((raw, idx) => {
    const line = idx + 1;
    const res = parseRecord(raw);
    if (!res.ok) {
      const seq =
        typeof (raw as { seq?: unknown } | null)?.seq === "number"
          ? ((raw as { seq: number }).seq)
          : null;
      problems.push({ seq, line, kind: "invalid-record", reason: res.reason });
      return;
    }
    valid.push({ record: res.record, line });
  });

  // --- chain walk (physical order) ---
  let prevHash = GENESIS_PREV_HASH;
  let prevSeq = 0;
  for (const { record, line } of valid) {
    // (2) hash integrity — recompute over the cleared form.
    const recomputed = computeHash(record);
    if (recomputed !== record.hash) {
      problems.push({
        seq: record.seq,
        line,
        kind: "hash-mismatch",
        reason: `stored hash ${record.hash} != recomputed ${recomputed} (line tampered)`,
      });
    }
    // (3) linkage — prev_hash must equal the predecessor's stored hash.
    if (record.prev_hash !== prevHash) {
      problems.push({
        seq: record.seq,
        line,
        kind: "prev-hash-mismatch",
        reason: `prev_hash ${record.prev_hash} != predecessor hash ${prevHash} (chain broken)`,
      });
    }
    // (4) seq must strictly increase in physical order.
    if (record.seq <= prevSeq) {
      problems.push({
        seq: record.seq,
        line,
        kind: "seq-order",
        reason: `seq ${record.seq} does not strictly follow ${prevSeq} (reordered or duplicate)`,
      });
    }
    prevHash = record.hash;
    prevSeq = record.seq;
  }

  // --- completeness pass (separate): the valid seq set must be a gapless 1..N run ---
  // `expected` advances only when a NEW (non-duplicate) seq is consumed, so a duplicate seq does not
  // shift the gap arithmetic and fabricate a spurious "missing line" forensic for a seq that is present.
  const seqs = valid.map((v) => v.record.seq).sort((a, b) => a - b);
  const seen = new Set<number>();
  let expected = 1;
  for (const s of seqs) {
    if (seen.has(s)) {
      problems.push({ seq: s, line: -1, kind: "seq-duplicate", reason: `seq ${s} appears more than once` });
      continue;
    }
    seen.add(s);
    if (s !== expected) {
      problems.push({
        seq: s,
        line: -1,
        kind: "seq-gap",
        reason: `expected seq ${expected} but found ${s} (missing line)`,
      });
      break;
    }
    expected++;
  }

  const firstSeq = valid.length > 0 ? valid[0]!.record.seq : null;
  const lastSeq = valid.length > 0 ? valid[valid.length - 1]!.record.seq : null;

  return {
    ok: problems.length === 0,
    records: valid.length,
    firstSeq,
    lastSeq,
    problems,
  };
}

/**
 * Verify a ledger file at `path` (daemonless — the CLI `tally witness verify` entry). Reads the
 * file, splits on LF, `JSON.parse`s each non-blank line (collecting parse failures), and runs
 * {@link verifyRecords}. A missing file verifies as an empty, ok ledger.
 */
export function verifyLedgerFile(path: string): VerifyReport {
  if (!existsSync(path)) {
    return { ok: true, records: 0, firstSeq: null, lastSeq: null, problems: [] };
  }
  const text = readFileSync(path, "utf8");
  const rawLines: unknown[] = [];
  const parseErrors = new Map<number, string>();
  const physicalLines = text.split("\n");
  let physicalLineNo = 0;
  for (const line of physicalLines) {
    physicalLineNo++;
    if (line.trim().length === 0) continue;
    try {
      rawLines.push(JSON.parse(line));
    } catch {
      parseErrors.set(physicalLineNo, line);
    }
  }
  return verifyRecords(rawLines, parseErrors);
}
