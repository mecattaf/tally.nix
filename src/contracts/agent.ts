// tally — agent leg of the data model (CLI-SURFACE §0, §2.2 `agents[]`; SPEC "The view plane").
//
// The status enum is FROZEN at exactly four (CLI-SURFACE §0): herdr's fifth value `unknown` is an
// internal transient only — it never reaches the wire (it collapses to last-known, or `idle` at
// first sight). Adding a fifth wire status is a protocol bump.

/** The only four agent statuses that reach the wire (CLI-SURFACE §0 — FROZEN). */
export type AgentStatus = "blocked" | "working" | "done" | "idle";

/** All four wire statuses, in canonical order — the golden-tested tuple. */
export const AGENT_STATUSES = ["blocked", "working", "done", "idle"] as const satisfies readonly AgentStatus[];

/**
 * The internal transient status that NEVER reaches the wire (CLI-SURFACE §0). The detector uses it
 * before it collapses to last-known / `idle`. Kept here so the detector shares one name, but it is
 * deliberately NOT part of `AgentStatus`.
 */
export type InternalAgentStatus = AgentStatus | "unknown";

/** The three agent kinds dispatched through the one enqueue verb (CLI-SURFACE §0, §1.1a; PS#20). */
export type AgentKind = "pi" | "claude-code" | "shell";

/** All three kinds, canonical order. */
export const AGENT_KINDS = ["pi", "claude-code", "shell"] as const satisfies readonly AgentKind[];

/**
 * Which detection strategy produced a record/event (CLI-SURFACE §3.3): `hook` is cooperative and
 * AUTHORITATIVE, `scrape` is the universal throttled-`kitty @ get-text` fallback.
 */
export type DetectorStrategy = "hook" | "scrape";

export const DETECTOR_STRATEGIES = ["hook", "scrape"] as const satisfies readonly DetectorStrategy[];

/** Reasons an agent's authority ends (`agent.released` payload; CLI-SURFACE §2.3). */
export type AgentReleaseReason = "exited" | "cleared" | "pane_closed";

/**
 * An agent record — the `{kind, status}` leg keyed to a pane (CLI-SURFACE §2.2 `agents[]`).
 * `custom_status` is an opaque harness sub-label, NOT canonical; the field stays in the schema even
 * though `pane annotate` (its only writer) is deferred out of v0 (IMPLEMENTATION-PLAN §1 item 2).
 */
export interface AgentRecord {
  id: string;
  pane_id: string;
  session_id: string;
  kind: AgentKind;
  status: AgentStatus;
  /** Opaque harness sub-label; not canonical (CLI-SURFACE §2.2). */
  custom_status?: string;
  detector: DetectorStrategy;
  /** The zmx session handle (never conflated with `session_ref`; CLI-SURFACE §0). */
  persistence_session_id: string;
  /** The harness JSONL id (`--resume` join). May be null on shell / not-yet-known. */
  session_ref: string | null;
  /** Set when this agent is a dispatched job; null if interactive. */
  job_id: string | null;
  /** ISO-8601 timestamp of the current status's onset. */
  since: string;
}
