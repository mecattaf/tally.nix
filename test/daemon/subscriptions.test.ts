// daemon-core subscriptions: filters, backpressure/overflow, ack cursor (CLI-SURFACE §2.1, §2.4).

import { describe, expect, test } from "bun:test";
import {
  SubscriptionRegistry,
  resolveFilter,
  type FrameSink,
} from "../../src/daemon/subscriptions";
import { ReplayRing } from "../../src/daemon/replay-ring";
import { MAX_UNACKED } from "../../src/contracts/constants";
import type { JobHeartbeatPayload } from "../../src/contracts/events";

class ArraySink implements FrameSink {
  lines: string[] = [];
  closed = false;
  open = true;
  write(line: string): boolean {
    if (!this.open) return false;
    this.lines.push(line);
    return true;
  }
  close(): void {
    this.closed = true;
    this.open = false;
  }
  frames(): Record<string, unknown>[] {
    return this.lines.map((l) => JSON.parse(l.trim()) as Record<string, unknown>);
  }
}

function hb(job_id: string): JobHeartbeatPayload {
  return { job_id, gpu_seconds: 0 };
}

const encode = (f: unknown) => JSON.stringify(f) + "\n";

describe("subscriptions", () => {
  test("delivers a stamped event flattened onto the frame (seq/id/event + payload)", () => {
    const reg = new SubscriptionRegistry();
    const ring = new ReplayRing();
    const sink = new ArraySink();
    reg.create({ filter: resolveFilter({}), sink, encode });
    const ev = ring.append("job.heartbeat", hb("j1"));
    reg.fanout(ev, ring);
    const frame = sink.frames()[0]!;
    expect(frame.seq).toBe(ev.seq);
    expect(frame.id).toBe(ev.id);
    expect(frame.event).toBe("job.heartbeat");
    expect(frame.job_id).toBe("j1");
  });

  test("names filter drops non-matching events", () => {
    const reg = new SubscriptionRegistry();
    const ring = new ReplayRing();
    const sink = new ArraySink();
    reg.create({ filter: resolveFilter({ names: ["job.completed"] }), sink, encode });
    reg.fanout(ring.append("job.heartbeat", hb("j1")), ring);
    expect(sink.lines.length).toBe(0);
  });

  test("categories filter admits by family", () => {
    const reg = new SubscriptionRegistry();
    const ring = new ReplayRing();
    const sink = new ArraySink();
    reg.create({ filter: resolveFilter({ categories: ["job"] }), sink, encode });
    reg.fanout(ring.append("job.heartbeat", hb("j1")), ring);
    expect(sink.frames()[0]!.event).toBe("job.heartbeat");
  });

  test("heartbeat gated by include_heartbeat=false", () => {
    const reg = new SubscriptionRegistry();
    const on = new ArraySink();
    const off = new ArraySink();
    reg.create({ filter: resolveFilter({ include_heartbeat: true }), sink: on, encode });
    reg.create({ filter: resolveFilter({ include_heartbeat: false }), sink: off, encode });
    reg.fanoutControl("heartbeat", { ts: "t", latest_seq: 3 });
    expect(on.frames()[0]!.event).toBe("heartbeat");
    expect(off.lines.length).toBe(0);
  });

  test("two subscribers on one stream both receive the event", () => {
    const reg = new SubscriptionRegistry();
    const ring = new ReplayRing();
    const a = new ArraySink();
    const b = new ArraySink();
    reg.create({ filter: resolveFilter({}), sink: a, encode });
    reg.create({ filter: resolveFilter({}), sink: b, encode });
    const delivered = reg.fanout(ring.append("job.heartbeat", hb("j1")), ring);
    expect(delivered).toBe(2);
    expect(a.frames()[0]!.job_id).toBe("j1");
    expect(b.frames()[0]!.job_id).toBe("j1");
  });

  test("slow subscriber over MAX_UNACKED gets a final stream.overflow and is dropped", () => {
    const reg = new SubscriptionRegistry();
    const ring = new ReplayRing(MAX_UNACKED + 100);
    const sink = new ArraySink();
    reg.create({ filter: resolveFilter({}), sink, encode });
    // Push MAX_UNACKED+1 events without any ack.
    for (let i = 0; i < MAX_UNACKED + 1; i++) {
      reg.fanout(ring.append("job.heartbeat", hb(`j${i}`)), ring);
    }
    expect(sink.closed).toBe(true);
    const last = sink.frames().at(-1)!;
    expect(last.event).toBe("stream.overflow");
    expect(reg.size).toBe(0);
  });

  test("ack resets the pressure so a healthy reader never overflows", () => {
    const reg = new SubscriptionRegistry();
    const ring = new ReplayRing(MAX_UNACKED * 3);
    const sink = new ArraySink();
    const sub = reg.create({ filter: resolveFilter({}), sink, encode });
    for (let i = 0; i < MAX_UNACKED * 2; i++) {
      const ev = ring.append("job.heartbeat", hb(`j${i}`));
      reg.fanout(ev, ring);
      // Ack every 100 frames.
      if (i % 100 === 0) sub.ack(ev.seq);
    }
    expect(sink.closed).toBe(false);
    expect(reg.size).toBe(1);
  });
});
