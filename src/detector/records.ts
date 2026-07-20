// tally — detector-internal EXPLAIN-DATA (IMPLEMENTATION-PLAN M2.3, risk 9 — the single-store ruling).
//
// This is NOT a second agent store. The ONE authoritative session store is `model/store.ts` (the
// detector writes the `agents[]` leg into it via the Bus). This module holds only the retained
// explain-data surfaced via `agent.explain` (CLI-SURFACE §1.4): *why* a pane is working/blocked/done
// — the matched rule, the manifest source/version, and the detection strategy (hook|scrape). Detector
// logs are TTL-prunable, never proof (PS#21 retention).

import type { AgentStatus, DetectorStrategy } from "../contracts/agent";

/**
 * The retained explain-data for one agent, keyed by `agent_id`. Populated on every classification (a
 * scrape or hook decision) and read by the `agent.explain` RPC handler.
 */
export interface ExplainRecord {
  agent_id: string;
  pane_id: string;
  /** The last decided four-state status. */
  status: AgentStatus;
  /** hook (authoritative) or scrape (fallback) — which strategy produced the last decision. */
  strategy: DetectorStrategy;
  /** The manifest kind + version the scrape decision used (null for a hook decision). */
  manifest: { kind: string; version: string } | null;
  /** The matched rule id + priority (null when a hook decided, or no rule matched). */
  matched_rule: { id: string; priority: number } | null;
  /** The region the matched rule scoped (null when hook-decided / unmatched). */
  region: string | null;
  /** ISO-8601 timestamp of this decision. */
  at: string;
}

/**
 * The `agent.explain` payload shape (CLI-SURFACE §1.4 `{pane, state, manifest, matched_rule,
 * strategy}`), projected from an `ExplainRecord`.
 */
export interface AgentExplain {
  pane: string;
  state: AgentStatus;
  manifest: string | null;
  matched_rule: string | null;
  strategy: DetectorStrategy;
}

/**
 * The detector-internal explain store. In-memory, per-agent, last-write-wins — an operator-facing
 * debug surface, not durable state. Pruned when an agent is released.
 */
export class ExplainStore {
  private readonly byAgent = new Map<string, ExplainRecord>();

  /** Record (replace) the explain-data for an agent. */
  put(record: ExplainRecord): void {
    this.byAgent.set(record.agent_id, record);
  }

  /** The raw retained record for an agent, or undefined. */
  get(agentId: string): ExplainRecord | undefined {
    return this.byAgent.get(agentId);
  }

  /** Project an agent's explain-data into the `agent.explain` payload, or null if unknown. */
  explain(agentId: string): AgentExplain | null {
    const r = this.byAgent.get(agentId);
    if (!r) return null;
    return {
      pane: r.pane_id,
      state: r.status,
      manifest: r.manifest ? `${r.manifest.kind}@${r.manifest.version}` : null,
      matched_rule: r.matched_rule ? r.matched_rule.id : null,
      strategy: r.strategy,
    };
  }

  /** Forget an agent's explain-data (on release). */
  forget(agentId: string): void {
    this.byAgent.delete(agentId);
  }

  /** Drop all explain-data (daemon reset / test teardown). */
  clear(): void {
    this.byAgent.clear();
  }

  /** The number of retained records (for tests / metrics). */
  get size(): number {
    return this.byAgent.size;
  }
}
