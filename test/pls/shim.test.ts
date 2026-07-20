// test/pls/shim.test.ts
//
// Direct coverage of the `pls` shim itself (nix/pls-shim.py) — test/pls/pls.test.ts only exercises
// broker.ts against the in-process fake-pls fake and never spawns the actual script an operator's
// `pls` binary IS in production (see nix/pls-shim.py's header). Regression for issue #1: cmd_acquire
// (single-pool path) must release a not-granted (queued) waiting ticket before it exits — mirroring
// cmd_coalloc's existing "release the just-created waiting ticket too" rollback — because a one-shot
// CLI invocation leaves nothing alive to hold the ticket. Left orphaned, the broker later promotes it
// to holder with no live process behind it, burning its TTL for nothing and deadlocking the pool.
//
// Regression for issue #8: the shim had no lease-inspection subcommand at all — `pls status --pool
// <p>` (the frozen wire contract broker.ts parses) only ever reports one pool's held/queued COUNTS,
// discarding the held/waiting ticket contents GET /status already returns. `pls list` (and `pls
// status` with no --pool) must enumerate every pool this box knows about (PLS_POOL_URLS) and print
// each pool's actual holder/waiting tickets (id/age/ttl), human-readable and --json.
//
// Regression for issue #4: the shim's grant counter and the daemon's boot-fence `epoch` counter
// must converge on ONE monotone lease_epoch series — a grant issued after a daemon restart floors
// at the daemon's announced epoch, never re-interleaving two independent counters under one name.
//
// Spawns python3 against a tiny in-test HTTP broker fake (Bun.serve) standing in for upstream pls.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { afterEach, describe, expect, test } from "bun:test";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { makeTmpEnv, type TmpEnv } from "../helpers/tmp.ts";

const SHIM = join(import.meta.dir, "..", "..", "nix", "pls-shim.py");

interface RecordedRequest {
  method: string;
  path: string;
}

/** A minimal fake upstream broker: /acquire returns a scripted state, /release/<id> is recorded. */
function fakeBroker(acquireState: "held" | "waiting") {
  const requests: RecordedRequest[] = [];
  const server = Bun.serve({
    port: 0,
    fetch(req) {
      const url = new URL(req.url);
      requests.push({ method: req.method, path: url.pathname });
      if (url.pathname === "/acquire" && req.method === "POST") {
        if (acquireState === "held") {
          return Response.json({ id: "lease-1", state: "held" });
        }
        return Response.json({ id: "wait-1", state: "waiting", position: 1 });
      }
      if (url.pathname.startsWith("/release/") && req.method === "POST") {
        return Response.json({ released: true });
      }
      return new Response("not found", { status: 404 });
    },
  });
  return { server, requests, url: `http://127.0.0.1:${server.port}` };
}

/** A fake upstream broker whose GET /status reports real held/waiting ticket contents. */
function statusBroker(pool: string, held: unknown[], waiting: unknown[]) {
  const server = Bun.serve({
    port: 0,
    fetch(req) {
      const url = new URL(req.url);
      if (url.pathname === "/status" && req.method === "GET") {
        return Response.json({ pool, capacity: 1, budget: 24, held, waiting });
      }
      return new Response("not found", { status: 404 });
    },
  });
  return { server, url: `http://127.0.0.1:${server.port}` };
}

let server: ReturnType<typeof Bun.serve> | undefined;
let servers: ReturnType<typeof Bun.serve>[] = [];
let tmp: TmpEnv | undefined;

afterEach(() => {
  server?.stop(true);
  server = undefined;
  for (const s of servers) s.stop(true);
  servers = [];
  tmp?.cleanup();
  tmp = undefined;
});

describe("pls-shim `acquire` — single-pool path (issue #1 regression)", () => {
  test("a granted acquire prints held and issues no release", async () => {
    tmp = makeTmpEnv();
    const broker = fakeBroker("held");
    server = broker.server;
    const proc = Bun.spawn(
      ["python3", SHIM, "acquire", "--pool", "worker-gpu", "--cost", "8", "--priority", "10"],
      { env: { ...process.env, ...tmp.env, PLS_URL: broker.url }, stdout: "pipe", stderr: "pipe" },
    );
    const code = await proc.exited;
    const out = await new Response(proc.stdout).text();
    const err = await new Response(proc.stderr).text();
    expect(err).toBe("");
    expect(code).toBe(0);
    const parsed = JSON.parse(out);
    expect(parsed.granted).toBe(true);
    expect(parsed.lease_id).toBe("lease-1");
    expect(broker.requests.some((r) => r.path.startsWith("/release/"))).toBe(false);
  });

  test("a queued (non-grant) acquire releases its waiting ticket before exiting", async () => {
    tmp = makeTmpEnv();
    const broker = fakeBroker("waiting");
    server = broker.server;
    const proc = Bun.spawn(
      ["python3", SHIM, "acquire", "--pool", "worker-gpu", "--cost", "8", "--priority", "10"],
      { env: { ...process.env, ...tmp.env, PLS_URL: broker.url }, stdout: "pipe", stderr: "pipe" },
    );
    const code = await proc.exited;
    const out = await new Response(proc.stdout).text();
    const err = await new Response(proc.stderr).text();
    expect(err).toBe("");
    expect(code).toBe(0);
    const parsed = JSON.parse(out);
    expect(parsed.granted).toBe(false);
    expect(parsed.queued).toBe(true);
    // The regression: the not-granted waiting ticket must be released, not orphaned to deadlock
    // the pool once the daemon's backoff-retry re-drives and piles up more orphans behind it.
    const releaseCalls = broker.requests.filter((r) => r.path.startsWith("/release/"));
    expect(releaseCalls).toHaveLength(1);
    expect(releaseCalls[0]!.path).toBe("/release/wait-1");
  });
});

describe("pls-shim generation — ONE monotone series with the daemon's boot fence (issue #4)", () => {
  test("a grant's generation floors at the daemon's `epoch` counter file, not just pls-generation", async () => {
    tmp = makeTmpEnv();
    // The two counters as the on-device run left them: grants had reached 7 when a daemon restart
    // pushed the boot fence to 41. The next grant must land STRICTLY past the fence (42) — before
    // the convergence the shim only read its own file and issued 8, re-interleaving the restart
    // fence and the grant series under the one `lease_epoch` wire name.
    writeFileSync(tmp.plsGenerationPath, "7\n");
    writeFileSync(tmp.epochPath, "41\n");
    const broker = fakeBroker("held");
    server = broker.server;
    const proc = Bun.spawn(
      ["python3", SHIM, "acquire", "--pool", "worker-gpu", "--cost", "8", "--priority", "10"],
      { env: { ...process.env, ...tmp.env, PLS_URL: broker.url }, stdout: "pipe", stderr: "pipe" },
    );
    const code = await proc.exited;
    const out = await new Response(proc.stdout).text();
    const err = await new Response(proc.stderr).text();
    expect(err).toBe("");
    expect(code).toBe(0);
    const parsed = JSON.parse(out);
    expect(parsed.granted).toBe(true);
    expect(parsed.generation).toBe(42);
    // The shim persists the new floor to ITS file (single writer per file: the daemon never loses
    // ownership of `epoch`, the shim never loses ownership of `pls-generation`).
    expect(readFileSync(tmp.plsGenerationPath, "utf8").trim()).toBe("42");
    expect(readFileSync(tmp.epochPath, "utf8").trim()).toBe("41");
  });
});

describe("pls-shim `list` — multi-pool lease inspection (issue #8)", () => {
  test("enumerates every pool in PLS_POOL_URLS with each held/waiting ticket's id/age/ttl", async () => {
    tmp = makeTmpEnv();
    const worker = statusBroker(
      "worker-gpu",
      [{ id: "lease-9", age_s: 42, ttl_s: 300, tenant: "tally" }],
      [{ id: "wait-3", age_s: 5, ttl_s: 60 }],
    );
    const controller = statusBroker("controller-gpu", [], []);
    servers = [worker.server, controller.server];
    const poolUrls = JSON.stringify({ "worker-gpu": worker.url, "controller-gpu": controller.url });
    const proc = Bun.spawn(["python3", SHIM, "list", "--json"], {
      env: { ...process.env, ...tmp.env, PLS_POOL_URLS: poolUrls },
      stdout: "pipe",
      stderr: "pipe",
    });
    const code = await proc.exited;
    const out = await new Response(proc.stdout).text();
    const err = await new Response(proc.stderr).text();
    expect(err).toBe("");
    expect(code).toBe(0);
    const results = JSON.parse(out) as Array<{
      pool: string;
      held: Array<{ id: string; age_s: number; ttl_s: number }>;
      waiting: Array<{ id: string; age_s: number; ttl_s: number }>;
    }>;
    expect(results).toHaveLength(2);
    const worker_ = results.find((r) => r.pool === "worker-gpu")!;
    expect(worker_.held).toEqual([{ id: "lease-9", age_s: 42, ttl_s: 300 }]);
    expect(worker_.waiting).toEqual([{ id: "wait-3", age_s: 5, ttl_s: 60 }]);
    const controller_ = results.find((r) => r.pool === "controller-gpu")!;
    expect(controller_.held).toEqual([]);
    expect(controller_.waiting).toEqual([]);
  });

  test("`pls status` with NO --pool aliases to the multi-pool list, not a single guessed pool", async () => {
    tmp = makeTmpEnv();
    const worker = statusBroker("worker-gpu", [{ id: "lease-9", age_s: 1, ttl_s: 300 }], []);
    const controller = statusBroker("controller-gpu", [], [{ id: "wait-7", age_s: 2, ttl_s: 60 }]);
    servers = [worker.server, controller.server];
    const poolUrls = JSON.stringify({ "worker-gpu": worker.url, "controller-gpu": controller.url });
    const proc = Bun.spawn(["python3", SHIM, "status", "--json"], {
      env: { ...process.env, ...tmp.env, PLS_POOL_URLS: poolUrls },
      stdout: "pipe",
      stderr: "pipe",
    });
    const code = await proc.exited;
    const out = await new Response(proc.stdout).text();
    const err = await new Response(proc.stderr).text();
    expect(err).toBe("");
    expect(code).toBe(0);
    const results = JSON.parse(out) as Array<{ pool: string; held: unknown[]; waiting: unknown[] }>;
    // Both pools reported (the OLD `status` with no --pool talked to a single guessed default
    // broker — PLS_POOL_URLS was never consulted at all — so this would previously either miss a
    // pool or fail to connect).
    expect(results.map((r) => r.pool).sort()).toEqual(["controller-gpu", "worker-gpu"]);
  });

  test("`pls list --pool <p>` narrows to one pool and prints full ticket detail (not just counts)", async () => {
    tmp = makeTmpEnv();
    const broker = statusBroker(
      "worker-gpu",
      [{ id: "lease-9", age_s: 42, ttl_s: 300 }],
      [{ id: "wait-3", age_s: 5, ttl_s: 60 }],
    );
    server = broker.server;
    const proc = Bun.spawn(["python3", SHIM, "list", "--pool", "worker-gpu"], {
      env: { ...process.env, ...tmp.env, PLS_URL: broker.url },
      stdout: "pipe",
      stderr: "pipe",
    });
    const code = await proc.exited;
    const out = await new Response(proc.stdout).text();
    const err = await new Response(proc.stderr).text();
    expect(err).toBe("");
    expect(code).toBe(0);
    expect(out).toContain("worker-gpu  capacity=1 budget=24 held=1 waiting=1");
    expect(out).toContain("held    id=lease-9 age=42s ttl=300s");
    expect(out).toContain("waiting id=wait-3 age=5s ttl=60s");
  });

  test("`pls status --pool <p>` keeps reporting counts only — the frozen broker.ts contract", async () => {
    tmp = makeTmpEnv();
    const broker = statusBroker("worker-gpu", [{ id: "lease-9", age_s: 42, ttl_s: 300 }], []);
    server = broker.server;
    const proc = Bun.spawn(["python3", SHIM, "status", "--pool", "worker-gpu"], {
      env: { ...process.env, ...tmp.env, PLS_URL: broker.url },
      stdout: "pipe",
      stderr: "pipe",
    });
    const code = await proc.exited;
    const out = await new Response(proc.stdout).text();
    expect(code).toBe(0);
    const parsed = JSON.parse(out);
    expect(parsed).toEqual({ pool: "worker-gpu", capacity: 1, budget: 24, held: 1, queued: 0 });
  });
});
