// test/cli/socket-verbs.test.ts
//
// The CLI's thin-socket verbs against a fake daemon speaking the frozen §2 NDJSON wire
// (IMPLEMENTATION-PLAN M3.1 tests: "JSON output golden shapes per the §1 tables", "verdict-mirroring
// exit codes"). A minimal in-test daemon answers the RPC carriers (queue.enqueue/cancel/pause/resume,
// session.list, query.status, pane.*, agent.*, session.snapshot/subscribe) and — for the `--wait`
// barrier — pushes terminal `job.*` event frames, so the whole request→shape→output→exit path is
// exercised without booting the real daemon.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runCli } from "../../src/cli/index.ts";
import { captureWriter } from "../../src/cli/output.ts";

// ---------------------------------------------------------------------------------------------
// A minimal fake daemon: NDJSON over a Unix socket, request/response + pushed events.
// ---------------------------------------------------------------------------------------------

/** A thrown marker the FakeDaemon serializes into an error frame (the RpcError → exit mapping). */
class RpcThrow extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
  }
}

type RpcHandler = (params: unknown, ctx: FakeConn) => unknown;

interface FakeConn {
  /** Push an event frame to this connection (the `--wait` / `session watch` stream). */
  emit(event: string, payload: Record<string, unknown>): void;
}

class FakeDaemon {
  private server: ReturnType<typeof Bun.listen> | null = null;
  private readonly handlers = new Map<string, RpcHandler>();
  readonly received: Array<{ method: string; params: unknown }> = [];

  constructor(readonly socketPath: string) {}

  on(method: string, handler: RpcHandler): this {
    this.handlers.set(method, handler);
    return this;
  }

  async start(): Promise<void> {
    const handlers = this.handlers;
    const received = this.received;
    this.server = Bun.listen<{ buf: string }>({
      unix: this.socketPath,
      socket: {
        open(socket) {
          socket.data = { buf: "" };
        },
        data(socket, chunk) {
          socket.data.buf += chunk.toString("utf8");
          let nl: number;
          while ((nl = socket.data.buf.indexOf("\n")) !== -1) {
            const line = socket.data.buf.slice(0, nl);
            socket.data.buf = socket.data.buf.slice(nl + 1);
            if (line.trim().length === 0) continue;
            let req: { id?: unknown; method?: unknown; params?: unknown };
            try {
              req = JSON.parse(line);
            } catch {
              continue;
            }
            const method = String(req.method);
            received.push({ method, params: req.params });
            const conn: FakeConn = {
              emit(event, payload) {
                socket.write(JSON.stringify({ seq: 1, id: "ev-1", event, ...payload }) + "\n");
              },
            };
            const handler = handlers.get(method);
            if (!handler) {
              socket.write(JSON.stringify({ id: req.id ?? null, error: { code: "unknown_method", message: `no handler for ${method}` } }) + "\n");
              continue;
            }
            void Promise.resolve()
              .then(() => handler(req.params, conn))
              .then((result) => {
                socket.write(JSON.stringify({ id: req.id ?? null, result }) + "\n");
              })
              .catch((err: unknown) => {
                // A thrown `RpcThrow` becomes a wire `error` frame (the RpcError → exit-code path).
                if (err instanceof RpcThrow) {
                  socket.write(JSON.stringify({ id: req.id ?? null, error: { code: err.code, message: err.message } }) + "\n");
                } else {
                  socket.write(JSON.stringify({ id: req.id ?? null, error: { code: "internal", message: String(err) } }) + "\n");
                }
              });
          }
        },
      },
    });
  }

  stop(): void {
    this.server?.stop(true);
  }
}

let dir: string;
let sock: string;
let daemon: FakeDaemon;

beforeEach(async () => {
  dir = mkdtempSync(join(tmpdir(), "tally-cli-"));
  mkdirSync(dir, { recursive: true });
  sock = join(dir, "tally.sock");
});

afterEach(() => {
  daemon?.stop();
  rmSync(dir, { recursive: true, force: true });
});

// ---------------------------------------------------------------------------------------------
// queue enqueue.
// ---------------------------------------------------------------------------------------------

describe("queue enqueue (Seam A)", () => {
  test("--detach returns the enqueue result as the §1.1 JSON shape", async () => {
    daemon = new FakeDaemon(sock).on("queue.enqueue", (params) => {
      expect((params as { kind: string }).kind).toBe("shell");
      return {
        task_uuid: "uuid-1",
        lease_epoch: 42,
        pool: "worker-gpu",
        status: "queued",
        session_ref: null,
        dedup_key: null,
        witness_lsn: null,
        verdict: null,
      };
    });
    await daemon.start();

    const w = captureWriter();
    const code = await runCli(["enqueue", "--kind", "shell", "--source", "manual", "--detach", "--json", "--", "true"], {
      writer: w,
      socket: sock,
    });
    expect(code).toBe(0);
    const out = JSON.parse(w.stdout);
    expect(out.task_uuid).toBe("uuid-1");
    expect(out.pool).toBe("worker-gpu");
    expect(out.status).toBe("queued");
  });

  test("the top-level alias `tally enqueue` == `tally queue enqueue`", async () => {
    daemon = new FakeDaemon(sock).on("queue.enqueue", () => ({
      task_uuid: "uuid-2",
      lease_epoch: 1,
      pool: "worker-gpu",
      status: "queued",
      session_ref: null,
      dedup_key: null,
      witness_lsn: null,
      verdict: null,
    }));
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["queue", "enqueue", "--kind", "shell", "--detach", "--json", "--", "true"], { writer: w, socket: sock });
    expect(code).toBe(0);
    expect(JSON.parse(w.stdout).task_uuid).toBe("uuid-2");
  });

  test("a dedup `reused` result exits 0 and does not block", async () => {
    daemon = new FakeDaemon(sock).on("queue.enqueue", () => ({
      task_uuid: null,
      lease_epoch: 7,
      pool: "worker-gpu",
      status: "reused",
      session_ref: null,
      dedup_key: "k1",
      witness_lsn: 12,
      verdict: "reused",
    }));
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["enqueue", "--kind", "shell", "--dedup-key", "k1", "--wait", "--json", "--", "true"], { writer: w, socket: sock });
    expect(code).toBe(0);
    expect(JSON.parse(w.stdout).status).toBe("reused");
  });
});

// ---------------------------------------------------------------------------------------------
// --wait barrier (CLI-side stream filter; verdict-mirroring exit codes).
// ---------------------------------------------------------------------------------------------

describe("enqueue --wait — CLI-side barrier, verdict mirrors the exit code", () => {
  test("job.completed ⇒ exit 0 (via queue.await_job over the BarrierTracker)", async () => {
    daemon = new FakeDaemon(sock)
      .on("queue.enqueue", () => ({
        task_uuid: "uuid-w1",
        lease_epoch: 42,
        pool: "worker-gpu",
        status: "dispatched",
        session_ref: null,
        dedup_key: null,
        witness_lsn: null,
        verdict: null,
      }))
      // The CLI now blocks on the daemon-side BarrierTracker (drains already-terminal deltas), not a
      // live stream count — so a job that already finished resolves immediately, no hang.
      .on("queue.await_job", () => ({ verdict: "pass", exit_code: 0, timed_out: false }));
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["enqueue", "--kind", "shell", "--wait", "--json", "--", "true"], { writer: w, socket: sock });
    expect(code).toBe(0);
    const out = JSON.parse(w.stdout);
    expect(out.waited).toBe(true);
    expect(out.exit_code).toBe(0);
  });

  test("job.failed ⇒ non-zero exit", async () => {
    daemon = new FakeDaemon(sock)
      .on("queue.enqueue", () => ({
        task_uuid: "uuid-w2",
        lease_epoch: 42,
        pool: "worker-gpu",
        status: "dispatched",
        session_ref: null,
        dedup_key: null,
        witness_lsn: null,
        verdict: null,
      }))
      .on("queue.await_job", () => ({ verdict: "failed", exit_code: 1, timed_out: false }));
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["enqueue", "--kind", "shell", "--wait", "--json", "--", "false"], { writer: w, socket: sock });
    expect(code).toBe(1);
  });

  test("clean-exit-no-artifact ⇒ non-zero exit (distinguished forensic)", async () => {
    daemon = new FakeDaemon(sock)
      .on("queue.enqueue", () => ({
        task_uuid: "uuid-w3",
        lease_epoch: 42,
        pool: "worker-gpu",
        status: "dispatched",
        session_ref: null,
        dedup_key: null,
        witness_lsn: null,
        verdict: null,
      }))
      .on("queue.await_job", () => ({ verdict: "clean-exit-no-artifact", exit_code: 3, timed_out: false }));
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["enqueue", "--kind", "shell", "--wait", "--", "true"], { writer: w, socket: sock });
    expect(code).toBe(3);
  });

  test("a rowless --wait blocks by its OWN job_id via queue.await_job (issue #4)", async () => {
    // A rowless (task_uuid:null) unit has no task_uuid to key the daemon-side barrier by, but the
    // enqueue result carries its job_id — the key EVERY terminal delta is recorded under. The wait
    // must go through queue.await_job {job_id} (exact identity, drains an already-terminal delta),
    // never a stream-side lease_epoch fence: the real engine's job.completed/job.failed/
    // job.evidence_fail payloads carry NO lease_epoch field at all (contracts/events.ts), so an
    // epoch filter is a no-op that resolves on the first terminal delta of ANY job on the box.
    let awaitedParams: unknown;
    daemon = new FakeDaemon(sock)
      .on("queue.enqueue", () => ({
        task_uuid: null,
        job_id: "job-rowless-1",
        lease_epoch: 5,
        pool: "worker-gpu",
        status: "dispatched",
        session_ref: null,
        dedup_key: null,
        witness_lsn: null,
        verdict: null,
      }))
      .on("queue.await_job", (params) => {
        awaitedParams = params;
        return { verdict: "pass", exit_code: 0, timed_out: false };
      });
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(
      ["enqueue", "--kind", "shell", "--wait", "--timeout", "2s", "--json", "--", "true"],
      { writer: w, socket: sock },
    );
    expect(code).toBe(0);
    const out = JSON.parse(w.stdout);
    expect(out.waited).toBe(true);
    expect(out.exit_code).toBe(0);
    // The barrier was keyed by the job_id — the exact identity, not a task_uuid and not an epoch.
    expect((awaitedParams as { job_id?: unknown }).job_id).toBe("job-rowless-1");
    expect((awaitedParams as { task_uuid?: unknown }).task_uuid).toBeUndefined();
  });

  test("a rowless --wait mirrors ITS job's failure verdict, never a stranger's (issue #4)", async () => {
    // The daemon-side barrier resolves this wait with the waited-on job's own terminal (failed,
    // exit 1) — regardless of any other job's deltas on the box (which the old stream fence would
    // have matched first). Timeout still maps to 124 when the tracker reports timed_out.
    daemon = new FakeDaemon(sock)
      .on("queue.enqueue", () => ({
        task_uuid: null,
        job_id: "job-rowless-2",
        lease_epoch: 5,
        pool: "worker-gpu",
        status: "dispatched",
        session_ref: null,
        dedup_key: null,
        witness_lsn: null,
        verdict: null,
      }))
      .on("queue.await_job", (params) => {
        // Only the exact job_id resolves; anything else times out (barrier ≠ first-event-wins).
        if ((params as { job_id?: unknown }).job_id !== "job-rowless-2") {
          return { verdict: null, exit_code: 0, timed_out: true };
        }
        return { verdict: "failed", exit_code: 1, timed_out: false };
      });
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(
      ["enqueue", "--kind", "shell", "--wait", "--timeout", "2s", "--json", "--", "false"],
      { writer: w, socket: sock },
    );
    expect(code).toBe(1);
    const out = JSON.parse(w.stdout);
    expect(out.verdict).toBe("failed");
  });

  test("a --wait-group barrier awaits N terminals via queue.await_barrier (group-filtered)", async () => {
    let awaitedParams: unknown;
    daemon = new FakeDaemon(sock)
      .on("queue.enqueue", () => ({
        task_uuid: "uuid-g1",
        lease_epoch: 42,
        pool: "worker-gpu",
        status: "dispatched",
        session_ref: null,
        dedup_key: null,
        witness_lsn: null,
        verdict: null,
      }))
      .on("queue.await_barrier", (params) => {
        awaitedParams = params;
        return { satisfied: 3, exit_code: 0, timed_out: false };
      });
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["enqueue", "--kind", "shell", "--wait-group", "g1", "--wait-count", "3", "--json", "--", "true"], { writer: w, socket: sock });
    expect(code).toBe(0);
    const out = JSON.parse(w.stdout);
    expect(out.satisfied).toBe(3);
    // The barrier is filtered by the group id (not a blind stream count) + the requested count.
    expect((awaitedParams as { group: string }).group).toBe("g1");
    expect((awaitedParams as { count: number }).count).toBe(3);
  });

  test("the documented wait-ONLY barrier form (--wait-group without leaf argv) blocks the group and never enqueues", async () => {
    let enqueued = false;
    daemon = new FakeDaemon(sock)
      .on("queue.enqueue", () => {
        enqueued = true;
        return { task_uuid: null, lease_epoch: 42, pool: "worker-gpu", status: "queued", session_ref: null, dedup_key: null, witness_lsn: null, verdict: null };
      })
      .on("queue.await_barrier", () => ({ satisfied: 2, exit_code: 0, timed_out: false }));
    await daemon.start();
    const w = captureWriter();
    // No `--` argv, no --invocation: the documented §1.1a barrier form. It must NOT fail validation
    // ("exactly one of invocation / argv is required") and must NOT enqueue anything.
    const code = await runCli(["enqueue", "--wait-group", "g9", "--wait-count", "2", "--json"], { writer: w, socket: sock });
    expect(code).toBe(0);
    expect(enqueued).toBe(false);
    const out = JSON.parse(w.stdout);
    expect(out.satisfied).toBe(2);
  });
});

// ---------------------------------------------------------------------------------------------
// queue cancel / pause / resume.
// ---------------------------------------------------------------------------------------------

describe("queue cancel / pause / resume", () => {
  test("cancel returns the frozen §1.1 shape {task_uuid, status, was, lease_epoch} and exits 0 when a unit was affected", async () => {
    daemon = new FakeDaemon(sock).on("queue.cancel", (params) => {
      expect((params as { task_uuid: string }).task_uuid).toBe("uuid-1");
      expect((params as { force: boolean }).force).toBe(true);
      return { ok: true, affected: 1, task_uuid: "uuid-1", was: "started", lease_epoch: 42 };
    });
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["queue", "cancel", "uuid-1", "--force", "--json"], { writer: w, socket: sock });
    expect(code).toBe(0);
    const out = JSON.parse(w.stdout);
    // The frozen §1.1 --json shape: {task_uuid, status:"cancelled", was, lease_epoch}.
    expect(out.task_uuid).toBe("uuid-1");
    expect(out.status).toBe("cancelled");
    expect(out.was).toBe("started");
    expect(out.lease_epoch).toBe(42);
  });

  test("cancel with nothing affected exits non-zero (not-found class)", async () => {
    daemon = new FakeDaemon(sock).on("queue.cancel", () => ({ ok: true, affected: 0 }));
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["queue", "cancel", "nope", "--json"], { writer: w, socket: sock });
    expect(code).toBe(4);
  });

  test("pause reports the paused shape", async () => {
    daemon = new FakeDaemon(sock).on("queue.pause", (params) => {
      expect((params as { pool?: string }).pool).toBe("worker-gpu");
      return { ok: true, affected: 3 };
    });
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["queue", "pause", "worker-gpu", "--json"], { writer: w, socket: sock });
    expect(code).toBe(0);
    const out = JSON.parse(w.stdout);
    expect(out.paused).toBe(true);
    expect(out.queued_depth).toBe(3);
  });

  test("resume --all reports paused:false", async () => {
    daemon = new FakeDaemon(sock).on("queue.resume", () => ({ ok: true, affected: 0 }));
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["queue", "resume", "--all", "--json"], { writer: w, socket: sock });
    expect(code).toBe(0);
    expect(JSON.parse(w.stdout).paused).toBe(false);
  });
});

// ---------------------------------------------------------------------------------------------
// session list / query status / pane / agent — golden JSON shapes.
// ---------------------------------------------------------------------------------------------

describe("read-projection verbs", () => {
  test("session list emits one JSON record per session", async () => {
    daemon = new FakeDaemon(sock).on("session.list", () => [
      { session: "term-0707", persistence_session_id: "term-0707", workspace: "ws", status_rollup: { blocked: 0, working: 1, done: 0, idle: 1 }, panes: [{ pane: "term-0707:p1", kitty_window_id: 7, agent: { kind: "claude-code", status: "working" } }] },
    ]);
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["session", "list", "--json"], { writer: w, socket: sock });
    expect(code).toBe(0);
    const rec = JSON.parse(w.stdout.trim());
    expect(rec.session).toBe("term-0707");
    expect(rec.panes[0].agent.status).toBe("working");
  });

  test("query status is the ping (protocol_version + pools)", async () => {
    daemon = new FakeDaemon(sock).on("query.status", () => ({
      protocol_version: 1,
      pools: [{ pool: "worker-gpu", held: 1, queued: 4, budget: 128 }],
      sessions: [],
    }));
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["query", "status", "--json"], { writer: w, socket: sock });
    expect(code).toBe(0);
    const out = JSON.parse(w.stdout);
    expect(out.protocol_version).toBe(1);
    expect(out.pools[0].queued).toBe(4);
  });

  test("pane send builds the pane.send params and reports sent:true", async () => {
    daemon = new FakeDaemon(sock).on("pane.send", (params) => {
      const p = params as { pane: string; text: string };
      expect(p.pane).toBe("term-0707:p1");
      expect(p.text).toBe("hello\r"); // --enter appends CR
      return { pane: p.pane, kitty_window_id: 7, sent: true };
    });
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["pane", "send", "term-0707:p1", "hello", "--enter", "--json"], { writer: w, socket: sock });
    expect(code).toBe(0);
    expect(JSON.parse(w.stdout).sent).toBe(true);
  });

  test("pane capture --source detection surfaces a viewer_rejected error as a non-zero exit", async () => {
    daemon = new FakeDaemon(sock).on("pane.capture", () => {
      // The daemon refuses a viewer pane (anti-loop invariant #4) with a `viewer_rejected` error.
      throw new RpcThrow("viewer_rejected", "pane vw:p1 is a viewer (is_viewer=true); refused");
    });
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["pane", "capture", "vw:p1", "--source", "detection", "--json"], { writer: w, socket: sock });
    expect(code).toBe(5);
    expect(w.stderr).toContain("viewer_rejected");
  });

  test("agent list emits the table projection as JSON lines", async () => {
    daemon = new FakeDaemon(sock).on("agent.list", () => [
      { pane: "term-0707:p1", kind: "claude-code", status: "blocked", session_ref: "ref-1", cwd: "/w" },
    ]);
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["agent", "list", "--status", "blocked", "--json"], { writer: w, socket: sock });
    expect(code).toBe(0);
    expect(JSON.parse(w.stdout.trim()).status).toBe("blocked");
  });

  test("agent list rejects an invalid --status before hitting the socket", async () => {
    daemon = new FakeDaemon(sock).on("agent.list", () => []);
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["agent", "list", "--status", "bogus"], { writer: w, socket: sock });
    expect(code).toBe(2);
    expect(w.stderr).toContain("--status");
  });
});

// ---------------------------------------------------------------------------------------------
// session watch — register_viewer + snapshot.
// ---------------------------------------------------------------------------------------------

describe("session watch (Seam B)", () => {
  test("--snapshot-only marks the viewer, reads the snapshot, and stops", async () => {
    let viewerRegistered = false;
    daemon = new FakeDaemon(sock)
      .on("session.register_viewer", (params) => {
        expect((params as { kitty_window_id: number }).kitty_window_id).toBe(9);
        viewerRegistered = true;
        return { ok: true };
      })
      .on("session.snapshot", () => ({
        protocol: "tally.delta",
        protocol_version: 1,
        daemon_version: "0.1.0",
        lease_epoch: 42,
        seq: 5,
        ts: "2026-07-09T12:00:00Z",
        focus: { workspace: null, session: null, pane: null },
        workspaces: [],
        sessions: [],
        panes: [],
        agents: [],
        jobs: [],
      }));
    await daemon.start();
    const w = captureWriter();
    const code = await runCli(["session", "watch", "--snapshot-only", "--format", "jsonl"], {
      writer: w,
      socket: sock,
      env: { KITTY_WINDOW_ID: "9" },
    });
    expect(code).toBe(0);
    expect(viewerRegistered).toBe(true);
    const snap = JSON.parse(w.stdout.trim());
    expect(snap.protocol).toBe("tally.delta");
    expect(snap.seq).toBe(5);
  });
});

// ---------------------------------------------------------------------------------------------
// error surfaces.
// ---------------------------------------------------------------------------------------------

describe("error handling", () => {
  test("an absent daemon socket exits 3 with a clear message", async () => {
    const w = captureWriter();
    const code = await runCli(["query", "status", "--json"], { writer: w, socket: join(dir, "nope.sock") });
    expect(code).toBe(3);
    expect(w.stderr).toContain("daemon");
  });

  test("an unknown noun exits 127", async () => {
    const w = captureWriter();
    const code = await runCli(["frobnicate"], { writer: w, socket: sock });
    expect(code).toBe(127);
  });
});

