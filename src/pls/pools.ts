// tally — pls pool configuration (IMPLEMENTATION-PLAN M1.5 pools.ts; SPEC "Compute pools", PS#5).
//
// tally OWNS the pls pool configuration (OUV-CM: "pool definitions/capacities/budgets/units are
// declared through the pls broker config tally owns — no second config system"). This file is the
// single source of the two GPU pools tally declares day-1:
//
//   worker-gpu     — PRIORITIZED, headless, dedicated to models: the default target for heavy work.
//   controller-gpu — shares the box with chrome/graphical, so light/co-resident tenants only
//                    (incl. the DS4 controller spill).
//
// Each pool is single-lease-per-pool (`PLS_CAPACITY=1`, PS#5), and `--cost` is the estimated
// VRAM-GB budget math that collapses the old "advisory-lock + MemAvailable check" into a pls
// budget pool (SPEC "pls IS the per-box governor"). The rendered pool config is consumed by the
// layer-3 nix module's pls broker units (M3.3) — this module owns the SHAPE, the module renders it
// onto disk. No vendor code (clean-room, CLI-SURFACE §4).

import type { Pool, PoolConfig } from "../contracts/index";
import { GPU_POOLS } from "../contracts/index";

/**
 * VRAM budget, in GB, of one Framework Desktop node (AMD Halo Strix, 128 GB unified). The `--cost`
 * a heavy tenant declares is its estimated VRAM-GB; a pool admits while the summed cost of tickets
 * held right now ≤ this budget (SPEC "Co-residency … pls-native BUDGET math"). This is the DEFAULT
 * budget; a `PoolConfig` never carries a budget field (tally's config surface is minimal, PS#17),
 * so the budget is a compiled-in pool property keyed on pool name and overridable per-pool below.
 */
export const DEFAULT_POOL_BUDGET_GB = 128;

/** Single-lease-per-pool: every v0 GPU pool has capacity exactly 1 (PS#5 `PLS_CAPACITY=1`). */
export const PLS_CAPACITY = 1;

/**
 * A fully-resolved pool descriptor — the SHAPE the broker leases against and the nix module renders
 * into a pls broker unit. `broker` is the address of the pls broker serving this pool (over
 * TB3/tailnet for the worker; DECISIONS Q9 — never a hardcoded hostname). `capacity` is 1.
 * `budgetGb` is the VRAM-GB budget the `--cost` admission math runs against.
 */
export interface PoolDescriptor {
  name: Pool;
  broker: string;
  /** Priority ordering hint — LOWER is served first when both pools are eligible. */
  priority: number;
  capacity: number;
  budgetGb: number;
}

/**
 * The default worker-GPU descriptor (PRIORITIZED — served first, `priority:0`). Headless,
 * uncontended: the default target for heavy work (OCR firehose, DS4 heavy hold).
 */
export function defaultWorkerPool(broker = "localhost"): PoolDescriptor {
  return { name: "worker-gpu", broker, priority: 0, capacity: PLS_CAPACITY, budgetGb: DEFAULT_POOL_BUDGET_GB };
}

/**
 * The default controller-GPU descriptor (`priority:1`). Shares the box with chrome/graphical — so
 * only light / co-resident tenants (the DS4 spill) should carry a large `--cost` here.
 */
export function defaultControllerPool(broker = "localhost"): PoolDescriptor {
  return { name: "controller-gpu", broker, priority: 1, capacity: PLS_CAPACITY, budgetGb: DEFAULT_POOL_BUDGET_GB };
}

/** The two GPU pools tally declares day-1, worker prioritized (SPEC "The pools"). */
export function defaultPools(brokers: { worker?: string; controller?: string } = {}): PoolDescriptor[] {
  return [defaultWorkerPool(brokers.worker), defaultControllerPool(brokers.controller)];
}

/**
 * The pool registry — the in-memory view of tally's owned pool config, keyed by name. Constructed
 * from the daemon config's `pools[]` (rendered by the nix module) so the broker leases against the
 * exact addresses/priorities the operator declared; `fromConfig` falls back to the compiled
 * defaults when a config pool omits a budget (config's `PoolConfig` has no budget field, by design).
 */
export class PoolRegistry {
  private readonly byName = new Map<Pool, PoolDescriptor>();

  constructor(descriptors: readonly PoolDescriptor[]) {
    for (const d of descriptors) this.byName.set(d.name, d);
  }

  /** Build a registry from the daemon config's `pools[]` (the nix-rendered shape). */
  static fromConfig(pools: readonly PoolConfig[]): PoolRegistry {
    const descriptors = pools.map<PoolDescriptor>((p) => ({
      name: p.name,
      broker: p.broker,
      priority: p.priority,
      capacity: p.capacity,
      budgetGb: DEFAULT_POOL_BUDGET_GB,
    }));
    return new PoolRegistry(descriptors);
  }

  /** The default two-GPU-pool registry (worker prioritized). */
  static default(brokers: { worker?: string; controller?: string } = {}): PoolRegistry {
    return new PoolRegistry(defaultPools(brokers));
  }

  /** Look up a pool descriptor by name, or `undefined` if tally does not declare it. */
  get(name: Pool): PoolDescriptor | undefined {
    return this.byName.get(name);
  }

  /** Look up a pool descriptor, throwing a descriptive error if it is unknown. */
  require(name: Pool): PoolDescriptor {
    const p = this.byName.get(name);
    if (!p) {
      throw new Error(
        `pls: unknown pool '${name}' (tally declares: ${this.names().join(", ") || "<none>"})`,
      );
    }
    return p;
  }

  /** True when tally declares this pool. */
  has(name: Pool): boolean {
    return this.byName.has(name);
  }

  /** Every declared pool name. */
  names(): Pool[] {
    return [...this.byName.keys()];
  }

  /** Every declared pool descriptor. */
  all(): PoolDescriptor[] {
    return [...this.byName.values()];
  }

  /**
   * The default heavy-work pool — the highest-priority (lowest `priority` value) GPU pool declared,
   * which is `worker-gpu` in the day-1 config. Heavy jobs with no explicit `--pool` hint land here
   * (SPEC "the default target for heavy work"; PS#2 — a hint is honored, never a model re-pick).
   */
  defaultHeavyPool(): PoolDescriptor {
    const gpu = this.all().filter((p) => (GPU_POOLS as readonly string[]).includes(p.name));
    const pool = (gpu.length > 0 ? gpu : this.all()).slice().sort((a, b) => a.priority - b.priority)[0];
    if (!pool) throw new Error("pls: no pools declared; cannot pick a default heavy pool");
    return pool;
  }
}

/**
 * A rendered pls broker pool entry — the JSON the layer-3 nix module writes into the pls broker's
 * config (tally owns pls config, PS#5). Field names mirror the pls broker's expected pool keys:
 * `name`, `capacity`, `budget` (VRAM-GB), and the `broker`/`priority` tally tracks. This is the
 * ONLY place the on-disk pls config shape is defined, so the module renders it rather than guessing.
 */
export interface RenderedPoolConfig {
  name: string;
  capacity: number;
  budget: number;
  priority: number;
  broker: string;
}

/** Render one pool descriptor to its on-disk pls broker config entry. */
export function renderPool(p: PoolDescriptor): RenderedPoolConfig {
  return { name: p.name, capacity: p.capacity, budget: p.budgetGb, priority: p.priority, broker: p.broker };
}

/**
 * Render the whole pool config the nix module writes for the pls broker units (M3.3). Deterministic
 * key order so the rendered file is stable across boots (a churny config triggers needless unit
 * restarts). Returned as a plain object the module serializes to JSON.
 */
export function renderPoolConfig(registry: PoolRegistry): { pools: RenderedPoolConfig[] } {
  return { pools: registry.all().map(renderPool) };
}
