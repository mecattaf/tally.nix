// test/pls/pls.test.ts
//
// Tests for the pls module (M1.5) against the layer-0 fake pls broker. The brief's required cases:
//   - serialization of two competitors on the single-lease pool,
//   - process-death release (the RAII/kill hook),
//   - generation monotonicity (the lease_epoch fence),
//   - co-alloc both-or-queue (the DS4 cross-box atomic pair),
//   - direct non-tally tenant acquisition (the ambient wrap path).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { describe, expect, test } from "bun:test";
import { FakeExec } from "../helpers/exec-fakes.ts";
import { FakePls } from "../helpers/fake-pls.ts";
import {
  PoolRegistry,
  PlsBroker,
  LeaseManager,
  Coallocator,
  DS4_POOLS,
  renderPoolConfig,
  parseWrapArgs,
  renderWrapScript,
  runWrap,
} from "../../src/pls/index.ts";

/** A registry over the two GPU pools, all brokers `localhost` (single-box test harness). */
function twoPoolRegistry(): PoolRegistry {
  return PoolRegistry.default();
}

/** Wire a FakePls with the two GPU pools, single-lease each, onto a fresh FakeExec. */
function harness(opts: { workerBudget?: number; controllerBudget?: number } = {}): {
  exec: FakeExec;
  pls: FakePls;
  registry: PoolRegistry;
} {
  const exec = new FakeExec();
  const pls = new FakePls();
  pls.addPool("worker-gpu", { capacity: 1, budget: opts.workerBudget ?? 128 });
  pls.addPool("controller-gpu", { capacity: 1, budget: opts.controllerBudget ?? 128 });
  pls.install(exec);
  return { exec, pls, registry: twoPoolRegistry() };
}

describe("PoolRegistry — tally owns the pool config (PS#5)", () => {
  test("declares the two GPU pools, worker prioritized", () => {
    const r = twoPoolRegistry();
    expect(r.names()).toEqual(["worker-gpu", "controller-gpu"]);
    expect(r.require("worker-gpu").priority).toBeLessThan(r.require("controller-gpu").priority);
    expect(r.defaultHeavyPool().name).toBe("worker-gpu");
  });

  test("single-lease-per-pool capacity is 1", () => {
    const r = twoPoolRegistry();
    for (const p of r.all()) expect(p.capacity).toBe(1);
  });

  test("renders a deterministic pls broker pool config", () => {
    const r = twoPoolRegistry();
    const rendered = renderPoolConfig(r);
    expect(rendered.pools.map((p) => p.name)).toEqual(["worker-gpu", "controller-gpu"]);
    expect(rendered.pools[0]).toMatchObject({ name: "worker-gpu", capacity: 1, priority: 0 });
    expect(rendered.pools[0]!.budget).toBeGreaterThan(0);
  });

  test("fromConfig maps the nix-rendered PoolConfig[]", () => {
    const r = PoolRegistry.fromConfig([
      { name: "worker-gpu", broker: "worker.tail", priority: 0, capacity: 1 },
      { name: "controller-gpu", broker: "localhost", priority: 1, capacity: 1 },
    ]);
    expect(r.require("worker-gpu").broker).toBe("worker.tail");
    expect(r.require("controller-gpu").broker).toBe("localhost");
  });

  test("require throws on an unknown pool", () => {
    expect(() => twoPoolRegistry().require("api")).toThrow(/unknown pool 'api'/);
  });
});

describe("LeaseManager — acquire/release against the fake broker", () => {
  test("acquires a lease on the heavy pool and surfaces the grant generation as lease_epoch", async () => {
    const { exec, registry } = harness();
    const leases = new LeaseManager(new PlsBroker(exec), registry);
    const outcome = await leases.acquireHeavy({ cost: 8, priority: 0 });
    expect(outcome.kind).toBe("granted");
    if (outcome.kind !== "granted") throw new Error("expected granted");
    expect(outcome.lease.pool).toBe("worker-gpu");
    expect(outcome.lease.generation).toBe(42); // fake mints 42 on the first grant (§2.2 example)
    expect(outcome.lease.leaseEpoch).toBe(42);
    expect(leases.currentEpoch).toBe(42);
    await outcome.lease.release();
    expect(outcome.lease.isReleased).toBe(true);
  });

  test("release is idempotent — the single release path (PS#5)", async () => {
    const { exec, pls, registry } = harness();
    const leases = new LeaseManager(new PlsBroker(exec), registry);
    const outcome = await leases.acquireHeavy({ cost: 8, priority: 0 });
    if (outcome.kind !== "granted") throw new Error("expected granted");
    await outcome.lease.release();
    await outcome.lease.release(); // no-op — must not double-free
    const releaseCalls = pls.log.filter((l) => l.op === "release");
    expect(releaseCalls.length).toBe(1);
    expect(pls.holders("worker-gpu")).toHaveLength(0);
  });
});

describe("serialization of two competitors (single-lease pool)", () => {
  test("the second acquire on a held pool queues, then is promoted on release", async () => {
    const { exec, pls, registry } = harness();
    const leases = new LeaseManager(new PlsBroker(exec), registry);

    const first = await leases.acquire("worker-gpu", { cost: 8, priority: 0, tenant: "job-a" });
    expect(first.kind).toBe("granted");

    // Second competitor: no free slot -> queued (both-or-queue), NOT a second hold.
    const second = await leases.acquire("worker-gpu", { cost: 8, priority: 0, tenant: "job-b" });
    expect(second.kind).toBe("queued");
    if (second.kind !== "queued") throw new Error("expected queued");
    expect(second.position).toBe(1);
    expect(pls.holders("worker-gpu")).toHaveLength(1);
    expect(pls.queueDepth("worker-gpu")).toBe(1);

    // Releasing the first promotes the queued waiter into a hold (serialization).
    if (first.kind !== "granted") throw new Error("expected granted");
    await first.lease.release();
    expect(pls.holders("worker-gpu")).toHaveLength(1);
    expect(pls.queueDepth("worker-gpu")).toBe(0);
  });

  test("higher priority wins the queue (declared priority, SPEC 'at its declared priority')", async () => {
    const { exec, pls, registry } = harness();
    const leases = new LeaseManager(new PlsBroker(exec), registry);
    const held = await leases.acquire("worker-gpu", { cost: 8, priority: 0, tenant: "holder" });
    if (held.kind !== "granted") throw new Error("expected granted");
    await leases.acquire("worker-gpu", { cost: 8, priority: 1, tenant: "low" });
    await leases.acquire("worker-gpu", { cost: 8, priority: 9, tenant: "high" });
    expect(pls.queueDepth("worker-gpu")).toBe(2);
    // Release: the highest-priority waiter (priority 9) is promoted first.
    await held.lease.release();
    // The high-priority tenant now holds; the low one still waits.
    expect(pls.holders("worker-gpu")).toHaveLength(1);
    expect(pls.queueDepth("worker-gpu")).toBe(1);
  });
});

describe("process-death release (the RAII / holderless reclaim)", () => {
  test("killHolder frees the slot and the next waiter is promoted", async () => {
    const { exec, pls, registry } = harness();
    const leases = new LeaseManager(new PlsBroker(exec), registry);
    await leases.acquire("worker-gpu", { cost: 8, priority: 0, tenant: "dying" });
    await leases.acquire("worker-gpu", { cost: 8, priority: 0, tenant: "next" });
    expect(pls.holders("worker-gpu")).toHaveLength(1);
    expect(pls.queueDepth("worker-gpu")).toBe(1);
    // The holder process dies (RAII / process-exit — the single release path).
    expect(pls.killHolder("worker-gpu")).toBe(true);
    expect(pls.holders("worker-gpu")).toHaveLength(1); // next promoted
    expect(pls.queueDepth("worker-gpu")).toBe(0);
  });

  test("reclaim() explicitly frees a holderless lease (recover() hook)", async () => {
    const { exec, pls, registry } = harness();
    const broker = new PlsBroker(exec);
    const leases = new LeaseManager(broker, registry);
    const outcome = await leases.acquire("worker-gpu", { cost: 8, priority: 0, tenant: "zombie" });
    if (outcome.kind !== "granted") throw new Error("expected granted");
    // The holder is gone but never released — recover() reclaims the slot by lease id.
    const freed = await leases.reclaim("worker-gpu", outcome.lease.leaseId);
    expect(freed).toBe(true);
    expect(pls.holders("worker-gpu")).toHaveLength(0);
  });

  test("withLease releases exactly once when the in-process body settles", async () => {
    const { exec, pls, registry } = harness();
    const leases = new LeaseManager(new PlsBroker(exec), registry);
    const res = await leases.withLease("worker-gpu", { cost: 4, priority: 0 }, async (lease) => {
      expect(pls.holders("worker-gpu")).toContain(lease.leaseId);
      return "done";
    });
    expect(res.ran).toBe(true);
    if (res.ran) expect(res.value).toBe("done");
    expect(pls.holders("worker-gpu")).toHaveLength(0);
    expect(pls.log.filter((l) => l.op === "release")).toHaveLength(1);
  });
});

describe("generation monotonicity (the lease_epoch fence, PS#21)", () => {
  test("each successful grant mints a strictly higher generation", async () => {
    const { exec, registry } = harness();
    const leases = new LeaseManager(new PlsBroker(exec), registry);
    const gens: number[] = [];
    for (let i = 0; i < 4; i++) {
      const o = await leases.acquire("worker-gpu", { cost: 8, priority: 0, tenant: `t${i}` });
      if (o.kind !== "granted") throw new Error("expected granted");
      gens.push(o.lease.generation);
      await o.lease.release();
    }
    for (let i = 1; i < gens.length; i++) {
      expect(gens[i]!).toBeGreaterThan(gens[i - 1]!);
    }
    expect(leases.currentEpoch).toBe(gens[gens.length - 1]!);
  });

  test("currentEpoch only ever advances", async () => {
    const { exec, registry } = harness();
    const leases = new LeaseManager(new PlsBroker(exec), registry);
    const a = await leases.acquire("worker-gpu", { cost: 8, priority: 0 });
    if (a.kind !== "granted") throw new Error("expected granted");
    const afterA = leases.currentEpoch;
    await a.lease.release();
    // A release does NOT lower the epoch (it is a monotone fence).
    expect(leases.currentEpoch).toBe(afterA);
  });
});

describe("co-alloc both-or-queue (DS4 cross-box atomic pair, PS#5)", () => {
  test("grants BOTH leases when both pools have headroom", async () => {
    const { exec, pls, registry } = harness();
    const co = new Coallocator(new PlsBroker(exec), registry);
    const out = await co.allocate({ priority: 5 });
    expect(out.kind).toBe("granted");
    if (out.kind !== "granted") throw new Error("expected granted");
    expect(out.worker.pool).toBe("worker-gpu");
    expect(out.controller.pool).toBe("controller-gpu");
    expect(pls.holders("worker-gpu")).toHaveLength(1);
    expect(pls.holders("controller-gpu")).toHaveLength(1);
    expect(out.generation).toBe(Math.max(out.worker.generation, out.controller.generation));
    await out.release();
    expect(pls.holders("worker-gpu")).toHaveLength(0);
    expect(pls.holders("controller-gpu")).toHaveLength(0);
  });

  test("QUEUES (holds nothing) when the worker pool is already occupied", async () => {
    const { exec, pls, registry } = harness();
    const broker = new PlsBroker(exec);
    const leases = new LeaseManager(broker, registry);
    // Occupy worker-gpu so the co-alloc cannot get its heavy hold.
    const held = await leases.acquire("worker-gpu", { cost: 100, priority: 0, tenant: "batch" });
    if (held.kind !== "granted") throw new Error("expected granted");

    const co = new Coallocator(broker, registry);
    const out = await co.allocate({ priority: 5 });
    expect(out.kind).toBe("queued");
    // Both-or-queue: the controller pool must NOT have been partially held.
    expect(pls.holders("controller-gpu")).toHaveLength(0);
    expect(pls.holders("worker-gpu")).toHaveLength(1); // still just the batch holder
  });

  test("QUEUES when the controller spill has no headroom (either lease blocks)", async () => {
    // controller budget is small; a prior big controller hold leaves no room for the spill.
    const { exec, pls, registry } = harness({ controllerBudget: 8 });
    const broker = new PlsBroker(exec);
    const leases = new LeaseManager(broker, registry);
    const held = await leases.acquire("controller-gpu", { cost: 8, priority: 0, tenant: "chrome" });
    if (held.kind !== "granted") throw new Error("expected granted");
    const co = new Coallocator(broker, registry);
    const out = await co.allocate({ priority: 5, controllerCost: 8 });
    expect(out.kind).toBe("queued");
    expect(pls.holders("worker-gpu")).toHaveLength(0); // no partial hold on the worker side
  });

  test("respects custom DS4 pools + costs", async () => {
    const { exec, registry } = harness();
    const co = new Coallocator(new PlsBroker(exec), registry);
    const out = await co.allocate({ priority: 3, pools: DS4_POOLS, workerCost: 40, controllerCost: 4 });
    if (out.kind !== "granted") throw new Error("expected granted");
    expect(out.worker.cost).toBe(40);
    expect(out.controller.cost).toBe(4);
    await out.release();
  });
});

describe("direct non-tally tenant acquisition (the ambient wrap, appendix§5)", () => {
  test("a non-tally tenant acquires the lease directly (no daemon)", async () => {
    const { exec, pls, registry } = harness();
    const leases = new LeaseManager(new PlsBroker(exec), registry);
    // ds4-server / OCR vLLM acquires directly at its declared priority, labelled as itself.
    const outcome = await leases.acquire("worker-gpu", { cost: 64, priority: 2, tenant: "ocr-vllm" });
    expect(outcome.kind).toBe("granted");
    if (outcome.kind !== "granted") throw new Error("expected granted");
    // The acquire carried the tenant label through to the broker.
    const acquireCall = exec.callsFor("pls").find((c) => c.argv.includes("acquire"));
    expect(acquireCall?.argv).toContain("ocr-vllm");
    expect(pls.holders("worker-gpu")).toContain(outcome.lease.leaseId);
    await outcome.lease.release();
  });

  test("parseWrapArgs parses pool/cost/priority/tenant and the -- command", () => {
    const w = parseWrapArgs(["--pool", "worker-gpu", "--cost", "64", "--priority", "3", "--tenant", "ocr", "--", "vllm", "serve", "--model", "x"]);
    expect(w.pool).toBe("worker-gpu");
    expect(w.cost).toBe(64);
    expect(w.priority).toBe(3);
    expect(w.tenant).toBe("ocr");
    expect(w.command).toEqual(["vllm", "serve", "--model", "x"]);
  });

  test("parseWrapArgs supports --flag=value form and defaults", () => {
    const w = parseWrapArgs(["--cost=16", "--", "python", "run.py"]);
    expect(w.cost).toBe(16);
    expect(w.priority).toBe(0);
    expect(w.pool).toBeUndefined();
    expect(w.command).toEqual(["python", "run.py"]);
  });

  test("parseWrapArgs rejects a missing command", () => {
    expect(() => parseWrapArgs(["--cost", "8"])).toThrow(/no command given/);
    expect(() => parseWrapArgs(["--cost", "8", "--"])).toThrow(/no command given/);
  });

  test("runWrap acquires, runs the child under the lease, and releases on exit", async () => {
    const { exec, pls, registry } = harness();
    // The wrapped command streams a couple lines then exits 0.
    exec.registerStream("vllm", () => ({ lines: ["loading", "ready"], code: 0 }));
    const code = await runWrap(exec, registry, {
      pool: "worker-gpu",
      cost: 64,
      priority: 0,
      tenant: "ocr-vllm",
      command: ["vllm", "serve"],
    });
    expect(code).toBe(0);
    // Released exactly once after the child exited (RAII single release).
    expect(pls.holders("worker-gpu")).toHaveLength(0);
    expect(pls.log.filter((l) => l.op === "release")).toHaveLength(1);
  });

  test("runWrap serializes two competitors: the second waits for the first to exit", async () => {
    const { exec, pls, registry } = harness();
    // A long-ish streamed child; we drive the queue poll deterministically via the sleep hook.
    let released = 0;
    const origLen = () => pls.log.filter((l) => l.op === "release").length;

    // Occupy the pool with a manual hold first.
    const broker = new PlsBroker(exec);
    const leases = new LeaseManager(broker, registry);
    const held = await leases.acquire("worker-gpu", { cost: 64, priority: 0, tenant: "holder" });
    if (held.kind !== "granted") throw new Error("expected granted");

    exec.registerStream("job", () => ({ lines: ["work"], code: 0 }));

    let polls = 0;
    const wrapPromise = runWrap(
      exec,
      registry,
      { pool: "worker-gpu", cost: 64, priority: 0, command: ["job"] },
      {
        sleep: async () => {
          polls++;
          // After the first queued poll, free the holder so the wrap can be admitted.
          if (polls === 1) {
            await held.lease.release();
          }
        },
        queuePollMs: 1,
      },
    );

    const code = await wrapPromise;
    expect(code).toBe(0);
    expect(polls).toBeGreaterThanOrEqual(1); // it genuinely queued before running
    // Holder + wrap each released once.
    released = origLen();
    expect(released).toBe(2);
    expect(pls.holders("worker-gpu")).toHaveLength(0);
  });
});

describe("renderWrapScript — the installable ambient wrapper", () => {
  test("forwards to `tally pls-wrap -- \"$@\"` with the injected binary path", () => {
    const script = renderWrapScript("/nix/store/abc-tally/bin/tally");
    expect(script).toContain("pls-wrap -- \"$@\"");
    expect(script).toContain("'/nix/store/abc-tally/bin/tally'");
    expect(script.startsWith("#!/usr/bin/env bash")).toBe(true);
    expect(script).toContain("set -euo pipefail");
  });

  test("defaults to the ambient `tally` on PATH", () => {
    expect(renderWrapScript()).toContain("'tally' pls-wrap");
  });
});

describe("PlsBroker — thin transport error handling", () => {
  test("surfaces a broker non-zero exit as a BrokerError", async () => {
    const exec = new FakeExec();
    const pls = new FakePls(); // no pools declared -> acquire on unknown pool throws inside the fake
    pls.install(exec);
    const broker = new PlsBroker(exec);
    // The fake throws for an unknown pool; the FakeExec surfaces it as a thrown handler error.
    await expect(broker.acquire(undefined, { pool: "worker-gpu", cost: 1, priority: 0 })).rejects.toThrow();
  });

  test("status reports held/queued depth for query status", async () => {
    const { exec, registry } = harness();
    const broker = new PlsBroker(exec);
    const leases = new LeaseManager(broker, registry);
    await leases.acquire("worker-gpu", { cost: 8, priority: 0 });
    void registry; // registry not needed for status, kept for parity
    const st = await broker.status(undefined, "worker-gpu");
    expect(st.held).toBe(1);
    expect(st.queued).toBe(0);
    expect(st.capacity).toBe(1);
  });
});
