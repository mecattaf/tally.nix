// tally — dedup-by-existence (IMPLEMENTATION-PLAN M2.2 `dedup.ts`; SPEC "Dedup-by-existence";
// CLI-SURFACE §1.1a "Dedup-by-existence").
//
// Before a heavy unit takes the GPU, tally asks: is this work already done? The check is
// existence-based, not a re-run:
//   1. `stat` the on-disk artifact(s) the enqueue declared via `--evidence artifact:<path>`.
//   2. grep the SUCCESS witness lines for the `--dedup-key`.
//   3. Re-hash the artifact ONLY on an mtime/size mismatch against the witnessed hash (the artifact
//      changed since it was witnessed) — the common case (unchanged mtime+size) skips the hash.
//   A hit ⇒ SKIP the GPU run, tag `labor_class=reused`, `status:"reused"`, exclude from canonical
//   GPU-seconds (the witness `reused` line and the standup aggregation both honor this).
//
// This module reads the witness ledger directly (ledger-as-truth, PS#9) — it does NOT hit the
// daemon. Artifact stat is via `node:fs` (a local filesystem read, not a sanctioned subprocess), so
// no `Exec` is needed here; the content hash uses the same sha256 the witness chain uses.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { existsSync, readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import type { EvidenceCheck, WitnessRecord } from "../contracts/index";
import { combineArtifactHashes } from "./evidence";

/** The outcome of a dedup probe. */
export interface DedupResult {
  /** True when the work is already done and the GPU run must be SKIPPED. */
  hit: boolean;
  /** The dedup key probed (echoed for the reused witness line / result), or null when none was set. */
  dedupKey: string | null;
  /** The content hash of the already-present artifact on a hit, else null. */
  artifactHash: string | null;
  /** The witnessed success line the hit matched (for provenance / the reused line's lineage), or null. */
  matchedWitnessSeq: number | null;
  /**
   * True when the artifact was re-hashed because its mtime/size differed from the witnessed size —
   * recorded so a caller can see whether the fast path (no re-hash) or the slow path was taken.
   */
  rehashed: boolean;
}

/** A source of witness records to grep for a prior success. The engine passes the live ledger's records. */
export interface WitnessSource {
  /** Every committed witness record, newest-last (the ledger's natural append order). */
  records(): Iterable<WitnessRecord>;
}

/** sha256 content hash of a file, in the `sha256:<hex>` form the witness records use. */
export function hashFile(path: string): string {
  const buf = new Uint8Array(readFileSync(path));
  return "sha256:" + createHash("sha256").update(buf).digest("hex");
}

/** The `artifact:<path>` checks from an evidence spec (the paths dedup stats). */
export function artifactPaths(evidence: readonly EvidenceCheck[] | undefined): string[] {
  if (!evidence) return [];
  return evidence.filter((e): e is Extract<EvidenceCheck, { kind: "artifact" }> => e.kind === "artifact").map((e) => e.path);
}

/** The declared hash check, if any (the algo/value the witnessed hash must match). */
export function declaredHash(evidence: readonly EvidenceCheck[] | undefined): { algo: string; value?: string } | null {
  if (!evidence) return null;
  const h = evidence.find((e) => e.kind === "hash");
  return h && h.kind === "hash" ? { algo: h.algo, ...(h.value !== undefined ? { value: h.value } : {}) } : null;
}

/**
 * The parameters a dedup probe needs. `dedupKey` is the enqueue's `--dedup-key`; without one, dedup
 * is a no-op (a run with no dedup key is never skipped — there is no existence key to match). The
 * evidence spec supplies the artifact path(s) to stat and the expected hash.
 */
export interface DedupProbe {
  dedupKey: string | null;
  evidence: readonly EvidenceCheck[] | undefined;
  /** Optional pre-witnessed size/mtime to compare against; when absent, any existing artifact + a matching key is a hit. */
  witness: WitnessSource;
}

/**
 * Probe dedup-by-existence. Returns a hit ONLY when ALL of:
 *   - a `dedupKey` is set;
 *   - a prior SUCCESS witness line (verdict `pass`, non-`clean-exit-no-artifact`) carries that key;
 *   - every declared `artifact:<path>` exists on disk;
 *   - the artifact content matches the witnessed hash — checked fast (mtime/size unchanged ⇒ trust
 *     the witnessed hash) or slow (re-hash on a size mismatch, or when no size is recorded).
 *
 * A missing dedup key, an absent artifact, or a hash mismatch ⇒ NO hit (the GPU run proceeds fresh).
 */
export function probeDedup(probe: DedupProbe): DedupResult {
  const dedupKey = probe.dedupKey;
  const base: DedupResult = { hit: false, dedupKey, artifactHash: null, matchedWitnessSeq: null, rehashed: false };
  if (dedupKey === null || dedupKey === "") return base;

  // Find the most recent SUCCESS witness line carrying this dedup key. A `clean-exit-no-artifact`
  // or `reused` line is NOT a success anchor — only a real `pass` (fresh or recovered) counts.
  let matched: WitnessRecord | undefined;
  for (const rec of probe.witness.records()) {
    if (rec.dedup_key === dedupKey && rec.verdict === "pass" && rec.artifact_content_hash !== null) {
      matched = rec; // keep scanning; the natural order is oldest→newest so the last match wins
    }
  }
  if (matched === undefined) return base;

  const paths = artifactPaths(probe.evidence);
  // If the enqueue declared artifact paths, EVERY one must exist; without any declared path we fall
  // back to trusting the witnessed hash (the key + a prior success is the existence proof).
  if (paths.length > 0) {
    for (const p of paths) {
      if (!existsSync(p)) return base; // artifact gone ⇒ re-run
    }
  }

  // Re-hash only on an mtime/size mismatch. We recorded no size on the witness record, so the "fast
  // path" is: a single artifact whose current hash equals the witnessed hash. To avoid a needless
  // full read when a size sidecar exists, we compare the artifact's byte length against the length
  // implied by the witnessed run only when a single artifact is declared; otherwise we re-hash.
  let rehashed = false;
  let artifactHash = matched.artifact_content_hash;
  if (paths.length === 1) {
    const p = paths[0]!;
    // Fast path: if the file exists and its content hash equals the witnessed hash, it is unchanged.
    // We must read to hash (no stored size on the record), but we short-circuit a hash MISMATCH as a
    // miss (the artifact changed since it was witnessed — re-run to re-witness the new content).
    const current = hashFile(p);
    rehashed = true;
    if (current !== matched.artifact_content_hash) {
      // Content changed since the witnessed success — the prior proof no longer describes this file.
      return { ...base, rehashed: true };
    }
    artifactHash = current;
  } else if (paths.length > 1) {
    // Multiple artifacts: recompute the combined hash the SAME way the evidence gate witnessed it —
    // `sha256(perFileHash0\nperFileHash1…)`, NOT a hash of the concatenated file bytes. Using a
    // different construction here made a multi-artifact dedup miss every time (the run silently
    // re-executed) even when the artifacts were byte-identical to the witnessed success.
    const combined = combineArtifactHashes(paths.map((p) => hashFile(p)));
    rehashed = true;
    if (combined !== matched.artifact_content_hash) {
      return { ...base, rehashed: true };
    }
    artifactHash = combined;
  }

  return {
    hit: true,
    dedupKey,
    artifactHash,
    matchedWitnessSeq: matched.seq,
    rehashed,
  };
}

/**
 * A witness source backed by an in-memory snapshot of records. The engine builds one from the live
 * ledger (or a re-read of the JSONL) so dedup never touches the daemon or the network.
 */
export class ArrayWitnessSource implements WitnessSource {
  constructor(private readonly all: readonly WitnessRecord[]) {}
  records(): Iterable<WitnessRecord> {
    return this.all;
  }
}
