// tally — job plane types: Priority, Source, Verdict, LaborClass, Seam-A enqueue, in-flight job
// record, and the EvidenceCheck grammar (CLI-SURFACE §1.1a, §2.2 `jobs[]`, §2.3 job.* events;
// SPEC "The spawn-tracked-agent-job", "Evidence gate").

import type { AgentKind } from "./agent";

/** Priority / TALLY_CLASS enum (CLI-SURFACE §1.1a; SPEC journald table). */
export type Priority = "high" | "medium" | "low";
export const PRIORITIES = ["high", "medium", "low"] as const satisfies readonly Priority[];

/** Provenance of an enqueue — no path is privileged (CLI-SURFACE §1.1a; SPEC journald `TALLY_SOURCE`; PS#16). */
export type Source = "r2" | "gh" | "calendar" | "manual" | "orchestrator";
export const SOURCES = ["r2", "gh", "calendar", "manual", "orchestrator"] as const satisfies readonly Source[];

/**
 * The evidence-gate verdict (SPEC "Record schema", "Evidence gate"). `pass` is clean success;
 * `clean-exit-no-artifact` is the gate-fail forensic (clean exit, no gate-passing artifact —
 * excluded from canonical GPU-seconds). The enum is open-ended (`| …` in the SPEC), so consumers
 * tolerate unknown verdicts.
 */
export type Verdict = "pass" | "clean-exit-no-artifact" | "failed" | "cancelled" | "reused";
export const VERDICTS = ["pass", "clean-exit-no-artifact", "failed", "cancelled", "reused"] as const satisfies readonly Verdict[];

/**
 * Labor class (SPEC "Record schema"). Non-`fresh` lines are EXCLUDED from canonical GPU-seconds
 * aggregation.
 */
export type LaborClass = "fresh" | "recovered" | "reused";
export const LABOR_CLASSES = ["fresh", "recovered", "reused"] as const satisfies readonly LaborClass[];

/**
 * The pool that serves a unit. Day-1 the two GPU pools are fully wired; `sub:<acct>` / `api` are
 * reserved (IMPLEMENTATION-PLAN §1 item 6 — witness `pool`/`charge` populated GPU-only), so this is
 * an open string with the two GPU literals named for ergonomics.
 */
export type Pool = "worker-gpu" | "controller-gpu" | (string & {});
export const GPU_POOLS = ["worker-gpu", "controller-gpu"] as const;

/**
 * The job lifecycle state carried in the §2.2 `jobs[]` bootstrap and mirrored by `job.*` events.
 * Values mirror the journald TALLY_EVENT vocabulary (one vocabulary; see `journal.ts`).
 */
export type JobState =
  | "enqueued"
  | "dispatched"
  | "started"
  | "preempted"
  | "resumed"
  | "evidence_pass"
  | "evidence_fail"
  | "completed"
  | "failed";

/**
 * An evidence check spec (CLI-SURFACE §1.1a `--evidence`; Seam A). Repeatable. The witness-span
 * check is implicit and NOT expressible here.
 */
export type EvidenceCheck =
  | { kind: "artifact"; path: string }
  | { kind: "hash"; algo: string; value?: string }
  | { kind: "exit"; code: number };

/**
 * Seam-A enqueue params (CLI-SURFACE §1.1a). `invocation` XOR `argv`; `cwd` XOR `worktree`.
 * `--wait`/barrier fields drive the CLI-side blocking barrier (never cancels; `--timeout` bounds).
 */
export interface EnqueueParams {
  priority: Priority;
  source: Source;
  kind: AgentKind;
  /** The leaf-worker command. Exactly one of `invocation` / `argv`. */
  invocation?: string;
  argv?: string[];
  /** Exactly one of `cwd` / `worktree` (worktree absorbs the orchestrator family as a field). */
  cwd?: string;
  worktree?: string;
  evidence?: EvidenceCheck[];
  /** Pool hint for the budget-gated assigner; never a model re-pick (PS#2). */
  pool?: Pool;
  /** DECLARED, carried from ignition — tally never escalates (PS#2). */
  model_class?: string;
  dedup_key?: string;
  /** OPTIONAL: bind to an EXISTING zmx session (read, never create). */
  session?: string;
  // Barrier / wait-group fields (CLI-SURFACE §1.1a):
  barrier?: string;
  wait_group?: string;
  wait_count?: number;
  wait?: boolean;
  timeout?: string;
  detach?: boolean;
}

/**
 * Seam-A enqueue result (CLI-SURFACE §1.1 `--json` shape; IMPLEMENTATION-PLAN §3 Seam A).
 * `task_uuid` is null for a live-orchestrator-spawned unit that gets a lease + witness line but no
 * durable TW row (durable-row admission; SPEC "taskwarrior-row-only-for-durable-autonomous").
 */
export interface EnqueueResult {
  task_uuid: string | null;
  /**
   * The in-daemon job handle (the BarrierTracker key every terminal delta is recorded under) —
   * the identity a rowless (task_uuid:null) unit's `--wait` blocks by (issue #4). Additive-optional
   * field, not a protocol bump (§2.5); null only for a dedup `reused` skip (nothing was admitted).
   */
  job_id: string | null;
  lease_epoch: number;
  pool: Pool;
  status: JobStatusResult;
  session_ref: string | null;
  dedup_key: string | null;
  /** The witness ledger sequence number of the line this enqueue produced/will produce. */
  witness_lsn: number | null;
  verdict: Verdict | null;
}

/** Terminal/transient status strings an enqueue result reports (`reused` is the dedup skip). */
export type JobStatusResult = "queued" | "dispatched" | "reused" | "completed" | "failed" | "cancelled";

/**
 * The in-flight job record carried in the §2.2 `jobs[]` bootstrap (CLI-SURFACE §2.2). Lets a late
 * subscriber see pending work for the `--wait` barrier.
 */
export interface JobRecord {
  job_id: string;
  task_uuid: string | null;
  state: JobState;
  class: Priority;
  source: Source;
  agent_kind: AgentKind;
  pane_id: string | null;
  lease_epoch: number;
  attempt: number;
  gpu_seconds: number;
}
