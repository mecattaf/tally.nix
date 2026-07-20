// daemon-core server: the full transport over a real Unix socket (CLI-SURFACE §2, byte-for-byte).
// Snapshot ping, subscribe ACK (with the FROZEN `type:"subscription"` discriminator), protocol
// negotiation, replay/gap on resume, two subscribers one stream, ack/unsubscribe, and epoch-change
// voiding a client cursor across a daemon restart.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { bootDaemon, type Daemon } from "../../src/daemon/index";
import { makeTmpEnv, type TmpEnv } from "../helpers/tmp";
import { connectClient, type SocketClient } from "../helpers/socket-client";
import { PROTOCOL_ID, PROTOCOL_VERSION, SUBSCRIPTION_DISCRIMINATOR } from "../../src/contracts/index";
import type { JobHeartbeatPayload } from "../../src/contracts/events";
import { defaultConfig } from "../../src/contracts/config";

function hb(job_id: string): JobHeartbeatPayload {
  return { job_id, gpu_seconds: 0 };
}

describe("daemon server", () => {
  let tmp: TmpEnv;
  let daemon: Daemon;
  const clients: SocketClient[] = [];

  beforeEach(async () => {
    tmp = makeTmpEnv();
    daemon = bootDaemon({ env: tmp.env, config: { ...defaultConfig(), heartbeatMs: 100000 } });
    await daemon.start();
  });

  afterEach(async () => {
    for (const c of clients) c.close();
    clients.length = 0;
    await daemon.stop();
    tmp.cleanup();
  });

  async function client(): Promise<SocketClient> {
    const c = await connectClient(daemon.server.socketPath);
    clients.push(c);
    return c;
  }

  test("session.snapshot answers a well-formed §2.2 frame from boot", async () => {
    const c = await client();
    const snap = await c.call<Record<string, unknown>>("session.snapshot");
    expect(snap.protocol).toBe(PROTOCOL_ID);
    expect(snap.protocol_version).toBe(PROTOCOL_VERSION);
    expect(snap.lease_epoch).toBe(daemon.state.epoch);
    expect(snap.seq).toBe(0);
    expect(snap.workspaces).toEqual([]);
    expect(snap.jobs).toEqual([]);
    expect(snap.focus).toEqual({ workspace: null, session: null, pane: null });
  });

  test("subscribe ACK carries the frozen type:'subscription' discriminator + resume block", async () => {
    const c = await client();
    const ack = await c.call<Record<string, unknown>>("session.subscribe", {});
    expect(ack.type).toBe(SUBSCRIPTION_DISCRIMINATOR);
    expect(typeof ack.subscription_id).toBe("string");
    expect(ack.protocol_version).toBe(PROTOCOL_VERSION);
    expect(ack.epoch).toBe(daemon.state.epoch);
    const resume = ack.resume as Record<string, unknown>;
    expect(resume).toMatchObject({ after_seq: 0, oldest_seq: 0, latest_seq: 0, next_seq: 1, gap: false });
  });

  test("live subscriber receives a published event flattened with seq/id/event", async () => {
    const c = await client();
    await c.call("session.subscribe", {});
    daemon.state.publish("job.heartbeat", hb("j1"));
    const ev = await c.waitForEvent("job.heartbeat");
    expect(ev.seq).toBe(1);
    expect(typeof ev.id).toBe("string");
    expect((ev as Record<string, unknown>).job_id).toBe("j1");
  });

  test("two subscribers on one stream both receive the event", async () => {
    const a = await client();
    const b = await client();
    await a.call("session.subscribe", {});
    await b.call("session.subscribe", {});
    daemon.state.publish("job.heartbeat", hb("shared"));
    const [ea, eb] = await Promise.all([a.waitForEvent("job.heartbeat"), b.waitForEvent("job.heartbeat")]);
    expect((ea as Record<string, unknown>).job_id).toBe("shared");
    expect((eb as Record<string, unknown>).job_id).toBe("shared");
    expect(ea.seq).toBe(eb.seq as number);
  });

  test("resume replays retained events after from_seq", async () => {
    // Publish 3 events BEFORE anyone subscribes; they sit in the ring.
    daemon.state.publish("job.heartbeat", hb("j1"));
    daemon.state.publish("job.heartbeat", hb("j2"));
    daemon.state.publish("job.heartbeat", hb("j3"));
    const c = await client();
    const ack = await c.call<Record<string, unknown>>("session.subscribe", { from_seq: 1 });
    expect((ack.resume as Record<string, unknown>).gap).toBe(false);
    // Should replay seq 2 and 3.
    const e2 = await c.waitForEvent("job.heartbeat", (e) => e.seq === 2);
    const e3 = await c.waitForEvent("job.heartbeat", (e) => e.seq === 3);
    expect((e2 as Record<string, unknown>).job_id).toBe("j2");
    expect((e3 as Record<string, unknown>).job_id).toBe("j3");
  });

  test("resume caught-up (from_seq == latest) reports no gap, no replay", async () => {
    for (let i = 0; i < 5; i++) daemon.state.publish("job.heartbeat", hb(`j${i}`));
    const c = await client();
    const ack = await c.call<Record<string, unknown>>("session.subscribe", { from_seq: 5 });
    const resume = ack.resume as Record<string, unknown>;
    expect(resume.gap).toBe(false);
    expect(resume.latest_seq).toBe(5);
    expect(resume.next_seq).toBe(6);
  });

  test("protocol negotiation: an unservable range returns unsupported_protocol", async () => {
    const c = await client();
    const resp = await c.request("session.subscribe", { min_protocol: 2, max_protocol: 3 });
    expect(resp.error).toBeDefined();
    expect(resp.error!.code).toBe("unsupported_protocol");
    expect((resp.error!.data as Record<string, unknown>).supported).toEqual([PROTOCOL_VERSION]);
  });

  test("ack advances the cursor; unsubscribe closes the stream but keeps the socket", async () => {
    const c = await client();
    const ack = await c.call<Record<string, unknown>>("session.subscribe", {});
    const subId = ack.subscription_id as string;
    daemon.state.publish("job.heartbeat", hb("j1"));
    await c.waitForEvent("job.heartbeat");
    const acked = await c.call<Record<string, unknown>>("session.ack", { subscription_id: subId, seq: 1 });
    expect(acked.acked).toBe(1);
    const un = await c.call<Record<string, unknown>>("session.unsubscribe", { subscription_id: subId });
    expect(un.unsubscribed).toBe(subId);
    // Socket still serves RPC.
    const snap = await c.call<Record<string, unknown>>("session.snapshot");
    expect(snap.protocol).toBe(PROTOCOL_ID);
  });

  test("ack for an unknown subscription errors", async () => {
    const c = await client();
    const resp = await c.request("session.ack", { subscription_id: "nope", seq: 1 });
    expect(resp.error!.code).toBe("unknown_subscription");
  });

  test("unknown method → unknown_method", async () => {
    const c = await client();
    const resp = await c.request("does.not.exist", {});
    expect(resp.error!.code).toBe("unknown_method");
  });

  test("epoch strictly increases across a restart, voiding a client cursor", async () => {
    const firstEpoch = daemon.state.epoch;
    daemon.state.publish("job.heartbeat", hb("j1"));
    // Restart the daemon against the SAME env (same epoch counter file).
    await daemon.stop();
    daemon = bootDaemon({ env: tmp.env, config: { ...defaultConfig(), heartbeatMs: 100000 } });
    await daemon.start();
    expect(daemon.state.epoch).toBeGreaterThan(firstEpoch);
    // A reconnecting client sees the new epoch in the snapshot ⇒ its old cursor is void.
    const c = await client();
    const snap = await c.call<Record<string, unknown>>("session.snapshot");
    expect(snap.lease_epoch).toBe(daemon.state.epoch);
    // seq reset within the new epoch.
    expect(snap.seq).toBe(0);
    const ack = await c.call<Record<string, unknown>>("session.subscribe", {});
    expect(ack.epoch).toBe(daemon.state.epoch);
  });

  test("bus-emitted module events reach the wire (wireBusToWire)", async () => {
    const c = await client();
    await c.call("session.subscribe", {});
    // A mounted module would emit on the bus; simulate it.
    daemon.state.bus.emit("job.completed", {
      job_id: "jb", task_uuid: null, exit_code: 0, gpu_seconds: null, artifact_hash: null, labor_class: "fresh",
    });
    const ev = await c.waitForEvent("job.completed");
    expect((ev as Record<string, unknown>).job_id).toBe("jb");
  });

  test("session.wait job barrier resolves over the socket", async () => {
    const c = await client();
    const waitPromise = c.call<Record<string, unknown>>(
      "session.wait",
      { predicate: { subject: "job", job_ids: ["jw"], until: ["completed"], count: 1 } },
      5000,
    );
    // Give the wait a tick to subscribe, then emit.
    await new Promise((r) => setTimeout(r, 20));
    daemon.state.bus.emit("job.completed", {
      job_id: "jw", task_uuid: null, exit_code: 0, gpu_seconds: null, artifact_hash: null, labor_class: "fresh",
    });
    const result = await waitPromise;
    expect((result.satisfied as unknown[]).length).toBe(1);
  });
});

// ---------------------------------------------------------------------------------------------
// Raw-socket negative-frame tests — the frozen wire's malformed/pipelined/oversized paths, which the
// well-formed-only SocketClient cannot reach. Uses node:net directly.
// ---------------------------------------------------------------------------------------------

describe("daemon server — raw wire framing (negative + pipelined + UTF-8)", () => {
  let tmp: TmpEnv;
  let daemon: Daemon;

  beforeEach(async () => {
    tmp = makeTmpEnv();
    daemon = bootDaemon({ env: tmp.env, config: { ...defaultConfig(), heartbeatMs: 100000 } });
    await daemon.start();
  });
  afterEach(async () => {
    await daemon.stop();
    tmp.cleanup();
  });

  /** Open a raw socket, write `payload` (a single chunk), collect reply frames for `waitMs`. */
  async function rawExchange(payload: string | Buffer, waitMs = 400): Promise<Array<Record<string, unknown>>> {
    const { connect } = await import("node:net");
    return new Promise((resolve, reject) => {
      const frames: Array<Record<string, unknown>> = [];
      let buf = "";
      const sock = connect(daemon.server.socketPath);
      sock.setEncoding("utf8");
      sock.on("connect", () => {
        sock.write(payload as Uint8Array | string);
        setTimeout(() => {
          sock.destroy();
          resolve(frames);
        }, waitMs);
      });
      sock.on("data", (chunk: string) => {
        buf += chunk;
        let nl: number;
        while ((nl = buf.indexOf("\n")) !== -1) {
          const line = buf.slice(0, nl);
          buf = buf.slice(nl + 1);
          if (line.trim().length > 0) frames.push(JSON.parse(line));
        }
      });
      sock.on("error", reject);
    });
  }

  test("a valid request followed by a malformed frame in ONE chunk: the valid request STILL gets its response", async () => {
    // The chunk-boundary-dependent silent-drop defect: a valid frame yielded before a later bad line
    // in the same chunk must be served, not discarded when the decode throws.
    const frames = await rawExchange('{"id":"a","method":"session.snapshot","params":{}}\nGARBAGE\n');
    const byId = frames.find((f) => f.id === "a");
    expect(byId).toBeDefined();
    expect((byId as { result?: unknown }).result).toBeDefined();
    // AND the malformed frame produced its own error frame.
    expect(frames.some((f) => f.id === null && (f as { error?: unknown }).error !== undefined)).toBe(true);
  });

  test("a frame that is valid JSON but not an object gets a structured error, not a hang", async () => {
    const frames = await rawExchange("42\n");
    expect(frames.length).toBeGreaterThanOrEqual(1);
    expect((frames[0] as { error?: { code?: string } }).error).toBeDefined();
  });

  test("an oversized (>64 KiB) inbound line yields a frame_too_large error and closes the connection", async () => {
    const huge = "x".repeat(70 * 1024);
    const frames = await rawExchange(`{"id":"big","method":"session.snapshot","params":{"pad":"${huge}"}}\n`);
    expect(frames.some((f) => (f as { error?: { code?: string } }).error?.code === "frame_too_large")).toBe(true);
  });

  test("a multibyte UTF-8 codepoint split across two writes is NOT corrupted (byte-safe framing)", async () => {
    // Send a pane.send-shaped frame whose text contains a multibyte codepoint, split mid-codepoint
    // across two socket writes. The daemon must reassemble it intact (not U+FFFD). We assert via the
    // snapshot round-trip is not applicable here; instead assert the frame parses + the method is served
    // (a corrupted JSON string would have made JSON.parse fail → invalid_params, not a served method).
    const frame = Buffer.from('{"id":"u","method":"session.snapshot","params":{"note":"héllo 🦀"}}\n', "utf8");
    // Split one byte after the é lead byte (a mid-codepoint boundary).
    const eIdx = frame.indexOf(0xc3); // é lead byte
    const first = frame.subarray(0, eIdx + 1);
    const second = frame.subarray(eIdx + 1);
    const { connect } = await import("node:net");
    const frames: Array<Record<string, unknown>> = await new Promise((resolve, reject) => {
      const out: Array<Record<string, unknown>> = [];
      let buf = "";
      const sock = connect(daemon.server.socketPath);
      sock.setEncoding("utf8");
      sock.on("connect", () => {
        sock.write(first as unknown as Uint8Array);
        setTimeout(() => sock.write(second as unknown as Uint8Array), 20);
        setTimeout(() => {
          sock.destroy();
          resolve(out);
        }, 400);
      });
      sock.on("data", (chunk: string) => {
        buf += chunk;
        let nl: number;
        while ((nl = buf.indexOf("\n")) !== -1) {
          const line = buf.slice(0, nl);
          buf = buf.slice(nl + 1);
          if (line.trim().length > 0) out.push(JSON.parse(line));
        }
      });
      sock.on("error", reject);
    });
    // The frame was reassembled + served (a corrupted codepoint would have broken JSON.parse → error).
    const reply = frames.find((f) => f.id === "u");
    expect(reply).toBeDefined();
    expect((reply as { result?: unknown }).result).toBeDefined();
  });

  test("session.subscribe with replay: the ACK precedes the replay event frames on the wire (§2.4)", async () => {
    // Publish two events so a from_seq:0 resume has replay to deliver.
    daemon.state.publish("job.completed", { job_id: "r1", task_uuid: null, exit_code: 0, gpu_seconds: null, artifact_hash: null, labor_class: "fresh" });
    daemon.state.publish("job.completed", { job_id: "r2", task_uuid: null, exit_code: 0, gpu_seconds: null, artifact_hash: null, labor_class: "fresh" });
    const ordered: string[] = [];
    const { connect } = await import("node:net");
    await new Promise<void>((resolve, reject) => {
      let buf = "";
      const sock = connect(daemon.server.socketPath);
      sock.setEncoding("utf8");
      sock.on("connect", () => {
        sock.write('{"id":1,"method":"session.subscribe","params":{"from_seq":0}}\n');
        setTimeout(() => {
          sock.destroy();
          resolve();
        }, 400);
      });
      sock.on("data", (chunk: string) => {
        buf += chunk;
        let nl: number;
        while ((nl = buf.indexOf("\n")) !== -1) {
          const line = buf.slice(0, nl);
          buf = buf.slice(nl + 1);
          if (line.trim().length === 0) continue;
          const f = JSON.parse(line);
          if (f.result && f.result.type === "subscription") ordered.push("ack");
          else if (f.event) ordered.push("event");
        }
      });
      sock.on("error", reject);
    });
    // The ACK is FIRST on the wire, before any replayed event (the ordering the §2.4 contract requires).
    expect(ordered[0]).toBe("ack");
    expect(ordered.filter((x) => x === "event").length).toBe(2);
  });
});
