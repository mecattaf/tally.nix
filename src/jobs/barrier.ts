// tally — the `--wait` / barrier primitive (IMPLEMENTATION-PLAN M2.2 `barrier.ts`; SPEC; CLI-SURFACE
// §1.1a "`--wait` / barrier semantics"; §2.4 `session.wait`).
//
// `--wait` blocks on a job's terminal delta (`job.completed | job.failed | job.evidence_fail`) off
// the daemon's own stream; the CLI process exit code mirrors the verdict (0 = evidence_pass/pass,
// non-zero = failed / clean-exit-no-artifact). A parallel barrier = **enqueue-N-await-N**: enqueue N
// jobs with `--barrier <gid>`, then await N terminal deltas for that group off ONE stream (this IS
// `wait_for_subagents`). `--timeout` bounds any wait but NEVER cancels (barrier ≠ cancel — the units
// keep running).
//
// The daemon side of the wait is `session.wait` (daemon-core owns the RPC); this module is the
// in-daemon barrier bookkeeping the jobs engine uses to satisfy a `job`-subject wait: it tracks the
// terminal deltas per task_uuid / per barrier group and resolves waiters as they arrive. The engine
// registers terminal transitions here; `daemon-core/wait.ts` reads a job-subject predicate through
// the engine's `awaitJobTerminal` / `awaitBarrier`.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Clock, JobState, Verdict } from "../contracts/index";
import { TallyError } from "../contracts/index";

/** A terminal delta the barrier tracker records for one job. */
export interface TerminalDelta {
  job_id: string;
  task_uuid: string | null;
  state: Extract<JobState, "completed" | "failed" | "evidence_fail">;
  verdict: Verdict;
  barrier: string | null;
}

/** The result of a satisfied job-subject wait. */
export interface JobWaitResult {
  timed_out: false;
  satisfied: TerminalDelta[];
}

/** The result of a timed-out job-subject wait (units keep running; barrier ≠ cancel). */
export interface JobWaitTimeout {
  timed_out: true;
  satisfied: TerminalDelta[];
  pending: number;
}

/** The exit code a `--wait` maps a terminal verdict to (0 = success, non-zero = failure). */
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

/** A pending waiter awaiting `count` terminal deltas matching a predicate. */
interface Waiter {
  count: number;
  matches: (d: TerminalDelta) => boolean;
  collected: TerminalDelta[];
  resolve: (r: JobWaitResult) => void;
  cancelTimer?: () => void;
}

/**
 * The in-daemon barrier tracker. The engine calls `recordTerminal` on every terminal job transition;
 * `awaitJobs` / `awaitBarrier` register a waiter that resolves once `count` matching terminals have
 * arrived (draining any already-recorded terminals first, so a late waiter still sees prior deltas).
 * `--timeout` resolves a waiter as timed-out WITHOUT cancelling the jobs.
 */
export class BarrierTracker {
  /** All terminal deltas seen so far, retained so a late `--wait` still resolves (idempotent dedupe by job_id). */
  private readonly terminals = new Map<string, TerminalDelta>();
  private readonly waiters = new Set<Waiter>();

  constructor(private readonly clock: Clock) {}

  /** Record a terminal delta and resolve any waiters it completes. Idempotent per job_id. */
  recordTerminal(delta: TerminalDelta): void {
    if (this.terminals.has(delta.job_id)) return; // one terminal per job (dedupe a resume overlap)
    this.terminals.set(delta.job_id, delta);
    for (const w of [...this.waiters]) {
      if (w.matches(delta)) {
        w.collected.push(delta);
        if (w.collected.length >= w.count) {
          this.settle(w);
        }
      }
    }
  }

  private settle(w: Waiter): void {
    if (!this.waiters.has(w)) return;
    this.waiters.delete(w);
    if (w.cancelTimer) w.cancelTimer();
    w.resolve({ timed_out: false, satisfied: w.collected.slice(0, w.count) });
  }

  /**
   * Await `count` terminal deltas matching `matches`. Drains already-recorded terminals first (so a
   * `--wait` issued after the job finished resolves immediately). On `timeoutMs` the promise
   * resolves as a timeout — the jobs are NOT cancelled (barrier ≠ cancel).
   */
  async await_(
    matches: (d: TerminalDelta) => boolean,
    count: number,
    timeoutMs?: number,
  ): Promise<JobWaitResult | JobWaitTimeout> {
    const already = [...this.terminals.values()].filter(matches);
    if (already.length >= count) {
      return { timed_out: false, satisfied: already.slice(0, count) };
    }
    return new Promise<JobWaitResult | JobWaitTimeout>((resolve) => {
      const waiter: Waiter = {
        count,
        matches,
        collected: [...already],
        resolve: resolve as (r: JobWaitResult) => void,
      };
      this.waiters.add(waiter);
      if (timeoutMs !== undefined) {
        waiter.cancelTimer = this.clock.setTimer(timeoutMs, () => {
          if (!this.waiters.has(waiter)) return;
          this.waiters.delete(waiter);
          resolve({
            timed_out: true,
            satisfied: waiter.collected,
            pending: count - waiter.collected.length,
          });
        });
      }
    });
  }

  /** Await one job's terminal delta by task_uuid (the `--wait` single-job barrier). */
  awaitJob(taskUuid: string, timeoutMs?: number): Promise<JobWaitResult | JobWaitTimeout> {
    return this.await_((d) => d.task_uuid === taskUuid, 1, timeoutMs);
  }

  /** Await one job's terminal delta by job_id (rowless units have no task_uuid). */
  awaitJobId(jobId: string, timeoutMs?: number): Promise<JobWaitResult | JobWaitTimeout> {
    return this.await_((d) => d.job_id === jobId, 1, timeoutMs);
  }

  /** Await N terminal deltas for a barrier group (enqueue-N-await-N; `wait_for_subagents`). */
  awaitBarrier(group: string, count: number, timeoutMs?: number): Promise<JobWaitResult | JobWaitTimeout> {
    return this.await_((d) => d.barrier === group, count, timeoutMs);
  }

  /** Await a set of job_ids all reaching terminal (the `job {job_ids[], count}` predicate). */
  awaitJobIds(jobIds: readonly string[], count: number, timeoutMs?: number): Promise<JobWaitResult | JobWaitTimeout> {
    const set = new Set(jobIds);
    return this.await_((d) => set.has(d.job_id), count, timeoutMs);
  }

  /** Terminal deltas recorded so far (for the snapshot / a completeness check). */
  recorded(): readonly TerminalDelta[] {
    return [...this.terminals.values()];
  }
}

/** Raised when a barrier wait is asked for a group/job that can never resolve (0 count). */
export class BarrierError extends TallyError {
  constructor(message: string) {
    super("invalid_params", message);
    this.name = "BarrierError";
    Object.setPrototypeOf(this, BarrierError.prototype);
  }
}
