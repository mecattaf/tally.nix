// Byte-for-byte golden tests over the FROZEN Seam-B wire contract (CLI-SURFACE §2;
// IMPLEMENTATION-PLAN §3). These pin the SubscribeAck discriminator, the Event `{seq,id,event,…}`
// field names, every RPC method name, and the four-value AgentStatus acceptance — so no omission
// can survive a green build ("any deviation from §2 field names is a build failure").

import { describe, expect, test } from "bun:test";
import {
  AGENT_STATUSES,
  PUBLIC_RPC_METHODS,
  RPC_METHODS,
  SUBSCRIPTION_DISCRIMINATOR,
  isRpcMethod,
  makeSubscribeAck,
  validateAckParams,
  validateEnqueueParams,
  validateRegisterViewerParams,
  validateRequestFrame,
  validateSubscribeParams,
  validateUnsubscribeParams,
  validateWaitParams,
  parseEvidenceSpec,
  renderEvidenceSpec,
  isRequestFrame,
  isResponseFrame,
  isEventFrame,
  PROTOCOL_VERSION,
} from "../../src/contracts/index";

describe("RPC method inventory (golden)", () => {
  test("the frozen public five are present, exact, and in order", () => {
    expect([...PUBLIC_RPC_METHODS]).toEqual([
      "session.snapshot",
      "session.subscribe",
      "session.wait",
      "session.ack",
      "session.unsubscribe",
    ]);
  });

  test("the full inventory contains every internal-additive carrier by name", () => {
    const required = [
      "queue.enqueue",
      "queue.cancel",
      "queue.pause",
      "queue.resume",
      "queue.drain",
      "queue.await_job",
      "queue.await_barrier",
      "pane.send",
      "pane.send_key",
      "pane.focus",
      "pane.capture",
      "agent.list",
      "agent.get",
      "agent.read",
      "agent.explain",
      "query.status",
      "query.render",
      "session.list",
      "session.register_viewer",
      "kitty.watcher_event",
      "agent.hook_event",
    ];
    for (const m of required) expect(RPC_METHODS).toContain(m);
    // Exact set — a drop or an unexpected addition both fail.
    const total: number = RPC_METHODS.length;
    expect(new Set(RPC_METHODS).size).toBe(total);
    expect(total).toBe(PUBLIC_RPC_METHODS.length + required.length);
  });

  test("isRpcMethod recognizes known and rejects unknown", () => {
    expect(isRpcMethod("session.snapshot")).toBe(true);
    expect(isRpcMethod("queue.drain")).toBe(true);
    expect(isRpcMethod("nope.method")).toBe(false);
  });
});

describe("SubscribeAck discriminator (FROZEN)", () => {
  test("carries the literal type:'subscription'", () => {
    expect(SUBSCRIPTION_DISCRIMINATOR).toBe("subscription");
    const ack = makeSubscribeAck({
      subscription_id: "sub_1",
      epoch: 42,
      resume: { after_seq: 0, oldest_seq: 0, latest_seq: 10, next_seq: 11, gap: false },
    });
    expect(ack.type).toBe("subscription");
    expect(ack.protocol_version).toBe(PROTOCOL_VERSION);
    // The full ACK shape, pinned.
    expect(Object.keys(ack)).toEqual(["type", "subscription_id", "protocol_version", "epoch", "resume"]);
    expect(Object.keys(ack.resume)).toEqual(["after_seq", "oldest_seq", "latest_seq", "next_seq", "gap"]);
  });
});

describe("frame discriminators (structural, §2.1)", () => {
  test("request vs response vs event", () => {
    expect(isRequestFrame({ id: 1, method: "session.snapshot" })).toBe(true);
    expect(isResponseFrame({ id: 1, result: {} })).toBe(true);
    expect(isResponseFrame({ id: 1, error: { code: "internal", message: "x" } })).toBe(true);
    expect(isEventFrame({ seq: 5, id: "e1", event: "agent.status_changed", payload: {} })).toBe(true);
    // A response is not a request and not an event.
    expect(isRequestFrame({ id: 1, result: {} })).toBe(false);
    expect(isEventFrame({ id: 1, result: {} })).toBe(false);
  });
});

describe("session.wait predicate — FULL four-value AgentStatus", () => {
  test("all four AgentStatus values are accepted by the agent predicate", () => {
    expect([...AGENT_STATUSES]).toEqual(["blocked", "working", "done", "idle"]);
    for (const status of AGENT_STATUSES) {
      const parsed = validateWaitParams({
        predicate: { subject: "agent", agent_ids: ["ag_1"], until_status: status, count: 1 },
      });
      expect(parsed.predicate.subject).toBe("agent");
      if (parsed.predicate.subject === "agent") expect(parsed.predicate.until_status).toBe(status);
    }
  });

  test("a fifth status is rejected (a narrowing would be a build failure)", () => {
    expect(() =>
      validateWaitParams({
        predicate: { subject: "agent", agent_ids: ["ag_1"], until_status: "unknown", count: 1 },
      }),
    ).toThrow();
  });

  test("job predicate barrier shape", () => {
    const p = validateWaitParams({
      predicate: { subject: "job", job_ids: ["j1", "j2"], until: ["completed", "failed"], count: 2 },
      timeout_ms: 5000,
    });
    expect(p.predicate.subject).toBe("job");
    expect(p.timeout_ms).toBe(5000);
  });

  test("pane_output predicate shape", () => {
    const p = validateWaitParams({ predicate: { subject: "pane_output", pane_id: "s:p1", regex: "done" } });
    expect(p.predicate.subject).toBe("pane_output");
  });
});

describe("request-frame validator", () => {
  test("accepts a well-formed request", () => {
    const f = validateRequestFrame({ id: "r1", method: "session.snapshot", params: {} });
    expect(f.method).toBe("session.snapshot");
  });
  test("rejects a missing method / bad id", () => {
    expect(() => validateRequestFrame({ id: "r1" })).toThrow();
    expect(() => validateRequestFrame({ method: "x" })).toThrow();
    expect(() => validateRequestFrame({ id: {}, method: "x" })).toThrow();
    expect(() => validateRequestFrame("nope")).toThrow();
  });
});

describe("subscribe/ack/unsubscribe/register_viewer validators", () => {
  test("subscribe: filters + protocol negotiation", () => {
    const s = validateSubscribeParams({
      from_seq: 10,
      names: ["agent.status_changed"],
      categories: ["agent", "job", "control"],
      include_heartbeat: false,
      min_protocol: 1,
      max_protocol: 1,
    });
    expect(s.from_seq).toBe(10);
    expect(s.categories).toEqual(["agent", "job", "control"]);
    expect(s.include_heartbeat).toBe(false);
  });
  test("subscribe: empty params is a valid live subscribe", () => {
    expect(validateSubscribeParams(undefined)).toEqual({});
  });
  test("subscribe: bad category rejected", () => {
    expect(() => validateSubscribeParams({ categories: ["bogus"] })).toThrow();
  });
  test("ack requires subscription_id + seq", () => {
    expect(validateAckParams({ subscription_id: "s1", seq: 3 })).toEqual({ subscription_id: "s1", seq: 3 });
    expect(() => validateAckParams({ subscription_id: "s1" })).toThrow();
  });
  test("unsubscribe requires subscription_id", () => {
    expect(validateUnsubscribeParams({ subscription_id: "s1" })).toEqual({ subscription_id: "s1" });
    expect(() => validateUnsubscribeParams({})).toThrow();
  });
  test("register_viewer requires a numeric kitty_window_id", () => {
    expect(validateRegisterViewerParams({ kitty_window_id: 7 })).toEqual({ kitty_window_id: 7 });
    expect(() => validateRegisterViewerParams({ kitty_window_id: "7" })).toThrow();
  });
});

describe("Seam-A enqueue validator (§1.1a)", () => {
  test("accepts an invocation + cwd + evidence + dedup + pool", () => {
    const p = validateEnqueueParams({
      priority: "high",
      source: "orchestrator",
      kind: "shell",
      invocation: "ocr paper.pdf",
      cwd: "/work",
      evidence: ["artifact:/out/paper.txt", "hash:sha256", "exit:0"],
      dedup_key: "paper.pdf",
      pool: "worker-gpu",
    });
    expect(p.evidence).toHaveLength(3);
    expect(p.evidence![0]).toEqual({ kind: "artifact", path: "/out/paper.txt" });
    expect(p.evidence![1]).toEqual({ kind: "hash", algo: "sha256" });
    expect(p.evidence![2]).toEqual({ kind: "exit", code: 0 });
  });

  test("accepts argv form", () => {
    const p = validateEnqueueParams({ priority: "low", source: "manual", kind: "pi", argv: ["pi", "--session", "x"] });
    expect(p.argv).toEqual(["pi", "--session", "x"]);
  });

  test("rejects both invocation and argv (XOR)", () => {
    expect(() =>
      validateEnqueueParams({ priority: "low", source: "manual", kind: "pi", invocation: "x", argv: ["y"] }),
    ).toThrow();
  });

  test("rejects neither invocation nor argv", () => {
    expect(() => validateEnqueueParams({ priority: "low", source: "manual", kind: "pi" })).toThrow();
  });

  test("rejects both cwd and worktree", () => {
    expect(() =>
      validateEnqueueParams({ priority: "low", source: "manual", kind: "pi", invocation: "x", cwd: "/a", worktree: "b" }),
    ).toThrow();
  });

  test("rejects a bad enum value", () => {
    expect(() => validateEnqueueParams({ priority: "urgent", source: "manual", kind: "pi", invocation: "x" })).toThrow();
    expect(() => validateEnqueueParams({ priority: "low", source: "email", kind: "pi", invocation: "x" })).toThrow();
    expect(() => validateEnqueueParams({ priority: "low", source: "manual", kind: "vim", invocation: "x" })).toThrow();
  });
});

describe("EvidenceCheck grammar (§1.1a)", () => {
  test("parse + render round-trip for each kind", () => {
    expect(parseEvidenceSpec("artifact:/out/x.txt")).toEqual({ kind: "artifact", path: "/out/x.txt" });
    expect(parseEvidenceSpec("hash:sha256")).toEqual({ kind: "hash", algo: "sha256" });
    expect(parseEvidenceSpec("hash:sha256:abcd")).toEqual({ kind: "hash", algo: "sha256", value: "abcd" });
    expect(parseEvidenceSpec("exit:0")).toEqual({ kind: "exit", code: 0 });
    expect(parseEvidenceSpec("exit:137")).toEqual({ kind: "exit", code: 137 });

    expect(renderEvidenceSpec({ kind: "artifact", path: "/out/x.txt" })).toBe("artifact:/out/x.txt");
    expect(renderEvidenceSpec({ kind: "hash", algo: "sha256" })).toBe("hash:sha256");
    expect(renderEvidenceSpec({ kind: "hash", algo: "sha256", value: "abcd" })).toBe("hash:sha256:abcd");
    expect(renderEvidenceSpec({ kind: "exit", code: 0 })).toBe("exit:0");
  });

  test("rejects malformed specs", () => {
    expect(() => parseEvidenceSpec("noseparator")).toThrow();
    expect(() => parseEvidenceSpec("artifact:")).toThrow();
    expect(() => parseEvidenceSpec("exit:notanumber")).toThrow();
    expect(() => parseEvidenceSpec("bogus:x")).toThrow();
  });
});
