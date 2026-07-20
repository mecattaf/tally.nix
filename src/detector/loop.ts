// tally — the detector LOOP (IMPLEMENTATION-PLAN M2.3; CLI-SURFACE §3.3, §0, §2.3).
//
// The in-daemon SUPERVISED thread that classifies exactly `blocked|working|done|idle` and drives the
// agent spine. It composes:
//   - a pane REGISTRY built from the Bus (`pane.created`/`pane.closed`/`pane.focused` from
//     session-model) — the detector never imports the store; it learns panes off the wire and
//     honors `is_viewer` (anti-loop invariant #4);
//   - STRATEGY 1 (hook, AUTHORITATIVE): `agent.hook_event` frames update status + gate the scraper;
//   - STRATEGY 2 (scrape, UNIVERSAL FALLBACK): the OSC fast path (`kitty @ ls`) then the throttled
//     grid read (`kitty @ get-text` via the shared ReadThrottle) against the per-kind manifest;
//   - the internal `unknown` collapse to last-known / `idle` (never reaches the wire, §0);
//   - the emission of `agent.detected` / `agent.status_changed` (the SPINE) + the convenience frames
//     `agent.blocked`/`agent.done` + `agent.released`, AND `pane.output_matched` (detector is the
//     SOLE emitter) for both scrape matches and `WaitScrapeProvider`-fulfilled `session.wait` reads;
//   - the `SnapshotSectionProvider<"agents">` (the store reads the `agents[]` leg at assembly) and
//     the `WaitScrapeProvider` (daemon-core/wait.ts consumes its match as the wait result).
//
// It rides `supervise.ts` via a `SupervisedLoop` — a crash in the loop restarts the loop, not the
// daemon (restart isolation, PS#15a).

import type { Bus, SupervisedLoop, WaitScrapeProvider, PaneOutputWaitRequest, PaneOutputWaitResult } from "../contracts/bus";
import type { Clock, Exec } from "../contracts/exec";
import type { AgentKind, AgentRecord, AgentStatus, DetectorStrategy, InternalAgentStatus } from "../contracts/agent";
import type { SnapshotSectionProvider } from "../contracts/snapshot";
import type { PaneRead } from "../contracts/events";
import type { DetectorConfig } from "../contracts/config";
import { TallyError, ViewerRejected } from "../contracts/errors";
import { FRAME_CAP } from "../contracts/constants";
import { KittyRc } from "../kitty/rc";
import { ReadThrottle } from "../kitty/throttle";
import type { Manifest, Rule } from "./manifest";
import { classify, classifyOscFastPath } from "./classify";
import { ExplainStore, type ExplainRecord } from "./records";
import {
  lifecycleToStatus,
  turnGate,
  notificationImpliesBlocked,
  type HookEventParams,
} from "./hooks";

/** One pane the detector tracks — learned from the Bus, keyed by pane composite id. */
interface PaneEntry {
  pane_id: string;
  session_id: string;
  kitty_window_id: number;
  /** The zmx handle (from `session.observed`) — carried into `agent.detected`. */
  persistence_session_id: string;
  is_viewer: boolean;
  /** The classified agent kind (from a hook, or inferred at first scrape). null until known. */
  kind: AgentKind | null;
  /** The last four-state status on the wire (drives the `unknown` collapse + rollup cadence). */
  status: AgentStatus | null;
  /** The detector's own agent id for this pane (stable for the pane's agent lifetime). */
  agent_id: string | null;
  /** The strategy behind the last decision. */
  strategy: DetectorStrategy;
  /** Whether a turn is currently OPEN (scraper runs at active cadence). Hooks gate this. */
  turnOpen: boolean;
  /** The harness resume ref (from a hook), carried into detected + recover(). */
  session_ref: string | null;
  /** Whether `agent.detected` has already fired for this agent. */
  announced: boolean;
  /** ISO timestamp of the current status's onset. */
  since: string;
}

/**
 * The kind→manifest map the detector classifies against. Injected (loaded from `manifests/*.toml` at
 * boot by the composition root / index). `shell` has no manifest (flag 5: shell derives status from
 * job/process state, not the grid — the detector never scrapes a shell pane).
 */
export type ManifestSet = Partial<Record<AgentKind, Manifest>>;

/**
 * A resolver the loop uses to learn a pane's `is_viewer` + identity when a hook arrives before the
 * pane-registry Bus event, or to re-resolve a `kitty_window_id` to a pane. Optional; when absent the
 * loop relies solely on the Bus-fed registry.
 */
export interface PaneResolver {
  byWindowId(kittyWindowId: number): PaneEntry | undefined;
}

export interface DetectorLoopOptions {
  exec: Exec;
  bus: Bus;
  clock: Clock;
  manifests: ManifestSet;
  cadence: DetectorConfig;
  /** Injectable kitty binary override (tests). */
  kittyBin?: string;
}

/** How the loop names an agent for a pane — stable within the pane's agent lifetime. */
function agentIdFor(paneId: string, kind: AgentKind): string {
  return `agent:${kind}:${paneId}`;
}

/**
 * The detector loop. Constructed once per daemon; `mount`ed via `index.ts`. Exposes the poll tick
 * (driven by the supervise host / an interval), the hook-event handler, the `WaitScrapeProvider`, the
 * `SnapshotSectionProvider<"agents">`, and the `agent.explain` read.
 */
export class DetectorLoop implements SupervisedLoop, WaitScrapeProvider, SnapshotSectionProvider<"agents"> {
  readonly name = "detector";
  readonly section = "agents" as const;

  private readonly bus: Bus;
  private readonly clock: Clock;
  private readonly manifests: ManifestSet;
  private readonly cadence: DetectorConfig;
  private readonly rc: KittyRc;
  private readonly throttle: ReadThrottle;
  private readonly explain = new ExplainStore();

  private readonly panes = new Map<string, PaneEntry>();
  private readonly byWindow = new Map<number, string>();

  private ticking = false;
  private cancelInterval: (() => void) | null = null;
  private readonly unsubscribers: Array<() => void> = [];
  /** Set by stop(): in-flight `awaitPaneOutput` loops observe it and abort so the daemon can exit. */
  private closed = false;

  constructor(opts: DetectorLoopOptions) {
    this.bus = opts.bus;
    this.clock = opts.clock;
    this.manifests = opts.manifests;
    this.cadence = opts.cadence;
    this.rc = new KittyRc(opts.exec, opts.kittyBin);
    // The loop's grid reads go through the SHARED read budget (M1.6): one poll per window per cadence,
    // coalesced with `pane capture`. The reader is bound to KittyRc.getText.
    this.throttle = new ReadThrottle((windowId) => this.rc.getText(windowId), opts.clock, opts.cadence);
  }

  // -------------------------------------------------------------------------------------------
  // SupervisedLoop — start/stop under the supervise host (restart isolation, PS#15a).
  // -------------------------------------------------------------------------------------------

  start(): void {
    this.wireBus();
    // A fixed fallback poll (flag 4): the watcher edge is the primary trigger, but a periodic tick
    // guarantees liveness even without an edge. The per-window throttle enforces the real cadence.
    if (this.cancelInterval === null) {
      const base = Math.max(250, Math.min(this.cadence.working_poll_ms, this.cadence.idle_poll_ms));
      this.cancelInterval = this.clock.setInterval(base, () => {
        // CATCH the tick rejection: a sensor failure (kitty restart, socket hiccup, a window closing
        // mid-read) throws out of tick() into this VOIDED promise; an unhandled rejection from a timer
        // callback terminates the whole Bun process (defeating the supervisor's restart isolation).
        void this.tick().catch((err: unknown) => {
          process.stderr.write(`tally[detector]: tick failed (degraded, daemon survives): ${err instanceof Error ? err.message : String(err)}\n`);
        });
      });
    }
  }

  stop(): void {
    this.closed = true;
    if (this.cancelInterval) {
      this.cancelInterval();
      this.cancelInterval = null;
    }
    for (const un of this.unsubscribers.splice(0)) un();
  }

  private wireBus(): void {
    if (this.unsubscribers.length > 0) return; // already wired
    this.unsubscribers.push(
      this.bus.on("pane.created", (p) => this.onPaneCreated(p.pane_id, p.session_id, p.kitty_window_id, p.is_viewer)),
      this.bus.on("pane.closed", (p) => this.onPaneClosed(p.pane_id, p.reason)),
      this.bus.on("session.observed", (p) => this.onSessionObserved(p.session_id, p.persistence_session_id)),
      this.bus.on("session.ended", (p) => this.onSessionEnded(p.session_id)),
    );
  }

  // -------------------------------------------------------------------------------------------
  // Pane registry (learned from the Bus — the detector never imports the store).
  // -------------------------------------------------------------------------------------------

  private onPaneCreated(paneId: string, sessionId: string, kittyWindowId: number, isViewer: boolean): void {
    const existing = this.panes.get(paneId);
    if (existing) {
      existing.kitty_window_id = kittyWindowId;
      existing.is_viewer = isViewer;
      this.byWindow.set(kittyWindowId, paneId);
      return;
    }
    const entry: PaneEntry = {
      pane_id: paneId,
      session_id: sessionId,
      kitty_window_id: kittyWindowId,
      persistence_session_id: this.sessionHandle(sessionId),
      is_viewer: isViewer,
      kind: null,
      status: null,
      agent_id: null,
      strategy: "scrape",
      turnOpen: true, // no hook yet ⇒ scrape at active cadence until a `Stop` gate closes it
      session_ref: null,
      announced: false,
      since: this.clock.nowIso(),
    };
    this.panes.set(paneId, entry);
    this.byWindow.set(kittyWindowId, paneId);
  }

  private onPaneClosed(paneId: string, _reason: string): void {
    const entry = this.panes.get(paneId);
    if (!entry) return;
    if (entry.agent_id) {
      this.bus.emit("agent.released", {
        agent_id: entry.agent_id,
        pane_id: entry.pane_id,
        session_id: entry.session_id,
        reason: "pane_closed",
      });
      this.explain.forget(entry.agent_id);
    }
    this.byWindow.delete(entry.kitty_window_id);
    this.throttle.forget(entry.kitty_window_id);
    this.panes.delete(paneId);
  }

  private readonly sessionHandles = new Map<string, string>();

  private onSessionObserved(sessionId: string, persistenceSessionId: string): void {
    this.sessionHandles.set(sessionId, persistenceSessionId);
    for (const entry of this.panes.values()) {
      if (entry.session_id === sessionId) entry.persistence_session_id = persistenceSessionId;
    }
  }

  private onSessionEnded(sessionId: string): void {
    for (const [paneId, entry] of [...this.panes.entries()]) {
      if (entry.session_id === sessionId) this.onPaneClosed(paneId, "session_ended");
    }
    this.sessionHandles.delete(sessionId);
  }

  private sessionHandle(sessionId: string): string {
    return this.sessionHandles.get(sessionId) ?? sessionId;
  }

  // -------------------------------------------------------------------------------------------
  // Strategy 1 — the `agent.hook_event` RPC handler (AUTHORITATIVE).
  // -------------------------------------------------------------------------------------------

  /**
   * Apply a validated hook event. Locates the pane (by `pane_id` or `kitty_window_id`), updates the
   * turn gate + status (hook is authoritative), and carries the resume ref. A hook for a viewer pane
   * is a no-op (the detector never classifies a viewer, anti-loop #4).
   */
  applyHookEvent(params: HookEventParams): void {
    const entry = this.locatePane(params);
    if (!entry || entry.is_viewer) return;

    if (entry.kind === null) entry.kind = params.kind;
    if (params.session_ref !== undefined) entry.session_ref = params.session_ref;

    // Turn gating (CLI-SURFACE §3.3): `UserPromptSubmit` opens, `Stop` closes.
    if (params.turn) {
      const gate = turnGate(params.turn);
      if (gate === "open") entry.turnOpen = true;
      else if (gate === "close") entry.turnOpen = false;
    }

    // Status from lifecycle, or from a Notification (needs-input) turn event.
    let status: InternalAgentStatus | null = null;
    if (params.lifecycle) status = lifecycleToStatus(params.lifecycle);
    else if (params.turn === "Notification" && notificationImpliesBlocked()) status = "blocked";
    else if (params.turn === "Stop") status = "done";
    else if (params.turn === "UserPromptSubmit") status = "working";

    if (status !== null) {
      this.commitStatus(entry, status, "hook", { manifest: null, matchedRule: null });
    }
  }

  private locatePane(params: HookEventParams): PaneEntry | undefined {
    if (params.pane_id) {
      const byId = this.panes.get(params.pane_id);
      if (byId) return byId;
    }
    if (params.kitty_window_id !== undefined) {
      const paneId = this.byWindow.get(params.kitty_window_id);
      if (paneId) return this.panes.get(paneId);
    }
    return undefined;
  }

  // -------------------------------------------------------------------------------------------
  // Strategy 2 — the throttled scrape poll (UNIVERSAL FALLBACK).
  // -------------------------------------------------------------------------------------------

  /**
   * One poll tick: classify every due, non-viewer agent pane. Re-entrant-guarded (a slow read never
   * overlaps its own next tick). Viewer panes are NEVER read (anti-loop #4). Shell panes have no
   * manifest and are never scraped (flag 5).
   */
  async tick(): Promise<void> {
    if (this.ticking) return;
    this.ticking = true;
    try {
      for (const entry of this.panes.values()) {
        if (entry.is_viewer) continue;
        await this.pollPane(entry);
      }
    } finally {
      this.ticking = false;
    }
  }

  private async pollPane(entry: PaneEntry): Promise<void> {
    const kind = entry.kind;
    // Shell (or an unknown kind with no manifest) is not grid-classified (flag 5).
    if (kind === "shell") return;

    // A turn that a hook explicitly closed ⇒ do not scrape (the last hook status stands). If no hook
    // has ever spoken (kind still null) we may not know the manifest; try to infer the kind below.
    const manifest = kind ? this.manifests[kind] : undefined;
    if (!manifest) {
      // No known manifest yet: attempt kind inference by trying each manifest's OSC fast path against
      // the `@ ls` record (cheap, no grid read). If one matches, adopt that kind.
      await this.inferKindAndClassify(entry);
      return;
    }

    // The scraper is gated to active turns for hooked panes: if the hook closed the turn AND the last
    // decision was hook-authoritative, skip the grid read (the settled state stands).
    if (!entry.turnOpen && entry.strategy === "hook") return;

    // Update the throttle's cadence selector from the current status.
    if (entry.status) this.throttle.setStatus(entry.kitty_window_id, entry.status);

    // --- OSC fast path (zero-latency, `@ ls` only, checked FIRST). ---
    const window = await this.lsWindow(entry.kitty_window_id);
    if (window) {
      const fast = classifyOscFastPath(manifest, window);
      if (fast.status !== "unknown" && fast.matchedRule) {
        this.commitStatus(entry, fast.status, "scrape", { manifest, matchedRule: fast.matchedRule });
        return;
      }
    }

    // --- Grid read (throttled). Only when due, so the cadence budget is respected. ---
    if (!this.throttle.isDue(entry.kitty_window_id)) return;
    // A failed read (kitty down / socket hiccup / the window just closed) must DEGRADE, not throw out
    // of the poll and (via the voided tick) kill the daemon: swallow it and leave the last status
    // standing — the next tick retries once kitty answers again.
    let read: import("../kitty/throttle").ThrottledRead;
    try {
      read = await this.throttle.read(entry.kitty_window_id);
    } catch (err) {
      process.stderr.write(`tally[detector]: read of window ${entry.kitty_window_id} failed (skipping this tick): ${err instanceof Error ? err.message : String(err)}\n`);
      return;
    }
    const win = window ?? (await this.lsWindow(entry.kitty_window_id));
    const result = classify(manifest, read.text, win ?? this.stubWindow(entry.kitty_window_id));
    this.commitStatus(entry, result.status, "scrape", { manifest, matchedRule: result.matchedRule });
  }

  private async inferKindAndClassify(entry: PaneEntry): Promise<void> {
    const window = await this.lsWindow(entry.kitty_window_id);
    if (!window) return;
    for (const [k, manifest] of Object.entries(this.manifests) as Array<[AgentKind, Manifest]>) {
      const fast = classifyOscFastPath(manifest, window);
      if (fast.status !== "unknown" && fast.matchedRule) {
        entry.kind = k;
        this.commitStatus(entry, fast.status, "scrape", { manifest, matchedRule: fast.matchedRule });
        return;
      }
    }
    // No OSC signal to infer kind: leave the pane unclassified until a hook arrives or a grid rule
    // in a later tick (once a kind is known) fires. The pane stays absent from `agents[]`.
  }

  private async lsWindow(kittyWindowId: number): Promise<import("../kitty/rc").KittyWindow | undefined> {
    try {
      const windows = await this.rc.ls();
      return windows.find((w) => w.id === kittyWindowId);
    } catch {
      return undefined;
    }
  }

  private stubWindow(kittyWindowId: number): import("../kitty/rc").KittyWindow {
    return {
      id: kittyWindowId,
      is_focused: false,
      is_active: false,
      title: "",
      cwd: "",
      foreground_processes: [],
      user_vars: {},
      tab_id: 0,
      os_window_id: 0,
    };
  }

  // -------------------------------------------------------------------------------------------
  // Status commit — the `unknown` collapse + the SPINE emission.
  // -------------------------------------------------------------------------------------------

  private commitStatus(
    entry: PaneEntry,
    raw: InternalAgentStatus,
    strategy: DetectorStrategy,
    evidence: { manifest: Manifest | null; matchedRule: Rule | null },
  ): void {
    // `unknown` never reaches the wire (CLI-SURFACE §0): collapse to last-known, or `idle` at first sight.
    const resolved: AgentStatus = raw === "unknown" ? (entry.status ?? "idle") : raw;

    if (entry.kind === null) {
      // A hook already set kind, or inference set it; if still null (grid-only path with no kind),
      // we cannot announce an agent — bail. This is unreachable for the grid path (kind is required
      // to select a manifest) and for the hook path (kind is set on arrival).
      return;
    }

    if (entry.agent_id === null) entry.agent_id = agentIdFor(entry.pane_id, entry.kind);
    const prev = entry.status;
    const changed = prev !== resolved;
    const now = this.clock.nowIso();

    entry.strategy = strategy;

    // Record explain-data every decision (matched rule / manifest / strategy).
    const explainRec: ExplainRecord = {
      agent_id: entry.agent_id,
      pane_id: entry.pane_id,
      status: resolved,
      strategy,
      manifest: evidence.manifest ? { kind: evidence.manifest.kind, version: evidence.manifest.version } : null,
      matched_rule: evidence.matchedRule ? { id: evidence.matchedRule.id, priority: evidence.matchedRule.priority } : null,
      region: evidence.matchedRule ? evidence.matchedRule.region : null,
      at: now,
    };
    this.explain.put(explainRec);

    // First identification for this agent (once per agent per pane lifetime).
    if (!entry.announced) {
      entry.announced = true;
      entry.status = resolved;
      entry.since = now;
      this.bus.emit("agent.detected", {
        agent_id: entry.agent_id,
        pane_id: entry.pane_id,
        session_id: entry.session_id,
        kind: entry.kind,
        status: resolved,
        detector: strategy,
        persistence_session_id: entry.persistence_session_id,
        session_ref: entry.session_ref,
        kitty_window_id: entry.kitty_window_id,
      });
      // The `detected` frame carries the initial status; emit the transition spine only on a change
      // hereafter. But also emit the convenience frame at first sight if blocked/done.
      this.emitConvenience(entry, resolved, strategy, now, evidence);
      return;
    }

    if (!changed) return;

    entry.status = resolved;
    entry.since = now;
    const changePayload: Parameters<typeof this.bus.emit<"agent.status_changed">>[1] = {
      agent_id: entry.agent_id,
      pane_id: entry.pane_id,
      session_id: entry.session_id,
      status: resolved,
      detector: strategy,
      since: now,
    };
    if (prev !== null) changePayload.prev_status = prev;
    this.bus.emit("agent.status_changed", changePayload);
    this.emitConvenience(entry, resolved, strategy, now, evidence);
  }

  private emitConvenience(
    entry: PaneEntry,
    status: AgentStatus,
    strategy: DetectorStrategy,
    now: string,
    evidence: { matchedRule: Rule | null },
  ): void {
    if (!entry.agent_id) return;
    if (status === "blocked") {
      const blockedPayload: Parameters<typeof this.bus.emit<"agent.blocked">>[1] = {
        agent_id: entry.agent_id,
        pane_id: entry.pane_id,
        session_id: entry.session_id,
        detector: strategy,
        since: now,
      };
      if (evidence.matchedRule) blockedPayload.reason = evidence.matchedRule.id;
      this.bus.emit("agent.blocked", blockedPayload);
    } else if (status === "done") {
      this.bus.emit("agent.done", {
        agent_id: entry.agent_id,
        pane_id: entry.pane_id,
        session_id: entry.session_id,
        detector: strategy,
        since: now,
      });
    }
  }

  // -------------------------------------------------------------------------------------------
  // SnapshotSectionProvider<"agents"> — the store reads the `agents[]` leg at assembly.
  // -------------------------------------------------------------------------------------------

  read(): AgentRecord[] {
    const out: AgentRecord[] = [];
    for (const entry of this.panes.values()) {
      if (entry.agent_id === null || entry.kind === null || entry.status === null) continue;
      out.push({
        id: entry.agent_id,
        pane_id: entry.pane_id,
        session_id: entry.session_id,
        kind: entry.kind,
        status: entry.status,
        detector: entry.strategy,
        persistence_session_id: entry.persistence_session_id,
        session_ref: entry.session_ref,
        job_id: null,
        since: entry.since,
      });
    }
    return out;
  }

  /** The `agent.explain` read (CLI-SURFACE §1.4) — surfaced by the `agent.explain` RPC handler. */
  explainAgent(agentId: string) {
    return this.explain.explain(agentId);
  }

  // -------------------------------------------------------------------------------------------
  // WaitScrapeProvider — daemon-core/wait.ts calls this for a `session.wait pane_output` read.
  // -------------------------------------------------------------------------------------------

  /**
   * Resolve when the pane's output matches `regex`. The detector performs the throttled `get-text`
   * read (it holds the read path), rejects `is_viewer` panes at the seam (anti-loop #4), and — as the
   * SOLE emitter of `pane.output_matched` — emits that event for the match before returning it. It
   * polls up to the deadline; a per-window read is throttled + coalesced with the scrape loop.
   */
  async awaitPaneOutput(req: PaneOutputWaitRequest): Promise<PaneOutputWaitResult> {
    const entry = this.panes.get(req.pane_id);
    if (!entry) {
      throw new TallyError("not_found", `pane ${req.pane_id} is not observed`, { pane_id: req.pane_id });
    }
    if (entry.is_viewer) throw new ViewerRejected(req.pane_id);

    const re = new RegExp(req.regex);
    const deadline = req.timeout_ms !== undefined ? this.clock.now() + req.timeout_ms : Infinity;
    const interval = Math.max(100, this.cadence.working_poll_ms);

    for (;;) {
      // Abort a no-timeout (deadline=Infinity) wait when the detector is stopping (daemon SIGTERM):
      // otherwise the sleep/read chain keeps the event loop alive forever and the daemon never exits
      // (systemd would SIGKILL it after TimeoutStopSec).
      if (this.closed) {
        throw new TallyError("unsupported", `pane ${req.pane_id} wait aborted: detector shutting down`, {
          pane_id: req.pane_id,
        });
      }
      const read = await this.throttle.forceRead(entry.kitty_window_id);
      const matched = this.matchLine(read.text, re);
      if (matched !== null) {
        return this.emitPaneOutputMatched(entry, matched, read.text, read.revision);
      }
      if (this.clock.now() >= deadline) {
        throw new TallyError("timeout", `pane ${req.pane_id} output did not match within deadline`, {
          pane_id: req.pane_id,
        });
      }
      await this.clock.sleep(interval);
    }
  }

  private matchLine(text: string, re: RegExp): string | null {
    for (const line of text.split("\n")) {
      if (re.test(line)) return line;
    }
    return null;
  }

  /**
   * Emit `pane.output_matched` (the detector is the SOLE emitter, M2.3) and return the result. Sets
   * `read.truncated=true` when the matched read hit the 64 KiB FRAME_CAP (the detector performs the
   * read, so it knows the framing budget).
   */
  private emitPaneOutputMatched(entry: PaneEntry, matchedLine: string, text: string, revision: number): PaneOutputWaitResult {
    const byteLen = Buffer.byteLength(text, "utf8");
    const truncated = byteLen >= FRAME_CAP;
    const payloadText = truncated ? this.truncateToFrameCap(text) : text;
    const read: PaneRead = {
      source: "detection",
      format: "text",
      text: payloadText,
      revision,
      truncated,
    };
    this.bus.emit("pane.output_matched", {
      pane_id: entry.pane_id,
      session_id: entry.session_id,
      matched_line: matchedLine,
      read,
    });
    return {
      pane_id: entry.pane_id,
      session_id: entry.session_id,
      matched_line: matchedLine,
      read,
    };
  }

  /** Truncate UTF-8 text so its byte length stays under FRAME_CAP (whole codepoints only). */
  private truncateToFrameCap(text: string): string {
    // Leave headroom for the surrounding JSON frame envelope; cap the text at FRAME_CAP bytes.
    let out = text;
    while (Buffer.byteLength(out, "utf8") >= FRAME_CAP && out.length > 0) {
      out = out.slice(0, Math.floor(out.length * 0.9) || out.length - 1);
    }
    return out;
  }

  /**
   * Emit `pane.output_matched` for a SCRAPE region+regex match found during the poll (the other path
   * that fires the event — a manifest rule may declare a `regex`/`line_regex` an operator watches).
   * Exposed for the loop's own use and tested directly. `is_viewer` panes are excluded by construction
   * (they are never polled).
   */
  emitScrapeMatch(paneId: string, matchedLine: string, text: string, revision: number): void {
    const entry = this.panes.get(paneId);
    if (!entry || entry.is_viewer) return;
    this.emitPaneOutputMatched(entry, matchedLine, text, revision);
  }

  // -------------------------------------------------------------------------------------------
  // Test / introspection accessors (detector-internal; not on the wire).
  // -------------------------------------------------------------------------------------------

  /** The current tracked status for a pane (test/introspection). */
  statusOf(paneId: string): AgentStatus | null {
    return this.panes.get(paneId)?.status ?? null;
  }

  /** The current detector strategy for a pane (test/introspection). */
  strategyOf(paneId: string): DetectorStrategy | null {
    return this.panes.get(paneId)?.strategy ?? null;
  }

  /** Whether a pane is currently tracked. */
  hasPane(paneId: string): boolean {
    return this.panes.has(paneId);
  }
}
