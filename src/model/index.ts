// tally — session-model barrel + the mountable composition (IMPLEMENTATION-PLAN M2.1). This assembles
// the single store, discovery join, workspace source, and snapshot provider into one `SessionModel`
// the composition root (`main.ts`) mounts onto daemon-core via the `DaemonMount` seam.
//
// What session-model owns and mounts:
//   • the ONE authoritative `SessionStore` (tree tiers + focus + the composed agents[]/jobs[] legs);
//   • the `SnapshotProvider` daemon-core calls for `session.snapshot` (assembles the §2.2 body);
//   • the `session.register_viewer {kitty_window_id}` RPC (a `session watch` client marks its own
//     pane is_viewer — anti-loop invariant #4);
//   • the `session.list {workspace?, short?}` RPC (the zmx-delegated enumeration join projection);
//   • a supervised reconcile loop that joins zmx × kitty × workspace into the store and emits the
//     observational deltas, refreshed on watcher edges and on a periodic cadence.
//
// session-model does NOT own the `agents[]`/`jobs[]` legs (the detector/jobs write them into the
// store via the Bus, single-store ruling); it composes them at snapshot time. It never imports its
// layer-2 siblings — the join is over the Bus + the section-provider seam.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

export { SessionStore } from "./store";
export type { StoreOptions } from "./store";
export {
  Discovery,
  SESSION_USER_VAR,
  PANE_USER_VAR,
  type DiscoveryOptions,
} from "./discovery";
export {
  WorkspaceSource,
  parseNiriWorkspaces,
  defaultWorkspace,
  NIRI_BIN,
} from "./workspace";
export {
  rollupForPanes,
  rollupForSessions,
  stampSessionRollups,
} from "./rollup";
export { SnapshotAssembler, makeSnapshotProvider } from "./snapshot";

import type { DaemonMount, DaemonModule, Bus, Unsubscribe } from "../contracts/bus";
import type { SnapshotProvider } from "../contracts/snapshot";
import type { Exec, Clock } from "../contracts/exec";
import { systemClock } from "../contracts/exec";
import type { TallyConfig } from "../contracts/config";
import { ValidationError } from "../contracts/errors";
import { rollupForSessions } from "./rollup";
import { SessionStore } from "./store";
import { Discovery } from "./discovery";
import { SnapshotAssembler } from "./snapshot";

/** Options for building the session-model composition. */
export interface SessionModelOptions {
  bus: Bus;
  exec: Exec;
  config: TallyConfig;
  clock?: Clock;
}

/** The `session.list` projection record (CLI-SURFACE §1.2 `--json` shape). */
export interface SessionListRecord {
  session: string;
  persistence_session_id: string;
  workspace: string;
  status_rollup: { blocked: number; working: number; done: number; idle: number };
  panes: Array<{
    pane: string;
    kitty_window_id: number;
    agent: { kind: string; status: string } | null;
  }>;
}

/**
 * The mountable session-model. Constructs the single store + discovery + snapshot provider from the
 * config, and exposes them plus the mount glue. The composition root:
 *   1. constructs it,
 *   2. calls `daemon.registerSnapshotProvider(model.snapshotProvider)` (the `Daemon` concrete method),
 *   3. calls `model.mount(daemon)` (the `DaemonMount` seam) to register the RPCs + watcher + loop,
 *   4. starts the daemon.
 */
export class SessionModel implements DaemonModule {
  readonly store: SessionStore;
  readonly discovery: Discovery;
  readonly snapshotProvider: SnapshotProvider;

  private readonly clock: Clock;
  private readonly config: TallyConfig;
  private legUnwire: Unsubscribe | null = null;
  private watcherUnwire: (() => void) | null = null;
  private reconcileTimerCancel: (() => void) | null = null;
  /** Resolver of the supervised reconcile loop's pending `start()` promise — settled only on stop. */
  private resolveReconcileDone: (() => void) | null = null;

  constructor(opts: SessionModelOptions) {
    this.clock = opts.clock ?? systemClock;
    this.config = opts.config;
    this.store = new SessionStore({
      bus: opts.bus,
      clock: this.clock,
      daemonVersion: opts.config.daemonVersion,
    });
    const discoveryOpts: ConstructorParameters<typeof Discovery>[0] = {
      store: this.store,
      bus: opts.bus,
      exec: opts.exec,
      sessions: opts.config.sessions,
      conductorHost: opts.config.conductorHost,
      clock: this.clock,
    };
    this.discovery = new Discovery(discoveryOpts);
    this.snapshotProvider = new SnapshotAssembler(this.store);
  }

  /**
   * Mount onto daemon-core (the `DaemonMount` seam). Registers the two RPC carriers session-model
   * owns and a supervised reconcile loop. The snapshot provider is registered separately by the
   * composition root via the concrete `Daemon.registerSnapshotProvider` (it is not part of the
   * `DaemonMount` interface — daemon-core owns that registration).
   */
  mount(daemon: DaemonMount): void {
    // Wire the store's agents[]/jobs[] leg mirror off the Bus.
    this.legUnwire = this.store.wireLegs();
    // Reconcile on watcher edges (event-driven refresh, replaces existence polling).
    this.watcherUnwire = this.discovery.wireWatcher();

    daemon.registerRpc("session.register_viewer", this.discovery.registerViewer);
    daemon.registerRpc("session.list", this.handleSessionList);

    daemon.registerSupervised({
      name: "session-model.reconcile",
      // A LONG-RUNNING loop: prime the tree once, install the periodic reconcile timer, and return a
      // promise that stays PENDING until `stop()`. It must NOT resolve while running — a supervised
      // `start()` that settles is re-invoked by the supervise host in an endless restart loop (and each
      // re-invocation here would leak another interval). Idempotent: a second start() while already
      // running installs nothing and shares the pending promise.
      start: () => {
        if (this.reconcileTimerCancel !== null) {
          return new Promise<void>((resolve) => {
            const prev = this.resolveReconcileDone;
            this.resolveReconcileDone = () => {
              prev?.();
              resolve();
            };
          });
        }
        const intervalMs = this.config.detector.idle_poll_ms;
        // Prime the tree once (fire-and-forget; first-read failures self-heal on the next tick), then
        // reconcile on the fixed fallback cadence (watcher edges also trigger reconciles between ticks).
        void this.discovery.reconcile().catch(() => {
          /* first reconcile read failures self-heal on the next tick. */
        });
        this.reconcileTimerCancel = this.clock.setInterval(intervalMs, () => {
          void this.discovery.reconcile().catch(() => {
            /* periodic reconcile never throws upward. */
          });
        });
        return new Promise<void>((resolve) => {
          this.resolveReconcileDone = resolve;
        });
      },
      stop: () => {
        if (this.reconcileTimerCancel) {
          this.reconcileTimerCancel();
          this.reconcileTimerCancel = null;
        }
        if (this.resolveReconcileDone) {
          this.resolveReconcileDone();
          this.resolveReconcileDone = null;
        }
      },
    });
  }

  /** Tear down the model's Bus/watcher subscriptions and reconcile timer (test/shutdown symmetry). */
  unmount(): void {
    if (this.reconcileTimerCancel) {
      this.reconcileTimerCancel();
      this.reconcileTimerCancel = null;
    }
    if (this.resolveReconcileDone) {
      this.resolveReconcileDone();
      this.resolveReconcileDone = null;
    }
    if (this.watcherUnwire) {
      this.watcherUnwire();
      this.watcherUnwire = null;
    }
    if (this.legUnwire) {
      this.legUnwire();
      this.legUnwire = null;
    }
  }

  /**
   * The `session.list {workspace?, short?}` RPC handler — the zmx-delegated enumeration join, as the
   * §1.2 one-record-per-session projection. `workspace` filters to one panel; `short` is honored by
   * the CLI's text projection (the JSON shape is identical), so it is validated but not shape-altering
   * here. Hand-rolled validation (no zod).
   */
  handleSessionList = (params: unknown): SessionListRecord[] => {
    let workspaceFilter: string | undefined;
    if (params !== undefined && params !== null) {
      if (typeof params !== "object" || Array.isArray(params)) {
        throw new ValidationError("session.list params must be an object", "params");
      }
      const p = params as Record<string, unknown>;
      if (p.workspace !== undefined) {
        if (typeof p.workspace !== "string") {
          throw new ValidationError("workspace must be a string", "workspace");
        }
        workspaceFilter = p.workspace;
      }
      if (p.short !== undefined && typeof p.short !== "boolean") {
        throw new ValidationError("short must be a boolean", "short");
      }
    }

    const agentsById = new Map(this.store.readAgents().map((a) => [a.id, a] as const));
    const records: SessionListRecord[] = [];
    for (const session of this.store.listSessions()) {
      if (workspaceFilter !== undefined && session.workspace_id !== workspaceFilter) continue;
      const panes = this.store.panesOfSession(session.id).map((pane) => {
        const agent = pane.agent_id !== null ? agentsById.get(pane.agent_id) : undefined;
        return {
          pane: pane.id,
          kitty_window_id: pane.kitty_window_id,
          agent: agent ? { kind: agent.kind, status: agent.status } : null,
        };
      });
      records.push({
        session: session.id,
        persistence_session_id: session.persistence_session_id,
        workspace: session.workspace_id,
        status_rollup: { ...session.status_rollup },
        panes,
      });
    }
    // Return the BARE array (§1.2: one record per session, no envelope) — the CLI iterates it directly.
    return records;
  };

  /** The workspace-level status rollup (a convenience projection for `query render`). */
  workspaceRollup(workspaceId: string): { blocked: number; working: number; done: number; idle: number } {
    const sessions = this.store.listSessions().filter((s) => s.workspace_id === workspaceId);
    return rollupForSessions(sessions);
  }
}
