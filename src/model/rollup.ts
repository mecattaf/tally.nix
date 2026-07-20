// tally — status-dot rollup aggregation up the Workspace → Session → Pane tier (PS#6b; CLI-SURFACE
// §0, §2.2 `status_rollup`). The snapshot's `sessions[].status_rollup` is a client aggregation hint:
// the count of agent statuses across the panes of a session. This file owns the ONE aggregation
// rule so `session list`, `query render`, and the §2.2 snapshot never disagree on how a rollup is
// computed.
//
// A pane contributes at most one status — the status of the agent bound to it (via `pane.agent_id →
// agents[]`). A pane with no agent (a bare shell / viewer) contributes NOTHING to the rollup: only
// classified agent panes carry a status dot (a bare shell has no `blocked/working/done/idle`).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { AgentRecord } from "../contracts/agent";
import type { PaneRecord, SessionRecord, StatusRollup } from "../contracts/snapshot";
import { emptyRollup, tallyStatus } from "../contracts/snapshot";

/**
 * Compute the status rollup for one session from its panes and the agent index. Each pane that has
 * an `agent_id` resolving to a live agent contributes that agent's status; panes without an agent
 * contribute nothing. The four counters always sum to the number of agent-bearing panes.
 */
export function rollupForPanes(
  panes: readonly PaneRecord[],
  agentsById: ReadonlyMap<string, AgentRecord>,
): StatusRollup {
  const rollup = emptyRollup();
  for (const pane of panes) {
    if (pane.agent_id === null) continue;
    const agent = agentsById.get(pane.agent_id);
    if (!agent) continue;
    tallyStatus(rollup, agent.status);
  }
  return rollup;
}

/**
 * Recompute and stamp the `status_rollup` of every session in-place, given the pane and agent legs.
 * Sessions are keyed to their panes by `pane.session_id === session.id`. Returns the same array for
 * chaining. This is the single call the snapshot assembler makes so the hint is always consistent
 * with the `agents[]`/`panes[]` legs it emits alongside.
 */
export function stampSessionRollups(
  sessions: SessionRecord[],
  panes: readonly PaneRecord[],
  agents: readonly AgentRecord[],
): SessionRecord[] {
  const agentsById = new Map<string, AgentRecord>();
  for (const a of agents) agentsById.set(a.id, a);

  const panesBySession = new Map<string, PaneRecord[]>();
  for (const p of panes) {
    let list = panesBySession.get(p.session_id);
    if (!list) {
      list = [];
      panesBySession.set(p.session_id, list);
    }
    list.push(p);
  }

  for (const session of sessions) {
    const sessionPanes = panesBySession.get(session.id) ?? [];
    session.status_rollup = rollupForPanes(sessionPanes, agentsById);
  }
  return sessions;
}

/**
 * A workspace-level rollup — the union of every session rollup under a workspace. Not a §2.2 field
 * (the frozen snapshot carries only per-session rollups) but the shape `query render --scope
 * sessions` and `session list --workspace` project, so the rule lives here beside the session
 * rollup to keep one aggregation altitude.
 */
export function rollupForSessions(sessions: readonly SessionRecord[]): StatusRollup {
  const rollup = emptyRollup();
  for (const s of sessions) {
    rollup.blocked += s.status_rollup.blocked;
    rollup.working += s.status_rollup.working;
    rollup.done += s.status_rollup.done;
    rollup.idle += s.status_rollup.idle;
  }
  return rollup;
}
