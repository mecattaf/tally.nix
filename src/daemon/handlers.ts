// tally — the daemon-side handlers for the nine read/write RPC carriers the frozen §1 inventory
// requires but which no single layer-2 module owns end to end (IMPLEMENTATION-PLAN §3 RPC inventory;
// CLI-SURFACE §1.3, §1.4, §1.5):
//
//   pane.send / pane.send_key / pane.focus / pane.capture   — the kitty-native binding (KittyRc)
//   agent.list / agent.get / agent.read                     — read-projections of the detector loop
//   query.status                                            — pls pool depth × store sessions
//   query.render                                            — the grouping-tier projection over the store
//
// These are wired by the composition root (`src/compose.ts`), which owns the seams (the model store
// for selector→kitty_window_id resolution, the detector loop for agent records, the pls lease manager
// + pools for pool depth). This module is a pure function of those seams so it is testable without a
// socket. `agent.explain` (detector), `agent.hook_event` (detector), and the `pane`/`agent` selector
// resolution all key on the never-conflated three-key grammar (CLI-SURFACE §0).
//
// `agent send` / `agent focus` route through `pane.send` / `pane.focus` from the CLI, so the pane
// handlers accept BOTH a `<session>:<pane>` composite / bare pane token AND an `agent_id` selector and
// resolve either to the one `kitty_window_id` (the daemon holds the live model). `agent wait` routes
// through `session.wait` (daemon-core), not here.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { AgentRecord } from "../contracts/agent";
import { AGENT_STATUSES, AGENT_KINDS } from "../contracts/agent";
import type { Pool } from "../contracts/job";
import { TallyError, ValidationError, ViewerRejected } from "../contracts/errors";
import { parseSelector } from "../contracts/selectors";
import { PROTOCOL_VERSION } from "../contracts/constants";
import type { KittyRc, GetTextExtent } from "../kitty/rc";
import type { SessionStore } from "../model/store";
import type { PaneRecord } from "../contracts/snapshot";
import type { DetectorLoop } from "../detector/loop";
import type { LeaseManager } from "../pls/lease";
import type { PoolRegistry } from "../pls/pools";
import type { PlsBroker } from "../pls/broker";

/**
 * A read of the detector's live agent set. The composition root passes the detector loop's
 * `read()` (its `SnapshotSectionProvider<"agents">` projection) so this module never imports the loop
 * concretely for the list/get/read reads. `readText` performs the throttled `get-text` detection read.
 */
export interface AgentSource {
  /** Every currently-detected agent (the detector's `agents[]` leg). */
  agents(): AgentRecord[];
}

/** The seams the nine handlers need — supplied by the composition root. */
export interface HandlerDeps {
  store: SessionStore;
  kitty: KittyRc;
  detector: DetectorLoop | null;
  leases: LeaseManager;
  pools: PoolRegistry;
  broker: PlsBroker;
  /** Per-pool queue depth (the jobs engine's `queueDepth`). */
  queueDepth: (pool?: Pool) => number;
}

// ---------------------------------------------------------------------------------------------
// Selector resolution — the ONE resolver the daemon uses to turn a <sel> into a live pane.
// ---------------------------------------------------------------------------------------------

/**
 * Resolve a selector (a `<session>:<pane>` composite, an `agent_id`, or a bare session/pane token) to
 * the concrete pane record in the store. Throws `not_found` when nothing matches. An `agent_id`
 * resolves via the detector's agent→pane binding; a composite resolves by exact id; a bare token
 * resolves against pane ids (exact, then suffix short-name) then falls back to a single-pane session.
 */
export function resolvePane(deps: HandlerDeps, raw: string): PaneRecord {
  const sel = parseSelector(raw);

  if (sel.kind === "agent") {
    const agent = findAgent(deps, sel.agent_id);
    if (!agent) throw new TallyError("not_found", `no agent for selector "${raw}"`, { selector: raw });
    const pane = deps.store.getPane(agent.pane_id);
    if (!pane) throw new TallyError("not_found", `agent "${agent.id}" has no observed pane`, { selector: raw });
    return pane;
  }

  if (sel.kind === "pane") {
    const pane = deps.store.getPane(sel.raw.trim());
    if (pane) return pane;
    throw new TallyError("not_found", `no pane "${sel.raw.trim()}" in the live model`, { selector: raw });
  }

  // Bare token: try an exact pane id, then a pane short-name suffix, then a single-pane session, then
  // an agent id without the `ag_` prefix (an `agent:<kind>:<pane>` detector id passed bare).
  const token = sel.token;
  const exact = deps.store.getPane(token);
  if (exact) return exact;

  const panes = deps.store.listPanes();
  const bySuffix = panes.filter((p) => p.id.endsWith(`:${token}`));
  if (bySuffix.length === 1) return bySuffix[0]!;

  const sessionPanes = panes.filter((p) => p.session_id === token && !p.is_viewer);
  if (sessionPanes.length === 1) return sessionPanes[0]!;

  const agent = findAgent(deps, token);
  if (agent) {
    const pane = deps.store.getPane(agent.pane_id);
    if (pane) return pane;
  }

  throw new TallyError("not_found", `selector "${raw}" did not resolve to a unique pane`, { selector: raw });
}

/** Find a detected agent by id (exact) or by pane binding. Returns undefined when the detector is off. */
function findAgent(deps: HandlerDeps, agentId: string): AgentRecord | undefined {
  if (!deps.detector) {
    // Fall back to the store's agent leg (the Bus mirror) when the detector is not mounted.
    return deps.store.readAgents().find((a) => a.id === agentId);
  }
  return deps.detector.read().find((a) => a.id === agentId);
}

function asObject(v: unknown): Record<string, unknown> {
  if (typeof v !== "object" || v === null || Array.isArray(v)) {
    throw new ValidationError("params must be an object", "params");
  }
  return v as Record<string, unknown>;
}

function reqString(v: unknown, name: string): string {
  if (typeof v !== "string" || v.length === 0) throw new ValidationError(`${name} must be a non-empty string`, name);
  return v;
}

// ---------------------------------------------------------------------------------------------
// pane.* — the kitty-native binding (keyed on kitty_window_id).
// ---------------------------------------------------------------------------------------------

/** `pane.send {pane, text}` — write text into the resolved pane via `kitty @ send-text`. */
export async function paneSend(deps: HandlerDeps, params: unknown): Promise<{ pane: string; kitty_window_id: number; sent: boolean }> {
  const p = asObject(params);
  const sel = reqString(p.pane, "pane");
  const text = typeof p.text === "string" ? p.text : "";
  const pane = resolvePane(deps, sel);
  await deps.kitty.sendText(pane.kitty_window_id, text);
  return { pane: pane.id, kitty_window_id: pane.kitty_window_id, sent: true };
}

/** `pane.send_key {pane, keys}` — send a named key/chord as its escape sequence via `@ send-text`. */
export async function paneSendKey(deps: HandlerDeps, params: unknown): Promise<{ pane: string; kitty_window_id: number; key: string; sent: boolean }> {
  const p = asObject(params);
  const sel = reqString(p.pane, "pane");
  const key = reqString(p.keys, "keys");
  const pane = resolvePane(deps, sel);
  await deps.kitty.sendKey(pane.kitty_window_id, key);
  return { pane: pane.id, kitty_window_id: pane.kitty_window_id, key, sent: true };
}

/** `pane.focus {pane}` — focus the resolved pane's kitty window (the tunnel-in affordance). */
export async function paneFocus(deps: HandlerDeps, params: unknown): Promise<{ pane: string; kitty_window_id: number; focused: boolean }> {
  const p = asObject(params);
  const sel = reqString(p.pane, "pane");
  const pane = resolvePane(deps, sel);
  await deps.kitty.focusWindow(pane.kitty_window_id);
  return { pane: pane.id, kitty_window_id: pane.kitty_window_id, focused: true };
}

/** Map the CLI's `--source` to the kitty `get-text` extent. `detection` refuses viewer panes (#4). */
function extentForSource(source: string): GetTextExtent {
  switch (source) {
    case "recent":
      return "last_cmd_output";
    case "detection":
    case "visible":
    default:
      return "screen";
  }
}

/** `pane.capture {pane, source, format, lines?}` — the throttled grid read via `kitty @ get-text`. */
export async function paneCapture(
  deps: HandlerDeps,
  params: unknown,
): Promise<{ pane: string; kitty_window_id: number; source: string; lines: number; text: string }> {
  const p = asObject(params);
  const sel = reqString(p.pane, "pane");
  const source = typeof p.source === "string" ? p.source : "visible";
  if (source !== "visible" && source !== "recent" && source !== "detection") {
    throw new ValidationError(`unknown source "${source}" (expected visible|recent|detection)`, "source");
  }
  const format = typeof p.format === "string" ? p.format : "text";
  if (format !== "text" && format !== "ansi") {
    throw new ValidationError(`unknown format "${format}" (expected text|ansi)`, "format");
  }
  const pane = resolvePane(deps, sel);
  // `--source detection` refuses a viewer pane (anti-loop invariant #4) — mirrors the detector.
  if (source === "detection" && pane.is_viewer) throw new ViewerRejected(pane.id);

  const text = await deps.kitty.getText(pane.kitty_window_id, {
    extent: extentForSource(source),
    ansi: format === "ansi",
  });
  let out = text;
  if (typeof p.lines === "number" && Number.isInteger(p.lines) && p.lines > 0) {
    const all = out.split("\n");
    out = all.slice(Math.max(0, all.length - p.lines)).join("\n");
  }
  const lineCount = out.length === 0 ? 0 : out.split("\n").length;
  return { pane: pane.id, kitty_window_id: pane.kitty_window_id, source, lines: lineCount, text: out };
}

// ---------------------------------------------------------------------------------------------
// agent.* — read-projections of the in-daemon detector (agent panes only; no `agent start`).
// ---------------------------------------------------------------------------------------------

/**
 * The one-row projection the CLI's `agent list` / `agent get` render over. Carries the FROZEN §1.4
 * documented fields — `session`, `cwd` (list) and `agent_session:{kind,value}`, `foreground_cwd` (get)
 * — joined from the agent's bound pane record, alongside the additive engine fields.
 */
interface AgentRow {
  id: string;
  session: string;
  pane: string;
  pane_id: string;
  kind: string;
  status: string;
  detector: string;
  session_ref: string | null;
  /** The frozen §1.4 `agent get` structured session ref (kind + value), or null when none. */
  agent_session: { kind: string; value: string } | null;
  persistence_session_id: string;
  cwd: string | null;
  foreground_cwd: string | null;
  since: string;
}

function projectAgent(a: AgentRecord, pane: PaneRecord | undefined): AgentRow {
  const cwd = pane?.cwd ?? null;
  return {
    id: a.id,
    session: a.session_id,
    pane: a.pane_id,
    pane_id: a.pane_id,
    kind: a.kind,
    status: a.status,
    detector: a.detector,
    session_ref: a.session_ref,
    agent_session: a.session_ref !== null ? { kind: a.kind, value: a.session_ref } : null,
    persistence_session_id: a.persistence_session_id,
    cwd,
    foreground_cwd: cwd,
    since: a.since,
  };
}

function liveAgents(deps: HandlerDeps): AgentRecord[] {
  return deps.detector ? deps.detector.read() : deps.store.readAgents();
}

/** `agent.list {status?, kind?}` — the detector's agent set, filtered. */
export function agentList(deps: HandlerDeps, params: unknown): AgentRow[] {
  let statusFilter: string | undefined;
  let kindFilter: string | undefined;
  if (params !== undefined && params !== null) {
    const p = asObject(params);
    if (p.status !== undefined) {
      const s = reqString(p.status, "status");
      if (!(AGENT_STATUSES as readonly string[]).includes(s)) {
        throw new ValidationError(`status must be one of ${AGENT_STATUSES.join("|")}`, "status");
      }
      statusFilter = s;
    }
    if (p.kind !== undefined) {
      const k = reqString(p.kind, "kind");
      if (!(AGENT_KINDS as readonly string[]).includes(k)) {
        throw new ValidationError(`kind must be one of ${AGENT_KINDS.join("|")}`, "kind");
      }
      kindFilter = k;
    }
  }
  return liveAgents(deps)
    .filter((a) => (statusFilter === undefined || a.status === statusFilter) && (kindFilter === undefined || a.kind === kindFilter))
    .map((a) => projectAgent(a, deps.store.getPane(a.pane_id)));
}

/** `agent.get {agent_id}` — one agent record in full (selector may be an id or a pane token). */
export function agentGet(deps: HandlerDeps, params: unknown): AgentRow {
  const p = asObject(params);
  const sel = reqString(p.agent_id, "agent_id");
  const agent = resolveAgent(deps, sel);
  return projectAgent(agent, deps.store.getPane(agent.pane_id));
}

/**
 * `agent.read {agent_id, source?, format?}` — the detection snapshot for an agent's pane (the throttled
 * `get-text` read scoped to the pane the agent occupies). Refuses viewer panes at the seam (#4).
 */
export async function agentRead(
  deps: HandlerDeps,
  params: unknown,
): Promise<{ pane: string; agent_id: string; source: string; text: string }> {
  const p = asObject(params);
  const sel = reqString(p.agent_id, "agent_id");
  const source = typeof p.source === "string" ? p.source : "detection";
  const format = typeof p.format === "string" ? p.format : "text";
  const agent = resolveAgent(deps, sel);
  const pane = deps.store.getPane(agent.pane_id);
  if (!pane) throw new TallyError("not_found", `agent "${agent.id}" has no observed pane`, { agent_id: sel });
  if (pane.is_viewer) throw new ViewerRejected(pane.id);
  const text = await deps.kitty.getText(pane.kitty_window_id, {
    extent: source === "recent" ? "last_cmd_output" : "screen",
    ansi: format === "ansi",
  });
  return { pane: pane.id, agent_id: agent.id, source, text };
}

/** Resolve a selector to a detected agent (by exact id, by pane binding, or by bare pane token). */
function resolveAgent(deps: HandlerDeps, raw: string): AgentRecord {
  const agents = liveAgents(deps);
  const direct = agents.find((a) => a.id === raw);
  if (direct) return direct;
  // Resolve the selector to a pane, then find the agent bound to that pane.
  const pane = resolvePane(deps, raw);
  const bound = agents.find((a) => a.pane_id === pane.id);
  if (bound) return bound;
  throw new TallyError("not_found", `no detected agent for selector "${raw}"`, { selector: raw });
}

// ---------------------------------------------------------------------------------------------
// query.status — per-pool lease/queue depth + protocol_version (the ping) × store sessions.
// ---------------------------------------------------------------------------------------------

interface QueryStatusResult {
  protocol_version: number;
  pools: Array<{
    pool: string;
    held: number;
    queued: number;
    budget: number;
    broker_queued?: number;
    diverged?: boolean;
  }>;
  sessions: Array<{ session: string; status_rollup: Record<string, number> }>;
}

/**
 * `query.status {pool?}` — the read-time join of the pls pool depth (held/queued/budget per pool) and
 * the store's per-session status rollup. Queries the broker for live held/budget; `queued` keeps its
 * frozen §1.5 meaning (the jobs engine's per-pool depth). `broker_queued` is an additive field — the
 * broker's own waiting-ticket count (`pls status`'s `queued`, BrokerPoolStatus) — surfaced alongside it
 * so a daemon/broker divergence (issue #5: `queued:2` engine-side vs `queued:93` broker-side during the
 * post-restart pool-deadlock incident) is visible in one command instead of silently swallowed.
 * `diverged:true` flags when the two disagree. Both are additive-optional (CLI-SURFACE §2.5: a breaking
 * change is only removing/renaming/narrowing a field), so this is not a protocol bump. On a broker read
 * failure `broker_queued`/`diverged` are omitted — the pool still reports (queued from the engine,
 * held/budget 0) so the ping never hard-fails on a transient pls hiccup.
 */
export async function queryStatus(deps: HandlerDeps, params: unknown): Promise<QueryStatusResult> {
  let poolFilter: string | undefined;
  if (params !== undefined && params !== null) {
    const p = asObject(params);
    if (p.pool !== undefined) poolFilter = reqString(p.pool, "pool");
  }

  const descriptors = deps.pools.all().filter((d) => poolFilter === undefined || d.name === poolFilter);
  const pools: QueryStatusResult["pools"] = [];
  for (const d of descriptors) {
    let held = 0;
    let budget = d.budgetGb;
    let brokerQueued: number | undefined;
    try {
      const status = await deps.broker.status(d.broker, d.name);
      held = status.held;
      budget = status.budget;
      brokerQueued = status.queued;
    } catch {
      // A transient broker read failure ⇒ report the engine's queue depth with a zero held/budget
      // seed rather than failing the whole ping (the pool row is still useful); broker_queued omitted.
    }
    const queued = deps.queueDepth(d.name);
    pools.push({
      pool: d.name,
      held,
      queued,
      budget,
      ...(brokerQueued !== undefined ? { broker_queued: brokerQueued, diverged: brokerQueued !== queued } : {}),
    });
  }

  const sessions = deps.store.listSessions().map((s) => ({
    session: s.id,
    status_rollup: { ...s.status_rollup } as Record<string, number>,
  }));

  return { protocol_version: PROTOCOL_VERSION, pools, sessions };
}

// ---------------------------------------------------------------------------------------------
// query.render — the Workspace→Session→Pane tree / status / ledger projection over the store.
// ---------------------------------------------------------------------------------------------

interface RenderPane {
  pane: string;
  kitty_window_id: number;
  agent: { kind: string; status: string } | null;
}
interface RenderSession {
  session: string;
  persistence_session_id: string;
  status_rollup: Record<string, number>;
  panes: RenderPane[];
}
interface RenderWorkspace {
  workspace: string;
  sessions: RenderSession[];
}

/**
 * `query.render {format?, scope?, collapse?}` — projects the single store into the grouping tree the
 * CLI renders (`text`/`tree`/`json`/`jsonl`). The daemon returns the canonical `workspaces[]` shape;
 * the CLI chooses the rendering. `scope=queue` returns the jobs leg; `scope=witness` returns nothing
 * daemon-side (the witness projection is the daemonless `query log` path) — the tree scope is default.
 */
export function queryRender(deps: HandlerDeps, params: unknown): { workspaces: RenderWorkspace[]; jobs?: unknown[]; scope: string } {
  let scope = "sessions";
  let collapse = false;
  if (params !== undefined && params !== null) {
    const p = asObject(params);
    if (p.scope !== undefined) scope = reqString(p.scope, "scope");
    if (p.collapse === true) collapse = true;
  }

  const agentsByPane = new Map<string, AgentRecord>();
  for (const a of liveAgents(deps)) agentsByPane.set(a.pane_id, a);

  const workspaces: RenderWorkspace[] = [];
  const byWorkspace = new Map<string, RenderSession[]>();
  for (const session of deps.store.listSessions()) {
    const panes: RenderPane[] = deps.store.panesOfSession(session.id).map((pane) => {
      const agent = agentsByPane.get(pane.id);
      return {
        pane: pane.id,
        kitty_window_id: pane.kitty_window_id,
        agent: agent ? { kind: agent.kind, status: agent.status } : null,
      };
    });
    const rec: RenderSession = {
      session: session.id,
      persistence_session_id: session.persistence_session_id,
      status_rollup: { ...session.status_rollup } as Record<string, number>,
      panes: collapse ? [] : panes,
    };
    const list = byWorkspace.get(session.workspace_id) ?? [];
    list.push(rec);
    byWorkspace.set(session.workspace_id, list);
  }

  const wsRecords = deps.store.listWorkspaces();
  const wsIds = wsRecords.length > 0 ? wsRecords.map((w) => w.id) : [...byWorkspace.keys()];
  for (const id of wsIds) {
    workspaces.push({ workspace: id, sessions: byWorkspace.get(id) ?? [] });
  }

  if (scope === "queue") {
    return { workspaces, jobs: deps.store.readJobs(), scope };
  }
  return { workspaces, scope };
}
