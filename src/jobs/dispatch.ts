// tally — job dispatch: priority queue → pls lease → transient-unit execution (IMPLEMENTATION-PLAN
// M2.2 `dispatch.ts`; SPEC "pls IS the per-box governor", "acquire the pls lease before it touches
// the GPU"; CLI-SURFACE §1.1a `--pool`).
//
// The dispatch path, in order:
//   1. A priority queue admits the next job (high > medium > low, FIFO within a class), gated by the
//      pool admission drain (pause/resume).
//   2. The pls lease is acquired BEFORE the GPU is touched — the declared `--pool` hint is honored
//      (worker-gpu default for heavy work; NEVER a model re-pick, PS#2). If the pool is full the
//      acquire QUEUES (both-or-queue) and the job stays enqueued until a slot frees.
//   3. Execution runs as a transient systemd user unit `tally-job-<id>` via `systemd-run --user
//      --unit … -- <argv>`, carrying the `TALLY_TASK_UUID`/`TALLY_SESSION_REF`/`TALLY_YIELD_FD` env
//      conventions; a DIRECT `Bun.spawn`-shaped fallback (through the same `Exec`) runs when
//      systemd-run is absent (dev rig / tests).
//
// This module owns the queue + the lease/exec mechanics; the engine drives it and fans the lifecycle
// events. All subprocess access is via the injected `Exec` seam.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Exec, ExecResult, Pool, Priority } from "../contracts/index";
import type { LeaseManager } from "../pls/index";
import type { JobEntry } from "./lifecycle";

/** The numeric rank of a tally priority for the pls lease `--priority` and the queue ordering. */
export function priorityRank(p: Priority): number {
  return p === "high" ? 100 : p === "medium" ? 50 : 10;
}

/**
 * A FIFO-within-priority queue of pending jobs. `push` appends; `next` pops the highest-priority
 * job whose pool is not paused (the admission drain). Cancelled jobs are removed by id.
 */
export class PriorityQueue {
  private readonly items: JobEntry[] = [];
  /** Pools currently paused (admission drained) — a job whose pool is paused is not admitted. */
  private readonly pausedPools = new Set<Pool>();
  /** True when ALL pools are paused (`queue pause` with no pool). */
  private pausedAll = false;

  /** Enqueue a job (tail of its priority class). */
  push(entry: JobEntry): void {
    this.items.push(entry);
  }

  /** Current queued jobs, highest-priority first (stable within a class). */
  peekAll(): readonly JobEntry[] {
    return [...this.items].sort((a, b) => priorityRank(b.priority) - priorityRank(a.priority));
  }

  /** Queue depth (optionally for one pool). */
  depth(pool?: Pool): number {
    return pool ? this.items.filter((j) => j.pool === pool).length : this.items.length;
  }

  /** Pause admission for a pool (or all pools). Running holders keep their lease (drain gate). */
  pause(pool?: Pool): void {
    if (pool) this.pausedPools.add(pool);
    else this.pausedAll = true;
  }

  /** Resume admission for a pool (or all pools). */
  resume(pool?: Pool): void {
    if (pool) this.pausedPools.delete(pool);
    else {
      this.pausedAll = false;
      this.pausedPools.clear();
    }
  }

  /** True when a pool's admission is drained. */
  isPaused(pool: Pool): boolean {
    return this.pausedAll || this.pausedPools.has(pool);
  }

  /** Remove a job from the queue by id (cancel). Returns the removed entry, or undefined. */
  remove(jobId: string): JobEntry | undefined {
    const i = this.items.findIndex((j) => j.job_id === jobId);
    if (i === -1) return undefined;
    return this.items.splice(i, 1)[0];
  }

  /**
   * Pop the next admissible job: highest priority, FIFO within a class, whose pool is not paused (and
   * not `skipPool`-blocked). `skipPool` lets the engine exclude a pool whose lease is currently held
   * elsewhere (a queued-acquire "blocked" pool) so a job on that pool is parked, not re-popped in a
   * tight spin. Returns undefined when nothing is admissible.
   */
  next(skipPool?: (pool: Pool) => boolean): JobEntry | undefined {
    let bestIdx = -1;
    let bestRank = -Infinity;
    for (let i = 0; i < this.items.length; i++) {
      const j = this.items[i]!;
      if (this.isPaused(j.pool)) continue;
      if (skipPool !== undefined && skipPool(j.pool)) continue;
      const r = priorityRank(j.priority);
      if (r > bestRank) {
        bestRank = r;
        bestIdx = i;
      }
    }
    if (bestIdx === -1) return undefined;
    return this.items.splice(bestIdx, 1)[0];
  }

  /** Peek (without removing) the highest-priority job whose pool matches `pool`, or undefined. */
  peekForPool(pool: Pool): JobEntry | undefined {
    let best: JobEntry | undefined;
    let bestRank = -Infinity;
    for (const j of this.items) {
      if (j.pool !== pool) continue;
      const r = priorityRank(j.priority);
      if (r > bestRank) {
        bestRank = r;
        best = j;
      }
    }
    return best;
  }
}

/** The result of dispatching one job to a transient unit — the run's raw outcome. */
export interface DispatchExecResult {
  unit: string;
  exitCode: number;
  stdout: string;
  stderr: string;
  /** ms-epoch start/end for the witness span. */
  startedAtMs: number;
  endedAtMs: number;
}

/** Options controlling how the leaf runs (the env conventions + the systemd-run toggle). */
export interface DispatchExecOptions {
  /** The transient unit name (`tally-job-<id>`). */
  unit: string;
  /** The env carried to the leaf (`TALLY_*` conventions merged over ambient). */
  env: Record<string, string>;
  /** The working directory for the run (from `--cwd`/`--worktree`), or undefined. */
  cwd?: string;
  /** Millisecond timeout guard for the run, or undefined (no timeout). */
  timeoutMs?: number;
  /** Force the direct-spawn fallback (skip systemd-run) — used by the dev rig / a systemd-absent host. */
  forceDirect?: boolean;
  /**
   * The durable exit-record file the transient unit's ExecStopPost writes its `$EXIT_STATUS` to.
   * `--collect` (below) garbage-collects the unit the moment it stops — including while NO daemon is
   * alive to observe the exit — so recovery's `systemctl show` probe finds `LoadState=not-found` and
   * never an ExecMainStatus; this record is the exit-code conjunct that survives (issue #3).
   */
  exitFile?: string;
}

/**
 * The env-convention keys the transient unit carries (SPEC "The inner fold"; the yield-FD is the
 * cooperative-preemption checkpoint channel). The engine populates the values.
 */
export const ENV_TASK_UUID = "TALLY_TASK_UUID";
export const ENV_SESSION_REF = "TALLY_SESSION_REF";
export const ENV_YIELD_FD = "TALLY_YIELD_FD";
export const ENV_LEASE_EPOCH = "TALLY_LEASE_EPOCH";
export const ENV_JOB_ID = "TALLY_JOB_ID";

/**
 * The transient-unit executor. Wraps the `Exec` seam. `run` executes the leaf argv either as a
 * `systemd-run --user --unit tally-job-<id>` transient unit or, when systemd-run is absent (or
 * `forceDirect`), by spawning the argv directly through the same `Exec` — the "direct spawn fallback
 * when systemd is absent" the plan mandates. Detection of absence is exit 127 (command not found).
 */
export class TransientRunner {
  constructor(private readonly exec: Exec) {}

  /** Build the `systemd-run --user --unit … --setenv … -- <argv>` argv. */
  buildSystemdRunArgv(argv: string[], opts: DispatchExecOptions): string[] {
    const out = ["systemd-run", "--user", "--wait", "--collect", "--quiet", "--unit", opts.unit];
    if (opts.exitFile !== undefined) {
      // ExecStopPost runs INSIDE systemd when the unit stops, daemon alive or not, so the exit
      // status outlives the `--collect` unload that erases ExecMainStatus (issue #3). `$$` keeps
      // systemd from expanding the variable at Exec-line parse time; the shell reads `$EXIT_STATUS`
      // from the env systemd sets for stop-post commands. Temp-then-rename so a torn write is never
      // read as a status (the PS#10 discipline).
      out.push(
        "--property",
        `ExecStopPost=/bin/sh -c 'printf %s "$$EXIT_STATUS" > "${opts.exitFile}.tmp" && mv "${opts.exitFile}.tmp" "${opts.exitFile}"'`,
      );
    }
    if (opts.cwd !== undefined) out.push("--working-directory", opts.cwd);
    for (const [k, v] of Object.entries(opts.env)) {
      out.push("--setenv", `${k}=${v}`);
    }
    out.push("--", ...argv);
    return out;
  }

  async run(argv: string[], opts: DispatchExecOptions): Promise<DispatchExecResult> {
    const startedAtMs = Date.now();
    let result: ExecResult;
    let usedDirect = opts.forceDirect === true;

    if (!usedDirect) {
      const sysArgv = this.buildSystemdRunArgv(argv, opts);
      const sysOpts = this.execOpts(opts);
      result = await this.exec.run(sysArgv, sysOpts);
      // systemd-run absent (exit 127) ⇒ fall back to a direct spawn of the leaf argv.
      if (result.code === 127) {
        usedDirect = true;
      }
    }

    if (usedDirect) {
      result = await this.exec.run(argv, this.execOpts(opts));
    }

    const endedAtMs = Date.now();
    return {
      unit: opts.unit,
      exitCode: result!.code,
      stdout: result!.stdout,
      stderr: result!.stderr,
      startedAtMs,
      endedAtMs,
    };
  }

  private execOpts(opts: DispatchExecOptions): { env: Record<string, string>; cwd?: string; timeoutMs?: number } {
    const o: { env: Record<string, string>; cwd?: string; timeoutMs?: number } = { env: opts.env };
    if (opts.cwd !== undefined) o.cwd = opts.cwd;
    if (opts.timeoutMs !== undefined) o.timeoutMs = opts.timeoutMs;
    return o;
  }
}

/** The transient unit name for an id (`tally-job-<id>`; the `TALLY_UNIT` field). */
export function unitName(id: string): string {
  return `tally-job-${id}`;
}

/**
 * The transient unit name for an in-flight job: DETERMINISTIC for a rowed job
 * (`tally-job-<task_uuid>`) so a rebooted daemon can find a row's SURVIVING unit at recovery — a
 * random job_id dies with the daemon that minted it, orphaning the unit's completion (issue #3). A
 * rowless unit keys on its job_id (unique per admission; nothing durable ever looks for it).
 */
export function unitNameFor(job: Pick<JobEntry, "job_id" | "task_uuid">): string {
  return unitName(job.task_uuid ?? job.job_id);
}

/** Build the `TALLY_*` env conventions a transient unit carries for a job. */
export function jobEnv(entry: JobEntry, yieldFd?: number): Record<string, string> {
  const env: Record<string, string> = {
    [ENV_JOB_ID]: entry.job_id,
    [ENV_LEASE_EPOCH]: String(entry.lease_epoch),
  };
  if (entry.task_uuid !== null) env[ENV_TASK_UUID] = entry.task_uuid;
  if (entry.session_ref !== null) env[ENV_SESSION_REF] = entry.session_ref;
  if (yieldFd !== undefined) env[ENV_YIELD_FD] = String(yieldFd);
  return env;
}

/**
 * The lease-acquire helper: acquire the declared pool at the job's priority, sized by the VRAM-GB
 * cost. Returns the acquire outcome (granted lease or queued ticket) — the engine keeps a queued job
 * enqueued and retries on the next admission. `cost` defaults to 1 (a light unit); a heavy OCR/model
 * unit declares a larger cost via the pool budget math upstream.
 */
export async function acquireLease(
  leases: LeaseManager,
  pool: Pool,
  priority: Priority,
  cost: number,
): Promise<Awaited<ReturnType<LeaseManager["acquire"]>>> {
  return leases.acquire(pool, { cost, priority: priorityRank(priority) });
}
