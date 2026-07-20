// Compile-time-shape + golden-shape tests over the non-wire contracts: the §2.2 snapshot shape,
// the event-name union, enums, witness record + hash canonicalization, journald field matrix +
// AgentKind→TALLY_AGENT map, config loader, paths, selectors, and the TW admission predicate.

import { describe, expect, test } from "bun:test";
import {
  // enums
  AGENT_KINDS,
  AGENT_STATUSES,
  DETECTOR_STRATEGIES,
  PRIORITIES,
  SOURCES,
  VERDICTS,
  LABOR_CLASSES,
  TRUST_VALUES,
  CHARGE_CLASSES,
  // events
  EVENT_NAMES,
  EVENT_CATEGORIES,
  NON_REPLAYABLE_EVENTS,
  eventCategory,
  isReplayable,
  // snapshot
  SNAPSHOT_KEY_ORDER,
  emptySnapshot,
  emptyRollup,
  tallyStatus,
  // witness
  GENESIS_PREV_HASH,
  HASH_PREFIX,
  canonicalHashInput,
  toProjection,
  countsTowardCanonicalGpuSeconds,
  type WitnessRecord,
  // journald
  TALLY_EVENTS,
  TALLY_FIELD_MATRIX,
  ALWAYS_FIELDS,
  tallyAgent,
  agentKindFromTally,
  // config
  defaultConfig,
  loadConfig,
  // paths
  resolvePaths,
  socketPath,
  ledgerPath,
  // selectors
  makePaneId,
  parsePaneId,
  parseSelector,
  // task
  admitsDurableRow,
  twPriority,
  TALLY_UDA_NAMES,
  // constants
  PROTOCOL_ID,
  PROTOCOL_VERSION,
  REPLAY_RING,
  FRAME_CAP,
  MAX_UNACKED,
  HEARTBEAT_MS,
} from "../../src/contracts/index";

describe("enums are frozen at their exact members", () => {
  test("AgentStatus is exactly four", () => {
    expect([...AGENT_STATUSES]).toEqual(["blocked", "working", "done", "idle"]);
  });
  test("AgentKind is exactly three", () => {
    expect([...AGENT_KINDS]).toEqual(["pi", "claude-code", "shell"]);
  });
  test("detector strategy / priority / source / labor / trust / charge", () => {
    expect([...DETECTOR_STRATEGIES]).toEqual(["hook", "scrape"]);
    expect([...PRIORITIES]).toEqual(["high", "medium", "low"]);
    expect([...SOURCES]).toEqual(["r2", "gh", "calendar", "manual", "orchestrator"]);
    expect([...LABOR_CLASSES]).toEqual(["fresh", "recovered", "reused"]);
    expect([...TRUST_VALUES]).toEqual(["unreviewed", "reviewed", "recalled"]);
    expect([...CHARGE_CLASSES]).toEqual(["verifiable", "annotation"]);
    expect(VERDICTS).toContain("pass");
    expect(VERDICTS).toContain("clean-exit-no-artifact");
  });
});

describe("constants (tally's own frame budget)", () => {
  test("pinned values", () => {
    expect(PROTOCOL_ID).toBe("tally.delta");
    expect(PROTOCOL_VERSION).toBe(1);
    expect(REPLAY_RING).toBe(4096);
    expect(FRAME_CAP).toBe(65536);
    expect(MAX_UNACKED).toBe(1024);
    expect(HEARTBEAT_MS).toBe(15000);
  });
});

describe("event-name union (§2.3, golden)", () => {
  test("every documented event is present", () => {
    const required = [
      "agent.detected",
      "agent.status_changed",
      "agent.blocked",
      "agent.done",
      "agent.released",
      "pane.created",
      "pane.closed",
      "pane.focused",
      "pane.output_matched",
      "session.observed",
      "session.ended",
      "workspace.focused",
      "job.enqueued",
      "job.dispatched",
      "job.started",
      "job.heartbeat",
      "job.preempted",
      "job.resumed",
      "job.evidence_pass",
      "job.evidence_fail",
      "job.completed",
      "job.failed",
      "job.witness_emitted",
      "heartbeat",
      "stream.overflow",
    ];
    for (const e of required) expect(EVENT_NAMES).toContain(e);
    const total: number = EVENT_NAMES.length;
    expect(new Set(EVENT_NAMES).size).toBe(total);
    expect(total).toBe(required.length);
  });

  test("categories + replayability", () => {
    expect([...EVENT_CATEGORIES]).toEqual(["agent", "pane", "session", "workspace", "job", "control"]);
    expect(eventCategory("agent.status_changed")).toBe("agent");
    expect(eventCategory("job.completed")).toBe("job");
    expect(eventCategory("workspace.focused")).toBe("workspace");
    expect(eventCategory("heartbeat")).toBe("control");
    // heartbeat + stream.overflow are the only non-replayable frames.
    expect([...NON_REPLAYABLE_EVENTS]).toEqual(["heartbeat", "stream.overflow"]);
    expect(isReplayable("heartbeat")).toBe(false);
    expect(isReplayable("stream.overflow")).toBe(false);
    expect(isReplayable("agent.status_changed")).toBe(true);
  });
});

describe("§2.2 snapshot shape (golden)", () => {
  test("key order and legs match the frozen frame", () => {
    const snap = emptySnapshot({ daemon_version: "0.1.0", lease_epoch: 42, seq: 90714, ts: "2026-07-07T15:30:04.512Z" });
    expect(Object.keys(snap)).toEqual([...SNAPSHOT_KEY_ORDER]);
    expect(snap.protocol).toBe("tally.delta");
    expect(snap.protocol_version).toBe(1);
    expect(snap.focus).toEqual({ workspace: null, session: null, pane: null });
    expect(snap.workspaces).toEqual([]);
    expect(snap.sessions).toEqual([]);
    expect(snap.panes).toEqual([]);
    expect(snap.agents).toEqual([]);
    expect(snap.jobs).toEqual([]);
  });

  test("rollup aggregation", () => {
    const r = emptyRollup();
    tallyStatus(r, "working");
    tallyStatus(r, "working");
    tallyStatus(r, "idle");
    expect(r).toEqual({ blocked: 0, working: 2, done: 0, idle: 1 });
  });
});

describe("witness record + hash canonicalization (SPEC)", () => {
  const rec: WitnessRecord = {
    task_uuid: "b2c4-uuid",
    transition_timestamp: "2026-07-07T15:30:04.512Z",
    verdict: "pass",
    exit_code: 0,
    artifact_content_hash: "sha256:deadbeef",
    gpu_seconds: 12.5,
    wall_clock: 40,
    attempt: 1,
    lease_epoch: 42,
    dedup_key: "paper.pdf",
    labor_class: "fresh",
    pool: "worker-gpu",
    charge: { unit: "gpu_seconds", amount: 12.5, class: "verifiable" },
    model: "anthropic/claude-x",
    seq: 5,
    prev_hash: "sha256:" + "a".repeat(64),
    hash: "sha256:" + "b".repeat(64),
  };

  test("canonicalHashInput clears ONLY the hash field, keeping seq + prev_hash", () => {
    const input = canonicalHashInput(rec);
    const parsed = JSON.parse(input);
    expect(parsed.hash).toBe("");
    expect(parsed.seq).toBe(5);
    expect(parsed.prev_hash).toBe(rec.prev_hash);
    expect(parsed.task_uuid).toBe("b2c4-uuid");
    // The input is independent of the current hash value (clearing is deterministic).
    const rec2 = { ...rec, hash: "sha256:" + "c".repeat(64) };
    expect(canonicalHashInput(rec2)).toBe(input);
  });

  test("genesis prev_hash + prefix", () => {
    expect(GENESIS_PREV_HASH).toBe("sha256:" + "0".repeat(64));
    expect(HASH_PREFIX).toBe("sha256:");
  });

  test("5-field projection", () => {
    const proj = toProjection(rec, ["artifact:/out", "exit:0"]);
    expect(proj).toEqual({
      task_uuid: "b2c4-uuid",
      gpu_seconds: 12.5,
      artifact_hash: "sha256:deadbeef",
      exit_code: 0,
      evidence_checks: ["artifact:/out", "exit:0"],
    });
  });

  test("canonical-GPU-seconds inclusion excludes non-fresh + gate-fail", () => {
    expect(countsTowardCanonicalGpuSeconds(rec)).toBe(true);
    expect(countsTowardCanonicalGpuSeconds({ ...rec, labor_class: "reused" })).toBe(false);
    expect(countsTowardCanonicalGpuSeconds({ ...rec, labor_class: "recovered" })).toBe(false);
    expect(countsTowardCanonicalGpuSeconds({ ...rec, verdict: "clean-exit-no-artifact" })).toBe(false);
  });
});

describe("journald field matrix + AgentKind→TALLY_AGENT map (SPEC / risk 10)", () => {
  test("TALLY_EVENT vocabulary is complete", () => {
    expect([...TALLY_EVENTS]).toEqual([
      "enqueued",
      "dispatched",
      "started",
      "heartbeat",
      "preempted",
      "resumed",
      "completed",
      "failed",
      "evidence_pass",
      "evidence_fail",
      "witness_emitted",
    ]);
  });

  test("AgentKind maps to the SHORT vocabulary, claude-code→cc", () => {
    expect(tallyAgent("claude-code")).toBe("cc");
    expect(tallyAgent("pi")).toBe("pi");
    expect(tallyAgent("shell")).toBe("shell");
    // Inverse for the reader/join.
    expect(agentKindFromTally("cc")).toBe("claude-code");
    expect(agentKindFromTally("pi")).toBe("pi");
    expect(agentKindFromTally("shell")).toBe("shell");
    // A raw <worker> label has no AgentKind.
    expect(agentKindFromTally("nvidia-worker-0")).toBeNull();
  });

  test("the always-required fields include the SPEC 'always' set", () => {
    for (const f of ["SYSLOG_IDENTIFIER", "TALLY_EVENT", "TALLY_TASK_UUID", "TALLY_CLASS", "TALLY_SOURCE", "MESSAGE"]) {
      expect(TALLY_FIELD_MATRIX[f as keyof typeof TALLY_FIELD_MATRIX]).toBe("always");
      expect(ALWAYS_FIELDS).toContain(f);
    }
    // Stage-gated examples.
    expect(TALLY_FIELD_MATRIX.TALLY_UNIT).toBe("at-start+");
    expect(TALLY_FIELD_MATRIX.TALLY_ARTIFACT_HASH).toBe("at-completed");
    expect(TALLY_FIELD_MATRIX.TALLY_AGENT).toBe("at-dispatch+");
  });
});

describe("config loader (hand-rolled, no zod)", () => {
  test("defaults are a valid conductor config with two GPU pools and gh intake OFF", () => {
    const c = defaultConfig();
    expect(c.role).toBe("conductor");
    expect(c.pools.map((p) => p.name)).toEqual(["worker-gpu", "controller-gpu"]);
    expect(c.intake.gh.enable).toBe(false);
    expect(c.sessions).toEqual([]);
  });

  test("a partial override merges over defaults", () => {
    const c = loadConfig({ conductorHost: "box-a", sessions: ["term-*"], intake: { gh: { enable: true, sources: ["notifications"] } } });
    expect(c.conductorHost).toBe("box-a");
    expect(c.sessions).toEqual(["term-*"]);
    expect(c.intake.gh.enable).toBe(true);
    // Untouched keys keep defaults.
    expect(c.pools.map((p) => p.name)).toEqual(["worker-gpu", "controller-gpu"]);
    expect(c.detector.working_poll_ms).toBe(2000);
  });

  test("null/undefined => defaults; unknown keys ignored", () => {
    expect(loadConfig(undefined).role).toBe("conductor");
    expect(loadConfig(null).role).toBe("conductor");
    expect(loadConfig({ bogusKey: 1 }).role).toBe("conductor");
  });

  test("bad values rejected", () => {
    expect(() => loadConfig({ role: "worker" })).toThrow();
    expect(() => loadConfig({ sessions: [1, 2] })).toThrow();
    expect(() => loadConfig({ pools: "nope" })).toThrow();
    expect(() => loadConfig({ role: "conductor", conductorHost: "" })).toThrow();
  });
});

describe("paths (XDG, injectable env)", () => {
  test("resolve from an explicit env", () => {
    const env = {
      XDG_RUNTIME_DIR: "/run/user/1000",
      XDG_DATA_HOME: "/data",
      XDG_STATE_HOME: "/state",
      XDG_CONFIG_HOME: "/cfg",
      HOME: "/home/tom",
    };
    expect(socketPath(env)).toBe("/run/user/1000/tally/tally.sock");
    expect(ledgerPath(env)).toBe("/data/tally/witness.jsonl");
    const p = resolvePaths(env);
    expect(p.events).toBe("/state/tally/events");
    expect(p.eventsDone).toBe("/state/tally/events/done");
    expect(p.eventsRejected).toBe("/state/tally/events/rejected");
    expect(p.epoch).toBe("/state/tally/epoch");
    expect(p.config).toBe("/cfg/tally/config.json");
  });

  test("falls back to HOME-relative XDG defaults", () => {
    const env = { HOME: "/home/tom" };
    expect(ledgerPath(env)).toBe("/home/tom/.local/share/tally/witness.jsonl");
  });
});

describe("selectors + pane composite id (§0, §1.3)", () => {
  test("pane id make/parse round-trip", () => {
    expect(makePaneId("term-0707-1530", "p2")).toBe("term-0707-1530:p2");
    expect(parsePaneId("term-0707-1530:p2")).toEqual({ session: "term-0707-1530", pane: "p2" });
    expect(parsePaneId("noseparator")).toBeNull();
    expect(parsePaneId(":p2")).toBeNull();
    expect(parsePaneId("s:")).toBeNull();
  });

  test("selector classification", () => {
    expect(parseSelector("term-0707-1530:p2")).toMatchObject({ kind: "pane", session: "term-0707-1530", pane: "p2" });
    expect(parseSelector("ag_91be")).toMatchObject({ kind: "agent", agent_id: "ag_91be" });
    expect(parseSelector("term-0707-1530")).toMatchObject({ kind: "bare", token: "term-0707-1530" });
  });
});

describe("TW veneer: admission predicate + priority map + UDA vocabulary", () => {
  test("durable-row admission (SPEC)", () => {
    // Live-orchestrator-spawned NEVER earns a row.
    expect(
      admitsDurableRow({ source: "orchestrator", liveOrchestratorSpawned: true, autonomous: true, crashSurvivable: true, needsCrossSourceUrgency: true }),
    ).toBe(false);
    // Autonomous / crash-survivable / cross-source-urgency each qualify.
    expect(
      admitsDurableRow({ source: "r2", liveOrchestratorSpawned: false, autonomous: true, crashSurvivable: false, needsCrossSourceUrgency: false }),
    ).toBe(true);
    expect(
      admitsDurableRow({ source: "gh", liveOrchestratorSpawned: false, autonomous: false, crashSurvivable: false, needsCrossSourceUrgency: true }),
    ).toBe(true);
    // None of the qualifying conditions => no row.
    expect(
      admitsDurableRow({ source: "manual", liveOrchestratorSpawned: false, autonomous: false, crashSurvivable: false, needsCrossSourceUrgency: false }),
    ).toBe(false);
  });

  test("priority maps to native TW letters", () => {
    expect(twPriority("high")).toBe("H");
    expect(twPriority("medium")).toBe("M");
    expect(twPriority("low")).toBe("L");
  });

  test("UDA vocabulary carries the frozen names incl. trust", () => {
    for (const n of ["agent", "labor_class", "pool", "session_ref", "model_class", "cwd", "worktree", "trust", "dedup_key", "lease_epoch"]) {
      expect(TALLY_UDA_NAMES).toContain(n);
    }
  });
});
