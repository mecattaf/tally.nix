// tally — the job lifecycle state machine + one-vocabulary event fan-out (IMPLEMENTATION-PLAN M2.2
// `lifecycle.ts`; SPEC "Three planes", "journald TALLY_* event schema"; CLI-SURFACE §2.3 job.*).
//
// The `job.*` bus/wire events MIRROR the journald `TALLY_EVENT` vocabulary VERBATIM — ONE vocabulary
// (SPEC): enqueued → dispatched → started → heartbeat → (preempted → resumed)* → evidence_pass |
// evidence_fail → completed | failed (+ witness_emitted). This module owns:
//   - the in-flight `JobEntry` record + its legal transitions;
//   - the fan-out that emits every transition to ALL THREE sinks at once — the in-daemon `Bus`
//     (fans onto the wire + the single store's `jobs[]` leg), the journald emitter (observability),
//     and, at the terminal transition, the witness ledger (proof) — so the three never disagree on
//     what happened.
//
// The engine (engine.ts) drives the machine; this module is the transition vocabulary + the sink
// fan-out helper it calls. No subprocess access here (emission is Bus + stdout + ledger append).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type {
  AgentKind,
  Bus,
  JobRecord,
  JobState,
  LaborClass,
  Pool,
  Priority,
  Source,
  Verdict,
} from "../contracts/index";
import type { EnqueueParams, EvidenceCheck } from "../contracts/index";

export type { JobState } from "../contracts/index";

/**
 * The full in-flight record for one job the engine tracks in memory. A superset of the wire
 * `JobRecord` (which is the §2.2 bootstrap projection); the extra fields are the engine's own
 * bookkeeping (the resolved leaf argv, the lease, the run timing, the evidence spec).
 */
export interface JobEntry {
  job_id: string;
  task_uuid: string | null;
  state: JobState;
  priority: Priority;
  source: Source;
  agent_kind: AgentKind;
  /** The resolved leaf argv the transient unit runs (from the adapter). */
  argv: string[];
  /** The transient systemd unit name (`tally-job-<id>`) once dispatched, else null. */
  unit: string | null;
  cwd: string | null;
  worktree: string | null;
  pool: Pool;
  model_class: string | null;
  model: string | null;
  dedup_key: string | null;
  /** The bound/derived zmx session (read, never created) and the harness resume ref. */
  session: string | null;
  session_ref: string | null;
  trace_ref: string | null;
  evidence: EvidenceCheck[];
  lease_epoch: number;
  attempt: number;
  /** The pls lease id held while running, else null. */
  lease_id: string | null;
  gpu_seconds: number;
  /** The pane the run is observed in (populated when a detector agent binds to the unit), else null. */
  pane_id: string | null;
  agent_id: string | null;
  /** Run timing (ms epoch) for the witness span. */
  started_at_ms: number | null;
  ended_at_ms: number | null;
  /** The terminal verdict once the evidence gate ran, else null. */
  verdict: Verdict | null;
  labor_class: LaborClass;
  /** The barrier / wait-group this job participates in, for the enqueue-N-await-N barrier. */
  barrier: string | null;
  wait_group: string | null;
  /** The witness ledger seq of this job's terminal line, once emitted. */
  witness_lsn: number | null;
  /** True once the terminal completion has been ACKed (recover() only retries un-ACKed units). */
  acked: boolean;
}

/** The legal forward transitions of the job state machine (SPEC one-vocabulary lifecycle). */
const LEGAL_TRANSITIONS: Record<JobState, JobState[]> = {
  enqueued: ["dispatched", "failed"],
  dispatched: ["started", "failed", "preempted"],
  started: ["preempted", "evidence_pass", "evidence_fail", "failed"],
  preempted: ["resumed", "failed"],
  resumed: ["started", "evidence_pass", "evidence_fail", "failed", "preempted"],
  evidence_pass: ["completed"],
  evidence_fail: ["failed"],
  completed: [],
  failed: [],
};

/** True when `from → to` is a legal job-state transition. */
export function canTransition(from: JobState, to: JobState): boolean {
  return LEGAL_TRANSITIONS[from].includes(to);
}

/** The terminal states (no further transition; a `--wait` barrier resolves on these). */
export const TERMINAL_STATES: readonly JobState[] = ["completed", "failed"];

/** True when a state is terminal. */
export function isTerminal(state: JobState): boolean {
  return TERMINAL_STATES.includes(state);
}

/** Project the engine's in-flight `JobEntry` to the wire `JobRecord` for the §2.2 snapshot leg. */
export function toJobRecord(entry: JobEntry): JobRecord {
  return {
    job_id: entry.job_id,
    task_uuid: entry.task_uuid,
    state: entry.state,
    class: entry.priority,
    source: entry.source,
    agent_kind: entry.agent_kind,
    pane_id: entry.pane_id,
    lease_epoch: entry.lease_epoch,
    attempt: entry.attempt,
    gpu_seconds: entry.gpu_seconds,
  };
}

/**
 * Build a fresh in-flight `JobEntry` from a validated enqueue. `job_id` is the engine's opaque id
 * (distinct from `task_uuid` — a rowless unit has a job_id but a null task_uuid). Starts `enqueued`,
 * `labor_class:fresh`, attempt 1.
 */
export function newJobEntry(args: {
  job_id: string;
  task_uuid: string | null;
  params: EnqueueParams;
  argv: string[];
  pool: Pool;
  session: string | null;
  session_ref: string | null;
  model: string | null;
  lease_epoch: number;
}): JobEntry {
  const p = args.params;
  return {
    job_id: args.job_id,
    task_uuid: args.task_uuid,
    state: "enqueued",
    priority: p.priority,
    source: p.source,
    agent_kind: p.kind,
    argv: args.argv,
    unit: null,
    cwd: p.cwd ?? null,
    worktree: p.worktree ?? null,
    pool: args.pool,
    model_class: p.model_class ?? null,
    model: args.model,
    dedup_key: p.dedup_key ?? null,
    session: args.session,
    session_ref: args.session_ref,
    trace_ref: null,
    evidence: p.evidence ?? [],
    lease_epoch: args.lease_epoch,
    attempt: 1,
    lease_id: null,
    gpu_seconds: 0,
    pane_id: null,
    agent_id: null,
    started_at_ms: null,
    ended_at_ms: null,
    verdict: null,
    labor_class: "fresh",
    barrier: p.barrier ?? null,
    wait_group: p.wait_group ?? null,
    witness_lsn: null,
    acked: false,
  };
}

/**
 * The sinks a lifecycle transition fans out to. The engine wires the real `Bus`, the journald
 * emitter's `emit`, and the witness ledger `append`; tests inject collectors. Kept as a narrow
 * structural type so lifecycle.ts does not import the concrete journal/witness classes (it only
 * needs the two call shapes), keeping the transition machine testable in isolation.
 */
export interface LifecycleSinks {
  bus: Bus;
  /** Emit one journald line for a lifecycle event (the caller maps to the emit event shape). */
  journal: (entry: JobEntry, event: JobState | "heartbeat" | "witness_emitted", extra?: JournalExtra) => void;
}

/** Extra per-event journald detail the caller supplies (evidence string, exit code, etc.). */
export interface JournalExtra {
  exit_code?: number;
  gpu_seconds?: number;
  artifact_hash?: string;
  evidence?: string;
  verdict?: Verdict;
}

/**
 * Apply a state transition to a job entry (mutating it), asserting legality. Returns the entry for
 * chaining. The engine calls this then fans the matching event out via the sinks.
 */
export function transition(entry: JobEntry, to: JobState): JobEntry {
  if (!canTransition(entry.state, to)) {
    throw new Error(`illegal job transition ${entry.state} → ${to} for job ${entry.job_id}`);
  }
  entry.state = to;
  return entry;
}
