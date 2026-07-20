// tally — the delta-event enumeration (CLI-SURFACE §2.3, FROZEN; IMPLEMENTATION-PLAN §3 Wire).
//
// Every consumer MUST ignore unknown event names and unknown fields (forward-compat is a hard
// contract, §2.3). Job event names mirror the journald TALLY_EVENT vocabulary VERBATIM — one
// vocabulary (see `journal.ts`). Selected `job.*`/`pane.*` events carry optional `prev_*` shadow
// fields (§2.3 note; additive-optional, never a protocol bump).

import type { AgentKind, AgentReleaseReason, AgentStatus, DetectorStrategy } from "./agent";
import type { LaborClass, Priority, Source, Verdict } from "./job";
import type { PrevShadow } from "./task";

// ---------------------------------------------------------------------------------------------
// Agent family (the spine).
// ---------------------------------------------------------------------------------------------

/** One per agent per pane lifetime — first identification (CLI-SURFACE §2.3). */
export interface AgentDetectedPayload {
  agent_id: string;
  pane_id: string;
  session_id: string;
  kind: AgentKind;
  status: AgentStatus;
  detector: DetectorStrategy;
  persistence_session_id: string;
  session_ref?: string | null;
  kitty_window_id: number;
}

/**
 * SPINE event — every real 4-state transition (CLI-SURFACE §2.3). Internal `unknown` never reaches
 * the wire. `prev_status` here is the prior `AgentStatus` (the transition's source state), distinct
 * from the TW-op-log `prev_*` shadow that job/pane events carry.
 */
export interface AgentStatusChangedPayload {
  agent_id: string;
  pane_id: string;
  session_id: string;
  status: AgentStatus;
  prev_status?: AgentStatus;
  detector: DetectorStrategy;
  custom_status?: string;
  since: string;
}

/** Convenience frame right after the `status_changed` whose target is `blocked`. */
export interface AgentBlockedPayload {
  agent_id: string;
  pane_id: string;
  session_id: string;
  detector: DetectorStrategy;
  reason?: string;
  prompt_excerpt?: string;
  since: string;
}

/** Convenience frame right after the `status_changed` whose target is `done`. */
export interface AgentDonePayload {
  agent_id: string;
  pane_id: string;
  session_id: string;
  detector: DetectorStrategy;
  since: string;
}

/** Agent authority ends: process exited, authority cleared, or pane closed. */
export interface AgentReleasedPayload {
  agent_id: string;
  pane_id: string;
  session_id: string;
  reason: AgentReleaseReason;
}

// ---------------------------------------------------------------------------------------------
// Pane family (observed kitty windows).
// ---------------------------------------------------------------------------------------------

export interface PaneCreatedPayload {
  pane_id: string;
  session_id: string;
  kitty_window_id: number;
  cwd: string | null;
  worktree?: string | null;
  is_viewer: boolean;
}

export interface PaneClosedPayload {
  pane_id: string;
  session_id: string;
  reason: string;
}

export interface PaneFocusedPayload {
  pane_id: string;
  session_id: string;
  workspace_id: string;
  prev_pane_id?: string | null;
}

/** The out-of-band `kitty @ get-text` read shape carried by `pane.output_matched` (CLI-SURFACE §2.3). */
export interface PaneRead {
  source: "visible" | "recent" | "detection";
  format: "text" | "ansi";
  text: string;
  revision: number;
  /** Set true when the matched read hit the 64 KiB FRAME_CAP (§2.1). */
  truncated: boolean;
}

/**
 * The detector (or an active `session.wait pane_output` predicate) matched a region+regex — agent
 * panes only; `is_viewer` rejected. The detector is the SOLE emitter (IMPLEMENTATION-PLAN M2.3).
 */
export interface PaneOutputMatchedPayload extends PrevShadow {
  pane_id: string;
  session_id: string;
  matched_line: string;
  read: PaneRead;
}

// ---------------------------------------------------------------------------------------------
// Session / Workspace family (observational — tally never creates).
// ---------------------------------------------------------------------------------------------

export interface SessionObservedPayload {
  session_id: string;
  workspace_id: string;
  persistence_session_id: string;
  backend: "zmx";
  observed_at: string;
}

export interface SessionEndedPayload {
  session_id: string;
  workspace_id: string;
  reason: string;
}

export interface WorkspaceFocusedPayload {
  workspace_id: string;
  prev_workspace_id?: string | null;
}

// ---------------------------------------------------------------------------------------------
// Job family (mirror journald TALLY_EVENT verbatim — one vocabulary, not a second store).
// ---------------------------------------------------------------------------------------------

export interface JobEnqueuedPayload {
  job_id: string;
  task_uuid: string | null;
  class: Priority;
  source: Source;
  agent_kind: AgentKind;
  invocation: string;
  cwd: string | null;
  worktree?: string | null;
  evidence_spec: string[];
  priority: Priority;
}

export interface JobDispatchedPayload extends PrevShadow {
  job_id: string;
  task_uuid: string | null;
  agent_kind: AgentKind;
  unit: string;
  lease_epoch: number;
  attempt: number;
}

export interface JobStartedPayload {
  job_id: string;
  task_uuid: string | null;
  pane_id?: string | null;
  agent_id?: string | null;
  session_ref?: string | null;
  unit: string;
  ts: string;
}

export interface JobHeartbeatPayload {
  job_id: string;
  gpu_seconds: number;
}

export interface JobPreemptedPayload {
  job_id: string;
  reason: string;
}

export interface JobResumedPayload {
  job_id: string;
  labor_class: Extract<LaborClass, "recovered" | "reused">;
  lease_epoch: number;
  attempt: number;
}

export interface JobEvidencePayload {
  job_id: string;
  task_uuid: string | null;
  verdict: Verdict;
  checked_paths: string[];
}

export interface JobCompletedPayload extends PrevShadow {
  job_id: string;
  task_uuid: string | null;
  exit_code: number;
  gpu_seconds: number | null;
  artifact_hash: string | null;
  labor_class: LaborClass;
}

export interface JobFailedPayload extends PrevShadow {
  job_id: string;
  task_uuid: string | null;
  exit_code: number;
  gpu_seconds: number | null;
  verdict?: Verdict;
  labor_class: LaborClass;
}

export interface JobWitnessEmittedPayload {
  job_id: string;
  task_uuid: string | null;
  witness_ref: string;
}

// ---------------------------------------------------------------------------------------------
// Stream-control frames.
// ---------------------------------------------------------------------------------------------

/** ~15s idle heartbeat — NOT replayable (no own `seq`, does not advance the cursor). */
export interface HeartbeatPayload {
  ts: string;
  latest_seq: number;
}

/** Final frame to a slow subscriber whose unacked backlog exceeded MAX_UNACKED, before disconnect. */
export interface StreamOverflowPayload {
  reason: string;
  oldest_seq: number;
  latest_seq: number;
}

// ---------------------------------------------------------------------------------------------
// The event-name → payload map, the union, and the category grouping.
// ---------------------------------------------------------------------------------------------

/**
 * The exhaustive event-name → payload-type map (CLI-SURFACE §2.3). Adding an entry is additive
 * (never a protocol bump); the golden tests pin the current set so no name is silently dropped.
 */
export interface EventPayloadMap {
  "agent.detected": AgentDetectedPayload;
  "agent.status_changed": AgentStatusChangedPayload;
  "agent.blocked": AgentBlockedPayload;
  "agent.done": AgentDonePayload;
  "agent.released": AgentReleasedPayload;
  "pane.created": PaneCreatedPayload;
  "pane.closed": PaneClosedPayload;
  "pane.focused": PaneFocusedPayload;
  "pane.output_matched": PaneOutputMatchedPayload;
  "session.observed": SessionObservedPayload;
  "session.ended": SessionEndedPayload;
  "workspace.focused": WorkspaceFocusedPayload;
  "job.enqueued": JobEnqueuedPayload;
  "job.dispatched": JobDispatchedPayload;
  "job.started": JobStartedPayload;
  "job.heartbeat": JobHeartbeatPayload;
  "job.preempted": JobPreemptedPayload;
  "job.resumed": JobResumedPayload;
  "job.evidence_pass": JobEvidencePayload;
  "job.evidence_fail": JobEvidencePayload;
  "job.completed": JobCompletedPayload;
  "job.failed": JobFailedPayload;
  "job.witness_emitted": JobWitnessEmittedPayload;
  heartbeat: HeartbeatPayload;
  "stream.overflow": StreamOverflowPayload;
}

/** The set of all event names (CLI-SURFACE §2.3). */
export type EventName = keyof EventPayloadMap;

/** All event names, canonical order — golden-tested for completeness. */
export const EVENT_NAMES = [
  "agent.detected",
  "agent.status_changed",
  "agent.blocked",
  "agent.done",
  "agent.released",
  "pane.created",
  "pane.closed",
  "pane.focused",
  "pane.output_matched",
  "session.observed",
  "session.ended",
  "workspace.focused",
  "job.enqueued",
  "job.dispatched",
  "job.started",
  "job.heartbeat",
  "job.preempted",
  "job.resumed",
  "job.evidence_pass",
  "job.evidence_fail",
  "job.completed",
  "job.failed",
  "job.witness_emitted",
  "heartbeat",
  "stream.overflow",
] as const satisfies readonly EventName[];

/** The subscribe-filter categories (CLI-SURFACE §2.4 `categories?`). */
export type EventCategory = "agent" | "pane" | "session" | "workspace" | "job" | "control";
export const EVENT_CATEGORIES = ["agent", "pane", "session", "workspace", "job", "control"] as const satisfies readonly EventCategory[];

/**
 * The event names that are NOT replayable (no own `seq`, do not advance the cursor; CLI-SURFACE
 * §2.1, §2.3). `heartbeat` and `stream.overflow` are control frames.
 */
export const NON_REPLAYABLE_EVENTS = ["heartbeat", "stream.overflow"] as const satisfies readonly EventName[];

/** Map an event name to its subscribe-filter category. */
export function eventCategory(name: EventName): EventCategory {
  if (name === "heartbeat" || name === "stream.overflow") return "control";
  if (name.startsWith("agent.")) return "agent";
  if (name.startsWith("pane.")) return "pane";
  if (name.startsWith("workspace.")) return "workspace";
  if (name.startsWith("session.")) return "session";
  if (name.startsWith("job.")) return "job";
  // Exhaustive over the current set; unreachable, but keeps the function total.
  return "control";
}

/** Whether an event advances the replay cursor / carries its own `seq`. */
export function isReplayable(name: EventName): boolean {
  return !(NON_REPLAYABLE_EVENTS as readonly EventName[]).includes(name);
}

/** A helper: the discriminated payload for one specific event name. */
export type PayloadOf<N extends EventName> = EventPayloadMap[N];
