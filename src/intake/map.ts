// tally — gh signal → TaskChampion row mapping (IMPLEMENTATION-PLAN M2.4 `map.ts`; octo.nvim
// surface scan §6). Turns a qualifying {@link Signal} into a durable TW row (`source=gh`, priority
// from the signal class) — proving CROSS-SOURCE URGENCY RANKING over the one store: a gh signal
// out-ranks the OCR firehose (BUILD-SEQUENCE step 8). Dedup is on the GraphQL node id so re-polls
// never duplicate rows.
//
// The mapper depends only on the TaskChampion veneer (M1.3) and the contracts. It DOES NOT dispatch —
// a gh row is an admitted durable enqueue whose lease/dispatch is the jobs engine's concern; the
// intake's job is to LAND the row in the one store with correct provenance + urgency. Read/poll half
// only (scan §6): no mutation of the gh subject.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { createHash } from "node:crypto";

import type { TaskChampion } from "../tw/index";
import type { RowSeed } from "../tw/index";
import type { TaskRow } from "../contracts/task";
import type { Priority } from "../contracts/job";

import type { Signal, SignalPolicy } from "./signals";
import { priorityFor } from "./signals";

/** The dedup-key prefix for a gh-sourced row — the stable GraphQL node id, namespaced. */
export const GH_DEDUP_PREFIX = "gh:";

/** The dedup key for a signal (namespaced node id) — written to the row's `dedup_key` UDA. */
export function dedupKeyFor(signal: Signal): string {
  return `${GH_DEDUP_PREFIX}${signal.node_id}`;
}

/**
 * Derive a STABLE row uuid from a signal's dedup key (a UUIDv5-shaped, deterministic id). Re-polling
 * the same subject yields the same uuid, so a duplicate `task import` overwrites the existing row
 * rather than creating a second — the dedup guarantee at the taskwarrior level. Uses a sha256 of the
 * dedup key formatted into the 8-4-4-4-12 UUID shape with version nibble 5 and RFC-4122 variant.
 */
export function stableUuid(dedupKey: string): string {
  const h = createHash("sha256").update(dedupKey, "utf8").digest("hex");
  const b = h.slice(0, 32).split("");
  // version 5 (name-based sha1/sha256 family) in the 13th hex nibble
  b[12] = "5";
  // RFC-4122 variant (10xx) in the 17th hex nibble → one of 8,9,a,b
  const variant = (parseInt(b[16]!, 16) & 0x3) | 0x8;
  b[16] = variant.toString(16);
  const s = b.join("");
  return `${s.slice(0, 8)}-${s.slice(8, 12)}-${s.slice(12, 16)}-${s.slice(16, 20)}-${s.slice(20, 32)}`;
}

/**
 * A one-line human description for a gh row: the class, the subject type, and the `repo#number` +
 * title so the row reads at a glance in `task` and the standup join.
 */
export function describeSignal(signal: Signal): string {
  const ref = signal.number > 0 ? `${signal.repo}#${signal.number}` : signal.repo;
  const cls = signal.class.replace(/_/g, " ");
  return `[gh:${cls}] ${ref} — ${signal.title}`.trim();
}

/**
 * Build the {@link RowSeed} for a signal. `source=gh`, `kind=shell` (a gh signal is a follow-up unit
 * of work, not an agent run by default — the human/orchestrator decides the agent later; the row
 * carries no session_ref/model until dispatch), priority from the signal class, dedup on the node
 * id, and a stable uuid so a re-poll never duplicates.
 */
export function seedForSignal(signal: Signal, policy: SignalPolicy): RowSeed {
  const dedup = dedupKeyFor(signal);
  return {
    uuid: stableUuid(dedup),
    description: describeSignal(signal),
    priority: priorityFor(signal.class, policy),
    source: "gh",
    kind: "shell",
    dedup_key: dedup,
  };
}

/** The outcome of mapping one signal into the store. */
export interface MapOutcome {
  /** The signal's dedup key. */
  dedup_key: string;
  /** The row uuid (created or already-present). */
  uuid: string;
  /** `created` on first sight, `existing` when a row with this dedup key was already present. */
  status: "created" | "existing";
  /** The priority class the row carries (for the journald `TALLY_CLASS`). */
  priority: Priority;
  /** The row (created or the pre-existing one). */
  row: TaskRow;
}

/**
 * The signal → row mapper over the TaskChampion veneer. Idempotent per node id: a re-poll of an
 * already-landed subject is a no-op (returns `existing`), so re-polls never duplicate rows (scan §6).
 * The dedup check is a `dedup_key` query against the one store BEFORE import — the authoritative
 * de-dup, in addition to the deterministic uuid (belt-and-braces so a partially-written prior row is
 * still de-duplicated on its key).
 */
export class SignalMapper {
  constructor(
    private readonly tw: TaskChampion,
    private readonly policy: SignalPolicy,
  ) {}

  /** Whether a row with this signal's dedup key already exists (across any status). */
  private async existingRow(dedup: string): Promise<TaskRow | undefined> {
    // taskwarrior UDA filter: `dedup_key:<value>`. Any status (pending/completed/deleted) counts as
    // "already landed" — a completed/deleted gh row must NOT be resurrected by a re-poll.
    const rows = await this.tw.query([`dedup_key:${dedup}`]);
    return rows.find((r) => r.dedup_key === dedup);
  }

  /**
   * Land one signal as a durable row, deduped on the node id. Returns whether the row was created or
   * already present. Never dispatches — the row is an admitted durable enqueue the jobs engine leases
   * later.
   */
  async map(signal: Signal): Promise<MapOutcome> {
    const dedup = dedupKeyFor(signal);
    const priority = priorityFor(signal.class, this.policy);
    const existing = await this.existingRow(dedup);
    if (existing !== undefined) {
      return { dedup_key: dedup, uuid: existing.uuid, status: "existing", priority, row: existing };
    }
    const seed = seedForSignal(signal, this.policy);
    const row = await this.tw.createRow(seed);
    return { dedup_key: dedup, uuid: row.uuid, status: "created", priority, row };
  }

  /**
   * Map a batch of signals, de-conflicting duplicates WITHIN the batch (two origins can surface the
   * same subject — a notification and a search hit) so one subject lands once per cycle. The first
   * occurrence of a node id wins its class; later duplicates are skipped in-memory before any
   * `task import`.
   */
  async mapAll(signals: Signal[]): Promise<MapOutcome[]> {
    const seen = new Set<string>();
    const out: MapOutcome[] = [];
    for (const signal of signals) {
      const dedup = dedupKeyFor(signal);
      if (seen.has(dedup)) continue;
      seen.add(dedup);
      out.push(await this.map(signal));
    }
    return out;
  }
}

/**
 * De-conflict a signal list in priority order: when the same node id appears with multiple classes
 * (e.g. review_requested from search AND mention from a notification), keep the HIGHEST-urgency class
 * so the row's priority reflects the strongest signal. Pure — used by the poller before mapping.
 */
export function dedupeSignals(signals: Signal[]): Signal[] {
  const byNode = new Map<string, Signal>();
  const rank = (cls: Signal["class"]): number => {
    const order = ["review_requested", "mention", "assign", "author", "other"];
    const i = order.indexOf(cls);
    return i === -1 ? order.length : i;
  };
  for (const s of signals) {
    const prior = byNode.get(s.node_id);
    if (prior === undefined || rank(s.class) < rank(prior.class)) {
      byNode.set(s.node_id, s);
    }
  }
  return [...byNode.values()];
}
