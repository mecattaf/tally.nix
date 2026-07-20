// tally — preemption-as-policy (IMPLEMENTATION-PLAN M2.2 `preempt.ts`; SPEC "The inner fold —
// preemption-as-policy").
//
// The pls lease is NON-PREEMPTIBLE — nothing force-evicts a hold (SPEC; PS#5). Preemption is a
// POLICY one layer up, by COOPERATIVE YIELD: when higher-priority work needs the lease a low-priority
// holder is signalled to yield at a safe checkpoint (an OCR page boundary). The holder records its
// `session_ref`, releases the lease via process-exit (the SINGLE release path — never a forced
// eviction, never a second release), hands the GPU over, and the batch is re-dispatched later via
// `--resume` (recover()'s re-present machinery). The interactive job never queues behind the whole
// batch.
//
// Mechanism (SPEC "The inner fold"): the yield signal is `SIGUSR1` to the holder's transient unit —
// a cooperative checkpoint request the leaf worker honors at its next safe boundary. tally sends the
// signal (via the `Exec` seam → `systemctl --user kill --signal=SIGUSR1 <unit>` in production, or a
// direct `kill` fallback); it NEVER SIGKILLs a holder to seize the lease.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Exec } from "../contracts/index";
import type { JobEntry } from "./lifecycle";

/** The cooperative-yield signal (SPEC "The inner fold": SIGUSR1 at a safe checkpoint). */
export const YIELD_SIGNAL = "SIGUSR1";

/** The record of one preemption decision — who yields for whom, and why. */
export interface PreemptionDecision {
  /** The low-priority holder asked to yield. */
  holder: JobEntry;
  /** The higher-priority job that requested the lease. */
  requester: JobEntry;
  reason: string;
}

/**
 * Decide whether an incoming higher-priority job should preempt a current holder on the same pool.
 * Returns a decision when a strictly-lower-priority holder exists on the requester's pool, else null.
 * The engine consults this before it queues a job behind a held lease.
 */
export function shouldPreempt(requester: JobEntry, holders: readonly JobEntry[]): PreemptionDecision | null {
  const requesterRank = rankOf(requester);
  // Candidate holders: same pool, strictly lower priority, currently running (started/resumed).
  const candidates = holders
    .filter((h) => h.pool === requester.pool && (h.state === "started" || h.state === "resumed"))
    .filter((h) => rankOf(h) < requesterRank)
    // Yield the LOWEST-priority holder first (least disruptive).
    .sort((a, b) => rankOf(a) - rankOf(b));
  const holder = candidates[0];
  if (holder === undefined) return null;
  return {
    holder,
    requester,
    reason: `${requester.priority} job ${requester.job_id} preempts ${holder.priority} holder ${holder.job_id} on pool ${requester.pool}`,
  };
}

/** The numeric priority rank used for preemption ordering (high > medium > low). */
function rankOf(job: JobEntry): number {
  return job.priority === "high" ? 100 : job.priority === "medium" ? 50 : 10;
}

/**
 * The cooperative-yield signaller. Sends `SIGUSR1` to a holder's transient unit so it yields at its
 * next safe checkpoint. In production the signal reaches the transient unit via
 * `systemctl --user kill --signal=SIGUSR1 <unit>`; when systemd is absent (dev rig / direct spawn)
 * it falls back to `kill -SIGUSR1 <pid>` — but tally never sends a KILLING signal to seize the lease
 * (the yield is cooperative; the holder releases via process-exit after checkpointing).
 */
export class YieldSignaller {
  constructor(private readonly exec: Exec) {}

  /**
   * Signal a holder to yield. Returns true when the signal was delivered. If the unit is absent
   * (systemd absent), the caller supplies the `pid` fallback. A holder with neither a unit nor a pid
   * cannot be signalled (returns false) — the engine then falls back to waiting for the holder to
   * finish its current checkpoint naturally.
   */
  async signalYield(entry: JobEntry, pid?: number): Promise<boolean> {
    if (entry.unit !== null) {
      const res = await this.exec.run([
        "systemctl",
        "--user",
        "kill",
        `--signal=${YIELD_SIGNAL}`,
        entry.unit,
      ]);
      if (res.code === 0) return true;
      // systemctl absent / unit not found ⇒ fall through to the pid path if available.
      if (res.code !== 127 && pid === undefined) return false;
    }
    if (pid !== undefined) {
      const res = await this.exec.run(["kill", `-${YIELD_SIGNAL}`, String(pid)]);
      return res.code === 0;
    }
    return false;
  }
}

/**
 * Mark a holder as preempted: record its session ref (so the re-dispatch resumes it), clear the
 * lease id (the holder releases via process-exit — the single release path), and set the yield
 * reason. The engine transitions the entry `started → preempted` and fans the `job.preempted` event.
 * The lease is NOT released here (RAII / process-exit owns that); this only records the intent.
 */
export function markPreempted(entry: JobEntry): void {
  entry.trace_ref = entry.session_ref ?? entry.trace_ref;
  // The holder will release its own lease via process-exit; the engine reclaims the slot when the
  // process is confirmed gone (recover()'s holderless-lease reclaim). We clear the lease id so the
  // engine knows this job no longer holds a live lease once it exits.
  entry.lease_id = null;
}

/**
 * Prepare a preempted job for re-dispatch (the `--resume` re-present): bump the attempt, tag
 * `labor_class:recovered`, and keep the recorded `session_ref` so the adapter resumes rather than
 * restarts. Returns the entry for chaining. The engine then re-enqueues it at its original priority.
 */
export function prepareResume(entry: JobEntry): JobEntry {
  entry.attempt += 1;
  entry.labor_class = "recovered";
  entry.state = "preempted"; // the engine transitions preempted → resumed on re-dispatch
  return entry;
}
