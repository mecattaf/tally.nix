// test/e2e/barrier.test.ts
//
// The barrier primitive (IMPLEMENTATION-PLAN M4.1 case 4): `enqueue --wait` + a 3-job barrier via
// `--wait-group`. barrier = enqueue-N-await-N over `session.wait` (subsumes wait_for_subagents,
// CLI-SURFACE §1.1a). `--wait` mirrors the verdict as the exit code; `--timeout` never cancels.
//
// Two real drives:
//   • the jobs engine's own barrier primitives — waitForJob (single, verdict → exit code) and
//     waitForBarrier (N-of-M group), the exact primitives `--wait` / `--wait-group --wait-count` map
//     to (as jobs.test.ts asserts at the unit level, replayed here as an integration drive);
//   • a live daemon: a §2 client issues `session.wait {subject:job, count:3}` over the socket while a
//     3-job group is admitted, and the terminal `job.completed` deltas resolve the barrier over the
//     wire — proving the enqueue → lease → dispatch → complete → barrier path across the transport.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  bootDaemonHarness,
  makeEngineHarness,
  ocrEnqueue,
  tick,
  type DaemonHarness,
  type EngineHarness,
} from "./helpers.ts";

describe("barrier — the engine primitive `--wait` / `--wait-group` map to (M4.1 case 4)", () => {
  let h: EngineHarness;
  beforeEach(() => {
    h = makeEngineHarness();
  });
  afterEach(() => h.cleanup());

  test("`--wait` on a single job mirrors the verdict (pass) as exit code 0", async () => {
    const res = await h.engine.enqueue(
      ocrEnqueue(h.artifactPath("wait-pass.txt"), { source: "r2", wait: true }),
    );
    const w = await h.engine.waitForJob(res.task_uuid!, 1000);
    expect(w.timedOut).toBe(false);
    expect(w.verdict).toBe("pass");
    expect(w.exitCode).toBe(0);
  });

  test("`--wait` on a failing job mirrors the failure as a non-zero exit code", async () => {
    const res = await h.engine.enqueue({
      priority: "low",
      source: "r2",
      kind: "shell",
      argv: ["boom"], // exits 7
      wait: true,
      evidence: [{ kind: "exit", code: 0 }],
    });
    const w = await h.engine.waitForJob(res.task_uuid!, 1000);
    expect(w.timedOut).toBe(false);
    expect(w.exitCode).not.toBe(0);
  });

  test("a 3-job `--wait-group` barrier resolves when all three reach terminal (enqueue-3-await-3)", async () => {
    const group = "crew-3";
    for (let i = 0; i < 3; i++) {
      await h.engine.enqueue(ocrEnqueue(h.artifactPath(`crew-${i}.txt`), { barrier: group }));
    }
    const r = await h.engine.waitForBarrier(group, 3, 1000);
    expect(r.timedOut).toBe(false);
    expect(r.satisfied).toBe(3);
    // All passed ⇒ the group exit code is 0 (the worst verdict in the group).
    expect(r.exitCode).toBe(0);
  });

  test("a mixed group's exit code reflects the worst verdict (any failure ⇒ non-zero)", async () => {
    const group = "crew-mixed";
    await h.engine.enqueue(ocrEnqueue(h.artifactPath("ok.txt"), { barrier: group }));
    await h.engine.enqueue({
      priority: "low",
      source: "orchestrator",
      kind: "shell",
      argv: ["boom"],
      barrier: group,
      evidence: [{ kind: "exit", code: 0 }],
    });
    const r = await h.engine.waitForBarrier(group, 2, 1000);
    expect(r.satisfied).toBe(2);
    expect(r.exitCode).not.toBe(0);
  });

  test("`--timeout` never cancels: the barrier reports timed_out but the jobs still ran", async () => {
    const group = "crew-timeout";
    // Await MORE than were enqueued so the count never completes ⇒ timeout.
    await h.engine.enqueue(ocrEnqueue(h.artifactPath("t-0.txt"), { barrier: group }));
    const r = await h.engine.waitForBarrier(group, 5, 30);
    expect(r.timedOut).toBe(true);
    // The one job that WAS enqueued still ran to terminal (timeout does not cancel work).
    expect(r.satisfied).toBeGreaterThanOrEqual(1);
  });
});

describe("barrier over the socket — `session.wait {subject:job}` resolves on the wire (M4.1 case 4)", () => {
  let dh: DaemonHarness;
  beforeEach(async () => {
    dh = await bootDaemonHarness();
  });
  afterEach(async () => {
    await dh.stop();
    dh.cleanup();
  });

  test("a §2 client's job barrier over count:3 resolves as the 3 group jobs complete", async () => {
    const c = await dh.client();

    // Pause admission so the group can be enqueued WITHOUT running (we need the job_ids before the
    // terminal deltas fire — the wait keys on job_id, §2.4). Enqueue 3, collect their job_ids, start
    // the socket wait, then resume so the deltas arrive on the subscribed stream.
    await c.call("queue.pause", {});

    const jobIds: string[] = [];
    for (let i = 0; i < 3; i++) {
      const res = (await c.call<{ status: string }>(
        "queue.enqueue",
        ocrEnqueue(dh.artifactPath(`grp-${i}.txt`), { barrier: "grp", source: "r2" }),
      )) as { status: string };
      expect(res.status).toBe("queued");
    }
    for (const j of dh.engine.allJobs()) jobIds.push(j.job_id);
    expect(jobIds.length).toBe(3);

    // Start the barrier wait over the socket (it subscribes to job.completed on the daemon bus).
    const waitPromise = c.call<{ satisfied: unknown[] }>(
      "session.wait",
      { predicate: { subject: "job", job_ids: jobIds, until: ["completed"], count: 3 } },
      5000,
    );
    await tick(20); // let the wait subscribe before the deltas fire

    // Resume: the three jobs admit, run, and each emits job.completed onto the wire.
    await c.call("queue.resume", {});

    const result = await waitPromise;
    expect(result.satisfied.length).toBe(3);
  });

  test("a job barrier over the socket times out cleanly when a job never completes", async () => {
    const c = await dh.client();
    // No job with this id will ever complete ⇒ a bounded timeout returns timed_out.
    const result = await c.call<{ timed_out?: boolean; pending?: string[] }>(
      "session.wait",
      { predicate: { subject: "job", job_ids: ["ghost-job"], until: ["completed"], count: 1 }, timeout_ms: 40 },
      5000,
    );
    expect(result.timed_out).toBe(true);
    expect(result.pending).toContain("ghost-job");
  });
});
