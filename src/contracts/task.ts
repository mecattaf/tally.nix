// tally — TaskChampion veneer contract: UDA vocabulary, the `trust` review enum, and the
// durable-row admission predicate signature (CLI-SURFACE §0, §1.1a durable-row admission;
// SPEC "Three records", "taskwarrior-row-only-for-durable-autonomous", "The trust review UDA";
// IMPLEMENTATION-PLAN M1.3).
//
// TW is a thin durable veneer — one row per durable job, one standing row per drain, and NO
// high-frequency machine state (heartbeats, leases, evidence) ever written to TW (SPEC veneer
// discipline). Access is `task export` / `task import` shell-out only (jul9 ruling) — this file
// defines only the shapes the veneer reads/writes, never the mechanism.

import type { AgentKind } from "./agent";
import type { LaborClass, Pool, Priority, Source } from "./job";

/**
 * The `trust` review UDA (SPEC "The trust review UDA"). Written `unreviewed` at completion, flipped
 * only by the morning-report skill or a voluntary recall. `recalled` is a post-hoc revert record —
 * the field describes PAST work and NEVER blocks future work.
 */
export type Trust = "unreviewed" | "reviewed" | "recalled";
export const TRUST_VALUES = ["unreviewed", "reviewed", "recalled"] as const satisfies readonly Trust[];

/** taskwarrior standard status values tally reads/writes on a row. */
export type TaskStatus = "pending" | "waiting" | "completed" | "deleted";

/**
 * The tally UDA vocabulary bootstrapped idempotently via `task config` (IMPLEMENTATION-PLAN M1.3).
 * Each entry is a UDA name → its declared taskwarrior type. `label` is optional prose. This table
 * is the single source of truth the `udas.ts` bootstrap and the row (de)serializer both read.
 */
export type UdaType = "string" | "numeric" | "date";

export interface UdaSpec {
  name: string;
  type: UdaType;
  /** For an enumerated string UDA, its permitted values (rendered as `uda.<name>.values`). */
  values?: readonly string[];
  label?: string;
}

/**
 * The frozen UDA vocabulary (IMPLEMENTATION-PLAN M1.3; SPEC "Canonical work store"). `agent`,
 * `labor_class`, `pool`, `session_ref`, `model_class`, `cwd`, `worktree`, `trust`, plus the
 * job-metadata UDAs (`dedup_key`, `lease_epoch`, `source`, `priority_class`, `attempt`,
 * `argv_json`, `evidence_json`) the row needs for cross-source urgency and crash-survival.
 * Priority maps to native TW `priority` (urgency engine, PS#1a) and is therefore NOT a UDA.
 *
 * `argv_json`/`evidence_json` carry the job's STATIC enqueue-time identity (the verbatim leaf argv
 * and the declared evidence gates), written ONCE at row creation and never updated — a row that
 * cannot reconstruct its argv/evidence fails its crash-survival charter (durable-row admission,
 * CLI-SURFACE §1.1a). They are NOT high-frequency machine state, so the veneer discipline
 * (heartbeats/leases/evidence RESULTS never touch TW) is untouched.
 */
export const TALLY_UDAS = [
  { name: "agent", type: "string", label: "agent kind" },
  { name: "labor_class", type: "string", values: ["fresh", "recovered", "reused"], label: "labor class" },
  { name: "pool", type: "string", label: "compute pool" },
  { name: "session_ref", type: "string", label: "harness JSONL session id" },
  { name: "model_class", type: "string", label: "declared model class" },
  { name: "cwd", type: "string", label: "working directory" },
  { name: "worktree", type: "string", label: "worktree branch" },
  { name: "trust", type: "string", values: ["unreviewed", "reviewed", "recalled"], label: "trust review" },
  { name: "dedup_key", type: "string", label: "dedup-by-existence key" },
  { name: "lease_epoch", type: "numeric", label: "lease epoch fence" },
  { name: "source", type: "string", label: "provenance" },
  { name: "priority_class", type: "string", label: "tally priority class" },
  { name: "attempt", type: "numeric", label: "retry attempt" },
  { name: "argv_json", type: "string", label: "leaf argv (JSON array, verbatim)" },
  { name: "evidence_json", type: "string", label: "evidence spec (JSON array)" },
] as const satisfies readonly UdaSpec[];

/** The UDA names as a set, for quick membership checks in the veneer. */
export const TALLY_UDA_NAMES: readonly string[] = TALLY_UDAS.map((u) => u.name);

/**
 * A tally-shaped TaskChampion row, in the taskwarrior JSON export/import shape. Standard columns
 * (`uuid`, `description`, `status`, `priority`, timestamps) plus the tally UDAs. Any field the
 * veneer does not manage passes through untouched (merge-not-clobber discipline).
 */
export interface TaskRow {
  uuid: string;
  description: string;
  status: TaskStatus;
  /** Native taskwarrior priority (`H|M|L`) — the urgency engine input; maps from `Priority`. */
  priority?: "H" | "M" | "L";
  entry?: string;
  modified?: string;
  end?: string;
  // tally UDAs:
  agent?: AgentKind | string;
  labor_class?: LaborClass;
  pool?: Pool;
  session_ref?: string;
  model_class?: string;
  cwd?: string;
  worktree?: string;
  trust?: Trust;
  dedup_key?: string;
  lease_epoch?: number;
  source?: Source;
  priority_class?: Priority;
  attempt?: number;
  /** The verbatim leaf argv, JSON-encoded — written once at enqueue, read back on recovery. */
  argv_json?: string;
  /** The declared evidence checks, JSON-encoded — written once at enqueue, read back on recovery. */
  evidence_json?: string;
  /** Passthrough for foreign attributes the veneer must not clobber. */
  [extra: string]: unknown;
}

/** Native taskwarrior priority letter for a tally `Priority`. */
export function twPriority(p: Priority): "H" | "M" | "L" {
  return p === "high" ? "H" : p === "medium" ? "M" : "L";
}

/**
 * The durable-row admission predicate input (CLI-SURFACE §1.1a; SPEC
 * "taskwarrior-row-only-for-durable-autonomous"). A row is written ONLY IF the unit needs
 * cross-source urgency ranking OR must survive a crash to be re-dispatched.
 */
export interface AdmissionInput {
  source: Source;
  /** True when the unit was spawned live by a running orchestrator (⇒ no row, `task_uuid: null`). */
  liveOrchestratorSpawned: boolean;
  /** True when the unit is autonomous/batch/queued (OCR firehose, gh intake, timers). */
  autonomous: boolean;
  /** True when the unit must survive a crash to be re-dispatched. */
  crashSurvivable: boolean;
  /** True when the unit needs cross-source urgency ranking against the one store. */
  needsCrossSourceUrgency: boolean;
}

/**
 * The durable-row admission predicate (SPEC appendix; CLI-SURFACE §1.1a). Returns true when the
 * unit earns a durable TaskChampion row. A live-orchestrator-spawned unit NEVER earns a row
 * (`task_uuid: null`) regardless of the other flags.
 */
export function admitsDurableRow(input: AdmissionInput): boolean {
  if (input.liveOrchestratorSpawned) return false;
  return input.autonomous || input.crashSurvivable || input.needsCrossSourceUrgency;
}

/**
 * The `prev_*` shadow projection (CLI-SURFACE §2.3 note; IMPLEMENTATION-PLAN M1.3 `oplog.ts`).
 * Derived from the row exported immediately before mutation — the op-log's computed delta at the
 * only legal access altitude. Additive-optional on the wire; never a protocol bump.
 */
export interface PrevShadow {
  prev_state?: string;
  prev_status?: TaskStatus;
}

