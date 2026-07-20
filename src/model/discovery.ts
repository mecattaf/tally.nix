// tally — discovery: the reconcile-from-substrate join (IMPLEMENTATION-PLAN M2.1 `discovery.ts`;
// CLI-SURFACE §2.2, §2.3, §3.2). It joins the three read surfaces into the single store's tree
// tiers and emits the OBSERVATIONAL delta vocabulary (`session.observed/ended`, `pane.created/
// closed/focused`, `workspace.focused`) per §2.3:
//
//   zmx list --short   → the session universe (persistence_session_id == zmx name; §3.2)
//   kitty @ ls         → the pane universe (one kitty window = one pane, keyed on kitty_window_id)
//   watcher edges      → the event-driven refresh trigger (replaces existence-polling; §3.1)
//
// tally OBSERVES; it never creates a zmx session or launches a kitty window. `observed_at` is the
// FIRST time tally saw a pane in a session (never creation). A pane leaving tally's view emits
// `pane.closed`; the last pane of a session leaving emits `session.ended` (the zmx session itself
// may persist — tally just stops seeing it).
//
// The window→session binding: a kitty window declares the zmx session it belongs to via the opaque
// identity user-var `tally_session` (the one back-reference sensors may write, §5 flag 1); its pane
// short-name comes from `tally_pane` when present, else `p<kitty_window_id>`. A window with no
// `tally_session` user-var is grouped under the zmx session whose name it matches, or — failing any
// match — is NOT admitted to the tree (tally only models panes it can place under an enumerated zmx
// session; an orphan kitty window outside the zmx substrate is not tally's to observe).
//
// `is_viewer`: a `tally session watch` pane marks ITSELF via `session.register_viewer
// {kitty_window_id}` (the client reads `$KITTY_WINDOW_ID` from its env). Discovery honors that
// marking across refreshes so the detector / `pane capture --source detection` / `session.wait
// pane_output` exclude it (anti-loop invariant #4).
//
// `sessions` scoping (IMPLEMENTATION-PLAN M3.3 risk 11): the config's `sessions` globs filter the
// zmx universe discovery observes; `[]` = observe ALL.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Bus } from "../contracts/bus";
import { ValidationError } from "../contracts/errors";
import type { Exec, ExecOptions, Clock } from "../contracts/exec";
import { systemClock } from "../contracts/exec";
import { makePaneId } from "../contracts/selectors";
import type { PaneRecord, SessionRecord, WorkspaceRecord } from "../contracts/snapshot";
import { KittyRc, type KittyWindow } from "../kitty/rc";
import { ZmxClient } from "../zmx/client";
import {
  KITTY_WATCHER_BUS_EVENT,
  sensorEdgeBus,
  validateWatcherEvent,
} from "../kitty/watcher-ingest";
import type { SessionStore } from "./store";
import { WorkspaceSource } from "./workspace";

/** The opaque identity user-var a kitty window carries to declare its zmx session (§5 flag 1). */
export const SESSION_USER_VAR = "tally_session" as const;
/** The opaque identity user-var a kitty window carries to declare its pane short-name. */
export const PANE_USER_VAR = "tally_pane" as const;

/** Options for the discovery join. */
export interface DiscoveryOptions {
  store: SessionStore;
  bus: Bus;
  exec: Exec;
  /** zmx session-name globs the discovery scopes to (`config.sessions`); `[]` = observe all. */
  sessions?: string[];
  /** The niri panel host used to name the default workspace (`config.conductorHost`). */
  conductorHost?: string;
  clock?: Clock;
  /** Injectable kitty client (defaults to a fresh `KittyRc(exec)`). */
  kitty?: KittyRc;
  /** Injectable zmx client (defaults to a fresh `ZmxClient(exec)`). */
  zmx?: ZmxClient;
  /** Injectable workspace source (defaults to a fresh `WorkspaceSource(exec, conductorHost)`). */
  workspaceSource?: WorkspaceSource;
}

/**
 * Compile one zmx `sessions` glob to a matcher. The glob grammar is minimal (the only forms the
 * dotfiles `term-MMDD-HHMMSS` names need): `*` matches any run of characters, `?` matches one, every
 * other character is literal. An exact name with no metacharacters matches only itself.
 */
function globToRegExp(glob: string): RegExp {
  let src = "^";
  for (const ch of glob) {
    if (ch === "*") src += ".*";
    else if (ch === "?") src += ".";
    else src += ch.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  }
  src += "$";
  return new RegExp(src);
}

/**
 * The discovery engine. Owns the reconcile join; mutates the single store and emits the
 * observational events. It NEVER writes the `agents[]`/`jobs[]` legs (those are the detector's /
 * jobs' via the Bus) — it owns tiers 1–3 and the focus triple only.
 */
export class Discovery {
  private readonly store: SessionStore;
  private readonly bus: Bus;
  private readonly kitty: KittyRc;
  private readonly zmx: ZmxClient;
  private readonly workspaceSource: WorkspaceSource;
  private readonly clock: Clock;
  private readonly sessionMatchers: RegExp[] | null;

  /** Viewer markings survive refreshes: kitty_window_id → is_viewer (set by register_viewer). */
  private readonly viewerWindows = new Set<number>();
  /** First-seen timestamps per session id (observed_at is stable across refreshes). */
  private readonly observedAt = new Map<string, string>();

  constructor(opts: DiscoveryOptions) {
    this.store = opts.store;
    this.bus = opts.bus;
    this.clock = opts.clock ?? systemClock;
    this.kitty = opts.kitty ?? new KittyRc(opts.exec);
    this.zmx = opts.zmx ?? new ZmxClient(opts.exec);
    this.workspaceSource =
      opts.workspaceSource ?? new WorkspaceSource(opts.exec, opts.conductorHost ?? "default");
    const globs = opts.sessions ?? [];
    this.sessionMatchers = globs.length === 0 ? null : globs.map(globToRegExp);
  }

  /** Whether a zmx session name is in the configured discovery scope (`[]` ⇒ everything). */
  private inScope(name: string): boolean {
    if (this.sessionMatchers === null) return true;
    return this.sessionMatchers.some((re) => re.test(name));
  }

  /**
   * Subscribe to the normalized kitty watcher edges on the Bus (from `watcher-ingest`) so a window
   * open/close/focus edge triggers a reconcile instead of the daemon existence-polling. Returns an
   * unsubscribe handle. The full reconcile is cheap (two shell reads + a diff); a per-edge fast path
   * would duplicate the join, so an edge simply schedules a reconcile.
   */
  wireWatcher(): () => void {
    const edgeBus = sensorEdgeBus(this.bus);
    return edgeBus.on(KITTY_WATCHER_BUS_EVENT, (payload) => {
      try {
        validateWatcherEvent(payload);
      } catch {
        return; // a malformed edge is dropped; the periodic reconcile self-heals.
      }
      // A user-var change carrying our identity vars, or any window lifecycle edge, warrants a
      // reconcile. Fire-and-forget: reconcile is idempotent, and an in-flight one is not re-entered.
      void this.reconcile().catch(() => {
        /* reconcile logs its own read failures; a watcher-driven reconcile never throws upward. */
      });
    });
  }

  /**
   * `session.register_viewer {kitty_window_id}` — a `session watch` client marks its OWN pane a
   * viewer (anti-loop invariant #4). The marking is remembered so it survives reconciles; if the
   * pane is already in the store, it is flipped immediately. Hand-rolled validation (no zod).
   */
  registerViewer = (params: unknown): { registered: true; kitty_window_id: number } => {
    if (typeof params !== "object" || params === null || Array.isArray(params)) {
      throw new ValidationError("register_viewer params must be an object", "params");
    }
    const wid = (params as Record<string, unknown>).kitty_window_id;
    if (typeof wid !== "number" || !Number.isFinite(wid)) {
      throw new ValidationError("kitty_window_id must be a finite number", "kitty_window_id");
    }
    this.viewerWindows.add(wid);
    // Flip any live pane bound to this window id.
    for (const pane of this.store.listPanes()) {
      if (pane.kitty_window_id === wid && !pane.is_viewer) {
        this.store.setViewer(pane.id, true);
      }
    }
    return { registered: true, kitty_window_id: wid };
  };

  /**
   * The reconcile join — the ONE discovery operation. Reads the zmx + kitty universes, joins them
   * into the store's tiers, and emits the observational deltas for what changed since the last
   * reconcile. Idempotent: running it twice with an unchanged substrate emits nothing.
   */
  async reconcile(opts?: ExecOptions): Promise<void> {
    const [zmxSessions, windows, workspaceResult] = await Promise.all([
      this.zmx.listShort(opts).catch(() => []),
      this.kitty.ls(opts).catch(() => [] as KittyWindow[]),
      this.workspaceSource.listWithFocus(opts).catch(() => ({ records: [] as WorkspaceRecord[], focusedId: null as string | null })),
    ]);
    const workspaces = workspaceResult.records;
    const niriFocusedId = workspaceResult.focusedId;

    this.reconcileWorkspaces(workspaces);

    // The zmx names in scope define the session universe tally is allowed to observe.
    const scopedSessions = new Set(zmxSessions.map((s) => s.name).filter((n) => this.inScope(n)));

    // Join every kitty window to a session; build the desired pane set.
    const desiredPanes = new Map<string, PaneRecord>();
    const sessionsWithPanes = new Set<string>();
    const defaultWorkspaceId = workspaces[0]?.id ?? "default";

    for (const w of windows) {
      const sessionName = this.sessionOfWindow(w, scopedSessions);
      if (sessionName === null) continue; // not placeable under an in-scope zmx session — not ours.
      const paneShort = w.user_vars[PANE_USER_VAR] ?? `p${w.id}`;
      const paneId = makePaneId(sessionName, paneShort);
      const isViewer = this.viewerWindows.has(w.id);
      const worktree = w.user_vars.tally_worktree;
      const record: PaneRecord = {
        id: paneId,
        session_id: sessionName,
        kitty_window_id: w.id,
        cwd: w.cwd.length > 0 ? w.cwd : null,
        agent_id: this.store.getPane(paneId)?.agent_id ?? null,
        is_viewer: isViewer,
      };
      if (worktree !== undefined) record.worktree = worktree;
      desiredPanes.set(paneId, record);
      sessionsWithPanes.add(sessionName);
    }

    // Reconcile sessions (tier 2): a session enters the tree the first time it has an observed pane.
    for (const sessionName of sessionsWithPanes) {
      this.ensureSession(sessionName, defaultWorkspaceId);
    }

    // Reconcile panes (tier 3): create/update the desired set, close the vanished ones.
    this.reconcilePanes(desiredPanes);

    // A session whose last pane vanished leaves tally's view.
    this.reconcileSessionEnds(sessionsWithPanes, defaultWorkspaceId);

    // Focus: the focused kitty window's pane, its session, and the focused workspace.
    this.reconcileFocus(windows, workspaces, desiredPanes, niriFocusedId);
  }

  // ---- Workspace tier (tier 1) ---------------------------------------------------------------

  private reconcileWorkspaces(workspaces: WorkspaceRecord[]): void {
    const desired = workspaces.length > 0 ? workspaces : [];
    const desiredIds = new Set(desired.map((w) => w.id));
    for (const ws of desired) {
      // Preserve a previously-computed focused_session (discovery stamps it on focus edges).
      const existing = this.store.getWorkspace(ws.id);
      const focused_session = existing?.focused_session ?? ws.focused_session;
      this.store.upsertWorkspace({ id: ws.id, label: ws.label, focused_session });
    }
    // Remove workspaces niri no longer reports (only when niri actually returned a set).
    if (desired.length > 0) {
      for (const ws of this.store.listWorkspaces()) {
        if (!desiredIds.has(ws.id)) this.store.removeWorkspace(ws.id);
      }
    }
  }

  // ---- Session tier (tier 2) -----------------------------------------------------------------

  private ensureSession(sessionName: string, workspaceId: string): void {
    if (this.store.hasSession(sessionName)) return;
    let observedAt = this.observedAt.get(sessionName);
    if (observedAt === undefined) {
      observedAt = this.clock.nowIso();
      this.observedAt.set(sessionName, observedAt);
    }
    const session: SessionRecord = {
      id: sessionName,
      workspace_id: workspaceId,
      persistence_session_id: sessionName,
      backend: "zmx",
      observed_at: observedAt,
      pane_ids: [],
      status_rollup: { blocked: 0, working: 0, done: 0, idle: 0 },
    };
    this.store.upsertSession(session);
    this.bus.emit("session.observed", {
      session_id: sessionName,
      workspace_id: workspaceId,
      persistence_session_id: sessionName,
      backend: "zmx",
      observed_at: observedAt,
    });
  }

  private reconcileSessionEnds(liveSessions: Set<string>, workspaceId: string): void {
    for (const session of this.store.listSessions()) {
      if (liveSessions.has(session.id)) continue;
      // No live pane in this session — it left tally's view.
      this.store.removeSession(session.id);
      this.observedAt.delete(session.id);
      this.bus.emit("session.ended", {
        session_id: session.id,
        workspace_id: session.workspace_id || workspaceId,
        reason: "no_panes",
      });
    }
  }

  // ---- Pane tier (tier 3) --------------------------------------------------------------------

  private reconcilePanes(desired: Map<string, PaneRecord>): void {
    // Close panes that vanished.
    for (const pane of this.store.listPanes()) {
      if (!desired.has(pane.id)) {
        this.store.removePane(pane.id);
        this.bus.emit("pane.closed", {
          pane_id: pane.id,
          session_id: pane.session_id,
          reason: "window_closed",
        });
      }
    }
    // Create/update the desired panes.
    for (const pane of desired.values()) {
      const existing = this.store.getPane(pane.id);
      this.store.upsertPane(pane);
      if (!existing) {
        const payload: {
          pane_id: string;
          session_id: string;
          kitty_window_id: number;
          cwd: string | null;
          is_viewer: boolean;
          worktree?: string | null;
        } = {
          pane_id: pane.id,
          session_id: pane.session_id,
          kitty_window_id: pane.kitty_window_id,
          cwd: pane.cwd,
          is_viewer: pane.is_viewer,
        };
        if (pane.worktree !== undefined) payload.worktree = pane.worktree;
        this.bus.emit("pane.created", payload);
      }
    }
  }

  // ---- Focus ---------------------------------------------------------------------------------

  private reconcileFocus(
    windows: KittyWindow[],
    workspaces: WorkspaceRecord[],
    desiredPanes: Map<string, PaneRecord>,
    niriFocusedId: string | null,
  ): void {
    const prev = this.store.getFocus();

    // Focused workspace: niri's focused panel (carried out-of-band from parseNiriWorkspaces, since the
    // frozen §2.2 WorkspaceRecord has no is_focused field) when it reported one AND that workspace is
    // in the current set; else keep the prior focus (or seed from workspaces[0] on first reconcile).
    let focusedWorkspace = prev.workspace;
    if (niriFocusedId !== null && workspaces.some((w) => w.id === niriFocusedId)) {
      focusedWorkspace = niriFocusedId;
    } else if (workspaces.length > 0 && (prev.workspace === null || !workspaces.some((w) => w.id === prev.workspace))) {
      focusedWorkspace = workspaces[0]!.id;
    }
    if (focusedWorkspace !== prev.workspace) {
      this.store.setFocus({ workspace: focusedWorkspace });
      this.bus.emit("workspace.focused", {
        workspace_id: focusedWorkspace ?? "",
        prev_workspace_id: prev.workspace,
      });
    }

    // Focused pane: the focused kitty window's pane (if it is in the tree).
    const focusedWindow = windows.find((w) => w.is_focused);
    let focusedPaneId: string | null = null;
    let focusedSessionId: string | null = null;
    if (focusedWindow) {
      for (const pane of desiredPanes.values()) {
        if (pane.kitty_window_id === focusedWindow.id) {
          focusedPaneId = pane.id;
          focusedSessionId = pane.session_id;
          break;
        }
      }
    }
    if (focusedPaneId !== prev.pane) {
      this.store.setFocus({ pane: focusedPaneId, session: focusedSessionId });
      if (focusedPaneId !== null) {
        const workspaceId = focusedWorkspace ?? this.store.getSession(focusedSessionId ?? "")?.workspace_id ?? "";
        this.bus.emit("pane.focused", {
          pane_id: focusedPaneId,
          session_id: focusedSessionId ?? "",
          workspace_id: workspaceId,
          prev_pane_id: prev.pane,
        });
        // Stamp the session as the workspace's focused session (tier-1 hint).
        if (focusedSessionId !== null && focusedWorkspace !== null) {
          const ws = this.store.getWorkspace(focusedWorkspace);
          if (ws && ws.focused_session !== focusedSessionId) {
            this.store.upsertWorkspace({ ...ws, focused_session: focusedSessionId });
          }
        }
      }
    }
  }

  // ---- Window → session join -----------------------------------------------------------------

  /**
   * Resolve the zmx session a kitty window belongs to, honoring the `sessions` scope. Precedence:
   *   1. the explicit `tally_session` user-var, if it names an in-scope enumerated zmx session;
   *   2. otherwise, if the window's title exactly equals an in-scope zmx session name, that session
   *      (the dotfiles convention titles the terminal with its session name);
   * else `null` — the window is not placeable under an enumerated zmx session and is not observed.
   */
  private sessionOfWindow(w: KittyWindow, scoped: Set<string>): string | null {
    const declared = w.user_vars[SESSION_USER_VAR];
    if (declared !== undefined && scoped.has(declared)) return declared;
    if (declared !== undefined && this.sessionMatchers === null && declared.length > 0) {
      // No scope configured (`[]` = observe all) and the window declares a session zmx did not
      // enumerate (e.g. a race between the watcher edge and `zmx list`): trust the declaration.
      return declared;
    }
    if (scoped.has(w.title)) return w.title;
    return null;
  }
}
