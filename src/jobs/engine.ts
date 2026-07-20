// tally — the jobs engine: the spawn-tracked-agent-job orchestrator (IMPLEMENTATION-PLAN M2.2
// `engine.ts`; SPEC "Three planes", "The inner fold", "Evidence gate", "recover()"; CLI-SURFACE
// §1.1a Seam A, §1.1 queue verbs, §2.3 job.* events).
//
// THE one execution primitive. This module composes the whole plane:
//   enqueue (Seam A admission + dedup-by-existence) → priority queue → pls lease (before the GPU) →
//   transient-unit execution → evidence gate → terminal commit (TW row completion + witness line +
//   journald event + bus delta) → the `--wait`/barrier resolution.
// Plus recover() (re-present, never replay), preemption-as-policy (cooperative yield), and the
// queue cancel/pause/resume admission drain.
//
// The engine writes the §2.2 `jobs[]` snapshot leg into the single `model/store.ts` via the `Bus`
// (through the `SnapshotSectionProvider<"jobs">` seam — issue-3 single-store ruling); it NEVER owns
// a snapshot section itself and never imports session-model/detector. It mounts its RPC carriers
// (`queue.*`) + the jobs section provider through the `DaemonMount` seam at boot.
//
// All subprocess access is via the injected `Exec` seam; time via `Clock`. Every heavy unit emits a
// witness line — row or no row (the ledger is broader than the TW row set).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { randomUUID } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import type {
  Bus,
  Clock,
  DaemonMount,
  DaemonModule,
  Exec,
  ExecResult,
  JobBarrierDelta,
  JobBarrierProvider,
  JobRecord,
  LaborClass,
  Pool,
  SnapshotSectionProvider,
  Verdict,
} from "../contracts/index";
import {
  renderEvidenceSpec,
  systemClock,
  TallyError,
  unitExitFileIn,
  validateEnqueueParams,
  type EnqueueParams,
  type EvidenceCheck,
  type EnqueueResult,
  type JobStatusResult,
} from "../contracts/index";
import type { TaskChampion } from "../tw/index";
import type { LeaseManager } from "../pls/index";
import { parseRecord, WitnessLedger, type WitnessBody } from "../witness/index";
import { JournalEmitter, type EmitEvent } from "../journal/index";
import { adapterFor } from "./enqueue";
import { admit, DEFAULT_HEAVY_POOL, isGpuPool, rowSeedFor, type AdmittedJob } from "./enqueue";
import {
  isTerminal,
  toJobRecord,
  transition,
  type JobEntry,
  type JobState,
} from "./lifecycle";
import {
  acquireLease,
  jobEnv,
  PriorityQueue,
  TransientRunner,
  unitName,
  unitNameFor,
  type DispatchExecResult,
} from "./dispatch";
import { ArrayWitnessSource, probeDedup, type DedupResult } from "./dedup";
import { checkedPaths, runEvidenceGate, type GateResult } from "./evidence";
import { BarrierTracker, verdictExitCode, type TerminalDelta } from "./barrier";
import {
  markPreempted,
  prepareResume,
  shouldPreempt,
  YieldSignaller,
} from "./preempt";
import {
  executeReclaims,
  planRecovery,
  type LiveLease,
  type RecoveryPlan,
  type RepresentPlan,
} from "./recover";

/** The dependencies the engine is constructed with (all injectable seams — testable against fakes). */
export interface JobsEngineDeps {
  exec: Exec;
  bus: Bus;
  tw: TaskChampion;
  leases: LeaseManager;
  ledger: WitnessLedger;
  journal?: JournalEmitter;
  clock?: Clock;
  /** VRAM-GB cost estimator per job (defaults to 1 for a light unit). */
  costFor?: (entry: JobEntry) => number;
  /** Force the direct-spawn fallback (dev rig / systemd-absent host / tests). */
  forceDirect?: boolean;
  /**
   * The durable per-unit exit-record directory (`unit-exit/`, contracts/paths). Each transient
   * unit's ExecStopPost writes its `$EXIT_STATUS` here so recovery can still gate the exit-code
   * conjunct after `--collect` unloads an exited unit (issue #3). Omitted ⇒ no records are written
   * or read (a bare engine under test without a tmp env).
   */
  unitExitDir?: string;
  /** The attempt cap for recover()'s bounded requeue (default DEFAULT_ATTEMPT_CAP). */
  attemptCap?: number;
  /**
   * The daemon's resolved boot lease epoch (daemon-core's counter-file/pls-generation fence). Used as
   * the epoch stamp until a live pls grant supersedes it — NEVER the witness-ledger seq (a wholly
   * unrelated counter). Defaults to 1 when the engine is constructed without a daemon (tests).
   */
  bootEpoch?: number;
}

/** The result carrier of a queue operation (pause/resume) for the RPC. */
export interface QueueOpResult {
  ok: true;
  affected: number;
}

/** The result carrier of `queue.cancel` — carries the frozen §1.1 `was` + `lease_epoch` fields. */
export interface CancelResult {
  ok: true;
  affected: number;
  task_uuid: string | null;
  /** The prior JobState of the matched entry (the frozen §1.1 `was` field), or null if none matched. */
  was: JobState | null;
  /** The matched entry's lease_epoch (the fence value `--force` is defined by), or null. */
  lease_epoch: number | null;
}

/**
 * The jobs engine. One instance per daemon. Holds the in-flight job map, the priority queue, the
 * barrier tracker, and the sinks (bus + journal + witness). Drives every job from enqueue through
 * the terminal commit.
 */
export class JobsEngine implements DaemonModule {
  private readonly exec: Exec;
  private readonly bus: Bus;
  private readonly tw: TaskChampion;
  private readonly leases: LeaseManager;
  private readonly ledger: WitnessLedger;
  private readonly journal: JournalEmitter;
  private readonly clock: Clock;
  private readonly runner: TransientRunner;
  private readonly signaller: YieldSignaller;
  private readonly barriers: BarrierTracker;
  private readonly queue = new PriorityQueue();
  private readonly costFor: (entry: JobEntry) => number;
  private readonly forceDirect: boolean;
  private readonly attemptCap: number;
  /** The `unit-exit/` record dir (durable ExecStopPost exit statuses), or null when unconfigured. */
  private readonly unitExitDir: string | null;
  /** The daemon boot lease epoch (the counter-file/pls-generation fence) — the pre-grant epoch stamp. */
  private readonly bootEpoch: number;

  /** Every job the engine knows, by job_id (queued, running, and terminal until pruned). */
  private readonly jobs = new Map<string, JobEntry>();
  /** The max witness lsn applied — the recover() cheap-check checkpoint. */
  private maxAppliedLsn = 0;
  /** True while a dispatch loop tick is in flight (serializes admission). */
  private draining = false;
  /** Set when drive() is requested while already draining — the loop re-runs one more sweep. */
  private pendingDrive = false;
  /** The current in-flight drive() promise (for test settling / observability), or null when idle. */
  private driveInFlight: Promise<void> | null = null;
  /**
   * Pools whose last acquire QUEUED (lease held elsewhere). A blocked pool is skipped by the drive
   * loop so a queued job is NOT re-popped in a tight spin; it is cleared (and drive re-triggered) when
   * a lease frees or a delayed backoff re-drive fires. This is the park-until-a-lease-frees mechanism
   * the queued-lease comment promised (the missing re-admission the busy-spin defect exposed).
   */
  private readonly blockedPools = new Set<Pool>();
  /** Pending backoff re-drive timers per pool (so we do not stack timers for one blocked pool). */
  private readonly blockedTimers = new Map<Pool, () => void>();
  /** Backoff before re-attempting a queued pool's acquire (ms). */
  private readonly queuedRetryMs = 250;
  /** Terminal-job retention: prune a terminal entry this long after it settles (memory bound). */
  private readonly terminalRetentionMs = 5 * 60_000;
  /** Per-running-job heartbeat timer cancellers (armed at started, cleared at terminal). */
  private readonly heartbeatTimers = new Map<string, () => void>();
  /** Throttled cadence for the running `job.heartbeat` liveness + gpu-seconds tick. */
  private readonly heartbeatMs = 15_000;
  /** Poll cadence while an ADOPTED surviving transient unit is watched for its exit (issue #3). */
  private readonly adoptPollMs = 250;
  /** In-flight adoption watchers (surviving units parked until exit), for tests / drain to await. */
  private readonly adoptions = new Set<Promise<void>>();

  constructor(deps: JobsEngineDeps) {
    this.exec = deps.exec;
    this.bus = deps.bus;
    this.tw = deps.tw;
    this.leases = deps.leases;
    this.ledger = deps.ledger;
    this.journal = deps.journal ?? new JournalEmitter();
    this.clock = deps.clock ?? systemClock;
    this.runner = new TransientRunner(this.exec);
    this.signaller = new YieldSignaller(this.exec);
    this.barriers = new BarrierTracker(this.clock);
    this.costFor = deps.costFor ?? (() => 1);
    this.forceDirect = deps.forceDirect ?? false;
    this.attemptCap = deps.attemptCap ?? 5;
    this.bootEpoch = deps.bootEpoch ?? 1;
    this.unitExitDir = deps.unitExitDir ?? null;
  }

  // -------------------------------------------------------------------------------------------
  // DaemonModule mount — register the queue.* RPC carriers + the jobs snapshot section provider.
  // -------------------------------------------------------------------------------------------

  mount(daemon: DaemonMount): void {
    daemon.registerRpc("queue.enqueue", (params) => this.rpcEnqueue(params));
    daemon.registerRpc("queue.cancel", (params) => this.rpcCancel(params));
    daemon.registerRpc("queue.pause", (params) => this.rpcPause(params));
    daemon.registerRpc("queue.resume", (params) => this.rpcResume(params));
    daemon.registerRpc("queue.await_job", (params) => this.rpcAwaitJob(params));
    daemon.registerRpc("queue.await_barrier", (params) => this.rpcAwaitBarrier(params));
    // Register the job-barrier provider so daemon-core's `session.wait {subject:job}` resolves through
    // the BarrierTracker (which drains already-terminal deltas — an already-finished job resolves
    // immediately instead of hanging on future-only bus events).
    if (hasJobBarrierHook(daemon)) {
      daemon.registerJobBarrierProvider({
        awaitJobIds: async (jobIds, count, timeoutMs) => {
          const r = await this.barriers.awaitJobIds(jobIds, count, timeoutMs);
          if (r.timed_out) {
            const satisfiedIds = new Set(r.satisfied.map((d) => d.job_id));
            return {
              satisfied: r.satisfied.map(toBarrierDelta),
              timed_out: true,
              pending: jobIds.filter((id) => !satisfiedIds.has(id)),
            };
          }
          return { satisfied: r.satisfied.map(toBarrierDelta), timed_out: false, pending: [] };
        },
      });
    }
  }

  /**
   * `queue.await_job {task_uuid | job_id, timeout_ms?}` — block until the job's terminal delta
   * (§1.1a `--wait`). Served over the BarrierTracker so an ALREADY-terminal job (its delta fired
   * before the wait was issued — the normal sequential client flow) resolves immediately instead of
   * hanging. A rowed job is addressed by `task_uuid`; a rowless (task_uuid:null) unit by the
   * `job_id` its enqueue result carried — the tracker records EVERY terminal delta under job_id, so
   * the rowless wait is an exact identity match, never a fence heuristic (issue #4).
   */
  private async rpcAwaitJob(params: unknown): Promise<{ verdict: Verdict | null; exit_code: number; timed_out: boolean }> {
    const p = asObject(params);
    const taskUuid = typeof p.task_uuid === "string" ? p.task_uuid : undefined;
    const jobId = typeof p.job_id === "string" ? p.job_id : undefined;
    if (taskUuid === undefined && jobId === undefined) {
      throw new TallyError("invalid_params", "queue.await_job requires task_uuid or job_id");
    }
    const timeoutMs = typeof p.timeout_ms === "number" ? p.timeout_ms : undefined;
    const r = taskUuid !== undefined ? await this.waitForJob(taskUuid, timeoutMs) : await this.waitForJobId(jobId!, timeoutMs);
    return { verdict: r.verdict, exit_code: r.exitCode, timed_out: r.timedOut };
  }

  /**
   * `queue.await_barrier {group, count, timeout_ms?}` — block until `count` terminal deltas for the
   * barrier group arrive (enqueue-N-await-N). Filters by the barrier GID (the CLI stream count keyed
   * only on job_id and would release early on unrelated completions) and drains already-terminal
   * group members.
   */
  private async rpcAwaitBarrier(params: unknown): Promise<{ satisfied: number; exit_code: number; timed_out: boolean }> {
    const p = asObject(params);
    const group = typeof p.group === "string" ? p.group : undefined;
    const count = typeof p.count === "number" ? p.count : undefined;
    if (group === undefined || count === undefined) throw new TallyError("invalid_params", "queue.await_barrier requires group + count");
    const timeoutMs = typeof p.timeout_ms === "number" ? p.timeout_ms : undefined;
    const r = await this.waitForBarrier(group, count, timeoutMs);
    return { satisfied: r.satisfied, exit_code: r.exitCode, timed_out: r.timedOut };
  }

  /** The `jobs[]` snapshot section provider the single store reads at assembly (issue-3 ruling). */
  sectionProvider(): SnapshotSectionProvider<"jobs"> {
    return {
      section: "jobs",
      read: (): JobRecord[] => this.snapshotJobs(),
    };
  }

  /** The in-flight jobs projected to the §2.2 wire shape (non-terminal + recently-terminal). */
  snapshotJobs(): JobRecord[] {
    return [...this.jobs.values()].filter((j) => !isTerminal(j.state)).map(toJobRecord);
  }

  /** The barrier tracker — daemon-core/wait.ts reaches a job-subject `session.wait` through this. */
  barrierTracker(): BarrierTracker {
    return this.barriers;
  }

  // -------------------------------------------------------------------------------------------
  // Seam A — enqueue.
  // -------------------------------------------------------------------------------------------

  /** The `queue.enqueue` RPC handler: validate params, then run the full admission + drive. */
  private async rpcEnqueue(params: unknown): Promise<EnqueueResult> {
    const validated = validateEnqueueParams(params);
    return this.enqueue(validated);
  }

  /**
   * Admit one Seam-A enqueue and drive it. Returns the `EnqueueResult` once ADMITTED (or, for a
   * dedup hit, once SKIPPED). With `--wait` the caller (the CLI) blocks on the barrier separately via
   * {@link waitForJob}; the engine returns the admitted result immediately either way (the wait is a
   * CLI-side concern off the stream — SPEC "tally owns the wait off its stream").
   */
  async enqueue(params: EnqueueParams): Promise<EnqueueResult> {
    const leaseEpoch = this.currentEpoch();

    // --- Dedup-by-existence: probe BEFORE any row/lease/GPU (skip the run on a hit). ---
    const dedup = this.probeDedup(params);
    if (dedup.hit) {
      return this.reusedResult(params, dedup, leaseEpoch);
    }

    // --- Durable-row admission: a row (task_uuid) or a rowless unit (task_uuid null). ---
    let taskUuid: string | null = null;
    if (this.tw.admits({ source: params.source })) {
      taskUuid = randomUUID();
      const seed = rowSeedFor(params, taskUuid, leaseEpoch);
      await this.tw.createRow(seed);
    }

    const admitted = admit({ params, taskUuid, leaseEpoch });
    const entry = admitted.entry;
    this.jobs.set(entry.job_id, entry);
    // Push onto the queue HERE (before driving) so an in-flight drive loop's queue.next() picks it up
    // and a job admitted mid-drive is never stranded (the lost-enqueue defect).
    this.queue.push(entry);

    // --- job.enqueued (bus + journald), one vocabulary. ---
    this.emitEnqueued(entry);

    // --- Preemption-as-policy (SPEC "The inner fold"): if this admission outranks a running holder on
    // its pool, signal the holder to cooperatively yield NOW — a high-priority interactive job never
    // queues behind the whole batch. The signalled holder's own runJob re-presents it once its leaf
    // exits, freeing the lease for this requester. Checked at enqueue because the drive loop is busy
    // inside the holder's execute() and cannot itself observe the new arrival mid-run. ---
    await this.maybePreempt(entry);

    // --- Drive dispatch FIRE-AND-FORGET (§1.1a `--detach` default: "return once admitted/queued").
    // The engine returns the admitted result IMMEDIATELY; a `--wait` caller blocks on the barrier off
    // the stream separately. Awaiting drive() here would block the RPC for the job's whole wall-clock
    // (violating `--detach`) and serialize every caller behind the running batch. ---
    void this.drive().catch((err: unknown) => {
      process.stderr.write(`tally[engine]: drive() failed: ${err instanceof Error ? err.message : String(err)}\n`);
    });

    return this.admittedResult(entry, dedup);
  }

  private probeDedup(params: EnqueueParams): DedupResult {
    const source = new ArrayWitnessSource(this.ledgerRecords());
    return probeDedup({
      dedupKey: params.dedup_key ?? null,
      evidence: params.evidence,
      witness: source,
    });
  }

  /** A snapshot of the ledger's committed records for the dedup grep (ledger-as-truth). */
  private ledgerRecords(): import("../contracts/index").WitnessRecord[] {
    // The dedup source re-reads the JSONL file the ledger owns (ledger-as-truth; PS#9), parsed with
    // the witness parser so a torn trailing line is skipped, never trusted as a success anchor.
    return readWitnessJsonl(this.ledger.filePath);
  }

  // -------------------------------------------------------------------------------------------
  // The drive loop — admit ready jobs onto free leases, running each to terminal.
  // -------------------------------------------------------------------------------------------

  /**
   * Admit as many queued jobs as leases allow, honoring preemption. Serialized by the `draining`
   * guard so two concurrent enqueues do not double-admit onto the one lease. Each admitted job is run
   * to terminal via {@link runJob} (which acquires the lease, executes, gates, and commits). A drive
   * requested WHILE draining sets `pendingDrive` so the loop re-sweeps once more — a job admitted
   * mid-drive is never stranded (the lost-enqueue defect). A pool whose acquire QUEUED is marked
   * blocked and skipped by `queue.next()`, then re-driven on a backoff timer — never re-popped in a
   * tight spin (the busy-spin/livelock defect).
   */
  async drive(): Promise<void> {
    if (this.draining) {
      this.pendingDrive = true;
      // Return the in-flight drive so a caller awaiting drive() sees the whole sweep settle (tests /
      // the recover() boot path both `await this.drive()`).
      return this.driveInFlight ?? undefined;
    }
    this.draining = true;
    const run = (async () => {
      do {
        this.pendingDrive = false;
        let next = this.queue.next((pool) => this.blockedPools.has(pool));
        while (next !== undefined) {
          await this.runJob(next);
          next = this.queue.next((pool) => this.blockedPools.has(pool));
        }
      } while (this.pendingDrive);
    })();
    this.driveInFlight = run;
    try {
      await run;
    } finally {
      this.driveInFlight = null;
      this.draining = false;
    }
  }

  /**
   * Await the dispatch loop to quiesce: resolve once no drive is in flight and no admissible job
   * remains (a blocked pool's parked jobs are NOT admissible without a lease, so they do not keep this
   * pending). Exposed for tests + the recover() boot path that need the fire-and-forget drive to settle
   * before asserting terminal state. Bounded so a genuinely-stuck queue cannot hang a test forever.
   */
  async settle(maxTicks = 10_000): Promise<void> {
    for (let i = 0; i < maxTicks; i++) {
      if (this.driveInFlight !== null) {
        await this.driveInFlight;
        continue;
      }
      // No drive running: are there admissible (non-blocked, non-paused) jobs still queued?
      const admissible = this.queue.peekAll().some((j) => !this.blockedPools.has(j.pool) && !this.queue.isPaused(j.pool));
      if (!admissible) return;
      await this.drive();
    }
  }

  /**
   * Run one admitted job to terminal: acquire the lease (before the GPU), dispatch as a transient
   * unit, run the evidence gate, and commit (witness + journald + TW completion + terminal delta).
   * On a QUEUED lease the pool is marked blocked, the job is re-queued (both-or-queue), preemption is
   * consulted for a higher-priority requester, and a backoff re-drive is scheduled — the job is NOT
   * re-popped immediately (no busy-spin).
   */
  private async runJob(entry: JobEntry): Promise<void> {
    // A job that went terminal (e.g. cancelled) between being popped and dispatched must not run.
    if (isTerminal(entry.state)) return;

    // --- Acquire the lease BEFORE touching the GPU (SPEC). ---
    const cost = this.costFor(entry);
    const outcome = await acquireLease(this.leases, entry.pool, entry.priority, cost);

    // The entry may have gone terminal (a concurrent cancel) WHILE the acquire subprocess ran. If so,
    // release any granted lease and return — never dispatch a cancelled/failed job.
    if (isTerminal(entry.state)) {
      if (outcome.kind === "granted") await outcome.lease.release();
      return;
    }

    if (outcome.kind === "queued") {
      // Both-or-queue: no partial GPU. Park the job on a blocked pool (so drive() skips it instead of
      // re-popping it) and consult preemption — a higher-priority requester signals a lower-priority
      // running holder to cooperatively yield. Re-drive on a backoff timer (or on the lease-freed
      // re-drive a holder's terminal commit triggers).
      this.queue.push(entry);
      this.blockedPools.add(entry.pool);
      await this.maybePreempt(entry);
      this.scheduleBlockedRetry(entry.pool);
      return;
    }
    const lease = outcome.lease;
    entry.lease_id = lease.leaseId;
    entry.lease_epoch = lease.leaseEpoch;
    // This pool now has a live grant — unblock it (a queued sibling may proceed after this holder
    // releases; the finally block re-drives).
    this.clearBlocked(entry.pool);

    try {
      // --- dispatched (bus + journald). ---
      // The unit name is DETERMINISTIC for a rowed job (keyed on task_uuid, not the ephemeral
      // job_id) so a rebooted daemon's recovery can find a surviving unit (issue #3).
      const unit = unitNameFor(entry);
      entry.unit = unit;
      transition(entry, entry.state === "preempted" ? "resumed" : "dispatched");
      if (entry.state === "resumed") {
        this.emitResumed(entry);
      } else {
        this.emitDispatched(entry);
      }

      // --- started (bus + journald). ---
      const startState: JobState = entry.state === "resumed" ? "started" : "started";
      transition(entry, startState);
      entry.started_at_ms = this.clock.now();
      this.emitStarted(entry);
      // Arm the throttled `job.heartbeat` liveness + running-gpu-seconds tick (§2.3 frozen event) so a
      // long-running job is distinguishable from a hung one on the wire between started and terminal.
      this.armHeartbeat(entry);

      // --- Execute the transient unit. ---
      const run = await this.execute(entry);
      entry.ended_at_ms = run.endedAtMs;
      entry.exitCode = run.exitCode;
      entry.gpu_seconds = this.spanSeconds(entry);

      // A force-cancel that fenced this holder WHILE it ran leaves it in state started/resumed with the
      // `fenced` flag set: skip the normal terminal commit, write a cancelled witness+delta instead
      // (the run's own side effects are done; we do not resurrect the deleted TW row as completed).
      if (entry.fenced === true) {
        await this.commitCancelled(entry, run);
        return;
      }

      // A yield was requested (preemption): the holder cooperatively exited. Re-present it via resume
      // instead of committing a terminal — the higher-priority requester takes the lease next.
      if (entry.yieldRequested === true) {
        this.preemptInFlight(entry);
        return;
      }

      // Refine session_ref/model/trace from the run's stdout (harness-reported).
      this.refineFromRun(entry, run.stdout);

      // --- Evidence gate. ---
      const gate = runEvidenceGate({
        exitCode: run.exitCode,
        wallClockSeconds: this.wallSeconds(entry),
        evidence: entry.evidence,
      });
      entry.verdict = gate.verdict;

      // --- Commit: witness line + journald + terminal delta + TW completion. ---
      await this.commit(entry, run, gate);
    } finally {
      // Cancel the heartbeat timer (the run reached terminal / preempted / fenced).
      this.clearHeartbeat(entry.job_id);
      // RAII release — the SINGLE release path (process-exit analogue for the in-daemon holder).
      await lease.release();
      entry.lease_id = null;
      // The pool's lease just freed: unblock it and re-drive so a parked sibling is admitted promptly
      // (the lease-freed re-admission the queued-lease path relies on).
      this.clearBlocked(entry.pool);
      this.reDrive();
      // Schedule terminal-entry pruning so a long-running daemon does not accumulate job records
      // without bound (the pruning the "terminal until pruned" comment promised).
      this.schedulePrune(entry.job_id);
    }
  }

  /** Arm a throttled per-job heartbeat: emits `job.heartbeat {job_id, gpu_seconds}` + the journald twin. */
  private armHeartbeat(entry: JobEntry): void {
    this.clearHeartbeat(entry.job_id);
    const tick = () => {
      const live = this.jobs.get(entry.job_id);
      if (live === undefined || isTerminal(live.state)) {
        this.clearHeartbeat(entry.job_id);
        return;
      }
      const gpuSeconds = this.wallSeconds(live);
      this.bus.emit("job.heartbeat", { job_id: live.job_id, gpu_seconds: gpuSeconds });
      this.journalEmit(live, "heartbeat", { gpu_seconds: gpuSeconds });
      this.heartbeatTimers.set(entry.job_id, this.clock.setTimer(this.heartbeatMs, tick));
    };
    this.heartbeatTimers.set(entry.job_id, this.clock.setTimer(this.heartbeatMs, tick));
  }

  /** Cancel a job's heartbeat timer, if armed. */
  private clearHeartbeat(jobId: string): void {
    const cancel = this.heartbeatTimers.get(jobId);
    if (cancel) {
      cancel();
      this.heartbeatTimers.delete(jobId);
    }
  }

  /**
   * Fire a fresh drive() without awaiting it (a lease freed / a blocked pool cleared). Safe to call
   * from within a drive: the `draining` guard turns it into a `pendingDrive` re-sweep.
   */
  private reDrive(): void {
    void this.drive().catch((err: unknown) => {
      process.stderr.write(`tally[engine]: re-drive failed: ${err instanceof Error ? err.message : String(err)}\n`);
    });
  }

  /** Clear a pool's blocked flag + cancel any pending backoff timer for it. */
  private clearBlocked(pool: Pool): void {
    this.blockedPools.delete(pool);
    const cancel = this.blockedTimers.get(pool);
    if (cancel) {
      cancel();
      this.blockedTimers.delete(pool);
    }
  }

  /**
   * Schedule a backoff re-drive for a blocked pool: after `queuedRetryMs` unblock the pool and drive,
   * so a queued acquire is retried WITHOUT a tight spin. Idempotent per pool (one timer at a time).
   */
  private scheduleBlockedRetry(pool: Pool): void {
    if (this.blockedTimers.has(pool)) return;
    const cancel = this.clock.setTimer(this.queuedRetryMs, () => {
      this.blockedTimers.delete(pool);
      this.blockedPools.delete(pool);
      this.reDrive();
    });
    this.blockedTimers.set(pool, cancel);
  }

  /** Schedule pruning of a terminal job entry after the retention window (memory bound). */
  private schedulePrune(jobId: string): void {
    this.clock.setTimer(this.terminalRetentionMs, () => {
      const entry = this.jobs.get(jobId);
      if (entry !== undefined && isTerminal(entry.state)) {
        this.jobs.delete(jobId);
      }
    });
  }

  /**
   * Commit a force-cancelled RUNNING holder: append a `cancelled` witness line + terminal delta, but
   * do NOT re-complete the deleted TW row (cancel already deleted it). The run's side effects have
   * already happened; this records the cancellation truthfully instead of a phantom `completed`.
   */
  private async commitCancelled(entry: JobEntry, run: DispatchExecResult): Promise<void> {
    entry.verdict = "cancelled";
    const gate: GateResult = {
      verdict: "cancelled",
      passed: false,
      artifactHash: null,
      checks: [],
      cleanExitNoArtifact: false,
    };
    // Transition started/resumed → failed (a cancelled terminal is modelled as failed with verdict
    // cancelled, matching a queue-cancel of a queued job).
    transition(entry, "failed");
    const witnessRecord = this.appendWitness(entry, run, gate);
    entry.witness_lsn = witnessRecord.seq;
    this.maxAppliedLsn = Math.max(this.maxAppliedLsn, witnessRecord.seq);
    this.emitWitnessEmitted(entry);
    this.emitFailedCancelled(entry);
    this.barriers.recordTerminal({
      job_id: entry.job_id,
      task_uuid: entry.task_uuid,
      state: "failed",
      verdict: "cancelled",
      barrier: entry.barrier,
    });
  }

  /** Execute a job's leaf argv as a transient unit (or direct fallback), carrying TALLY_* env. */
  private async execute(entry: JobEntry): Promise<DispatchExecResult> {
    const env = jobEnv(entry);
    const unit = entry.unit ?? unitNameFor(entry);
    const exitFile = this.prepareUnitExitFile(unit);
    const opts = {
      unit,
      env,
      forceDirect: this.forceDirect,
      ...(exitFile !== null ? { exitFile } : {}),
      ...(entry.cwd !== null ? { cwd: entry.cwd } : entry.worktree !== null ? { cwd: entry.worktree } : {}),
    };
    try {
      return await this.runner.run(entry.argv, opts);
    } finally {
      // The exit was OBSERVED (this daemon watched the span) — the durable record's only purpose is
      // the unobserved restart-window exit, so drop it here instead of accumulating one per run.
      this.clearUnitExitRecord(unit);
    }
  }

  /**
   * Ready a unit's durable exit-record file for a fresh dispatch: ensure `unit-exit/` exists and
   * drop any STALE record a prior attempt's ExecStopPost left (a crash between that unit's exit and
   * its reconciliation) so this attempt can never be gated on last attempt's status. Returns the
   * file path to hand `systemd-run`, or null when no dir is configured (or the fs refused —
   * best-effort: dispatch must never block on the record plumbing).
   */
  private prepareUnitExitFile(unit: string): string | null {
    if (this.unitExitDir === null) return null;
    try {
      mkdirSync(this.unitExitDir, { recursive: true });
      const file = unitExitFileIn(this.unitExitDir, unit);
      rmSync(file, { force: true });
      return file;
    } catch {
      return null;
    }
  }

  /** Best-effort removal of a unit's exit record (observed exits / consumed reconciliations). */
  private clearUnitExitRecord(unit: string): void {
    if (this.unitExitDir === null) return;
    try {
      rmSync(unitExitFileIn(this.unitExitDir, unit), { force: true });
    } catch {
      /* best-effort — a lingering record is dropped at the unit's next dispatch */
    }
  }

  /**
   * Consume (read + delete) a unit's durable exit record — the `$EXIT_STATUS` its ExecStopPost
   * persisted, the only exit evidence that survives `systemd-run --collect` unloading an exited
   * unit while no daemon watched (issue #3). Returns null when no usable record exists: absent
   * dir/file, or a non-numeric status (a signal name like `KILL` — suspect work, which then falls
   * through to the dedup probe / the planned re-present).
   */
  private consumeUnitExitRecord(unit: string): number | null {
    if (this.unitExitDir === null) return null;
    const file = unitExitFileIn(this.unitExitDir, unit);
    try {
      if (!existsSync(file)) return null;
      const raw = readFileSync(file, "utf8").trim();
      rmSync(file, { force: true });
      return /^\d+$/.test(raw) ? Number(raw) : null;
    } catch {
      return null;
    }
  }

  /** Refine the entry's session_ref/model/trace_ref from a completed run's captured stdout. */
  private refineFromRun(entry: JobEntry, stdout: string): void {
    const adapter = adapterFor(entry.agent_kind);
    const ext = adapter.extract(stdout);
    if (ext.sessionRef !== undefined && ext.sessionRef !== null) entry.session_ref = ext.sessionRef;
    if (ext.model !== undefined && ext.model !== null) entry.model = ext.model;
    if (ext.traceRef !== undefined && ext.traceRef !== null) entry.trace_ref = ext.traceRef;
  }

  // -------------------------------------------------------------------------------------------
  // Terminal commit — witness line, journald, terminal delta, TW completion.
  // -------------------------------------------------------------------------------------------

  private async commit(entry: JobEntry, run: DispatchExecResult, gate: GateResult): Promise<void> {
    // Evidence event (pass/fail) — mirrors journald evidence_pass/evidence_fail.
    const evidenceEvent: JobState = gate.passed ? "evidence_pass" : "evidence_fail";
    transition(entry, evidenceEvent);
    this.emitEvidence(entry, gate);

    // The witness line — EVERY heavy unit emits one, row or no row (appendix).
    const witnessRecord = this.appendWitness(entry, run, gate);
    entry.witness_lsn = witnessRecord.seq;
    this.maxAppliedLsn = Math.max(this.maxAppliedLsn, witnessRecord.seq);
    this.emitWitnessEmitted(entry);

    // Terminal state + delta.
    const terminal: JobState = gate.passed ? "completed" : "failed";
    transition(entry, terminal);

    // TW row completion (trust:unreviewed) — only for a durable row.
    if (entry.task_uuid !== null) {
      await this.tw.complete(entry.task_uuid, { laborClass: entry.labor_class, trust: "unreviewed" });
    }

    if (terminal === "completed") {
      this.emitCompleted(entry, gate);
    } else {
      this.emitFailed(entry, gate);
    }

    // Record the terminal delta for the barrier / `--wait`.
    const deltaState: TerminalDelta["state"] = gate.cleanExitNoArtifact
      ? "evidence_fail"
      : terminal === "completed"
        ? "completed"
        : "failed";
    this.barriers.recordTerminal({
      job_id: entry.job_id,
      task_uuid: entry.task_uuid,
      state: deltaState,
      verdict: entry.verdict ?? "failed",
      barrier: entry.barrier,
    });
  }

  /** Append the canonical witness line for a terminal job (proof-of-labor). */
  private appendWitness(entry: JobEntry, run: DispatchExecResult, gate: GateResult): { seq: number } {
    const gpuSeconds = isGpuPool(entry.pool) ? entry.gpu_seconds : null;
    const body: WitnessBody = {
      task_uuid: entry.task_uuid,
      transition_timestamp: this.clock.nowIso(),
      verdict: entry.verdict ?? "failed",
      exit_code: run.exitCode,
      artifact_content_hash: gate.artifactHash,
      gpu_seconds: gpuSeconds,
      wall_clock: this.wallSeconds(entry),
      attempt: entry.attempt,
      lease_epoch: entry.lease_epoch,
      dedup_key: entry.dedup_key,
      labor_class: entry.labor_class,
      ...(entry.trace_ref !== null ? { trace_ref: entry.trace_ref } : {}),
      pool: isGpuPool(entry.pool) ? entry.pool : null,
      charge: isGpuPool(entry.pool)
        ? { unit: "gpu-seconds", amount: entry.gpu_seconds, class: "verifiable" as const }
        : null,
      model: entry.model,
    };
    return this.ledger.append(body);
  }

  // -------------------------------------------------------------------------------------------
  // Preemption-as-policy — cooperative yield above the non-preemptible lease.
  // -------------------------------------------------------------------------------------------

  /**
   * Consider preempting a running lower-priority holder for a higher-priority requester. Signals the
   * holder to yield (SIGUSR1 at a safe checkpoint) and MARKS it yield-requested — but does NOT
   * transition it or re-queue it here: the holder's own in-flight `runJob` owns its terminal (it
   * observes the flag when `execute()` resolves after the cooperative exit, releases the lease, then
   * re-presents the job via resume). Transitioning the still-executing holder here would leave it in
   * `preempted` when its own runJob reaches commit() → illegal transition, aborting the drive loop.
   * Returns true when a yield was requested.
   */
  async maybePreempt(requester: JobEntry): Promise<boolean> {
    const running = [...this.jobs.values()].filter((j) => j.state === "started" || j.state === "resumed");
    const decision = shouldPreempt(requester, running);
    if (decision === null) return false;
    const holder = decision.holder;
    if (holder.yieldRequested === true) return false; // already asked to yield
    holder.yieldRequested = true;
    holder.yieldReason = decision.reason;
    // Signal the cooperative yield (never a forced eviction). The holder's leaf exits at its next safe
    // checkpoint; its runJob then observes the flag and re-presents it (see the yield handling below).
    await this.signaller.signalYield(holder);
    return true;
  }

  /**
   * The in-flight holder observed a yield request after its leaf cooperatively exited: transition it
   * `preempted`, emit the delta, prepare the resume (bump attempt, labor_class:recovered), and re-queue
   * it so the drive loop re-dispatches it via `--resume` once the pool frees. Called by runJob before
   * the terminal commit when `entry.yieldRequested` is set. The lease is released by runJob's finally.
   */
  private preemptInFlight(entry: JobEntry): void {
    const reason = entry.yieldReason ?? "preempted";
    entry.yieldRequested = false;
    delete entry.yieldReason;
    markPreempted(entry);
    transition(entry, "preempted");
    this.emitPreempted(entry, reason);
    prepareResume(entry); // sets state back to "preempted" + bumps attempt/labor_class
    this.queue.push(entry);
  }

  // -------------------------------------------------------------------------------------------
  // recover() — re-present, never replay (five invariants).
  // -------------------------------------------------------------------------------------------

  /**
   * Plan + execute recovery on boot: reconcile the witness head, reclaim fenced leases, re-present
   * undeleted un-acked in-budget rows via resume, abandon over-attempt rows. Returns the plan (for
   * observability / tests). This is called once by the composition root before serving.
   */
  async recover(input: {
    liveLeases?: readonly LiveLease[];
    ackedTaskUuids?: ReadonlySet<string>;
  } = {}): Promise<RecoveryPlan> {
    // Invariant 1 — witness reconciliation. A task_uuid with a TERMINAL (pass/fail) witness line but a
    // still-pending TW row is the torn crash window between ledger.append and tw.complete: the run
    // DID finish; completing the row (and treating it as acked) instead of re-presenting it prevents
    // re-executing already-completed work at boot. We derive the applied checkpoint from the real
    // witness lines (the max terminal seq), so witnessReconciled compares the ledger head against an
    // actual checkpoint rather than itself.
    const witnessed = this.witnessedTerminalsByTask();
    const acked = new Set(input.ackedTaskUuids ?? []);
    // Complete any pending row whose work is already witnessed terminal, and mark it acked so
    // planRecovery does not re-present it.
    for (const [taskUuid, seq] of witnessed) {
      if (acked.has(taskUuid)) continue;
      const row = await this.tw.getRow(taskUuid);
      if (row !== undefined && (row.status === "pending" || row.status === "waiting")) {
        await this.tw.complete(taskUuid, { trust: "unreviewed" }).catch(() => undefined);
      }
      acked.add(taskUuid);
      this.maxAppliedLsn = Math.max(this.maxAppliedLsn, seq);
    }

    // Invariant 2 corollary — never re-present a task that a LIVE in-process entry is already driving
    // (a drain on a live daemon calls recover(); the same durable row is pending while its job runs).
    const liveTaskUuids = new Set<string>();
    for (const j of this.jobs.values()) {
      if (j.task_uuid !== null && !isTerminal(j.state)) liveTaskUuids.add(j.task_uuid);
    }
    for (const t of liveTaskUuids) acked.add(t);

    const plan = await planRecovery({
      tw: this.tw,
      witnessHead: this.ledger.chainHead,
      lastAppliedLsn: this.maxAppliedLsn || this.ledger.chainHead.seq,
      currentEpoch: this.currentEpoch(),
      liveLeases: input.liveLeases ?? [],
      ackedTaskUuids: acked,
      attemptCap: this.attemptCap,
    });

    // Invariant 3 — reclaim fenced/holderless leases.
    await executeReclaims(this.leases, plan);

    // Invariant 5 — abandon over-attempt rows (mark the TW row + emit a failed delta).
    for (const ab of plan.abandon) {
      const row = await this.tw.getRow(ab.task_uuid);
      if (row !== undefined) {
        await this.tw.patchManaged(ab.task_uuid, { status: "deleted" });
      }
      this.barriers.recordTerminal({
        job_id: ab.task_uuid,
        task_uuid: ab.task_uuid,
        state: "failed",
        verdict: "failed",
        barrier: null,
      });
    }

    // Invariant 4 — re-present undeleted rows via resume (labor_class:recovered). Skip any row a live
    // entry is already driving (belt-and-braces over the ack guard above — no duplicate execution).
    // BEFORE scheduling attempt 2, each row is reconciled against reality (issue #3): a transient
    // unit intentionally outlives the daemon, so its restart-window work may already be done — a
    // surviving unit is ADOPTED, an unobserved exit is gated from Result/ExecMainStatus + disk, and
    // dedup-by-existence completes already-witnessed work as `reused` instead of re-running it.
    for (const rep of plan.represent) {
      if (liveTaskUuids.has(rep.task_uuid)) continue;
      if (await this.reconcileBeforeRepresent(rep)) continue;
      this.representNow(rep);
    }

    await this.drive();
    return plan;
  }

  /**
   * Scan the committed witness ledger for the LATEST terminal (pass/fail/clean-exit-no-artifact/failed)
   * line per task_uuid, returning a map task_uuid → that line's seq. Used at boot to detect the torn
   * crash window (a witnessed-terminal run whose TW row is still pending) so recover() completes the
   * row instead of re-running it. A rowless (task_uuid null) witness line is ignored.
   */
  private witnessedTerminalsByTask(): Map<string, number> {
    const out = new Map<string, number>();
    for (const rec of this.ledgerRecords()) {
      if (rec.task_uuid === null) continue;
      // Any recorded verdict is a terminal outcome (a witness line is only ever appended at terminal).
      const prev = out.get(rec.task_uuid);
      if (prev === undefined || rec.seq > prev) out.set(rec.task_uuid, rec.seq);
    }
    return out;
  }

  /** Build a re-presented job entry from a recovered TW row (labor_class:recovered, bumped attempt). */
  private representEntry(taskUuid: string, row: import("../contracts/index").TaskRow, sessionRef: string | null, attempt: number): JobEntry {
    const params = this.paramsFromRow(row);
    const admitted: AdmittedJob = admit({ params, taskUuid, leaseEpoch: this.currentEpoch() });
    const entry = admitted.entry;
    entry.attempt = attempt;
    entry.labor_class = "recovered";
    entry.session_ref = sessionRef;
    // Use the adapter's resume argv when a session ref exists (re-present, never replay).
    const adapter = adapterFor(entry.agent_kind);
    const resumed = adapter.resume({ params, command: admitted.leaf.argv.slice(), session: entry.session }, sessionRef);
    entry.argv = resumed.argv;
    return entry;
  }

  /**
   * Reconstruct the minimal Seam-A params from a durable TW row (for a re-present). The job's TRUE
   * identity lives in the write-once `argv_json`/`evidence_json` UDAs (issue #2): the persisted argv
   * is taken VERBATIM (never re-tokenized — joining-then-splitting `description` destroys quoting)
   * and the enqueue-time evidence gates are re-armed so a recovered attempt is witnessed against its
   * original requirements (PS#9 "never self-report"). Only a pre-UDA legacy row (or an intake row,
   * whose description IS its command) falls back to tokenizing the description.
   */
  private paramsFromRow(row: import("../contracts/index").TaskRow): EnqueueParams {
    const p: EnqueueParams = {
      priority: (row.priority_class ?? "medium") as EnqueueParams["priority"],
      source: (row.source ?? "orchestrator") as EnqueueParams["source"],
      kind: (row.agent ?? "shell") as EnqueueParams["kind"],
    };
    const argv = parseJsonArrayUda(row.argv_json);
    if (argv !== undefined && argv.length > 0 && argv.every((a) => typeof a === "string")) {
      p.argv = argv as string[];
    } else {
      p.invocation = row.description;
    }
    const evidence = parseJsonArrayUda(row.evidence_json);
    if (evidence !== undefined) p.evidence = evidence as EvidenceCheck[];
    if (row.cwd !== undefined) p.cwd = row.cwd;
    else if (row.worktree !== undefined) p.worktree = row.worktree;
    if (row.pool !== undefined) p.pool = row.pool;
    if (row.model_class !== undefined) p.model_class = row.model_class;
    if (row.dedup_key !== undefined) p.dedup_key = row.dedup_key;
    if (row.session_ref !== undefined && row.session_ref !== null) p.session = row.session_ref;
    return p;
  }

  // -------------------------------------------------------------------------------------------
  // Pre-represent reconciliation (issue #3) — a transient unit outlives the daemon BY DESIGN, so a
  // restart-window completion must be adopted and witnessed, never silently re-run as attempt 2.
  // -------------------------------------------------------------------------------------------

  /** Execute one represent-plan entry: admit the re-presented entry and queue it for the drive loop. */
  private representNow(rep: RepresentPlan): void {
    const entry = this.representEntry(rep.task_uuid, rep.row, rep.session_ref, rep.attempt);
    this.jobs.set(entry.job_id, entry);
    this.emitEnqueued(entry);
    this.queue.push(entry);
  }

  /**
   * Reconcile ONE planned re-present against reality before scheduling attempt 2:
   *   (a) the row's transient unit — findable by its DETERMINISTIC `tally-job-<task_uuid>` name —
   *       may still be RUNNING: adopt it (park the row, watch for exit, then the normal evidence
   *       gate) instead of re-presenting, which would race a duplicate concurrent run;
   *   (b) it may have EXITED unobserved: the exit-code conjunct comes from its ExecMainStatus in
   *       the narrow window where the unit is still loaded, and — the NORMAL case — from the
   *       durable ExecStopPost exit record once `--collect` has unloaded it (real systemd collects
   *       an exited transient unit within moments, long before the restarted daemon probes it);
   *       the artifact/hash conjuncts gate what is on disk — a pass witnesses the attempt-1
   *       completion;
   *   (c) dedup-by-existence (the same gate enqueue applies, over the row's restored evidence spec —
   *       issue #2 persists it): a prior SUCCESS witness line with the row's dedup key + the
   *       artifact intact ⇒ complete as `reused`, tagged out of canonical GPU-seconds (DECISIONS
   *       appendix "Dedup-by-existence").
   * Returns true when the row was settled (adopted or completed) — the caller skips the re-present.
   */
  private async reconcileBeforeRepresent(rep: RepresentPlan): Promise<boolean> {
    const params = this.paramsFromRow(rep.row);
    const unit = unitName(rep.task_uuid);
    const probe = await this.queryUnit(unit);
    if (probe !== null && probe.active) {
      this.adoptSurvivingUnit(rep, params);
      return true;
    }
    if (probe !== null && probe.loaded) {
      if (await this.completeIfGatePasses(rep, probe.exitCode ?? 0, params)) return true;
    } else {
      // Already COLLECTED — the normal answer for a unit that exited during the restart window:
      // `systemd-run --collect` unloads a stopped transient unit immediately, so `systemctl show`
      // says LoadState=not-found and its ExecMainStatus is gone. The unit's ExecStopPost exit
      // record is the exit-code conjunct that survives.
      const recorded = this.consumeUnitExitRecord(unit);
      if (recorded !== null && (await this.completeIfGatePasses(rep, recorded, params))) return true;
    }
    const dedup = probeDedup({
      dedupKey: rep.row.dedup_key ?? null,
      evidence: params.evidence,
      witness: new ArrayWitnessSource(this.ledgerRecords()),
    });
    if (dedup.hit) {
      await this.completeUnobserved(rep, {
        verdict: "reused",
        exitCode: 0,
        artifactHash: dedup.artifactHash,
        laborClass: "reused",
      });
      return true;
    }
    return false;
  }

  /**
   * Adopt a row's SURVIVING transient unit: the row is parked (not re-presented — a re-present
   * would run a duplicate concurrently), the unit is watched to exit on a poll timer, and the
   * normal evidence gate then decides — a pass witnesses the attempt-1 completion; a fail schedules
   * the re-present the recovery plan intended.
   */
  private adoptSurvivingUnit(rep: RepresentPlan, params: EnqueueParams): void {
    const unit = unitName(rep.task_uuid);
    let tracked: Promise<void>;
    const watch = (async () => {
      let probe = await this.queryUnit(unit);
      while (probe !== null && probe.active) {
        await new Promise<void>((resolve) => this.clock.setTimer(this.adoptPollMs, resolve));
        probe = await this.queryUnit(unit);
      }
      // The watched unit can be COLLECTED between polls (`--collect` unloads it the moment it
      // stops), losing its ExecMainStatus: the ExecStopPost exit record then carries the status.
      const exit = probe !== null && probe.loaded ? (probe.exitCode ?? 0) : (this.consumeUnitExitRecord(unit) ?? 0);
      if (await this.completeIfGatePasses(rep, exit, params)) return;
      // The adopted run did not prove its work — NOW run the re-present the plan scheduled.
      this.representNow(rep);
      this.reDrive();
    })();
    tracked = watch
      .catch((err: unknown) => {
        process.stderr.write(`tally[engine]: adoption watch for ${unit} failed: ${err instanceof Error ? err.message : String(err)}\n`);
      })
      .finally(() => {
        this.adoptions.delete(tracked);
      });
    this.adoptions.add(tracked);
  }

  /** Await every in-flight adoption watcher (a surviving unit parked until exit) to settle. */
  async adoptionsSettled(): Promise<void> {
    while (this.adoptions.size > 0) {
      await Promise.all([...this.adoptions]);
    }
  }

  /**
   * Gate an UNOBSERVED exited attempt (the unit outlived the daemon; nobody watched its span):
   * exit-code conjunct from systemd's ExecMainStatus, artifact/hash conjuncts from what is on disk
   * (PS#9 "never self-report"). On pass, witness + complete the attempt-1 work; on fail, return
   * false so the caller falls through to the re-present.
   */
  private async completeIfGatePasses(rep: RepresentPlan, exitCode: number, params: EnqueueParams): Promise<boolean> {
    const gate = runEvidenceGate({ exitCode, wallClockSeconds: 0, evidence: params.evidence });
    if (!gate.passed) return false;
    await this.completeUnobserved(rep, {
      verdict: gate.verdict,
      exitCode,
      artifactHash: gate.artifactHash,
      laborClass: rep.row.labor_class ?? "fresh",
    });
    return true;
  }

  /**
   * Witness + complete an attempt whose SPAN was never observed (the dispatching daemon is gone):
   * `gpu_seconds`/`charge` are null — the completion is the proof, the metering is unknowable (the
   * PS#9 treatment cloud runs get) — and the attempt number is the ROW's recorded attempt (the work
   * already done), never the bumped re-present attempt.
   */
  private async completeUnobserved(
    rep: RepresentPlan,
    outcome: { verdict: Verdict; exitCode: number; artifactHash: string | null; laborClass: LaborClass },
  ): Promise<void> {
    const pool: Pool = rep.row.pool ?? DEFAULT_HEAVY_POOL;
    const body: WitnessBody = {
      task_uuid: rep.task_uuid,
      transition_timestamp: this.clock.nowIso(),
      verdict: outcome.verdict,
      exit_code: outcome.exitCode,
      artifact_content_hash: outcome.artifactHash,
      gpu_seconds: null,
      wall_clock: 0,
      attempt: typeof rep.row.attempt === "number" ? rep.row.attempt : rep.attempt - 1,
      lease_epoch: typeof rep.row.lease_epoch === "number" ? rep.row.lease_epoch : this.currentEpoch(),
      dedup_key: rep.row.dedup_key ?? null,
      labor_class: outcome.laborClass,
      pool: isGpuPool(pool) ? pool : null,
      charge: null,
      model: null,
    };
    const record = this.ledger.append(body);
    this.maxAppliedLsn = Math.max(this.maxAppliedLsn, record.seq);
    // The attempt is settled — drop any leftover exit record (e.g. the completion came from a
    // still-loaded probe's ExecMainStatus or the dedup gate, leaving the ExecStopPost file behind).
    this.clearUnitExitRecord(unitName(rep.task_uuid));
    await this.tw.complete(rep.task_uuid, { laborClass: outcome.laborClass, trust: "unreviewed" }).catch(() => undefined);
    this.barriers.recordTerminal({
      job_id: rep.task_uuid,
      task_uuid: rep.task_uuid,
      state: "completed",
      verdict: outcome.verdict,
      barrier: null,
    });
  }

  /**
   * Probe systemd for a transient unit's state (`systemctl --user show`). Returns null when the
   * probe is unavailable (no systemctl on the host — the dev rig / direct-spawn fallback) or
   * errored: reconciliation then falls through to the dedup gate, never blocking recovery.
   */
  private async queryUnit(unit: string): Promise<UnitProbe | null> {
    let res: ExecResult;
    try {
      res = await this.exec.run([
        "systemctl",
        "--user",
        "show",
        unit,
        "--property=LoadState,ActiveState,Result,ExecMainStatus",
      ]);
    } catch {
      return null; // no systemctl at all — nothing to reconcile against.
    }
    if (res.code !== 0) return null; // absent (127) or errored — best-effort, fall through.
    const props = new Map<string, string>();
    for (const line of res.stdout.split("\n")) {
      const eq = line.indexOf("=");
      if (eq > 0) props.set(line.slice(0, eq), line.slice(eq + 1));
    }
    const loadState = props.get("LoadState");
    if (loadState === undefined) return null;
    if (loadState !== "loaded") return { loaded: false, active: false, exitCode: null };
    const activeState = props.get("ActiveState") ?? "";
    const active = activeState === "active" || activeState === "activating" || activeState === "deactivating";
    const rawExit = props.get("ExecMainStatus") ?? "";
    const exitCode = /^\d+$/.test(rawExit) ? Number(rawExit) : null;
    return { loaded: true, active, exitCode };
  }

  // -------------------------------------------------------------------------------------------
  // queue cancel / pause / resume — the admission drain gate.
  // -------------------------------------------------------------------------------------------

  private async rpcCancel(params: unknown): Promise<CancelResult> {
    const p = asObject(params);
    const target = typeof p.task_uuid === "string" ? p.task_uuid : undefined;
    const force = p.force === true;
    if (target === undefined) throw new TallyError("invalid_params", "queue.cancel requires task_uuid");
    return this.cancel(target, force);
  }

  /**
   * Cancel a job by task_uuid OR job_id (the frozen §1.1 `<uuid|selector>` accepts either). A QUEUED
   * job is removed from the queue and its row deleted. A RUNNING job is fenced (`--force` stops the
   * unit + reclaims the lease). Returns the frozen §1.1 shape: `{task_uuid, was, lease_epoch}` plus the
   * affected count. `was` is the matched entry's PRIOR state; `lease_epoch` its fence value.
   */
  async cancel(target: string, force: boolean): Promise<CancelResult> {
    let affected = 0;
    let matchedTaskUuid: string | null = null;
    let was: JobState | null = null;
    let leaseEpoch: number | null = null;
    for (const entry of this.jobs.values()) {
      // Match by task_uuid OR job_id (the selector grammar accepts either handle for a job).
      if (entry.task_uuid !== target && entry.job_id !== target) continue;
      if (isTerminal(entry.state)) continue; // already terminal — nothing to cancel
      if (was === null) {
        was = entry.state;
        leaseEpoch = entry.lease_epoch;
        matchedTaskUuid = entry.task_uuid;
      }

      // A job in state "enqueued" is either still in the queue OR was just popped by drive() and is
      // awaiting its lease-acquire subprocess (queue.remove returns undefined in that window). BOTH
      // cases are safe to cancel-before-dispatch: the entry is still "enqueued", so we transition it to
      // failed(cancelled) and runJob's post-acquire terminal re-check releases any lease and returns
      // WITHOUT dispatching. (Ignoring remove()===undefined here — and later calling transition on a
      // now-failed entry — is the illegal-transition/drive-abort defect we avoid.)
      if (entry.state === "enqueued") {
        this.queue.remove(entry.job_id); // no-op if already popped by drive()
        entry.verdict = "cancelled";
        transition(entry, "failed");
        this.emitFailedCancelled(entry);
        this.barriers.recordTerminal({
          job_id: entry.job_id,
          task_uuid: entry.task_uuid,
          state: "failed",
          verdict: "cancelled",
          barrier: entry.barrier,
        });
        affected++;
      } else if (force && (entry.state === "started" || entry.state === "resumed")) {
        // Force-fence a RUNNING holder: STOP the transient unit (so the leaf actually stops), MARK the
        // entry fenced (so its in-flight runJob commits a `cancelled` terminal instead of resurrecting
        // the deleted row as completed), and only THEN reclaim the lease. runJob's finally still owns
        // the RAII release; reclaim here is the belt-and-braces free of a stuck slot (idempotent).
        entry.fenced = true;
        await this.stopUnit(entry);
        if (entry.lease_id !== null) {
          await this.leases.reclaim(entry.pool, entry.lease_id);
          entry.lease_id = null;
        }
        affected++;
      }
    }
    // Delete the durable TW row (by the MATCHED task_uuid — the target may have been a job_id).
    if (affected > 0 && matchedTaskUuid !== null) {
      const row = await this.tw.getRow(matchedTaskUuid);
      if (row !== undefined) await this.tw.cancel(matchedTaskUuid);
    }
    return { ok: true, affected, task_uuid: matchedTaskUuid, was, lease_epoch: leaseEpoch };
  }

  /** Stop a running holder's transient unit (or signal the direct-spawn child) so the leaf halts. */
  private async stopUnit(entry: JobEntry): Promise<void> {
    if (entry.unit === null) return;
    // `systemctl --user stop <unit>` stops the transient unit; a systemd-absent host (dev/direct spawn)
    // returns 127 and we fall through — the fenced flag still makes runJob commit `cancelled` when the
    // leaf exits. Best-effort; never throw out of a cancel.
    try {
      await this.exec.run(["systemctl", "--user", "stop", entry.unit]);
    } catch {
      // ignore — the fence flag alone still guarantees a cancelled terminal.
    }
  }

  private rpcPause(params: unknown): QueueOpResult {
    const pool = optPool(params);
    this.queue.pause(pool);
    return { ok: true, affected: this.queue.depth(pool) };
  }

  private async rpcResume(params: unknown): Promise<QueueOpResult> {
    const pool = optPool(params);
    this.queue.resume(pool);
    const affected = this.queue.depth(pool);
    await this.drive();
    return { ok: true, affected };
  }

  // -------------------------------------------------------------------------------------------
  // The `--wait` barrier (CLI-side blocking, exposed for the engine's own callers / tests).
  // -------------------------------------------------------------------------------------------

  /** Block until a job (by task_uuid) reaches terminal; returns the verdict + exit code. */
  async waitForJob(taskUuid: string, timeoutMs?: number): Promise<{ verdict: Verdict | null; exitCode: number; timedOut: boolean }> {
    const r = await this.barriers.awaitJob(taskUuid, timeoutMs);
    if (r.timed_out) return { verdict: null, exitCode: 0, timedOut: true };
    const delta = r.satisfied[0]!;
    return { verdict: delta.verdict, exitCode: verdictExitCode(delta.verdict), timedOut: false };
  }

  /** Block until a job (by job_id — the rowless `--wait` identity, issue #4) reaches terminal. */
  async waitForJobId(jobId: string, timeoutMs?: number): Promise<{ verdict: Verdict | null; exitCode: number; timedOut: boolean }> {
    const r = await this.barriers.awaitJobId(jobId, timeoutMs);
    if (r.timed_out) return { verdict: null, exitCode: 0, timedOut: true };
    const delta = r.satisfied[0]!;
    return { verdict: delta.verdict, exitCode: verdictExitCode(delta.verdict), timedOut: false };
  }

  /** Block until N jobs in a barrier group reach terminal (enqueue-N-await-N). */
  async waitForBarrier(group: string, count: number, timeoutMs?: number): Promise<{ satisfied: number; timedOut: boolean; exitCode: number }> {
    const r = await this.barriers.awaitBarrier(group, count, timeoutMs);
    if (r.timed_out) return { satisfied: r.satisfied.length, timedOut: true, exitCode: 0 };
    // The group exit code is the worst verdict (any failure ⇒ non-zero).
    const worst = r.satisfied.reduce((code, d) => Math.max(code, verdictExitCode(d.verdict)), 0);
    return { satisfied: r.satisfied.length, timedOut: false, exitCode: worst };
  }

  // -------------------------------------------------------------------------------------------
  // Introspection (tests / query status).
  // -------------------------------------------------------------------------------------------

  /** A job entry by id (tests / query). */
  getJob(jobId: string): JobEntry | undefined {
    return this.jobs.get(jobId);
  }

  /** A job entry by task_uuid. */
  getJobByTask(taskUuid: string): JobEntry | undefined {
    for (const j of this.jobs.values()) if (j.task_uuid === taskUuid) return j;
    return undefined;
  }

  /** All job entries (tests). */
  allJobs(): JobEntry[] {
    return [...this.jobs.values()];
  }

  /** Per-pool queue depth (for query status). */
  queueDepth(pool?: Pool): number {
    return this.queue.depth(pool);
  }

  // -------------------------------------------------------------------------------------------
  // Result assembly.
  // -------------------------------------------------------------------------------------------

  private admittedResult(entry: JobEntry, _dedup: DedupResult): EnqueueResult {
    const status: JobStatusResult = isTerminal(entry.state)
      ? entry.state === "completed"
        ? "completed"
        : "failed"
      : entry.state === "enqueued"
        ? "queued"
        : "dispatched";
    return {
      task_uuid: entry.task_uuid,
      job_id: entry.job_id,
      lease_epoch: entry.lease_epoch,
      pool: entry.pool,
      status,
      session_ref: entry.session_ref,
      dedup_key: entry.dedup_key,
      witness_lsn: entry.witness_lsn,
      verdict: entry.verdict,
    };
  }

  private reusedResult(params: EnqueueParams, dedup: DedupResult, leaseEpoch: number): EnqueueResult {
    return {
      task_uuid: null,
      job_id: null, // a dedup skip admits nothing — there is no job to wait on
      lease_epoch: leaseEpoch,
      pool: params.pool ?? "worker-gpu",
      status: "reused",
      session_ref: null,
      dedup_key: dedup.dedupKey,
      witness_lsn: dedup.matchedWitnessSeq,
      verdict: "reused",
    };
  }

  /**
   * The current lease epoch: the live pls grant generation once any lease has been acquired, else the
   * daemon's boot epoch (the counter-file/pls-generation fence daemon-core resolved). NEVER the witness
   * ledger seq — that is an unrelated per-line counter whose use here mis-stamped `lease_epoch` and
   * would false-fence live current-epoch leases in recover() (ledger-line-count vs pls-generation).
   */
  private currentEpoch(): number {
    return this.leases.currentEpoch || this.bootEpoch;
  }

  private spanSeconds(entry: JobEntry): number {
    return this.wallSeconds(entry);
  }

  private wallSeconds(entry: JobEntry): number {
    if (entry.started_at_ms === null || entry.ended_at_ms === null) return 0;
    return Math.max(0, (entry.ended_at_ms - entry.started_at_ms) / 1000);
  }

  // -------------------------------------------------------------------------------------------
  // Event fan-out — bus + journald (one vocabulary).
  // -------------------------------------------------------------------------------------------

  private evidenceSpecStrings(entry: JobEntry): string[] {
    return entry.evidence.map(renderEvidenceSpec);
  }

  private emitEnqueued(entry: JobEntry): void {
    this.bus.emit("job.enqueued", {
      job_id: entry.job_id,
      task_uuid: entry.task_uuid,
      class: entry.priority,
      source: entry.source,
      agent_kind: entry.agent_kind,
      invocation: entry.argv.join(" "),
      cwd: entry.cwd,
      worktree: entry.worktree,
      evidence_spec: this.evidenceSpecStrings(entry),
      priority: entry.priority,
    });
    this.journalEmit(entry, "enqueued");
  }

  private emitDispatched(entry: JobEntry): void {
    this.bus.emit("job.dispatched", {
      job_id: entry.job_id,
      task_uuid: entry.task_uuid,
      agent_kind: entry.agent_kind,
      unit: entry.unit ?? unitNameFor(entry),
      lease_epoch: entry.lease_epoch,
      attempt: entry.attempt,
    });
    this.journalEmit(entry, "dispatched");
  }

  private emitStarted(entry: JobEntry): void {
    this.bus.emit("job.started", {
      job_id: entry.job_id,
      task_uuid: entry.task_uuid,
      pane_id: entry.pane_id,
      agent_id: entry.agent_id,
      session_ref: entry.session_ref,
      unit: entry.unit ?? unitNameFor(entry),
      ts: this.clock.nowIso(),
    });
    this.journalEmit(entry, "started");
  }

  private emitResumed(entry: JobEntry): void {
    this.bus.emit("job.resumed", {
      job_id: entry.job_id,
      labor_class: entry.labor_class === "reused" ? "reused" : "recovered",
      lease_epoch: entry.lease_epoch,
      attempt: entry.attempt,
    });
    this.journalEmit(entry, "resumed");
  }

  private emitPreempted(entry: JobEntry, reason: string): void {
    this.bus.emit("job.preempted", { job_id: entry.job_id, reason });
    this.journalEmit(entry, "preempted");
  }

  private emitEvidence(entry: JobEntry, gate: GateResult): void {
    const name = gate.passed ? "job.evidence_pass" : "job.evidence_fail";
    this.bus.emit(name, {
      job_id: entry.job_id,
      task_uuid: entry.task_uuid,
      verdict: gate.verdict,
      checked_paths: checkedPaths(gate),
    });
    this.journalEmit(entry, gate.passed ? "evidence_pass" : "evidence_fail", {
      // journald carries the verdict inside the evidence detail string (no dedicated verdict field
      // in the TALLY_* matrix — verdict is a witness/bus field); prefix it so the reader/join sees it.
      evidence: `${gate.verdict}:${checkedPaths(gate).join(",")}`,
    });
  }

  private emitWitnessEmitted(entry: JobEntry): void {
    this.bus.emit("job.witness_emitted", {
      job_id: entry.job_id,
      task_uuid: entry.task_uuid,
      witness_ref: `${this.ledger.filePath}#${entry.witness_lsn}`,
    });
    this.journalEmit(entry, "witness_emitted");
  }

  private emitCompleted(entry: JobEntry, gate: GateResult): void {
    this.bus.emit("job.completed", {
      job_id: entry.job_id,
      task_uuid: entry.task_uuid,
      exit_code: this.lastExit(entry),
      gpu_seconds: isGpuPool(entry.pool) ? entry.gpu_seconds : null,
      artifact_hash: gate.artifactHash,
      labor_class: entry.labor_class,
    });
    this.journalEmit(entry, "completed", {
      exit_code: this.lastExit(entry),
      gpu_seconds: entry.gpu_seconds,
      // TALLY_ARTIFACT_HASH is required at-completed (SPEC journald matrix). A completed run with no
      // gate-passing artifact (a run that declared none and passed on the exit+span floor) records
      // the sentinel `-` so the required field is present without fabricating a content hash.
      artifact_hash: gate.artifactHash ?? "-",
    });
  }

  private emitFailed(entry: JobEntry, gate: GateResult): void {
    this.bus.emit("job.failed", {
      job_id: entry.job_id,
      task_uuid: entry.task_uuid,
      exit_code: this.lastExit(entry),
      gpu_seconds: isGpuPool(entry.pool) ? entry.gpu_seconds : null,
      verdict: gate.verdict,
      labor_class: entry.labor_class,
    });
    this.journalEmit(entry, "failed", { exit_code: this.lastExit(entry), gpu_seconds: entry.gpu_seconds, evidence: gate.verdict });
  }

  private emitFailedCancelled(entry: JobEntry): void {
    // A queued job cancelled before dispatch never executed — the bus delta fires (so a `--wait`
    // resolves), but NO journald `failed` line is written: journald tracks execution, and a job that
    // never dispatched has no unit/exit/gpu to record (the at-start+/at-completed-or-failed fields
    // are structurally absent). The cancellation is durable in the TW row (status:deleted) + the bus.
    this.bus.emit("job.failed", {
      job_id: entry.job_id,
      task_uuid: entry.task_uuid,
      exit_code: 0,
      gpu_seconds: null,
      verdict: "cancelled",
      labor_class: entry.labor_class,
    });
  }

  /** The last observed leaf exit code for a job (from the run), or 0 when it never ran. */
  private lastExit(entry: JobEntry): number {
    return entry.exitCode ?? 0;
  }

  /** Map a lifecycle transition to a journald emit + write it (one vocabulary). */
  private journalEmit(
    entry: JobEntry,
    event: EmitEvent["event"],
    extra: Partial<Pick<EmitEvent, "exit_code" | "gpu_seconds" | "artifact_hash" | "evidence" | "message">> = {},
  ): void {
    // journald requires task_uuid always; a rowless unit uses its job_id as the anchor so the four-log
    // join still keys on a stable id (the witness line's task_uuid may be null, but journald wants a
    // non-empty anchor — the job_id is that anchor for a rowless unit).
    const anchor = entry.task_uuid ?? entry.job_id;
    const ev: EmitEvent = {
      event,
      task_uuid: anchor,
      class: entry.priority,
      source: entry.source,
      ...(entry.agent_kind !== undefined ? { agent_kind: entry.agent_kind } : {}),
      ...(entry.session_ref !== null ? { session_ref: entry.session_ref } : {}),
      ...(entry.unit !== null ? { unit: entry.unit } : {}),
      attempt: entry.attempt,
      lease_epoch: entry.lease_epoch,
      labor_class: entry.labor_class,
      ...extra,
    };
    this.journal.emit(ev);
  }
}

// Augment JobEntry with the transient last-exit bookkeeping the emitters read.
declare module "./lifecycle" {
  interface JobEntry {
    /** The leaf process exit code of the most recent run (engine bookkeeping). */
    exitCode?: number;
    /**
     * Set true by a force-cancel of a running holder: the in-flight runJob observes it after the leaf
     * exits and commits a `cancelled` terminal instead of the normal completed/failed path (so a
     * fenced run never resurrects the deleted TW row as completed).
     */
    fenced?: boolean;
    /**
     * Set true when a higher-priority requester asked this running holder to cooperatively yield. The
     * holder's own runJob observes it after the leaf exits and re-presents the job via resume (never a
     * transition of the still-executing holder — that would abort the drive loop).
     */
    yieldRequested?: boolean;
    /** The reason string for the pending yield (for the job.preempted delta). */
    yieldReason?: string;
  }
}

// ---------------------------------------------------------------------------------------------
// Local helpers.
// ---------------------------------------------------------------------------------------------

function asObject(v: unknown): Record<string, unknown> {
  if (typeof v !== "object" || v === null) return {};
  return v as Record<string, unknown>;
}

/** A `systemctl --user show` snapshot of a (possibly surviving) transient unit (issue #3). */
interface UnitProbe {
  /** True when systemd still knows the unit (LoadState=loaded, not garbage-collected). */
  loaded: boolean;
  /** True while the unit is still running (active/activating/deactivating). */
  active: boolean;
  /** The leaf's ExecMainStatus once exited, or null when unreadable. */
  exitCode: number | null;
}

/**
 * Parse a JSON-array-carrying UDA value defensively: undefined on absent/non-string/corrupt/non-array
 * so recovery falls back to the legacy description path instead of crashing at boot on a bad row.
 */
function parseJsonArrayUda(value: unknown): unknown[] | undefined {
  if (typeof value !== "string" || value === "") return undefined;
  try {
    const parsed: unknown = JSON.parse(value);
    return Array.isArray(parsed) ? parsed : undefined;
  } catch {
    return undefined;
  }
}

/** The daemon mount surface, widened with the optional job-barrier registration hook. */
interface JobBarrierMount extends DaemonMount {
  registerJobBarrierProvider(provider: JobBarrierProvider): void;
}

/** True when the daemon mount exposes the job-barrier registration hook (daemon-core does). */
function hasJobBarrierHook(daemon: DaemonMount): daemon is JobBarrierMount {
  return typeof (daemon as Partial<JobBarrierMount>).registerJobBarrierProvider === "function";
}

/** Project a BarrierTracker terminal delta to the daemon-core `JobBarrierDelta` seam shape. */
function toBarrierDelta(d: import("./barrier").TerminalDelta): JobBarrierDelta {
  return { job_id: d.job_id, task_uuid: d.task_uuid, state: d.state, verdict: d.verdict };
}

function optPool(params: unknown): Pool | undefined {
  const p = asObject(params);
  return typeof p.pool === "string" ? (p.pool as Pool) : undefined;
}

/**
 * Re-read the committed witness records from the ledger JSONL file for the dedup grep
 * (ledger-as-truth, PS#9). A line that fails JSON.parse or record validation (a torn trailing write)
 * is skipped — it can never anchor a dedup hit. No daemon, no socket: a plain local file read.
 */
function readWitnessJsonl(path: string): import("../contracts/index").WitnessRecord[] {
  if (!existsSync(path)) return [];
  const out: import("../contracts/index").WitnessRecord[] = [];
  const text = readFileSync(path, "utf8");
  for (const line of text.split("\n")) {
    if (line.trim().length === 0) continue;
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      continue; // torn / partial line — skip
    }
    const res = parseRecord(parsed);
    if (res.ok) out.push(res.record);
  }
  return out;
}
