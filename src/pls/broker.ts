// tally — the pls broker client (IMPLEMENTATION-PLAN M1.5 broker.ts; SPEC "pls IS the per-box
// governor", PS#5).
//
// This is tally's client to BOTH boxes' pls brokers (the controller's local broker + the worker's,
// reachable over the TB3/tailnet link — address from config, NEVER a hardcoded hostname, DECISIONS
// Q9). Every broker call shells out through the injectable `Exec` seam (contracts/exec.ts), so the
// broker is fully testable against the layer-0 fake pls (test/helpers/fake-pls.ts).
//
// The pls CLI surface tally binds to (the documented interface the fake models faithfully):
//   pls acquire --pool <p> --cost <c> --priority <n> [--tenant <t>]
//        -> {lease_id, pool, generation, granted:true, cost}          (a slot was free)
//        -> {lease_id, pool, granted:false, queued:true, position:N}  (both-or-queue)
//   pls release --lease <id>   -> {released:bool, lease_id}
//   pls status  --pool <p>     -> {pool, capacity, budget, held, queued, free_cost}
//   pls coalloc --pools p1,p2 --costs c1,c2 --priority <n> [--tenant <t>]
//        -> {granted:true, leases:[grant,grant], priority} | {granted:false, queued:true, pools}
//
// The broker is a THIN transport: it constructs the argv, runs it, and parses the JSON. Policy
// (RAII release, generation-as-lease_epoch, co-alloc both-or-queue) lives in lease.ts / coalloc.ts.
// No vendor code (clean-room, CLI-SURFACE §4).

import type { Exec, Pool } from "../contracts/index";

/** The `pls` binary name — resolved from PATH by the real Exec, stubbed by the fake. */
const PLS_BIN = "pls";

/** A granted lease as the broker reports it (the fake's `AcquireGrant`). */
export interface BrokerGrant {
  lease_id: string;
  pool: Pool;
  generation: number;
  granted: true;
  cost: number;
}

/** A queued acquire as the broker reports it (both-or-queue; the fake's `AcquireQueued`). */
export interface BrokerQueued {
  lease_id: string;
  pool: Pool;
  granted: false;
  queued: true;
  position: number;
}

export type BrokerAcquireResult = BrokerGrant | BrokerQueued;

/** A pool's live status as the broker reports it. */
export interface BrokerPoolStatus {
  pool: Pool;
  capacity: number;
  budget: number;
  held: number;
  queued: number;
  free_cost?: number;
}

/** The co-alloc broker result — atomic both-or-queue across two pools (DS4). */
export type BrokerCoallocResult =
  | { granted: true; leases: BrokerGrant[]; priority: number }
  | { granted: false; queued: true; pools: Pool[] };

/** Parameters for one `acquire`. */
export interface AcquireArgs {
  pool: Pool;
  /** Estimated VRAM-GB budget cost (SPEC `--cost`). */
  cost: number;
  /** Declared priority — higher wins the queue (SPEC "at its declared priority"). */
  priority: number;
  /** Identifies the acquiring tenant; defaults to `tally`. A direct non-tally tenant sets its own. */
  tenant?: string;
  /** Optional per-call timeout in ms (broker RPC guard). */
  timeoutMs?: number;
}

/** Thrown when the broker exits non-zero or returns unparseable / malformed JSON. */
export class BrokerError extends Error {
  readonly code: number;
  readonly stderr: string;
  constructor(message: string, code: number, stderr: string) {
    super(message);
    this.name = "BrokerError";
    this.code = code;
    this.stderr = stderr;
  }
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * The pls broker client. One instance per broker ADDRESS is not required — the address rides each
 * call's env so a single client fronts BOTH boxes' brokers (worker over TB3/tailnet, controller
 * local). The address is injected, never a frozen hostname (DECISIONS Q9): it is passed to the pls
 * binary via the `PLS_BROKER` env var, the documented broker-address knob tally binds to.
 */
export class PlsBroker {
  constructor(private readonly exec: Exec) {}

  /** Build the env for a call to a specific broker address (empty = the ambient/default broker). */
  private brokerEnv(broker: string | undefined): Record<string, string> | undefined {
    if (!broker || broker === "localhost" || broker.trim() === "") return undefined;
    return { PLS_BROKER: broker };
  }

  private async runJson(argv: string[], broker: string | undefined, timeoutMs?: number): Promise<unknown> {
    const env = this.brokerEnv(broker);
    const result = await this.exec.run([PLS_BIN, ...argv], {
      ...(env ? { env } : {}),
      ...(timeoutMs !== undefined ? { timeoutMs } : {}),
    });
    if (result.timedOut) {
      throw new BrokerError(`pls ${argv[0]} timed out after ${timeoutMs}ms`, result.code, result.stderr);
    }
    if (result.code !== 0) {
      throw new BrokerError(
        `pls ${argv[0]} failed (exit ${result.code}): ${result.stderr.trim() || "<no stderr>"}`,
        result.code,
        result.stderr,
      );
    }
    try {
      return JSON.parse(result.stdout);
    } catch {
      throw new BrokerError(
        `pls ${argv[0]} returned non-JSON output: ${result.stdout.slice(0, 200)}`,
        result.code,
        result.stderr,
      );
    }
  }

  /**
   * Acquire a lease on `pool` at `broker`. Returns a grant when a slot was free, else a queued
   * ticket (both-or-queue). The generation on a grant is the primary `lease_epoch` source
   * (PS#21) — lease.ts surfaces it.
   */
  async acquire(broker: string | undefined, args: AcquireArgs): Promise<BrokerAcquireResult> {
    const argv = [
      "acquire",
      "--pool",
      args.pool,
      "--cost",
      String(args.cost),
      "--priority",
      String(args.priority),
      "--tenant",
      args.tenant ?? "tally",
    ];
    const raw = await this.runJson(argv, broker, args.timeoutMs);
    return this.parseAcquire(raw, args.pool);
  }

  private parseAcquire(raw: unknown, pool: Pool): BrokerAcquireResult {
    if (!isObject(raw)) throw new BrokerError(`pls acquire: expected a JSON object`, 0, "");
    if (raw.granted === true) {
      if (typeof raw.lease_id !== "string" || typeof raw.generation !== "number") {
        throw new BrokerError(`pls acquire grant missing lease_id/generation`, 0, "");
      }
      return {
        lease_id: raw.lease_id,
        pool: (typeof raw.pool === "string" ? raw.pool : pool) as Pool,
        generation: raw.generation,
        granted: true,
        cost: typeof raw.cost === "number" ? raw.cost : 0,
      };
    }
    if (raw.queued === true || raw.granted === false) {
      return {
        lease_id: typeof raw.lease_id === "string" ? raw.lease_id : "",
        pool: (typeof raw.pool === "string" ? raw.pool : pool) as Pool,
        granted: false,
        queued: true,
        position: typeof raw.position === "number" ? raw.position : 0,
      };
    }
    throw new BrokerError(`pls acquire: unrecognized result shape`, 0, "");
  }

  /**
   * Release a held lease at `broker`. The SINGLE release path in production is RAII/process-exit
   * (lease.ts) — this explicit release exists for the reclaim hook (a holderless lease) and for the
   * co-alloc rollback, never as a second release of a live-held lease.
   */
  async release(broker: string | undefined, leaseId: string, timeoutMs?: number): Promise<boolean> {
    const raw = await this.runJson(["release", "--lease", leaseId], broker, timeoutMs);
    if (isObject(raw) && typeof raw.released === "boolean") return raw.released;
    return true;
  }

  /** Query one pool's live status (held/queued/budget) — backs `query status` per-pool depth. */
  async status(broker: string | undefined, pool: Pool, timeoutMs?: number): Promise<BrokerPoolStatus> {
    const raw = await this.runJson(["status", "--pool", pool], broker, timeoutMs);
    if (!isObject(raw)) throw new BrokerError(`pls status: expected a JSON object`, 0, "");
    return {
      pool: (typeof raw.pool === "string" ? raw.pool : pool) as Pool,
      capacity: typeof raw.capacity === "number" ? raw.capacity : 0,
      budget: typeof raw.budget === "number" ? raw.budget : 0,
      held: typeof raw.held === "number" ? raw.held : 0,
      queued: typeof raw.queued === "number" ? raw.queued : 0,
      ...(typeof raw.free_cost === "number" ? { free_cost: raw.free_cost } : {}),
    };
  }

  /**
   * Atomic co-allocation across two pools (the DS4 both-or-queue, PS#5). Grants BOTH or queues both
   * — never a partial hold. `broker` is the co-alloc coordinator (the controller, which fronts both
   * boxes' brokers). coalloc.ts wraps this with the RAII lease lifetime.
   */
  async coalloc(
    broker: string | undefined,
    pools: [Pool, Pool],
    costs: [number, number],
    priority: number,
    tenant = "tally",
    timeoutMs?: number,
  ): Promise<BrokerCoallocResult> {
    const argv = [
      "coalloc",
      "--pools",
      pools.join(","),
      "--costs",
      costs.join(","),
      "--priority",
      String(priority),
      "--tenant",
      tenant,
    ];
    const raw = await this.runJson(argv, broker, timeoutMs);
    if (!isObject(raw)) throw new BrokerError(`pls coalloc: expected a JSON object`, 0, "");
    if (raw.granted === true) {
      const leases = Array.isArray(raw.leases) ? raw.leases : [];
      const parsed = leases.map((l, i) => this.parseAcquire(l, pools[i] ?? pools[0]));
      const grants = parsed.filter((g): g is BrokerGrant => g.granted === true);
      if (grants.length !== 2) {
        throw new BrokerError(`pls coalloc: granted but did not report two leases`, 0, "");
      }
      return { granted: true, leases: grants, priority: typeof raw.priority === "number" ? raw.priority : priority };
    }
    return {
      granted: false,
      queued: true,
      pools: Array.isArray(raw.pools) ? (raw.pools as Pool[]) : [...pools],
    };
  }
}
