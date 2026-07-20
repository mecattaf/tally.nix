// tally — Seam A admission + leaf-invocation build (IMPLEMENTATION-PLAN M2.2 `enqueue.ts`;
// CLI-SURFACE §1.1a; SPEC "The spawn-tracked-agent-job", durable-row admission appendix).
//
// The one control verb (Seam A): `tally enqueue`. This module is the PURE admission half —
//   - validate the params (delegated to the frozen contracts validator, no drift);
//   - decide durable-row admission (TW veneer) → a row (task_uuid) or a rowless unit (task_uuid null);
//   - pick the pool (declared `--pool` honored, worker-gpu default for heavy work — NEVER a model
//     re-pick, PS#2);
//   - build the leaf invocation via the kind's adapter (session_ref/model extraction);
//   - assemble the fresh in-flight `JobEntry`.
// The stateful side (queue push, dedup probe, dispatch, lifecycle fan-out) is the engine's; this
// module produces the admitted entry the engine drives.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { randomUUID } from "node:crypto";
import type { EnqueueParams, Pool } from "../contracts/index";
import { GPU_POOLS } from "../contracts/index";
import type { AgentAdapter, AdapterContext, LeafInvocation } from "../agents/kinds";
import { resolveCommand } from "../agents/kinds";
import { piAdapter } from "../agents/pi";
import { claudeCodeAdapter } from "../agents/claude-code";
import { shellAdapter } from "../agents/shell";
import { newJobEntry, type JobEntry } from "./lifecycle";

/** The default heavy-work pool when no `--pool` is declared (worker-gpu, prioritized). */
export const DEFAULT_HEAVY_POOL: Pool = "worker-gpu";

/** The adapter for an agent kind (the three-kind dispatch). */
export function adapterFor(kind: EnqueueParams["kind"]): AgentAdapter {
  switch (kind) {
    case "pi":
      return piAdapter;
    case "claude-code":
      return claudeCodeAdapter;
    case "shell":
      return shellAdapter;
  }
}

/**
 * Resolve the pool a job runs on: the declared `--pool` hint verbatim (honored, never a model
 * re-pick), or the worker-gpu default. A declared pool tally does not know still passes through (the
 * reserved `sub:<acct>`/`api` pools; witness `pool`/`charge` are GPU-only in v0 but the field is
 * carried).
 */
export function resolvePool(params: Pick<EnqueueParams, "pool">): Pool {
  return params.pool ?? DEFAULT_HEAVY_POOL;
}

/** True when the pool is one of the two day-1 GPU pools (populates the witness `pool`/`charge`). */
export function isGpuPool(pool: Pool): boolean {
  return (GPU_POOLS as readonly string[]).includes(pool);
}

/** The admitted, ready-to-drive result of Seam-A admission. */
export interface AdmittedJob {
  entry: JobEntry;
  leaf: LeafInvocation;
  /** True when a durable TW row was admitted (task_uuid non-null). */
  hasRow: boolean;
}

/**
 * Admit a Seam-A enqueue into a fresh `JobEntry` + its leaf invocation. `taskUuid` is supplied by the
 * caller (the engine, which created the row when admitted, or null for a rowless unit). `leaseEpoch`
 * is the current epoch stamped onto the entry (the fence). Model choice is DECLARED — the adapter
 * carries `--model-class` verbatim, never re-picks (PS#2).
 */
export function admit(args: {
  params: EnqueueParams;
  taskUuid: string | null;
  leaseEpoch: number;
  jobId?: string;
}): AdmittedJob {
  const { params, taskUuid, leaseEpoch } = args;
  const jobId = args.jobId ?? randomUUID();
  const command = resolveCommand(params);
  const adapter = adapterFor(params.kind);
  const session = params.session ?? null;
  const ctx: AdapterContext = { params, command, session };
  const leaf = adapter.build(ctx);
  const pool = resolvePool(params);

  const entry = newJobEntry({
    job_id: jobId,
    task_uuid: taskUuid,
    params,
    argv: leaf.argv,
    pool,
    session,
    session_ref: leaf.sessionRef,
    model: leaf.model,
    lease_epoch: leaseEpoch,
  });

  return { entry, leaf, hasRow: taskUuid !== null };
}

/**
 * The row-seed fields an admitted job contributes to a durable TW row (when admitted). The engine
 * calls `tw.createRow` with this; a rowless unit skips it.
 *
 * The row's whole charter is crash-survivable re-dispatch (durable-row admission, CLI-SURFACE
 * §1.1a), so the seed persists the job's TRUE identity: `argv_json` carries the resolved argv
 * verbatim (JSON-encoded — never round-tripped through the whitespace-joined `description`, which
 * destroys quoting) and `evidence_json` the declared gates (so a recovered attempt keeps its
 * original evidence requirements, PS#9 "never self-report"). `description` is a cosmetic label only.
 */
export function rowSeedFor(params: EnqueueParams, uuid: string, leaseEpoch: number): {
  uuid: string;
  description: string;
  priority: EnqueueParams["priority"];
  source: EnqueueParams["source"];
  kind: EnqueueParams["kind"];
  cwd?: string;
  worktree?: string;
  pool?: string;
  model_class?: string;
  dedup_key?: string;
  session_ref?: string | null;
  lease_epoch: number;
  attempt: number;
  argv_json: string;
  evidence_json?: string;
} {
  const command = resolveCommand(params);
  const description = params.invocation ?? command.join(" ");
  const seed: ReturnType<typeof rowSeedFor> = {
    uuid,
    description,
    priority: params.priority,
    source: params.source,
    kind: params.kind,
    lease_epoch: leaseEpoch,
    attempt: 1,
    argv_json: JSON.stringify(command),
  };
  if (params.evidence !== undefined) seed.evidence_json = JSON.stringify(params.evidence);
  if (params.cwd !== undefined) seed.cwd = params.cwd;
  if (params.worktree !== undefined) seed.worktree = params.worktree;
  if (params.pool !== undefined) seed.pool = params.pool;
  if (params.model_class !== undefined) seed.model_class = params.model_class;
  if (params.dedup_key !== undefined) seed.dedup_key = params.dedup_key;
  if (params.session !== undefined) seed.session_ref = params.session;
  return seed;
}
