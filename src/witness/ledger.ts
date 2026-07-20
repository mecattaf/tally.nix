// tally — the append-only witness JSONL ledger (SPEC "Physical append", "Per-line hash chain";
// PS#9/#10a; IMPLEMENTATION-PLAN M1.2). Ledger-as-truth.
//
// Physical append: plain `O_APPEND` + `fsync` per line, each line a complete JSON object, LF
// terminated, no checksum prefix, no temp-then-rename (PS#10a). Ledger path
// `$XDG_DATA_HOME/tally/witness.jsonl` (resolved via contracts `ledgerPath`).
//
// Restart-surviving chain: on `open()` the ledger scans forward, discards a torn trailing line by
// JSON-parse failure (PS#10a rule), recovers `(last_seq, last_hash)` so ONE unbroken ledger-wide
// chain (IMPLEMENTATION-PLAN §4.6) spans daemon restarts. Every `append` stamps the next
// `seq`/`prev_hash`/`hash` via `chain.ts` and fsyncs the line before returning its seq.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import {
  closeSync,
  existsSync,
  fsyncSync,
  mkdirSync,
  openSync,
  readFileSync,
  truncateSync,
  writeSync,
} from "node:fs";
import { dirname } from "node:path";
import { ledgerPath, type PathEnv, type WitnessRecord } from "../contracts/index";
import {
  advanceHead,
  buildRecord,
  GENESIS_HEAD,
  serializeLine,
  type ChainHead,
  type WitnessBody,
} from "./chain";
import { parseRecord } from "./record";

/** The outcome of a boot-time chain-head recovery scan. */
export interface RecoverScan {
  /** The recovered chain head — genesis if the ledger is empty/absent. */
  head: ChainHead;
  /** Count of intact, valid records read. */
  records: number;
  /** True when a torn (unparseable / invalid) trailing line was discarded. */
  tornTrailingDiscarded: boolean;
  /**
   * Byte length of the intact prefix — the end offset (including the trailing LF) of the last
   * intact record. The ledger truncates the file to this length on open when `tornTrailingDiscarded`
   * is set, so the next `O_APPEND` write lands cleanly after the last good line rather than after
   * torn bytes (PS#10a: plain append, no temp-then-rename — a truncate of torn trailing bytes is
   * still that discipline).
   */
  intactBytes: number;
}

/**
 * Scan a ledger file forward to recover the chain head. Reads every complete line; a trailing line
 * that fails `JSON.parse` OR fails record validation is treated as a torn write and discarded
 * (PS#10a). A NON-trailing unparseable line is a corrupt ledger — recovery stops at the last intact
 * prefix and marks the head there (the daemon continues the chain; `verify` will still flag the
 * corruption on a full audit). Returns the head at the last intact, contiguous record.
 */
export function scanChainHead(path: string): RecoverScan {
  if (!existsSync(path)) {
    return { head: GENESIS_HEAD, records: 0, tornTrailingDiscarded: false, intactBytes: 0 };
  }
  const text = readFileSync(path, "utf8");
  const utf8 = new TextEncoder();
  // Split into physical lines. A file ending in LF yields a trailing "" we ignore. A file NOT
  // ending in LF whose last segment is non-empty is a candidate torn write.
  const segments = text.split("\n");
  // Determine intact record lines: everything that JSON-parses AND validates, contiguously from
  // the start. Track whether the final non-empty segment was discarded as torn, and the byte offset
  // of the end (including LF) of the last intact record.
  let head: ChainHead = GENESIS_HEAD;
  let records = 0;
  let tornTrailingDiscarded = false;
  let intactBytes = 0;
  // Running byte cursor at the START of segment i (each segment except the last was followed by an
  // LF in the source; account for those LF bytes as we advance).
  let cursor = 0;

  for (let i = 0; i < segments.length; i++) {
    const seg = segments[i]!;
    const isLast = i === segments.length - 1;
    // Byte length of this segment plus the LF that separated it from the next (present unless last).
    const segBytes = utf8.encode(seg).length + (isLast ? 0 : 1);
    if (seg.length === 0) {
      // Blank line: only legitimate as the terminator after the final LF (isLast). A blank line in
      // the middle is skipped (defensive), not a torn write.
      cursor += segBytes;
      continue;
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(seg);
    } catch {
      // Unparseable. If it is the final non-empty segment, it is the torn trailing write — discard.
      if (isLastNonEmpty(segments, i)) {
        tornTrailingDiscarded = true;
        break;
      }
      // Mid-file corruption: stop recovery at the last intact record (do not advance past it).
      break;
    }
    const res = parseRecord(parsed);
    if (!res.ok) {
      if (isLastNonEmpty(segments, i)) {
        tornTrailingDiscarded = true;
        break;
      }
      break;
    }
    head = advanceHead(res.record);
    records++;
    cursor += segBytes;
    intactBytes = cursor;
  }

  return { head, records, tornTrailingDiscarded, intactBytes };
}

/** True when `idx` is the last non-empty segment in `segments`. */
function isLastNonEmpty(segments: readonly string[], idx: number): boolean {
  for (let j = idx + 1; j < segments.length; j++) {
    if (segments[j]!.length > 0) return false;
  }
  return true;
}

/**
 * The append-only witness ledger. Open once at daemon boot (recovers the chain head), then `append`
 * heavy-unit lines. `fsync`s per line so "fully written line ⟺ work finished" (SPEC). Also usable
 * ad-hoc (tests) via {@link openLedger}.
 */
export class WitnessLedger {
  private readonly path: string;
  private fd: number | null = null;
  private head: ChainHead;
  private recordCount: number;

  private constructor(path: string, scan: RecoverScan) {
    this.path = path;
    this.head = scan.head;
    this.recordCount = scan.records;
  }

  /**
   * Open the ledger at an explicit path: ensure the parent dir exists, recover the chain head by a
   * forward scan (discarding a torn trailing line), and open the fd in append mode.
   */
  static openAtPath(path: string): WitnessLedger {
    mkdirSync(dirname(path), { recursive: true });
    const scan = scanChainHead(path);
    // A torn trailing line remains on disk (plain append, no rewrite). Truncate the file back to the
    // last intact record boundary so the next O_APPEND write lands cleanly after the last good line
    // rather than concatenating onto the torn bytes. Truncation of the torn tail is still the PS#10a
    // append discipline (no temp-then-rename, no checksum prefix) — it only drops the partial write
    // recover() already decided to discard.
    if (scan.tornTrailingDiscarded) {
      truncateSync(path, scan.intactBytes);
    }
    const ledger = new WitnessLedger(path, scan);
    // "a" = O_APPEND | O_CREAT | O_WRONLY. Every write lands at EOF atomically per the OS append
    // guarantee; no temp-then-rename (PS#10a).
    ledger.fd = openSync(path, "a");
    return ledger;
  }

  /** Open the ledger at the XDG-resolved `$XDG_DATA_HOME/tally/witness.jsonl` for the given env. */
  static open(env: PathEnv): WitnessLedger {
    return WitnessLedger.openAtPath(ledgerPath(env));
  }

  /** The resolved ledger file path. */
  get filePath(): string {
    return this.path;
  }

  /** The current chain head (last committed line's seq + hash). */
  get chainHead(): ChainHead {
    return this.head;
  }

  /** The seq the NEXT appended line will carry. */
  get nextSeq(): number {
    return this.head.seq + 1;
  }

  /** Count of records read at open + appended since. */
  get count(): number {
    return this.recordCount;
  }

  /**
   * Append one heavy-unit witness line. Stamps `seq`/`prev_hash`/`hash` from the current chain head,
   * serializes the complete JSON object + LF, writes it, and `fsync`s the fd before returning the
   * committed record (whose `seq` is the witness LSN). Advances the in-memory chain head.
   */
  append(body: WitnessBody): WitnessRecord {
    if (this.fd === null) throw new Error("witness ledger is closed");
    const record = buildRecord(body, this.head);
    const line = serializeLine(record) + "\n";
    // A fully-formed line must never itself contain a newline (the JSON object is single-line).
    // `serializeLine` uses JSON.stringify which never emits a raw LF, so this holds by construction.
    writeSync(this.fd, line, null, "utf8");
    fsyncSync(this.fd);
    this.head = advanceHead(record);
    this.recordCount++;
    return record;
  }

  /** Close the ledger fd. Idempotent. */
  close(): void {
    if (this.fd !== null) {
      closeSync(this.fd);
      this.fd = null;
    }
  }
}

/** Convenience: open a ledger at an explicit path (tests / `verify` do not need the XDG resolver). */
export function openLedger(path: string): WitnessLedger {
  return WitnessLedger.openAtPath(path);
}
