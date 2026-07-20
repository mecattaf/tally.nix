// tally — the pls lease (IMPLEMENTATION-PLAN M1.5 lease.ts; SPEC "pls IS the per-box governor",
// "The inner fold"; PS#5, PS#21).
//
// A `Lease` is a held pls ticket on one GPU pool. It is:
//   - acquire-BEFORE-GPU: the holder never touches the GPU until this resolves granted
//     (SPEC "tally-compatible therefore means exactly one thing: acquire the pls lease before it
//     touches the GPU").
//   - RAII / process-exit as the SINGLE release path: the lease frees exactly once, on the holder's
//     process exit (or an explicit `release()` that guards against a double free). NEVER a second
//     release (a second release path is precisely what PS#5 rules out).
//   - the primary `lease_epoch` SOURCE: the broker's monotonic `generation` on grant IS the lease
//     epoch (PS#21), surfaced as `.generation`; it is monotone across grants so it fences zombies.
//   - NON-PREEMPTIBLE: nothing here force-evicts a hold. Preemption is a policy one layer up (jobs),
//     by cooperative yield — the holder releases via process-exit, never by a forced eviction here.
//
// The `holderless-lease reclaim hook` (recover()) is `reclaim()`: an explicit release of a lease
// whose holder process is gone, used by the jobs recover() path to free a slot a dead epoch left
// held. It is the ONE sanctioned explicit release, and it is idempotent. No vendor code
// (clean-room, CLI-SURFACE §4).

import type { Pool } from "../contracts/index";
import { PlsBroker, type AcquireArgs, type BrokerGrant } from "./broker";
import type { PoolRegistry, PoolDescriptor } from "./pools";

/** The outcome of an acquire attempt: a held lease, or a queued ticket (both-or-queue). */
export type AcquireOutcome =
  | { kind: "granted"; lease: Lease }
  | { kind: "queued"; pool: Pool; position: number; leaseId: string };

/**
 * A held pls lease over one pool slot. Construct it only via `LeaseManager.acquire`. The lifetime
 * discipline: the lease is released EXACTLY once — via `release()` (the RAII drop) or via the
 * process-exit hook `armProcessExitRelease()`, whichever fires first. Both funnel through the same
 * idempotent `doRelease`, so the "single release path" holds even if both are wired.
 */
export class Lease {
  /** Whether this lease has been released (idempotency guard — the single-release invariant). */
  private released = false;
  /** The process-exit listener disarm handle, if armed. */
  private disarm: (() => void) | undefined;

  constructor(
    private readonly broker: PlsBroker,
    private readonly brokerAddress: string | undefined,
    readonly leaseId: string,
    readonly pool: Pool,
    /** The broker's monotonic grant generation — the PRIMARY `lease_epoch` source (PS#21). */
    readonly generation: number,
    /** The VRAM-GB cost this lease holds against the pool budget. */
    readonly cost: number,
  ) {}

  /** The lease epoch this hold contributes (the grant generation). */
  get leaseEpoch(): number {
    return this.generation;
  }

  /** True once released — a second `release()` is a no-op (single release path, PS#5). */
  get isReleased(): boolean {
    return this.released;
  }

  /**
   * Release the lease (the RAII drop). Idempotent: a second call is a no-op, so the process-exit
   * hook and an explicit release cannot double-free. Disarms the process-exit listener.
   */
  async release(): Promise<void> {
    await this.doRelease();
  }

  private async doRelease(): Promise<void> {
    if (this.released) return;
    this.released = true;
    if (this.disarm) {
      this.disarm();
      this.disarm = undefined;
    }
    await this.broker.release(this.brokerAddress, this.leaseId);
  }

  /**
   * Arm process-exit as the release path (RAII, SPEC "process-exit — the single release path").
   * On `beforeExit`/`SIGINT`/`SIGTERM` the lease releases. Returns a disarm function (also called
   * automatically by `release()`), so a caller that hands the lease off can transfer ownership.
   *
   * The signal handlers trigger a best-effort synchronous-ish release then re-raise the default
   * exit; because production release is via the broker over `Exec`, we fire the release and let the
   * event loop drain — the transient systemd unit / dev spawn holds until the RPC lands.
   */
  armProcessExitRelease(proc: NodeJS.Process = process): void {
    if (this.released || this.disarm) return;
    const onBeforeExit = () => {
      void this.doRelease();
    };
    const onSignal = (sig: NodeJS.Signals) => {
      void this.doRelease().finally(() => {
        proc.removeListener("beforeExit", onBeforeExit);
        // Re-raise the default signal disposition so the process still terminates.
        proc.kill(proc.pid, sig);
      });
    };
    const onSigint = () => onSignal("SIGINT");
    const onSigterm = () => onSignal("SIGTERM");
    proc.on("beforeExit", onBeforeExit);
    proc.on("SIGINT", onSigint);
    proc.on("SIGTERM", onSigterm);
    this.disarm = () => {
      proc.removeListener("beforeExit", onBeforeExit);
      proc.removeListener("SIGINT", onSigint);
      proc.removeListener("SIGTERM", onSigterm);
    };
  }
}

/**
 * Acquires and tracks pls leases against tally's owned pools. One `LeaseManager` fronts BOTH boxes'
 * brokers via the injected `PlsBroker` (the per-pool broker address comes from the `PoolRegistry`),
 * so the caller names a pool, not a host (DECISIONS Q9).
 */
export class LeaseManager {
  constructor(
    private readonly broker: PlsBroker,
    private readonly pools: PoolRegistry,
  ) {}

  /** The last (highest) lease generation this manager has observed — the live `lease_epoch`. */
  private lastGeneration = 0;

  /**
   * The highest lease generation observed so far. The primary `lease_epoch` source (PS#21); the
   * daemon backstops it with its own boot-bumped counter file (the daemon is that file's sole
   * incrementer, issue #9), but a live grant always advances this monotonically.
   */
  get currentEpoch(): number {
    return this.lastGeneration;
  }

  private noteGeneration(generation: number): void {
    if (generation > this.lastGeneration) this.lastGeneration = generation;
  }

  /**
   * Acquire a lease on `pool` at the declared `priority`, sized by `cost` (estimated VRAM-GB). If a
   * slot is free the returned outcome is `granted` with a live `Lease` (call `armProcessExitRelease`
   * or `release` to free it); otherwise it is `queued` (both-or-queue) — the caller queues, it does
   * NOT hold a partial GPU.
   */
  async acquire(
    pool: Pool,
    opts: { cost: number; priority: number; tenant?: string; timeoutMs?: number },
  ): Promise<AcquireOutcome> {
    const descriptor = this.pools.require(pool);
    return this.acquireOn(descriptor, opts);
  }

  /** Acquire the default heavy-work pool (worker-gpu) when no explicit pool is chosen. */
  async acquireHeavy(opts: { cost: number; priority: number; tenant?: string; timeoutMs?: number }): Promise<AcquireOutcome> {
    return this.acquireOn(this.pools.defaultHeavyPool(), opts);
  }

  private async acquireOn(
    descriptor: PoolDescriptor,
    opts: { cost: number; priority: number; tenant?: string; timeoutMs?: number },
  ): Promise<AcquireOutcome> {
    const args: AcquireArgs = {
      pool: descriptor.name,
      cost: opts.cost,
      priority: opts.priority,
      ...(opts.tenant !== undefined ? { tenant: opts.tenant } : {}),
      ...(opts.timeoutMs !== undefined ? { timeoutMs: opts.timeoutMs } : {}),
    };
    const result = await this.broker.acquire(descriptor.broker, args);
    if (result.granted) {
      this.noteGeneration(result.generation);
      const lease = new Lease(
        this.broker,
        descriptor.broker,
        result.lease_id,
        result.pool,
        result.generation,
        result.cost,
      );
      return { kind: "granted", lease };
    }
    return { kind: "queued", pool: result.pool, position: result.position, leaseId: result.lease_id };
  }

  /**
   * The holderless-lease reclaim hook (recover(), PS#9). Explicitly free a lease whose holder
   * process is gone — the ONE sanctioned explicit release outside the RAII drop. Idempotent: a
   * lease already gone returns `false`. Used by jobs' recover() to reclaim a slot a dead lease-epoch
   * left held.
   */
  async reclaim(pool: Pool, leaseId: string): Promise<boolean> {
    const descriptor = this.pools.require(pool);
    return this.broker.release(descriptor.broker, leaseId);
  }

  /**
   * Wrap a granted lease around an async `body`, releasing exactly once when `body` settles (the
   * RAII shape for an in-process holder, e.g. the dev-rig `Bun.spawn` fallback). If the acquire
   * queued, `body` never runs and the outcome is returned as-is. This is the in-daemon analogue of
   * `pls-wrap`'s process-exit release, used where the holder is a coroutine rather than a subprocess.
   */
  async withLease<T>(
    pool: Pool,
    opts: { cost: number; priority: number; tenant?: string; timeoutMs?: number },
    body: (lease: Lease) => Promise<T>,
  ): Promise<{ ran: true; value: T; lease: BrokerGrant } | { ran: false; queued: AcquireOutcome & { kind: "queued" } }> {
    const outcome = await this.acquire(pool, opts);
    if (outcome.kind === "queued") {
      return { ran: false, queued: outcome };
    }
    const lease = outcome.lease;
    try {
      const value = await body(lease);
      return {
        ran: true,
        value,
        lease: { lease_id: lease.leaseId, pool: lease.pool, generation: lease.generation, granted: true, cost: lease.cost },
      };
    } finally {
      await lease.release();
    }
  }
}
