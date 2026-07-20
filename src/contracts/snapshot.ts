// tally — the `session.snapshot` bootstrap frame (CLI-SURFACE §2.2, FROZEN byte-for-byte;
// IMPLEMENTATION-PLAN §3 Wire). The §2.2 field set and nesting are pinned by the golden tests so
// any deviation from a field name is a build failure.

import type { AgentRecord, AgentStatus } from "./agent";
import type { JobRecord } from "./job";
import { PROTOCOL_ID, PROTOCOL_VERSION } from "./constants";

/** The focus triple — the currently focused workspace/session/pane (CLI-SURFACE §2.2 `focus`). */
export interface Focus {
  workspace: string | null;
  session: string | null;
  pane: string | null;
}

/** A client-side status aggregation hint per session (CLI-SURFACE §2.2 `status_rollup`). */
export interface StatusRollup {
  blocked: number;
  working: number;
  done: number;
  idle: number;
}

/** TREE TIER 1 — Workspace (niri panel grouping; tally OBSERVES, niri owns layout). */
export interface WorkspaceRecord {
  id: string;
  label: string;
  focused_session: string | null;
}

/** TREE TIER 2 — Session (a zmx session; DOTFILES-OWNED — tally never creates/names/attaches). */
export interface SessionRecord {
  id: string;
  workspace_id: string;
  /** The handle `zmx attach <session>` uses (never conflated with `session_ref`). */
  persistence_session_id: string;
  backend: "zmx";
  /** When tally first SAW a pane in it (not creation). */
  observed_at: string;
  pane_ids: string[];
  status_rollup: StatusRollup;
}

/** TREE TIER 3 — Pane (one kitty window; one kitty terminal = one panel). */
export interface PaneRecord {
  /** Composite key `"<session>:<pane>"` (CLI-SURFACE §0). */
  id: string;
  session_id: string;
  /** Focus / tunnel-in key. */
  kitty_window_id: number;
  cwd: string | null;
  /** Carried from the job at ignition. */
  worktree?: string | null;
  /** → `agents[]` (null for a bare shell / viewer). */
  agent_id: string | null;
  /** True = a `tally watch` pane; the detector NEVER scrapes it (anti-loop invariant #4). */
  is_viewer: boolean;
}

/**
 * The complete `session.snapshot` bootstrap frame (CLI-SURFACE §2.2, FROZEN). Property order here
 * mirrors the doc's example for the golden shape test.
 */
export interface Snapshot {
  /** Fixed stream identifier (`"tally.delta"`). */
  protocol: typeof PROTOCOL_ID;
  /** Integer; bumps only on a breaking change (§2.5). */
  protocol_version: number;
  /** Informational semver of the one binary; never gates behavior. */
  daemon_version: string;
  /** Monotonic fence (PS#9/#11). Changes ONLY on daemon (re)start ⇒ client re-snapshots. */
  lease_epoch: number;
  /** Latest event seq at snapshot time; subscribe from here. */
  seq: number;
  ts: string;
  focus: Focus;
  workspaces: WorkspaceRecord[];
  sessions: SessionRecord[];
  panes: PaneRecord[];
  agents: AgentRecord[];
  jobs: JobRecord[];
}

/** The property order the §2.2 frame is emitted/tested in — pinned by the golden shape test. */
export const SNAPSHOT_KEY_ORDER = [
  "protocol",
  "protocol_version",
  "daemon_version",
  "lease_epoch",
  "seq",
  "ts",
  "focus",
  "workspaces",
  "sessions",
  "panes",
  "agents",
  "jobs",
] as const satisfies readonly (keyof Snapshot)[];

/** An empty rollup — the additive zero. */
export function emptyRollup(): StatusRollup {
  return { blocked: 0, working: 0, done: 0, idle: 0 };
}

/** Aggregate a status into a rollup (used by session-model rollup and client hints). */
export function tallyStatus(rollup: StatusRollup, status: AgentStatus): void {
  rollup[status] += 1;
}

/**
 * A minimal seed snapshot — a well-formed §2.2 frame with empty legs. The daemon uses this shape at
 * boot before any session is observed; tests use it as the golden skeleton.
 */
export function emptySnapshot(args: {
  daemon_version: string;
  lease_epoch: number;
  seq: number;
  ts: string;
}): Snapshot {
  return {
    protocol: PROTOCOL_ID,
    protocol_version: PROTOCOL_VERSION,
    daemon_version: args.daemon_version,
    lease_epoch: args.lease_epoch,
    seq: args.seq,
    ts: args.ts,
    focus: { workspace: null, session: null, pane: null },
    workspaces: [],
    sessions: [],
    panes: [],
    agents: [],
    jobs: [],
  };
}

// ---------------------------------------------------------------------------------------------
// Snapshot composition seams (IMPLEMENTATION-PLAN §3 Seams, risk 9 — the single-store ruling).
// ---------------------------------------------------------------------------------------------

/**
 * The seam daemon-core calls to assemble the §2.2 bootstrap frame (IMPLEMENTATION-PLAN M1.1). The
 * session-model registers ONE `SnapshotProvider`; daemon-core owns the transport and never the
 * model. The provider reads the single `model/store.ts` (which the detector/jobs wrote their legs
 * into via the Bus) and returns the frame verbatim.
 */
export interface SnapshotProvider {
  /** Assemble the current §2.2 bootstrap frame from the single store. */
  snapshot(): Snapshot;
}

/**
 * The section a writer feeds into the single store (IMPLEMENTATION-PLAN §3 Seams
 * `SnapshotSectionProvider`, risk 9). The DETECTOR feeds `agents`; JOBS feeds `jobs`. The store
 * knows which leg each writer owns, and `snapshot.ts` composes the frame from the store — the
 * writers never own snapshot legs themselves and never import each other.
 */
export type SnapshotSection = "agents" | "jobs";

/**
 * The typed interface a section-writer registers so the single store knows which leg it feeds
 * (IMPLEMENTATION-PLAN §3 Seams). The store subscribes via the `Bus`; this declares the contract of
 * what a writer provides on demand (a full-section re-read used at snapshot assembly / after a
 * supervised-loop restart), keyed by the section it owns.
 */
export interface SnapshotSectionProvider<S extends SnapshotSection = SnapshotSection> {
  /** Which snapshot leg this provider feeds. */
  readonly section: S;
  /** The current full contents of this section — read by the store at assembly time. */
  read(): S extends "agents" ? AgentRecord[] : JobRecord[];
}
