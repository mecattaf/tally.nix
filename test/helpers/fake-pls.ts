// test/helpers/fake-pls.ts
//
// A fake of the `pls` box-governor broker (github.com/sniarchos/pls) — the
// GPU-lease primitive tally leases against (SPEC "Compute pools"; PS#5). tally
// owns the pool config; each pool is single-lease (`PLS_CAPACITY=1`); a lease
// carries a monotonic GENERATION that is the primary `lease_epoch` source; the
// single release path is RAII/process-exit.
//
// This fake models exactly what the pls module (src/pls/*) binds to:
//   pls acquire --pool <p> --cost <c> --priority <n>
//        -> JSON {lease_id, pool, generation, granted:true} when a slot is free,
//           else {granted:false, queued:true, position:N} (both-or-queue).
//   pls release --lease <id>          -> frees the slot, grants the next waiter.
//   pls status  --pool <p>            -> {pool, capacity, held, queued, budget}
//   pls coalloc --pools p1,p2 --costs c1,c2 --priority n
//        -> atomic BOTH-or-QUEUE across two pools (the DS4 co-allocation).
//
// Generations increase by 1 on every successful grant across the whole broker,
// so the pls module's "generation monotonicity" test can assert strict growth
// and use it as the lease_epoch fence. `killHolder(pool)` simulates the holder
// process dying (RAII release) so the process-death-release test has a hook.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { type FakeExec, type ExecResult, fail, okJson, parseArgs } from "./exec-fakes.ts";

interface PoolState {
  name: string;
  capacity: number;
  budget: number;
  held: Array<{ leaseId: string; cost: number; generation: number; tenant: string }>;
  queue: Array<{ leaseId: string; cost: number; priority: number; tenant: string }>;
}

export interface AcquireGrant {
  lease_id: string;
  pool: string;
  generation: number;
  granted: true;
  cost: number;
}

export interface AcquireQueued {
  lease_id: string;
  pool: string;
  granted: false;
  queued: true;
  position: number;
}

/**
 * A programmable pls broker. Declare pools, then `install(exec)`. Two competing
 * `acquire` calls on the same single-capacity pool serialize (the second is
 * queued); `release` (or `killHolder`) hands the slot to the highest-priority
 * waiter and mints a fresh, higher generation.
 */
export class FakePls {
  private readonly pools = new Map<string, PoolState>();
  private generation = 41; // first grant becomes 42 — matches the §2.2 example
  private leaseSeq = 0;
  /** Every acquire/release call, for assertions. */
  readonly log: Array<{ op: string; pool?: string; leaseId?: string; generation?: number }> = [];

  /** Declare a pool. Default single-lease (capacity 1), matching PLS_CAPACITY=1. */
  addPool(name: string, opts: { capacity?: number; budget?: number } = {}): this {
    this.pools.set(name, {
      name,
      capacity: opts.capacity ?? 1,
      budget: opts.budget ?? 24,
      held: [],
      queue: [],
    });
    return this;
  }

  /** Current holders of a pool (lease ids). */
  holders(pool: string): string[] {
    return (this.pools.get(pool)?.held ?? []).map((h) => h.leaseId);
  }

  /** Current queue depth of a pool. */
  queueDepth(pool: string): number {
    return this.pools.get(pool)?.queue.length ?? 0;
  }

  /** The last generation minted so far. */
  currentGeneration(): number {
    return this.generation;
  }

  private freeCost(p: PoolState): number {
    return p.budget - p.held.reduce((s, h) => s + h.cost, 0);
  }

  private canGrant(p: PoolState, cost: number): boolean {
    return p.held.length < p.capacity && this.freeCost(p) >= cost;
  }

  private grant(p: PoolState, cost: number, tenant: string): AcquireGrant {
    const leaseId = `lease-${++this.leaseSeq}`;
    const generation = ++this.generation;
    p.held.push({ leaseId, cost, generation, tenant });
    this.log.push({ op: "grant", pool: p.name, leaseId, generation });
    return { lease_id: leaseId, pool: p.name, generation, granted: true, cost };
  }

  /** Try to acquire; grant if a slot is free, else enqueue. */
  private tryAcquire(
    poolName: string,
    cost: number,
    priority: number,
    tenant: string,
  ): AcquireGrant | AcquireQueued {
    const p = this.pools.get(poolName);
    if (!p) throw new Error(`fake-pls: unknown pool '${poolName}'`);
    if (this.canGrant(p, cost)) {
      return this.grant(p, cost, tenant);
    }
    const leaseId = `wait-${++this.leaseSeq}`;
    p.queue.push({ leaseId, cost, priority, tenant });
    // Highest priority first (stable within priority = FIFO).
    p.queue.sort((a, b) => b.priority - a.priority);
    this.log.push({ op: "queue", pool: p.name, leaseId });
    return {
      lease_id: leaseId,
      pool: p.name,
      granted: false,
      queued: true,
      position: p.queue.findIndex((q) => q.leaseId === leaseId) + 1,
    };
  }

  /** Release a held lease and promote the next waiter (RAII/explicit). */
  release(leaseId: string): boolean {
    for (const p of this.pools.values()) {
      const i = p.held.findIndex((h) => h.leaseId === leaseId);
      if (i !== -1) {
        p.held.splice(i, 1);
        this.log.push({ op: "release", pool: p.name, leaseId });
        this.promote(p);
        return true;
      }
    }
    return false;
  }

  /** Simulate the holder process dying — the single RAII release path. */
  killHolder(pool: string): boolean {
    const p = this.pools.get(pool);
    if (!p || p.held.length === 0) return false;
    const dead = p.held[0]!;
    return this.release(dead.leaseId);
  }

  private promote(p: PoolState): void {
    while (p.queue.length > 0 && this.canGrant(p, p.queue[0]!.cost)) {
      const next = p.queue.shift()!;
      this.grant(p, next.cost, next.tenant);
    }
  }

  install(exec: FakeExec): this {
    exec.register("pls", (args): ExecResult => {
      const verb = args[0];
      const parsed = parseArgs(args.slice(1));
      const tenant = parsed.value("tenant") ?? "tally";
      switch (verb) {
        case "acquire": {
          const pool = parsed.value("pool");
          if (!pool) return fail(2, "fake-pls acquire: --pool required");
          const cost = Number(parsed.value("cost") ?? "1");
          const priority = Number(parsed.value("priority") ?? "0");
          return okJson(this.tryAcquire(pool, cost, priority, tenant));
        }
        case "release": {
          const leaseId = parsed.value("lease");
          if (!leaseId) return fail(2, "fake-pls release: --lease required");
          const freed = this.release(leaseId);
          return okJson({ released: freed, lease_id: leaseId });
        }
        case "status": {
          const pool = parsed.value("pool");
          if (pool) {
            const p = this.pools.get(pool);
            if (!p) return fail(1, `no pool ${pool}`);
            return okJson({
              pool: p.name,
              capacity: p.capacity,
              budget: p.budget,
              held: p.held.length,
              queued: p.queue.length,
              free_cost: this.freeCost(p),
            });
          }
          return okJson(
            [...this.pools.values()].map((p) => ({
              pool: p.name,
              capacity: p.capacity,
              budget: p.budget,
              held: p.held.length,
              queued: p.queue.length,
            })),
          );
        }
        case "coalloc": {
          // Atomic both-or-queue across two pools (DS4 co-allocation, PS#5).
          const pools = (parsed.value("pools") ?? "").split(",").filter(Boolean);
          const costs = (parsed.value("costs") ?? "").split(",").filter(Boolean).map(Number);
          const priority = Number(parsed.value("priority") ?? "0");
          if (pools.length !== 2) return fail(2, "fake-pls coalloc: --pools p1,p2 required");
          const states = pools.map((n) => this.pools.get(n));
          if (states.some((s) => !s)) return fail(1, "fake-pls coalloc: unknown pool");
          const bothFree = states.every((s, i) => this.canGrant(s!, costs[i] ?? 1));
          if (!bothFree) {
            return okJson({ granted: false, queued: true, pools });
          }
          const grants = states.map((s, i) => this.grant(s!, costs[i] ?? 1, tenant));
          return okJson({ granted: true, leases: grants, priority });
        }
        default:
          return fail(2, `fake-pls: unsupported verb '${verb}'`);
      }
    });
    return this;
  }
}
