// tally — recover(): re-present, never replay (IMPLEMENTATION-PLAN M2.2 `recover.ts`; SPEC
// "recover() — re-present, never replay", PS#9's five invariants; PS#21 lease-epoch fence).
//
// On daemon boot, recovery re-derives the queue and re-PRESENTS in-flight work — it never
// deterministically replays agent work (agent work is non-replayable). The five invariants:
//   1. witness_lsn reconciliation — compare the ledger tail-hash / max applied lsn on boot; a
//      cheap-check trust path avoids a full replay unless the head mismatches.
//   2. ACK-gated retry — only re-present a unit whose completion was NOT acked.
//   3. zombie fencing via lease-epoch — a holder from a PRIOR lease epoch is fenced (its lease is
//      reclaimed, never trusted); the current epoch is the ONLY fence — no leader election.
//   4. undeleted-row = re-present — an unfinished (pending, non-deleted) TW row is re-dispatched
//      via `--resume` (labor_class:recovered), not replayed.
//   5. bounded requeue — attempt-capped; a unit past the cap is abandoned to `failed`, never
//      retried forever.
//
// This module is pure planning over the durable facts (the TW rows, the witness head, the live
// leases) — it produces a RECOVERY PLAN the engine executes (re-dispatch these, reclaim those leases,
// abandon these). It shells out only through the injected `TaskChampion` veneer and `LeaseManager`.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Pool, TaskRow } from "../contracts/index";
import type { TaskChampion } from "../tw/index";
import type { LeaseManager } from "../pls/index";
import type { ChainHead } from "../witness/index";

/** The attempt cap — a unit past this many attempts is abandoned, not re-presented (invariant 5). */
export const DEFAULT_ATTEMPT_CAP = 5;

/** One planned re-dispatch: an undeleted, un-acked, in-attempt-budget row to re-present via resume. */
export interface RepresentPlan {
  task_uuid: string;
  row: TaskRow;
  /** The resume session ref (`pi --session <id>` / `claude --resume <id>`), or null (fresh re-run). */
  session_ref: string | null;
  attempt: number;
  labor_class: "recovered";
}

/** One planned lease reclaim: a holderless lease from a fenced (prior-epoch) holder to free. */
export interface ReclaimPlan {
  pool: Pool;
  lease_id: string;
  reason: string;
}

/** One planned abandonment: a row past the attempt cap, marked failed rather than retried. */
export interface AbandonPlan {
  task_uuid: string;
  reason: string;
}

/** The full recovery plan the engine executes at boot. */
export interface RecoveryPlan {
  /** True when the witness head reconciled cleanly (no full replay needed) — invariant 1. */
  witnessReconciled: boolean;
  /** The recovered witness chain head (last_seq, last_hash). */
  witnessHead: ChainHead;
  represent: RepresentPlan[];
  reclaim: ReclaimPlan[];
  abandon: AbandonPlan[];
}

/** A live lease the recover path may need to fence/reclaim (a holder from a prior epoch). */
export interface LiveLease {
  pool: Pool;
  lease_id: string;
  /** The lease epoch (generation) that minted this hold. */
  lease_epoch: number;
  /** True when the holder process is confirmed gone (a holderless lease to reclaim). */
  holderless: boolean;
}

/** Inputs to the recovery planner. */
export interface RecoverInput {
  tw: TaskChampion;
  /** The recovered witness chain head from the ledger scan (M1.2 `scanChainHead`). */
  witnessHead: ChainHead;
  /**
   * The max applied `witness_lsn` the daemon had checkpointed before the crash (0 if none). When the
   * ledger head seq equals this, the projection is trusted (cheap check); on a mismatch the engine
   * does a full replay of the tail. Invariant 1.
   */
  lastAppliedLsn: number;
  /** The CURRENT lease epoch (from the pls generation / the systemd counter backstop). The fence. */
  currentEpoch: number;
  /** Live leases observed at boot (to fence prior-epoch zombies). */
  liveLeases: readonly LiveLease[];
  /** The set of task_uuids whose terminal completion WAS acked before the crash (ACK-gated retry). */
  ackedTaskUuids: ReadonlySet<string>;
  /** The attempt cap (invariant 5). */
  attemptCap?: number;
}

/**
 * Plan recovery from the durable facts. Does NOT execute anything — returns the plan the engine
 * carries out (re-present rows, reclaim fenced leases, abandon over-attempt rows). Honors all five
 * invariants:
 *   1. witnessReconciled = (witnessHead.seq === lastAppliedLsn) — a match means no full replay.
 *   2. an acked task_uuid is NOT re-presented.
 *   3. a live lease from a PRIOR epoch is reclaimed (fenced); a current-epoch lease is left held.
 *   4. an undeleted (pending/waiting) row is re-presented via resume; a completed/deleted row is not.
 *   5. a row whose attempt would exceed the cap is abandoned to failed.
 */
export async function planRecovery(input: RecoverInput): Promise<RecoveryPlan> {
  const cap = input.attemptCap ?? DEFAULT_ATTEMPT_CAP;
  const witnessReconciled = input.witnessHead.seq === input.lastAppliedLsn;

  // Invariant 3 — zombie fencing: reclaim every live lease minted by a PRIOR epoch, or any holderless
  // lease (holder process gone). A current-epoch lease with a live holder is left alone.
  const reclaim: ReclaimPlan[] = [];
  for (const lease of input.liveLeases) {
    if (lease.lease_epoch < input.currentEpoch) {
      reclaim.push({ pool: lease.pool, lease_id: lease.lease_id, reason: `fenced: lease epoch ${lease.lease_epoch} < current ${input.currentEpoch}` });
    } else if (lease.holderless) {
      reclaim.push({ pool: lease.pool, lease_id: lease.lease_id, reason: `holderless lease reclaimed` });
    }
  }

  // Invariants 2, 4, 5 — re-present undeleted, un-acked, in-budget rows.
  const represent: RepresentPlan[] = [];
  const abandon: AbandonPlan[] = [];

  // The durable rows still in flight: pending or waiting (an undeleted, unfinished row). Completed /
  // deleted rows are terminal and not re-presented.
  const inFlight = await input.tw.query(["status:pending"]);
  const waiting = await input.tw.query(["status:waiting"]);
  const rows = [...inFlight, ...waiting];

  for (const row of rows) {
    // Invariant 2 — ACK-gated: a row whose completion was acked is not retried.
    if (input.ackedTaskUuids.has(row.uuid)) continue;
    const priorAttempt = typeof row.attempt === "number" ? row.attempt : 1;
    const nextAttempt = priorAttempt + 1;
    // Invariant 5 — bounded requeue.
    if (nextAttempt > cap) {
      abandon.push({ task_uuid: row.uuid, reason: `attempt ${nextAttempt} exceeds cap ${cap}` });
      continue;
    }
    // Invariant 4 — re-present via resume (labor_class:recovered), carrying the recorded session ref.
    represent.push({
      task_uuid: row.uuid,
      row,
      session_ref: row.session_ref ?? null,
      attempt: nextAttempt,
      labor_class: "recovered",
    });
  }

  return { witnessReconciled, witnessHead: input.witnessHead, represent, reclaim, abandon };
}

/**
 * Execute the lease-reclaim leg of a recovery plan (the ONLY side effect this module performs on
 * request): reclaim every fenced/holderless lease via the LeaseManager's holderless-reclaim hook.
 * The re-present + abandon legs are executed by the engine (they touch the queue + the witness).
 * Returns the count reclaimed.
 */
export async function executeReclaims(leases: LeaseManager, plan: RecoveryPlan): Promise<number> {
  let n = 0;
  for (const r of plan.reclaim) {
    const freed = await leases.reclaim(r.pool, r.lease_id);
    if (freed) n++;
  }
  return n;
}
