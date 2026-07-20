// daemon-core replay ring: seq/id stamping, bounded retention, gap detection (CLI-SURFACE §2.1).

import { describe, expect, test } from "bun:test";
import { ReplayRing } from "../../src/daemon/replay-ring";
import type { JobHeartbeatPayload } from "../../src/contracts/events";

function hb(job_id: string): JobHeartbeatPayload {
  return { job_id, gpu_seconds: 0 };
}

describe("replay-ring", () => {
  test("assigns monotonic seq starting at 1 and a stable uuid id", () => {
    const ring = new ReplayRing(10);
    expect(ring.latestSeq).toBe(0);
    const a = ring.append("job.heartbeat", hb("j1"));
    const b = ring.append("job.heartbeat", hb("j2"));
    expect(a.seq).toBe(1);
    expect(b.seq).toBe(2);
    expect(typeof a.id).toBe("string");
    expect(a.id).not.toBe(b.id);
    expect(ring.latestSeq).toBe(2);
    expect(ring.nextSeq).toBe(3);
  });

  test("refuses to ring a non-replayable control event", () => {
    const ring = new ReplayRing();
    expect(() => ring.append("heartbeat", { ts: "t", latest_seq: 0 })).toThrow();
    expect(() => ring.append("stream.overflow", { reason: "x", oldest_seq: 0, latest_seq: 0 })).toThrow();
  });

  test("bounds retention at capacity, evicting the oldest", () => {
    const ring = new ReplayRing(3);
    for (let i = 0; i < 5; i++) ring.append("job.heartbeat", hb(`j${i}`));
    expect(ring.size).toBe(3);
    expect(ring.oldestSeq).toBe(3);
    expect(ring.latestSeq).toBe(5);
  });

  test("resume(undefined) is a live subscription: no replay, no gap", () => {
    const ring = new ReplayRing();
    ring.append("job.heartbeat", hb("j1"));
    const r = ring.resume(undefined);
    expect(r.gap).toBe(false);
    expect(r.replay).toEqual([]);
    expect(r.after_seq).toBe(1);
    expect(r.next_seq).toBe(2);
  });

  test("resume replays events strictly after from_seq when retained", () => {
    const ring = new ReplayRing(10);
    for (let i = 1; i <= 5; i++) ring.append("job.heartbeat", hb(`j${i}`));
    const r = ring.resume(2);
    expect(r.gap).toBe(false);
    expect(r.replay.map((e) => e.seq)).toEqual([3, 4, 5]);
  });

  test("resume reports gap when from_seq fell out of the ring", () => {
    const ring = new ReplayRing(3);
    for (let i = 1; i <= 10; i++) ring.append("job.heartbeat", hb(`j${i}`)); // retains seq 8,9,10
    const r = ring.resume(2);
    expect(r.gap).toBe(true);
    expect(r.replay).toEqual([]);
    expect(r.oldest_seq).toBe(8);
    expect(r.latest_seq).toBe(10);
  });

  test("resume at latest is caught-up: no replay, no gap", () => {
    const ring = new ReplayRing(10);
    for (let i = 1; i <= 5; i++) ring.append("job.heartbeat", hb(`j${i}`));
    const r = ring.resume(5);
    expect(r.gap).toBe(false);
    expect(r.replay).toEqual([]);
  });

  test("gap when trailing an empty ring after events existed is avoided at seq 0", () => {
    const ring = new ReplayRing(10);
    // fresh ring, from_seq 0 (a client that never received anything) — nothing retained, latest 0.
    const r = ring.resume(0);
    expect(r.gap).toBe(false);
    expect(r.replay).toEqual([]);
  });
});
