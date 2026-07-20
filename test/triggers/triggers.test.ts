// tally — triggers module tests (M2.5): the three-ingress trigger surface, ONE queue, no path
// privileged (PS#16b). Covers: drop-file → job with correct provenance `source`, malformed
// quarantine, `queue.drain` idempotence (double-drain enqueues once), socket-absent oneshot fails
// cleanly, and all three ingress paths landing in one queue.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { existsSync, readdirSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { join } from "node:path";
import { makeTmpEnv, type TmpEnv } from "../helpers/tmp";
import {
  EventsDir,
  Drainer,
  TriggersModule,
  runDrainOneshot,
  type EnqueueFn,
} from "../../src/triggers/index";
import type { EnqueueParams, EnqueueResult, Source } from "../../src/contracts/job";
import type { DaemonMount, RpcHandler, WatcherHandler, SupervisedLoop } from "../../src/contracts/bus";

// --- helpers ---------------------------------------------------------------------------------

/** A recording enqueue fn: captures every admitted param set and returns a canned result. */
function recordingEnqueue(): { fn: EnqueueFn; calls: EnqueueParams[] } {
  const calls: EnqueueParams[] = [];
  let n = 0;
  const fn: EnqueueFn = async (params) => {
    calls.push(params);
    n++;
    const result: EnqueueResult = {
      task_uuid: `uuid-${n}`,
      job_id: `job-${n}`,
      lease_epoch: 1,
      pool: params.pool ?? "worker-gpu",
      status: "queued",
      session_ref: null,
      dedup_key: params.dedup_key ?? null,
      witness_lsn: n,
      verdict: null,
    };
    return result;
  };
  return { fn, calls };
}

/** A minimal valid Seam-A enqueue payload with a chosen provenance source. */
function dropPayload(source: Source, extra: Partial<EnqueueParams> = {}): Record<string, unknown> {
  return {
    priority: "medium",
    source,
    kind: "shell",
    invocation: "echo hi",
    ...extra,
  };
}

/** Write a drop file into the events/ dir. */
function drop(env: TmpEnv, name: string, body: unknown): string {
  const path = join(env.eventsDir, name);
  writeFileSync(path, typeof body === "string" ? body : JSON.stringify(body), "utf8");
  return path;
}

/** A fake DaemonMount recording registered RPCs / watchers / supervised loops. */
class FakeMount implements DaemonMount {
  readonly rpcs = new Map<string, RpcHandler>();
  readonly watchers = new Map<string, WatcherHandler>();
  readonly supervised: SupervisedLoop[] = [];
  registerRpc(method: string, handler: RpcHandler): void {
    this.rpcs.set(method, handler);
  }
  registerWatcher(path: string, handler: WatcherHandler): void {
    this.watchers.set(path, handler);
  }
  registerSupervised(loop: SupervisedLoop): void {
    this.supervised.push(loop);
  }
}

let env: TmpEnv;
beforeEach(() => {
  env = makeTmpEnv();
});
afterEach(() => {
  env.cleanup();
});

// --- events-dir ingress ----------------------------------------------------------------------

describe("events/ drop-directory ingress", () => {
  test("a valid drop file becomes one job with the file's declared provenance source", async () => {
    const { fn, calls } = recordingEnqueue();
    const events = new EventsDir({ env, enqueue: fn });
    drop(env, "job-1.json", dropPayload("r2", { dedup_key: "paper-42" }));

    const res = await events.sweep();

    expect(res.accepted).toBe(1);
    expect(res.rejected).toBe(0);
    expect(calls.length).toBe(1);
    // Provenance is honored verbatim — no path is privileged, the events dir never rewrites source.
    expect(calls[0]!.source).toBe("r2");
    expect(calls[0]!.dedup_key).toBe("paper-42");
    expect(calls[0]!.kind).toBe("shell");
    // The file is archived to done/, not left in the drop dir.
    expect(existsSync(join(env.eventsDir, "job-1.json"))).toBe(false);
    expect(readdirSync(join(env.eventsDir, "done"))).toContain("job-1.json");
  });

  test("every provenance source passes through unchanged (no path privileged)", async () => {
    const { fn, calls } = recordingEnqueue();
    const events = new EventsDir({ env, enqueue: fn });
    const sources: Source[] = ["r2", "gh", "calendar", "manual", "orchestrator"];
    sources.forEach((s, i) => drop(env, `s-${i}.json`, dropPayload(s)));

    await events.sweep();

    expect(calls.map((c) => c.source).sort()).toEqual([...sources].sort());
  });

  test("a malformed file is quarantined to rejected/ + a diagnostic is emitted, and does not block siblings", async () => {
    const { fn, calls } = recordingEnqueue();
    const notices: string[] = [];
    const events = new EventsDir({ env, enqueue: fn, notice: (l) => notices.push(l) });

    drop(env, "bad-json.json", "{ this is not json ");
    drop(env, "bad-params.json", { priority: "medium", source: "r2" }); // missing kind + invocation/argv
    drop(env, "good.json", dropPayload("manual"));

    const res = await events.sweep();

    expect(res.accepted).toBe(1);
    expect(res.rejected).toBe(2);
    expect(calls.length).toBe(1); // only the good one enqueued

    const rejected = readdirSync(join(env.eventsDir, "rejected"));
    expect(rejected).toContain("bad-json.json");
    expect(rejected).toContain("bad-params.json");
    expect(readdirSync(join(env.eventsDir, "done"))).toContain("good.json");

    // Two diagnostic notices, structured for journald capture (SyslogIdentifier=tally).
    expect(notices.length).toBe(2);
    for (const n of notices) {
      const parsed = JSON.parse(n) as Record<string, unknown>;
      expect(parsed.SYSLOG_IDENTIFIER).toBe("tally");
      expect(parsed.TALLY_TRIGGER).toBe("events-dir");
      expect(typeof parsed.TALLY_REJECT_REASON).toBe("string");
    }
  });

  test("an enqueue failure quarantines the file rather than re-sweeping it forever", async () => {
    const notices: string[] = [];
    const failing: EnqueueFn = async () => {
      throw new Error("pls broker unreachable");
    };
    const events = new EventsDir({ env, enqueue: failing, notice: (l) => notices.push(l) });
    drop(env, "job.json", dropPayload("orchestrator"));

    const res = await events.sweep();

    expect(res.rejected).toBe(1);
    expect(readdirSync(join(env.eventsDir, "rejected"))).toContain("job.json");
    expect(notices[0]).toContain("enqueue failed");
  });

  test("a name collision in an archive dir does not overwrite an earlier file", async () => {
    const { fn } = recordingEnqueue();
    const events = new EventsDir({ env, enqueue: fn });

    drop(env, "dup.json", dropPayload("manual"));
    await events.sweep();
    drop(env, "dup.json", dropPayload("manual"));
    await events.sweep();

    const done = readdirSync(join(env.eventsDir, "done"));
    expect(done).toContain("dup.json");
    expect(done).toContain("dup.json.1");
  });
});

// --- drain (in-daemon: sweep + re-present) ---------------------------------------------------

describe("in-daemon drain", () => {
  test("queue.drain sweeps events/ and re-presents durable rows in one pass", async () => {
    const { fn, calls } = recordingEnqueue();
    let represented = 0;
    const events = new EventsDir({ env, enqueue: fn });
    const drainer = new Drainer({ eventsDir: events, represent: async () => (represented += 3) });

    drop(env, "a.json", dropPayload("r2"));
    drop(env, "b.json", dropPayload("calendar"));

    const res = await drainer.drain();

    expect(res.enqueued).toBe(2);
    expect(res.represented).toBe(3);
    expect(calls.length).toBe(2);
    expect(represented).toBe(3);
  });

  test("queue.drain is idempotent: a double-drain enqueues each file exactly once", async () => {
    const { fn, calls } = recordingEnqueue();
    const events = new EventsDir({ env, enqueue: fn });
    const drainer = new Drainer({ eventsDir: events });

    drop(env, "once.json", dropPayload("manual"));

    const first = await drainer.drain();
    const second = await drainer.drain();

    expect(first.enqueued).toBe(1);
    expect(second.enqueued).toBe(0); // already archived — the fence
    expect(calls.length).toBe(1);
  });

  test("drain without a represent seam still sweeps events/, re-presenting zero rows", async () => {
    const { fn, calls } = recordingEnqueue();
    const events = new EventsDir({ env, enqueue: fn });
    const drainer = new Drainer({ eventsDir: events });
    drop(env, "x.json", dropPayload("gh"));

    const res = await drainer.drain();

    expect(res.enqueued).toBe(1);
    expect(res.represented).toBe(0);
    expect(calls.length).toBe(1);
  });
});

// --- module mount ----------------------------------------------------------------------------

describe("TriggersModule mount", () => {
  test("mount registers the queue.drain RPC handler and the events/ watcher", () => {
    const { fn } = recordingEnqueue();
    const mod = new TriggersModule({ env, enqueue: fn });
    const mount = new FakeMount();

    mod.mount(mount);

    expect(mount.rpcs.has("queue.drain")).toBe(true);
    expect(mount.watchers.has(mod.events.path)).toBe(true);
    expect(mount.supervised.length).toBe(0); // triggers registers no supervised loop
  });

  test("the mounted queue.drain handler drives the in-daemon sweep", async () => {
    const { fn, calls } = recordingEnqueue();
    const mod = new TriggersModule({ env, enqueue: fn });
    const mount = new FakeMount();
    mod.mount(mount);

    drop(env, "handler.json", dropPayload("r2"));
    const result = (await mount.rpcs.get("queue.drain")!({})) as { enqueued: number };

    expect(result.enqueued).toBe(1);
    expect(calls.length).toBe(1);
  });

  test("the mounted events/ watcher edge triggers a sweep", async () => {
    const { fn, calls } = recordingEnqueue();
    const mod = new TriggersModule({ env, enqueue: fn });
    const mount = new FakeMount();
    mod.mount(mount);

    drop(env, "watched.json", dropPayload("manual"));
    await mount.watchers.get(mod.events.path)!(join(env.eventsDir, "watched.json"));

    expect(calls.length).toBe(1);
  });
});

// --- one queue: all three ingress paths converge ---------------------------------------------

describe("one queue, no path privileged (PS#16b)", () => {
  test("events/ drop, timer drain, and live socket-enqueue all land in ONE enqueue path", async () => {
    const { fn, calls } = recordingEnqueue();
    const mod = new TriggersModule({ env, enqueue: fn });

    // (1) events/ drop dir.
    drop(env, "from-events.json", dropPayload("r2"));
    // (3) live socket-enqueue is Seam A itself — model it as a direct call to the same fn.
    await fn({ priority: "high", source: "orchestrator", kind: "pi", invocation: "pi run" });
    // (2) timer drain sweeps the drop dir (a thin client would issue queue.drain → this in-daemon path).
    await mod.drain();

    // All three ingress paths funneled through the ONE enqueue fn.
    expect(calls.length).toBe(2); // the direct socket-enqueue + the drained drop file
    expect(calls.map((c) => c.source).sort()).toEqual(["orchestrator", "r2"]);
  });
});

// --- oneshot (thin socket client) ------------------------------------------------------------

describe("tally daemon drain oneshot", () => {
  test("fails cleanly (exit non-zero) when the socket is absent", async () => {
    const stderr: string[] = [];
    const code = await runDrainOneshot({ env, stderr: (l) => stderr.push(l) });

    expect(code).toBe(1);
    expect(stderr.join("\n")).toContain("socket absent");
  });

  test("issues queue.drain over the socket and returns 0 when the daemon serves it", async () => {
    // A minimal §2-framing server standing in for the daemon: it answers the one queue.drain request.
    let served: unknown = null;
    const server = createServer((sock) => {
      sock.setEncoding("utf8");
      let buf = "";
      sock.on("data", (chunk: string) => {
        buf += chunk;
        let idx: number;
        while ((idx = buf.indexOf("\n")) !== -1) {
          const line = buf.slice(0, idx);
          buf = buf.slice(idx + 1);
          if (line.trim().length === 0) continue;
          const req = JSON.parse(line) as { id: string; method: string; params: unknown };
          served = req.method;
          sock.write(JSON.stringify({ id: req.id, result: { enqueued: 0, represented: 0 } }) + "\n");
        }
      });
    });
    await new Promise<void>((resolve) => server.listen(env.socketPath, resolve));

    const stderr: string[] = [];
    const code = await runDrainOneshot({ env, stderr: (l) => stderr.push(l) });
    server.close();

    expect(code).toBe(0);
    expect(served).toBe("queue.drain");
    expect(stderr.join("\n")).toContain("drain served");
  });
});
