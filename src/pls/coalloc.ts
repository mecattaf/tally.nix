// tally — the DS4 cross-box atomic co-allocation (IMPLEMENTATION-PLAN M1.5 coalloc.ts; SPEC
// "The pools", PS#5).
//
// DS4 (deepseek-4-flash) is the ONE cross-box job: an ATOMIC co-allocation of a heavy worker-GPU
// hold + a LIGHT controller-GPU spill (secondary — a small co-resident `--cost` that leaves
// controller headroom for chrome/graphical). Semantics (PS#5): BOTH leases or the dispatch QUEUES;
// there is never a partial hold. tally runs on the controller and is a client of both boxes' pls
// brokers, so the co-alloc coordinator is the controller's broker fronting both.
//
// The atomicity is enforced by the broker's `coalloc` primitive (both-or-queue in ONE call); this
// module wraps it with the RAII lease lifetime — on grant it returns two `Lease`s bound to the
// same single-release discipline as any other lease; on queue it returns the queued marker and
// holds nothing. No vendor code (clean-room, CLI-SURFACE §4).

import type { Pool } from "../contracts/index";
import { PlsBroker } from "./broker";
import { Lease } from "./lease";
import type { PoolRegistry } from "./pools";

/** Default VRAM-GB costs for a DS4 co-allocation: a heavy worker hold + a light controller spill. */
export const DS4_DEFAULT_WORKER_COST = 64;
export const DS4_DEFAULT_CONTROLLER_SPILL_COST = 8;

/** The two pools a DS4 co-allocation spans (worker heavy, controller light spill). */
export interface CoallocPools {
  /** The heavy hold — worker-gpu by default (headless, prioritized). */
  worker: Pool;
  /** The light spill — controller-gpu by default (co-resident, small cost). */
  controller: Pool;
}

/** The default DS4 pools: worker-gpu heavy + controller-gpu spill. */
export const DS4_POOLS: CoallocPools = { worker: "worker-gpu", controller: "controller-gpu" };

/** A held co-allocation — both leases, released together (the atomic pair). */
export interface CoallocHold {
  kind: "granted";
  /** The heavy worker-GPU lease. */
  worker: Lease;
  /** The light controller-GPU spill lease. */
  controller: Lease;
  /** Release BOTH leases (idempotent; the single-release discipline applies to each). */
  release(): Promise<void>;
  /** Arm process-exit release for both leases (RAII). */
  armProcessExitRelease(proc?: NodeJS.Process): void;
  /** The higher of the two grant generations — the lease_epoch this co-allocation contributes. */
  readonly generation: number;
}

/** A queued co-allocation — neither pool was grantable, so the DS4 dispatch waits (both-or-queue). */
export interface CoallocQueued {
  kind: "queued";
  pools: Pool[];
}

export type CoallocOutcome = CoallocHold | CoallocQueued;

/**
 * Co-allocates the DS4 cross-box pair atomically. `broker` fronts both boxes' brokers; the
 * `PoolRegistry` supplies the coordinator address (the worker pool's broker, which the controller
 * reaches over TB3/tailnet — DECISIONS Q9).
 */
export class Coallocator {
  constructor(
    private readonly broker: PlsBroker,
    private readonly pools: PoolRegistry,
  ) {}

  /**
   * Attempt the atomic co-allocation. On success returns a `CoallocHold` binding BOTH leases; on
   * contention returns `CoallocQueued` holding nothing (PS#5 both-or-queue). Costs default to the
   * DS4 heavy/light split; `priority` is the declared queue priority.
   */
  async allocate(opts: {
    priority: number;
    pools?: CoallocPools;
    workerCost?: number;
    controllerCost?: number;
    tenant?: string;
    timeoutMs?: number;
  }): Promise<CoallocOutcome> {
    const cp = opts.pools ?? DS4_POOLS;
    const workerPool = this.pools.require(cp.worker);
    const controllerPool = this.pools.require(cp.controller);
    const workerCost = opts.workerCost ?? DS4_DEFAULT_WORKER_COST;
    const controllerCost = opts.controllerCost ?? DS4_DEFAULT_CONTROLLER_SPILL_COST;

    // The coordinator is the worker pool's broker (the cross-box link tally reaches over TB3);
    // the fake and the real pls broker both resolve both pools from this single coalloc call.
    const result = await this.broker.coalloc(
      workerPool.broker,
      [workerPool.name, controllerPool.name],
      [workerCost, controllerCost],
      opts.priority,
      opts.tenant ?? "tally",
      opts.timeoutMs,
    );

    if (!result.granted) {
      return { kind: "queued", pools: result.pools };
    }

    // Bind each granted lease to a Lease with the standard single-release discipline. The broker
    // returns the two grants in [worker, controller] order (the order we passed --pools).
    const [workerGrant, controllerGrant] = result.leases;
    if (!workerGrant || !controllerGrant) {
      // Defensive: the broker said granted but did not return two leases — release whatever landed
      // and surface it as a queue (never a partial hold).
      for (const g of result.leases) {
        await this.broker.release(workerPool.broker, g.lease_id);
      }
      return { kind: "queued", pools: [workerPool.name, controllerPool.name] };
    }

    const workerLease = new Lease(
      this.broker,
      workerPool.broker,
      workerGrant.lease_id,
      workerGrant.pool,
      workerGrant.generation,
      workerGrant.cost,
    );
    const controllerLease = new Lease(
      this.broker,
      controllerPool.broker,
      controllerGrant.lease_id,
      controllerGrant.pool,
      controllerGrant.generation,
      controllerGrant.cost,
    );

    const generation = Math.max(workerGrant.generation, controllerGrant.generation);

    return {
      kind: "granted",
      worker: workerLease,
      controller: controllerLease,
      generation,
      async release(): Promise<void> {
        // Release both; each release is idempotent so double-fire is safe.
        await Promise.all([workerLease.release(), controllerLease.release()]);
      },
      armProcessExitRelease(proc?: NodeJS.Process): void {
        workerLease.armProcessExitRelease(proc);
        controllerLease.armProcessExitRelease(proc);
      },
    };
  }
}
