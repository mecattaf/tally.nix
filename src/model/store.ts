// tally — THE single authoritative session store (IMPLEMENTATION-PLAN M2.1, risk 9 "single-store
// ruling"). `store.ts` is the ONE place the Workspace → Session → Pane tree, the focus triple, and
// the two composed legs (`agents[]`, `jobs[]`) live. Detector and jobs do NOT own snapshot legs —
// they WRITE into this store via the `Bus` (the detector writes `agents[]` from its records, jobs
// writes `jobs[]` from lifecycle), each registering a `SnapshotSectionProvider` so the store knows
// which leg it feeds. `snapshot.ts` composes the §2.2 frame from this store alone; `discovery.ts`
// mutates the tier tiers into it. No layer-2 sibling is imported here — the join happens over the
// Bus and the section-provider seam.
//
// The store keeps the section legs two ways, belt-and-braces:
//   1. It SUBSCRIBES to the detector's `agent.*` / jobs' `job.*` Bus events and maintains a live
//      mirror of each leg, so a snapshot is O(1) even if a provider is momentarily absent.
//   2. It reads any registered `SnapshotSectionProvider` at assembly time as the authoritative
//      re-read (used after a supervised-loop restart repopulates a writer's own state) — the
//      provider wins over the mirror when present, so a restarted detector's fresh `agents[]`
//      supersedes stale mirror entries.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { AgentRecord } from "../contracts/agent";
import type { JobRecord } from "../contracts/job";
import type {
  Focus,
  PaneRecord,
  SessionRecord,
  Snapshot,
  SnapshotSection,
  SnapshotSectionProvider,
  WorkspaceRecord,
} from "../contracts/snapshot";
import { emptySnapshot } from "../contracts/snapshot";
import type { Bus, Unsubscribe } from "../contracts/bus";
import type { Clock } from "../contracts/exec";
import { systemClock } from "../contracts/exec";
import { stampSessionRollups } from "./rollup";

/** Options for the single store. `bus` wires the detector/jobs leg subscriptions; `clock` for ts. */
export interface StoreOptions {
  bus: Bus;
  clock?: Clock;
  /** Informational binary semver placed in the assembled frame's `daemon_version`. */
  daemonVersion?: string;
}

/**
 * The single authoritative session store. Holds the three tree tiers + focus, mirrors the
 * `agents[]`/`jobs[]` legs from the Bus, and composes the §2.2 frame body (the transport header —
 * `lease_epoch`/`seq`/`ts` — is stamped by daemon-core over the returned frame, per M1.1).
 */
export class SessionStore {
  private readonly bus: Bus;
  private readonly clock: Clock;
  private readonly daemonVersion: string;

  // Tree tiers — keyed by their `id`. Insertion order is preserved for stable snapshot ordering.
  private readonly workspaces = new Map<string, WorkspaceRecord>();
  private readonly sessions = new Map<string, SessionRecord>();
  private readonly panes = new Map<string, PaneRecord>();

  // The composed legs, mirrored from the Bus (detector writes agents, jobs writes jobs).
  private readonly agents = new Map<string, AgentRecord>();
  private readonly jobs = new Map<string, JobRecord>();

  // The registered full-section re-readers (authoritative over the mirror at assembly time).
  private readonly sectionProviders = new Map<SnapshotSection, SnapshotSectionProvider>();

  private focus: Focus = { workspace: null, session: null, pane: null };
  private readonly unsubs: Unsubscribe[] = [];

  constructor(opts: StoreOptions) {
    this.bus = opts.bus;
    this.clock = opts.clock ?? systemClock;
    this.daemonVersion = opts.daemonVersion ?? "0.1.0";
  }

  // ---- Bus wiring: mirror the detector's `agents[]` and jobs' `jobs[]` legs -------------------

  /**
   * Subscribe to the leg-writers' Bus events so the store keeps a live mirror of `agents[]` and
   * `jobs[]` without importing the detector or jobs modules. Idempotent: calling twice is harmless
   * (a second call re-subscribes only if not already wired). Returns an unsubscribe-all handle.
   */
  wireLegs(): Unsubscribe {
    if (this.unsubs.length > 0) return () => this.unwireLegs();

    // Agent leg (detector-owned). `agent.detected` seeds a record; `agent.status_changed` updates
    // its status/since; `agent.released` removes it. The detector's SnapshotSectionProvider is the
    // authoritative re-read; this mirror keeps snapshots cheap between reads.
    this.unsubs.push(
      this.bus.on("agent.detected", (p) => {
        this.agents.set(p.agent_id, {
          id: p.agent_id,
          pane_id: p.pane_id,
          session_id: p.session_id,
          kind: p.kind,
          status: p.status,
          detector: p.detector,
          persistence_session_id: p.persistence_session_id,
          session_ref: p.session_ref ?? null,
          job_id: null,
          since: this.clock.nowIso(),
        });
        this.bindPaneAgent(p.pane_id, p.agent_id);
      }),
    );
    this.unsubs.push(
      this.bus.on("agent.status_changed", (p) => {
        const existing = this.agents.get(p.agent_id);
        if (!existing) return;
        existing.status = p.status;
        existing.since = p.since;
        existing.detector = p.detector;
        if (p.custom_status !== undefined) existing.custom_status = p.custom_status;
      }),
    );
    this.unsubs.push(
      this.bus.on("agent.released", (p) => {
        this.agents.delete(p.agent_id);
        const pane = this.panes.get(p.pane_id);
        if (pane && pane.agent_id === p.agent_id) pane.agent_id = null;
      }),
    );

    // Job leg (jobs-owned). Every lifecycle event carries the `job_id`; the store keeps the latest
    // `state`/`gpu_seconds`/`attempt`/`lease_epoch` so a late `--wait` subscriber sees pending work.
    this.unsubs.push(
      this.bus.on("job.enqueued", (p) => {
        this.jobs.set(p.job_id, {
          job_id: p.job_id,
          task_uuid: p.task_uuid,
          state: "enqueued",
          class: p.class,
          source: p.source,
          agent_kind: p.agent_kind,
          pane_id: null,
          lease_epoch: 0,
          attempt: 1,
          gpu_seconds: 0,
        });
      }),
    );
    this.unsubs.push(
      this.bus.on("job.dispatched", (p) => {
        const j = this.jobs.get(p.job_id);
        if (!j) return;
        j.state = "dispatched";
        j.lease_epoch = p.lease_epoch;
        j.attempt = p.attempt;
      }),
    );
    this.unsubs.push(
      this.bus.on("job.started", (p) => {
        const j = this.jobs.get(p.job_id);
        if (!j) return;
        j.state = "started";
        if (p.pane_id !== undefined && p.pane_id !== null) j.pane_id = p.pane_id;
      }),
    );
    this.unsubs.push(
      this.bus.on("job.heartbeat", (p) => {
        const j = this.jobs.get(p.job_id);
        if (j) j.gpu_seconds = p.gpu_seconds;
      }),
    );
    this.unsubs.push(
      this.bus.on("job.preempted", (p) => {
        const j = this.jobs.get(p.job_id);
        if (j) j.state = "preempted";
      }),
    );
    this.unsubs.push(
      this.bus.on("job.resumed", (p) => {
        const j = this.jobs.get(p.job_id);
        if (!j) return;
        j.state = "resumed";
        j.lease_epoch = p.lease_epoch;
        j.attempt = p.attempt;
      }),
    );
    this.unsubs.push(
      this.bus.on("job.completed", (p) => {
        const j = this.jobs.get(p.job_id);
        if (j) j.state = "completed";
      }),
    );
    this.unsubs.push(
      this.bus.on("job.failed", (p) => {
        const j = this.jobs.get(p.job_id);
        if (j) j.state = "failed";
      }),
    );

    return () => this.unwireLegs();
  }

  private unwireLegs(): void {
    for (const u of this.unsubs) u();
    this.unsubs.length = 0;
  }

  /**
   * Register a full-section re-reader (`SnapshotSectionProvider`). The detector registers its
   * `agents[]` reader; jobs registers its `jobs[]` reader. At assembly time the provider's read is
   * authoritative over the Bus mirror (so a restarted writer's fresh state wins). Idempotent-replace
   * per section.
   */
  registerSectionProvider(provider: SnapshotSectionProvider): void {
    this.sectionProviders.set(provider.section, provider);
  }

  // ---- Tier mutation (discovery.ts writes the tree into the store) ---------------------------

  /** Upsert a workspace record (tier 1). Preserves insertion order for a new id. */
  upsertWorkspace(ws: WorkspaceRecord): void {
    this.workspaces.set(ws.id, ws);
  }

  /** Remove a workspace by id. */
  removeWorkspace(id: string): void {
    this.workspaces.delete(id);
  }

  /** Upsert a session record (tier 2). */
  upsertSession(session: SessionRecord): void {
    this.sessions.set(session.id, session);
  }

  /** Remove a session by id (its panes are removed separately by discovery). */
  removeSession(id: string): void {
    this.sessions.delete(id);
  }

  /** Upsert a pane record (tier 3). */
  upsertPane(pane: PaneRecord): void {
    this.panes.set(pane.id, pane);
  }

  /** Remove a pane by id. */
  removePane(id: string): void {
    this.panes.delete(id);
  }

  /** Bind (or clear) the agent leg of a pane. Called when an agent is detected/released. */
  bindPaneAgent(paneId: string, agentId: string | null): void {
    const pane = this.panes.get(paneId);
    if (pane) pane.agent_id = agentId;
  }

  /** Mark (or unmark) a pane a viewer — the `session.register_viewer` mechanism (anti-loop #4). */
  setViewer(paneId: string, isViewer: boolean): boolean {
    const pane = this.panes.get(paneId);
    if (!pane) return false;
    pane.is_viewer = isViewer;
    return true;
  }

  /** Replace the focus triple (workspace/session/pane focus edges update it). */
  setFocus(focus: Partial<Focus>): void {
    this.focus = { ...this.focus, ...focus };
  }

  /** The current focus triple (a copy — callers never mutate the store's field). */
  getFocus(): Focus {
    return { ...this.focus };
  }

  // ---- Read accessors (discovery/workspace/snapshot read the store) --------------------------

  getWorkspace(id: string): WorkspaceRecord | undefined {
    return this.workspaces.get(id);
  }
  getSession(id: string): SessionRecord | undefined {
    return this.sessions.get(id);
  }
  getPane(id: string): PaneRecord | undefined {
    return this.panes.get(id);
  }
  hasSession(id: string): boolean {
    return this.sessions.has(id);
  }
  hasPane(id: string): boolean {
    return this.panes.has(id);
  }

  /** Every workspace, insertion order. */
  listWorkspaces(): WorkspaceRecord[] {
    return [...this.workspaces.values()];
  }
  /** Every session, insertion order. */
  listSessions(): SessionRecord[] {
    return [...this.sessions.values()];
  }
  /** Every pane, insertion order. */
  listPanes(): PaneRecord[] {
    return [...this.panes.values()];
  }
  /** Every pane belonging to one session. */
  panesOfSession(sessionId: string): PaneRecord[] {
    return [...this.panes.values()].filter((p) => p.session_id === sessionId);
  }

  /**
   * The current `agents[]` leg — the registered detector provider if present (authoritative
   * re-read), else the Bus mirror. Non-viewer panes only is enforced by the detector, not here (a
   * viewer pane never gets an agent record).
   */
  readAgents(): AgentRecord[] {
    const provider = this.sectionProviders.get("agents");
    if (provider) return provider.read() as AgentRecord[];
    return [...this.agents.values()];
  }

  /** The current `jobs[]` leg — the registered jobs provider if present, else the Bus mirror. */
  readJobs(): JobRecord[] {
    const provider = this.sectionProviders.get("jobs");
    if (provider) return provider.read() as JobRecord[];
    return [...this.jobs.values()];
  }

  // ---- Snapshot body composition -------------------------------------------------------------

  /**
   * Compose the §2.2 frame BODY from the single store: the three tree tiers, the focus triple, and
   * the two legs (agents/jobs) read at assembly time with rollups stamped. The transport header
   * (`lease_epoch`/`seq`/`ts`/`daemon_version`) is authoritatively overwritten by daemon-core's
   * `assembleSnapshot` after this returns, so the values here are seeds only.
   */
  composeSnapshot(): Snapshot {
    const agents = this.readAgents();
    const jobs = this.readJobs();
    const panes = this.listPanes();
    const sessions = this.listSessions();

    // Refresh each session's `pane_ids` from the live pane set, then stamp the rollups so the hint
    // matches the emitted legs byte-for-byte.
    const panesBySession = new Map<string, string[]>();
    for (const p of panes) {
      let list = panesBySession.get(p.session_id);
      if (!list) {
        list = [];
        panesBySession.set(p.session_id, list);
      }
      list.push(p.id);
    }
    for (const s of sessions) {
      s.pane_ids = panesBySession.get(s.id) ?? [];
    }
    stampSessionRollups(sessions, panes, agents);

    const base = emptySnapshot({
      daemon_version: this.daemonVersion,
      lease_epoch: 0,
      seq: 0,
      ts: this.clock.nowIso(),
    });
    base.focus = this.getFocus();
    base.workspaces = this.listWorkspaces();
    base.sessions = sessions;
    base.panes = panes;
    base.agents = agents;
    base.jobs = jobs;
    return base;
  }
}
