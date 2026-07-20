// tally CLI — the `queue` control plane (Seam A) + the top-level `enqueue` alias
// (IMPLEMENTATION-PLAN M3.1; CLI-SURFACE §1.1, §1.1a).
//
//   tally enqueue / tally queue enqueue   — Seam A: admit one spawn-tracked-agent-job. Every §1.1a
//                                            flag; `--wait`/`--barrier`/`--wait-group`/`--wait-count`/
//                                            `--timeout`/`--detach`; exit code MIRRORS the verdict.
//   tally queue cancel <uuid|selector> [--force]
//   tally queue pause  [pool | --all]
//   tally queue resume [pool | --all]
//
// enqueue is `queue.enqueue` over the socket (Seam A validated by `validateEnqueueParams`); the
// `--wait` barrier blocks on the daemon's BarrierTracker (`queue.await_job` / `queue.await_barrier`
// — the §2.4 job-subject wait), which drains an already-terminal delta so a fast job never races
// the wait: a rowed job keyed by `task_uuid`, a rowless (task_uuid:null) unit by the `job_id` its
// enqueue result carries (issue #4), a wait-group by its barrier gid counting N terminal deltas
// (`job.completed | job.failed | job.evidence_fail`); the process exit code mirrors the verdict.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { parseEvidenceSpec, validateEnqueueParams } from "../contracts/wire";
import type { EnqueueParams, EnqueueResult, Verdict } from "../contracts/job";
import { findUnquotedShellMetachars } from "../agents/kinds";
import { connectClient, type TallyClient } from "./client";
import { printError, printJson, printLine, type Writer } from "./output";
import { flag, flagAll, hasFlag, wantsJson, type CliContext } from "./index";

/** Route the `queue` noun. */
export async function runQueue(ctx: CliContext): Promise<number> {
  switch (ctx.verb) {
    case "enqueue":
      return doEnqueue(ctx);
    case "cancel":
      return doCancel(ctx);
    case "pause":
      return doPauseResume(ctx, "pause");
    case "resume":
      return doPauseResume(ctx, "resume");
    default:
      printError(ctx.writer, `unknown queue verb '${ctx.verb ?? "(none)"}' (expected enqueue|cancel|pause|resume)`);
      return 2;
  }
}

/** The top-level `tally enqueue` alias — identical to `queue enqueue`. */
export async function runEnqueueAlias(ctx: CliContext): Promise<number> {
  return doEnqueue(ctx);
}

// ---------------------------------------------------------------------------------------------
// enqueue (Seam A).
// ---------------------------------------------------------------------------------------------

/**
 * Assemble the Seam-A enqueue params from the parsed flags, validating through the ONE shared
 * `validateEnqueueParams` (so the grammar cannot drift between CLI and daemon). `--invocation` XOR
 * `-- <argv…>`; `--cwd` XOR `--worktree`; repeatable `--evidence`.
 */
export function buildEnqueueParams(ctx: CliContext): EnqueueParams {
  const a = ctx.args;
  const raw: Record<string, unknown> = {};

  const priority = flag(a, "--priority");
  if (priority !== undefined) raw.priority = priority;
  const source = flag(a, "--source");
  if (source !== undefined) raw.source = source;
  const kind = flag(a, "--kind");
  if (kind !== undefined) raw.kind = kind;

  const invocation = flag(a, "--invocation");
  if (invocation !== undefined) {
    raw.invocation = invocation;
    // `--invocation` is exec'd directly, never through a shell (CLI-SURFACE §1.1a); an unquoted
    // redirect/pipe/sequencing/substitution char becomes a literal argv token instead of doing what
    // it looks like it does. WARN, not error — the evidence gate is the ruled backstop (issue #6),
    // and a literal `>`/`;`/etc. argv token is legal (e.g. a script whose own args include one).
    const metachars = findUnquotedShellMetachars(invocation);
    if (metachars.length > 0) {
      printError(
        ctx.writer,
        `--invocation contains unquoted shell metacharacter(s) (${metachars.join(" ")}) — ` +
          `it is exec'd directly, NOT run through a shell, so ${metachars.join("/")} will NOT redirect/pipe/chain. ` +
          `Use \`-- sh -c "..."\` for shell semantics.`,
      );
    }
  }
  if (a.passthrough !== undefined) raw.argv = a.passthrough;

  const cwd = flag(a, "--cwd");
  if (cwd !== undefined) raw.cwd = cwd;
  const worktree = flag(a, "--worktree");
  if (worktree !== undefined) raw.worktree = worktree;

  const evidence = flagAll(a, "--evidence");
  if (evidence.length > 0) {
    // Validate each spec eagerly so a malformed `--evidence` fails at the CLI with a clear message.
    raw.evidence = evidence.map((spec) => parseEvidenceSpec(spec));
  }

  const pool = flag(a, "--pool");
  if (pool !== undefined) raw.pool = pool;
  const modelClass = flag(a, "--model-class");
  if (modelClass !== undefined) raw.model_class = modelClass;
  const dedupKey = flag(a, "--dedup-key");
  if (dedupKey !== undefined) raw.dedup_key = dedupKey;
  const session = flag(a, "--session");
  if (session !== undefined) raw.session = session;

  const barrier = flag(a, "--barrier");
  if (barrier !== undefined) raw.barrier = barrier;
  const waitGroup = flag(a, "--wait-group");
  if (waitGroup !== undefined) raw.wait_group = waitGroup;
  const waitCount = flag(a, "--wait-count");
  if (waitCount !== undefined) {
    const n = Number(waitCount);
    if (!Number.isInteger(n)) throw new Error(`--wait-count must be an integer: ${waitCount}`);
    raw.wait_count = n;
  }
  if (hasFlag(a, "--wait")) raw.wait = true;
  const timeout = flag(a, "--timeout");
  if (timeout !== undefined) raw.timeout = timeout;
  if (hasFlag(a, "--detach")) raw.detach = true;

  // Defaults (§1.1a): `--source manual` when omitted (a human at the CLI); `--kind shell` is the
  // safe default (no model, no session_ref); `--priority medium`. These are convenience defaults for
  // the CLI surface — an orchestrator always passes all three explicitly.
  if (raw.priority === undefined) raw.priority = "medium";
  if (raw.source === undefined) raw.source = "manual";
  if (raw.kind === undefined) raw.kind = "shell";

  return validateEnqueueParams(raw);
}

async function doEnqueue(ctx: CliContext): Promise<number> {
  const waitOnly = isWaitOnly(ctx);
  const groupFlag = flag(ctx.args, "--wait-group");
  const countFlag = flag(ctx.args, "--wait-count");

  // The documented §1.1a wait-ONLY barrier form: `tally enqueue --wait-group <gid> --wait-count N`
  // with NO leaf work. It must NOT be run through validateEnqueueParams (which requires invocation XOR
  // argv) — it enqueues nothing, it only blocks on N already-enqueued group members.
  if (waitOnly && groupFlag !== undefined && countFlag !== undefined) {
    const count = Number(countFlag);
    if (!Number.isInteger(count) || count <= 0) {
      printError(ctx.writer, `--wait-count must be a positive integer: ${countFlag}`);
      return 2;
    }
    const t = flag(ctx.args, "--timeout");
    const timeoutMs = t !== undefined ? parseDuration(t) : undefined;
    const client = await connectClient(clientOpts(ctx));
    try {
      const outcome = await awaitGroup(client, groupFlag, count, timeoutMs);
      emitBarrierOutcome(ctx.writer, groupFlag, outcome, wantsJson(ctx.args));
      return outcome.timedOut ? TIMEOUT_EXIT : outcome.exitCode;
    } finally {
      client.close();
    }
  }

  let params: EnqueueParams;
  try {
    params = buildEnqueueParams(ctx);
  } catch (err) {
    printError(ctx.writer, err instanceof Error ? err.message : String(err));
    return 2;
  }

  const wantWait = params.wait === true;
  const wantGroup = params.wait_group !== undefined && params.wait_count !== undefined;

  const client = await connectClient(clientOpts(ctx));
  try {
    const timeoutMs = params.timeout !== undefined ? parseDuration(params.timeout) : undefined;

    const result = await client.call<EnqueueResult>("queue.enqueue", params);

    if (!wantWait && !wantGroup) {
      emitEnqueueResult(ctx.writer, result, wantsJson(ctx.args));
      return 0;
    }

    if (wantGroup) {
      // Block on the barrier GROUP via the daemon's BarrierTracker (queue.await_barrier): it filters by
      // the barrier gid and drains already-terminal group members, so it never releases early on
      // unrelated jobs and never misses a member whose delta fired before the wait was issued.
      const n = params.wait_count!;
      const outcome = await awaitGroup(client, params.wait_group!, n, timeoutMs);
      emitBarrierOutcome(ctx.writer, params.wait_group ?? "", outcome, wantsJson(ctx.args), result);
      return outcome.timedOut ? TIMEOUT_EXIT : outcome.exitCode;
    }

    // Single `--wait`. If the enqueue already returned terminal (a dedup `reused` skip), do not block.
    if (result.status === "reused" || result.status === "completed" || result.status === "failed" || result.status === "cancelled") {
      emitEnqueueResult(ctx.writer, result, wantsJson(ctx.args));
      return result.verdict !== null ? verdictExitCode(result.verdict) : 0;
    }
    // Block on THIS job via the BarrierTracker (queue.await_job) — resolves even if the terminal delta
    // fired before the wait was issued (the normal sequential enqueue-then-wait flow). A rowed job is
    // keyed by task_uuid; a rowless (task_uuid:null) live-orchestrator unit by the job_id its enqueue
    // result carries — every terminal delta is recorded under job_id, so the rowless wait is the SAME
    // exact-identity barrier (issue #4: the old rowless fallback filtered the Seam-B stream on a
    // `lease_epoch` field that terminal deltas do not even carry, so the first terminal delta of ANY
    // job on the box satisfied the wait and the exit code mirrored a stranger's verdict).
    const key = result.task_uuid !== null ? { task_uuid: result.task_uuid } : { job_id: result.job_id };
    const awaited = await client.call<{ verdict: Verdict | null; exit_code: number; timed_out: boolean }>(
      "queue.await_job",
      timeoutMs !== undefined ? { ...key, timeout_ms: timeoutMs } : key,
    );
    emitJobWaitOutcome(ctx.writer, result, awaited, wantsJson(ctx.args));
    return awaited.timed_out ? TIMEOUT_EXIT : awaited.exit_code;
  } finally {
    client.close();
  }
}

/** True when the invocation carries NO leaf work (no `--invocation`, no `-- argv`) — a wait-only form. */
function isWaitOnly(ctx: CliContext): boolean {
  const hasInvocation = flag(ctx.args, "--invocation") !== undefined;
  const hasArgv = ctx.args.passthrough !== undefined && ctx.args.passthrough.length > 0;
  return !hasInvocation && !hasArgv;
}

/** Block on a barrier group via the daemon's BarrierTracker (queue.await_barrier). */
async function awaitGroup(
  client: TallyClient,
  group: string,
  count: number,
  timeoutMs: number | undefined,
): Promise<{ satisfied: number; exitCode: number; timedOut: boolean }> {
  const r = await client.call<{ satisfied: number; exit_code: number; timed_out: boolean }>(
    "queue.await_barrier",
    timeoutMs !== undefined ? { group, count, timeout_ms: timeoutMs } : { group, count },
  );
  return { satisfied: r.satisfied, exitCode: r.exit_code, timedOut: r.timed_out };
}

function emitBarrierOutcome(
  w: Writer,
  group: string,
  outcome: { satisfied: number; exitCode: number; timedOut: boolean },
  json: boolean,
  enqueue?: EnqueueResult,
): void {
  if (json) {
    printJson(w, { ...(enqueue ?? {}), barrier: group, waited: true, satisfied: outcome.satisfied, timed_out: outcome.timedOut, exit_code: outcome.timedOut ? TIMEOUT_EXIT : outcome.exitCode });
    return;
  }
  if (outcome.timedOut) {
    printLine(w, `barrier ${group} timeout (units keep running) — satisfied ${outcome.satisfied}`);
    return;
  }
  printLine(w, `barrier ${group} satisfied=${outcome.satisfied} exit=${outcome.exitCode}`);
}

function emitJobWaitOutcome(
  w: Writer,
  enqueue: EnqueueResult,
  awaited: { verdict: Verdict | null; exit_code: number; timed_out: boolean },
  json: boolean,
): void {
  if (json) {
    printJson(w, { ...enqueue, waited: true, timed_out: awaited.timed_out, exit_code: awaited.timed_out ? TIMEOUT_EXIT : awaited.exit_code, verdict: awaited.verdict });
    return;
  }
  if (awaited.timed_out) {
    printLine(w, `timeout (units keep running) — ${enqueue.task_uuid ?? "(no-row)"}`);
    return;
  }
  printLine(w, `${enqueue.task_uuid ?? "(no-row)"} ${awaited.verdict ?? "?"} exit=${awaited.exit_code}`);
}

function emitEnqueueResult(w: Writer, result: EnqueueResult, json: boolean): void {
  if (json) {
    printJson(w, result);
    return;
  }
  const uuid = result.task_uuid ?? "(no-row)";
  printLine(w, `${result.status} ${uuid} pool=${result.pool} lease_epoch=${result.lease_epoch}${result.verdict ? ` verdict=${result.verdict}` : ""}`);
}

/**
 * The exit code a `--wait`/`--wait-group` returns on TIMEOUT. A timeout has no verdict (the units keep
 * running, barrier ≠ cancel), so it must NOT return 0 (which mirrors a pass) — 124 is the conventional
 * timeout code and is distinct from every verdict exit code (0/1/3/4).
 */
const TIMEOUT_EXIT = 124;

/**
 * Map a verdict to the CLI exit code (§1.1a; the barrier exit mirrors the verdict). Kept identical to
 * the in-daemon `barrier.ts` verdictExitCode so the CLI-side and daemon-side barriers agree on a
 * verdict's exit code (clean-exit-no-artifact=3, cancelled=4 — the distinguished forensics).
 */
export function verdictExitCode(verdict: Verdict): number {
  switch (verdict) {
    case "pass":
    case "reused":
      return 0;
    case "clean-exit-no-artifact":
      return 3; // distinguished forensic exit (non-zero, distinct from a plain failure)
    case "cancelled":
      return 4;
    case "failed":
    default:
      return 1;
  }
}

/**
 * Parse a duration string (`--timeout`) into milliseconds. Accepts a bare number (seconds) or a
 * suffixed form `<n>ms|s|m|h`. A malformed value throws (surfaced as a CLI error).
 */
export function parseDuration(spec: string): number {
  const m = /^(\d+(?:\.\d+)?)(ms|s|m|h)?$/.exec(spec.trim());
  if (!m) throw new Error(`invalid duration: ${spec}`);
  const n = Number(m[1]);
  switch (m[2]) {
    case "ms":
      return Math.round(n);
    case "m":
      return Math.round(n * 60_000);
    case "h":
      return Math.round(n * 3_600_000);
    case "s":
    case undefined:
    default:
      return Math.round(n * 1000);
  }
}

// ---------------------------------------------------------------------------------------------
// cancel / pause / resume.
// ---------------------------------------------------------------------------------------------

async function doCancel(ctx: CliContext): Promise<number> {
  const target = ctx.args.positionals[0];
  if (target === undefined) {
    printError(ctx.writer, "queue cancel requires a <uuid|selector>");
    return 2;
  }
  const force = hasFlag(ctx.args, "--force");
  const client = await connectClient(clientOpts(ctx));
  try {
    const result = await client.call<{ ok: true; affected: number; task_uuid: string | null; was: string | null; lease_epoch: number | null }>(
      "queue.cancel",
      { task_uuid: target, force },
    );
    if (wantsJson(ctx.args)) {
      // The frozen §1.1 `--json` shape: {task_uuid, status:"cancelled", was, lease_epoch}.
      printJson(ctx.writer, {
        task_uuid: result.task_uuid ?? target,
        status: "cancelled",
        was: result.was,
        lease_epoch: result.lease_epoch,
      });
    } else {
      printLine(ctx.writer, `cancelled ${result.task_uuid ?? target} (was ${result.was ?? "-"}, affected ${result.affected}${force ? ", forced" : ""})`);
    }
    return result.affected > 0 ? 0 : 4;
  } finally {
    client.close();
  }
}

async function doPauseResume(ctx: CliContext, op: "pause" | "resume"): Promise<number> {
  const positional = ctx.args.positionals[0];
  const all = hasFlag(ctx.args, "--all");
  const params: { pool?: string } = {};
  if (!all && positional !== undefined) params.pool = positional;

  const client = await connectClient(clientOpts(ctx));
  try {
    const result = await client.call<{ ok: true; affected: number }>(`queue.${op}`, params);
    const paused = op === "pause";
    if (wantsJson(ctx.args)) {
      printJson(ctx.writer, { paused, pool: params.pool ?? (all ? "*" : null), queued_depth: result.affected });
    } else {
      printLine(ctx.writer, `${op}d ${params.pool ?? (all ? "all pools" : "default pool")} (queued_depth ${result.affected})`);
    }
    return 0;
  } finally {
    client.close();
  }
}

// ---------------------------------------------------------------------------------------------
// shared client construction.
// ---------------------------------------------------------------------------------------------

/** Build the client options from the CLI context (socket override for tests). */
export function clientOpts(ctx: CliContext): { socket?: string; env?: CliContext["env"] } {
  const opts: { socket?: string; env?: CliContext["env"] } = { env: ctx.env };
  if (ctx.socket !== undefined) opts.socket = ctx.socket;
  return opts;
}
