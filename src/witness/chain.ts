// tally — the per-line hash chain primitives (SPEC "Per-line hash chain", jul9; IMPLEMENTATION-PLAN
// M1.2). The one place the sha256 implementation and the on-disk record field order live, so the
// writer (`ledger.ts`) and the daemonless verifier (`verify.ts`) agree byte-for-byte on what is
// hashed and how a line is serialized.
//
// Chain rule (contracts `canonicalHashInput`): `hash = "sha256:" + hex(sha256(the line's own JSON
// with the `hash` field CLEARED to ""))`. `prev_hash` = the prior line's `hash`; the genesis line
// uses `GENESIS_PREV_HASH`. `seq` is a ledger-wide monotonic 1-based counter (ledger-wide chain,
// IMPLEMENTATION-PLAN §4.6).
//
// Authored fresh for tally; the mechanism is the ~150-line chain semantics ported by DESCRIPTION
// (SPEC), never by copying vendor/ code (clean-room, CLI-SURFACE §4).

import { createHash } from "node:crypto";
import {
  canonicalHashInput,
  GENESIS_PREV_HASH,
  HASH_PREFIX,
  type WitnessRecord,
} from "../contracts/index";

/** sha256 hex digest of a UTF-8 string. */
export function sha256Hex(input: string): string {
  return createHash("sha256").update(input, "utf8").digest("hex");
}

/**
 * Compute the `hash` value for a fully-populated record (all fields EXCEPT `hash` already set to
 * their final values; `hash` may hold any placeholder). Uses the shared canonicalization rule so
 * the writer and verifier can never disagree.
 */
export function computeHash(record: WitnessRecord): string {
  return HASH_PREFIX + sha256Hex(canonicalHashInput(record));
}

/**
 * The fields of a witness record other than the three chain fields (`seq`, `prev_hash`, `hash`) —
 * the "body" a caller supplies; the chain fields are stamped by {@link buildRecord}.
 */
export type WitnessBody = Omit<WitnessRecord, "seq" | "prev_hash" | "hash">;

/** The chain head carried in memory (last committed line's seq + hash). */
export interface ChainHead {
  /** The seq of the last committed line; 0 before any line exists. */
  readonly seq: number;
  /** The hash of the last committed line; `GENESIS_PREV_HASH` before any line exists. */
  readonly hash: string;
}

/** The chain head for an empty ledger — genesis. */
export const GENESIS_HEAD: ChainHead = { seq: 0, hash: GENESIS_PREV_HASH };

/**
 * Build the complete, chained {@link WitnessRecord} for a body appended after `head`.
 *
 * The field insertion order is fixed HERE to exactly match the contract `WitnessRecord` shape (and
 * the ledger fixtures), because `canonicalHashInput` serializes with `JSON.stringify` and key order
 * is insertion order. `seq = head.seq + 1`, `prev_hash = head.hash`, `hash` computed last over the
 * cleared form.
 */
export function buildRecord(body: WitnessBody, head: ChainHead): WitnessRecord {
  const seq = head.seq + 1;
  const prev_hash = head.hash;
  // Assemble in canonical field order (mirrors WitnessRecord / the fixtures). `trace_ref` is an
  // optional field: include it only when supplied so absent-vs-null matches the record's shape.
  const base = {
    task_uuid: body.task_uuid,
    transition_timestamp: body.transition_timestamp,
    verdict: body.verdict,
    exit_code: body.exit_code,
    artifact_content_hash: body.artifact_content_hash,
    gpu_seconds: body.gpu_seconds,
    wall_clock: body.wall_clock,
    attempt: body.attempt,
    lease_epoch: body.lease_epoch,
    dedup_key: body.dedup_key,
    labor_class: body.labor_class,
  };
  // Optional trace_ref sits between labor_class and pool in the schema; preserve that position.
  const withTrace =
    "trace_ref" in body && body.trace_ref !== undefined
      ? { ...base, trace_ref: body.trace_ref }
      : base;
  const record: WitnessRecord = {
    ...withTrace,
    pool: body.pool,
    charge: body.charge,
    model: body.model,
    seq,
    prev_hash,
    hash: "",
  };
  record.hash = computeHash(record);
  return record;
}

/** Advance a chain head past a committed record. */
export function advanceHead(record: WitnessRecord): ChainHead {
  return { seq: record.seq, hash: record.hash };
}

/**
 * Serialize a committed record to its single on-disk JSONL line (no trailing newline — the caller /
 * physical-append layer adds the LF). Uses the same `JSON.stringify` insertion-order the hash was
 * computed over, so the persisted `hash` verifies.
 */
export function serializeLine(record: WitnessRecord): string {
  return JSON.stringify(record);
}
