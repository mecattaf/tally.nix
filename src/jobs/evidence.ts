// tally — the evidence gate (IMPLEMENTATION-PLAN M2.2 `evidence.ts`; SPEC "Evidence gate", PS#21;
// CLI-SURFACE §1.1a `--evidence`).
//
// Terminal commit gates on **artifact-exists ∧ content-hash-matches ∧ exit-code-ok ∧ witness-span**
// — NEVER on an agent's self-report (SPEC). The witness-span is implicit (the ledger records the
// job's start/end natively, so a completed run always HAS a span; the gate only checks the other
// three). A clean exit that produces no gate-passing artifact ⇒ `verdict=clean-exit-no-artifact`,
// excluded from canonical GPU-seconds, mirrored by `TALLY_EVENT=evidence_fail` (the caller emits
// the journald/bus event from this verdict).
//
// This module is pure over a completed run's facts (exit code, wall span, the declared checks) plus
// the local filesystem (artifact stat + content hash) — it takes no daemon, no socket. The content
// hash uses the same sha256 the witness chain uses so the witnessed `artifact_content_hash` is the
// hash this gate computed.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { existsSync, readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import type { EvidenceCheck, Verdict } from "../contracts/index";

/** The facts a completed run presents to the gate. */
export interface RunOutcome {
  /** The leaf process exit code. */
  exitCode: number;
  /** Wall-clock span of the run in seconds (the witness span — always present on a completed run). */
  wallClockSeconds: number;
  /** The evidence checks the enqueue declared (repeatable `--evidence`). */
  evidence: readonly EvidenceCheck[] | undefined;
}

/** One check's pass/fail with the reason, for the forensic `checked_paths` / explain output. */
export interface CheckOutcome {
  spec: string;
  passed: boolean;
  reason: string;
}

/** The gate's verdict for one completed run. */
export interface GateResult {
  /** The verdict written to the witness line and mirrored by the journald/bus evidence event. */
  verdict: Verdict;
  /** True when the gate passed (artifact ∧ hash ∧ exit-ok ∧ span). */
  passed: boolean;
  /** The resolved content hash of the artifact(s) when the artifact check passed, else null. */
  artifactHash: string | null;
  /** Per-check forensics (the `checked_paths[]` the evidence event carries + the explain reason). */
  checks: CheckOutcome[];
  /** True on the specific `clean-exit-no-artifact` forensic (exit 0 but no gate-passing artifact). */
  cleanExitNoArtifact: boolean;
}

/** sha256 content hash of a file in the `sha256:<hex>` witness form. */
function hashFile(path: string): string {
  return "sha256:" + createHash("sha256").update(new Uint8Array(readFileSync(path))).digest("hex");
}

/**
 * The canonical combined artifact hash for a set of per-file `sha256:<hex>` hashes: for a single
 * artifact it IS that hash; for 2+ artifacts it is `sha256:` + sha256 over the per-file hash STRINGS
 * joined with "\n". This is the ONE rule the witnessed `artifact_content_hash` is computed with — the
 * dedup probe MUST recompute it the same way or a multi-artifact dedup can never hit (the exact defect
 * where the gate hashed hash-strings while dedup hashed concatenated file bytes).
 */
export function combineArtifactHashes(hashes: readonly string[]): string | null {
  if (hashes.length === 0) return null;
  if (hashes.length === 1) return hashes[0]!;
  return "sha256:" + createHash("sha256").update(hashes.join("\n")).digest("hex");
}

/** The per-file `sha256:<hex>` content hash of a path (the leaf hash the combined rule composes). */
export function hashArtifactFile(path: string): string {
  return hashFile(path);
}

/**
 * Run the evidence gate over a completed run. Returns the verdict + per-check forensics.
 *
 * Gate logic (SPEC "Evidence gate"):
 *  - exit-code-ok: an explicit `exit:<code>` check must match the run's exit code; absent, exit 0 is
 *    required (a heavy unit that exits non-zero without declaring the code is a failure).
 *  - artifact-exists: every declared `artifact:<path>` must exist.
 *  - content-hash-matches: a declared `hash:<algo>[:<value>]` — when a `value` is given, the
 *    computed artifact hash must equal it; when only an algo is given, the hash is computed and
 *    recorded (existence-of-content proof) but not compared to a fixed value.
 *  - witness-span: implicit — a completed run has a span by construction (`wallClockSeconds ≥ 0`),
 *    so it is always satisfied here; the check is present in the forensics for auditability.
 *
 * The distinguished `clean-exit-no-artifact` forensic: the run exited clean (exit-ok) but a declared
 * artifact is missing (or, when artifacts were declared, none passed). That is NOT a plain `failed`
 * — it is the gate-fail bucket excluded from canonical GPU-seconds (PS#21).
 */
export function runEvidenceGate(outcome: RunOutcome): GateResult {
  const checks: CheckOutcome[] = [];
  const evidence = outcome.evidence ?? [];

  // --- exit-code-ok ---
  const exitCheck = evidence.find((e) => e.kind === "exit") as Extract<EvidenceCheck, { kind: "exit" }> | undefined;
  const expectedExit = exitCheck ? exitCheck.code : 0;
  const exitOk = outcome.exitCode === expectedExit;
  checks.push({
    spec: `exit:${expectedExit}`,
    passed: exitOk,
    reason: exitOk ? `exit code ${outcome.exitCode} == ${expectedExit}` : `exit code ${outcome.exitCode} != expected ${expectedExit}`,
  });

  // --- witness-span (implicit, always satisfied on a completed run) ---
  const spanOk = outcome.wallClockSeconds >= 0;
  checks.push({
    spec: "witness-span",
    passed: spanOk,
    reason: spanOk ? `witness span ${outcome.wallClockSeconds}s recorded` : `no witness span`,
  });

  // --- artifact-exists ∧ content-hash-matches ---
  const artifactChecks = evidence.filter((e): e is Extract<EvidenceCheck, { kind: "artifact" }> => e.kind === "artifact");
  const hashCheck = evidence.find((e) => e.kind === "hash") as Extract<EvidenceCheck, { kind: "hash" }> | undefined;

  let artifactHash: string | null = null;
  let artifactsOk = true;
  let anyArtifactDeclared = artifactChecks.length > 0;

  if (anyArtifactDeclared) {
    const hashes: string[] = [];
    for (const a of artifactChecks) {
      if (!existsSync(a.path)) {
        artifactsOk = false;
        checks.push({ spec: `artifact:${a.path}`, passed: false, reason: `artifact does not exist` });
        continue;
      }
      const h = hashFile(a.path);
      hashes.push(h);
      checks.push({ spec: `artifact:${a.path}`, passed: true, reason: `artifact exists (${h})` });
    }
    if (artifactsOk) {
      artifactHash = combineArtifactHashes(hashes);
    }
  }

  // Content-hash check: when a fixed `value` was declared, the computed artifact hash must equal it.
  if (hashCheck) {
    if (hashCheck.value !== undefined) {
      const declared = normalizeHashValue(hashCheck.algo, hashCheck.value);
      const hashOk = artifactHash !== null && artifactHash === declared;
      checks.push({
        spec: `hash:${hashCheck.algo}:${hashCheck.value}`,
        passed: hashOk,
        reason: hashOk ? `content hash matches ${declared}` : `content hash ${artifactHash ?? "<none>"} != declared ${declared}`,
      });
      if (!hashOk) artifactsOk = false;
    } else {
      // Algo-only: record the computed hash as content-existence proof (no fixed value to compare).
      checks.push({
        spec: `hash:${hashCheck.algo}`,
        passed: artifactHash !== null,
        reason: artifactHash !== null ? `content hash ${artifactHash} recorded` : `no artifact to hash`,
      });
      if (artifactHash === null) artifactsOk = false;
      // A hash check with no artifact declared still demands SOME artifact to hash.
      if (!anyArtifactDeclared) anyArtifactDeclared = true;
    }
  }

  // --- verdict synthesis ---
  const gatePassed = exitOk && spanOk && (!anyArtifactDeclared || artifactsOk);
  if (gatePassed && anyArtifactDeclared) {
    return { verdict: "pass", passed: true, artifactHash, checks, cleanExitNoArtifact: false };
  }
  if (gatePassed && !anyArtifactDeclared) {
    // Exit-ok, span-ok, and NO artifact was declared: a heavy unit that declares no artifact still
    // passes the floor (the witness span is the proof) — but this is the clean-exit case where no
    // artifact gate exists. Per SPEC the gate is "artifact-exists ∧ …"; a run declaring no artifact
    // passes on the span+exit floor and is a `pass` (its proof is the witnessed span). This is the
    // cloud/shell-with-no-artifact case, NOT the forensic — the forensic is a DECLARED-but-missing
    // artifact.
    return { verdict: "pass", passed: true, artifactHash: null, checks, cleanExitNoArtifact: false };
  }

  // Gate failed. Distinguish the clean-exit-no-artifact forensic from a plain failure.
  if (exitOk && anyArtifactDeclared && !artifactsOk) {
    // Clean exit, but a declared artifact is missing / hash-mismatched: the distinguished forensic.
    return {
      verdict: "clean-exit-no-artifact",
      passed: false,
      artifactHash: null,
      checks,
      cleanExitNoArtifact: true,
    };
  }

  // Non-zero exit (or a failed span): a plain failure.
  return { verdict: "failed", passed: false, artifactHash, checks, cleanExitNoArtifact: false };
}

/** Normalize a declared hash value to the `sha256:<hex>` form the gate compares against. */
function normalizeHashValue(algo: string, value: string): string {
  if (value.includes(":")) return value; // already `<algo>:<hex>`
  return `${algo}:${value}`;
}

/** Render the checks a gate result exercised as the `checked_paths[]` an evidence event carries. */
export function checkedPaths(result: GateResult): string[] {
  return result.checks.map((c) => c.spec);
}
