// tally session-model (M2.1): the in-memory session store + reconcile-from-substrate discovery,
// rollup aggregation, §2.2 snapshot composition from the single store (incl. the detector-written
// agents[] and jobs-written jobs[] legs composed via the Bus), viewer marking + register_viewer,
// and pane-id encoding. Everything runs against the layer-0 fakes (FakeExec + FakeKitty + FakeZmx),
// no real substrate.

import { describe, expect, test } from "bun:test";
import { FakeExec, ok } from "../helpers/exec-fakes";
import { FakeKitty } from "../helpers/fake-kitty";
import { FakeZmx } from "../helpers/fake-zmx";
import { DaemonBus } from "../../src/daemon/state";
import { defaultConfig, type TallyConfig } from "../../src/contracts/config";
import { SNAPSHOT_KEY_ORDER } from "../../src/contracts/snapshot";
import { PROTOCOL_ID, PROTOCOL_VERSION } from "../../src/contracts/constants";
import { makePaneId, parsePaneId } from "../../src/contracts/selectors";
import type { AgentDetectedPayload, AgentStatusChangedPayload } from "../../src/contracts/events";
import type { JobEnqueuedPayload } from "../../src/contracts/events";
import {
  SessionStore,
  Discovery,
  SessionModel,
  rollupForPanes,
  stampSessionRollups,
  parseNiriWorkspaces,
  defaultWorkspace,
  SESSION_USER_VAR,
  PANE_USER_VAR,
} from "../../src/model";

// ---------------------------------------------------------------------------------------------
// Test rig: a FakeExec with kitty + zmx installed, plus an optional niri handler.
// ---------------------------------------------------------------------------------------------

interface Rig {
  exec: FakeExec;
  kitty: FakeKitty;
  zmx: FakeZmx;
  bus: DaemonBus;
  config: TallyConfig;
}

function makeRig(overrides: Partial<TallyConfig> = {}): Rig {
  const exec = new FakeExec();
  const kitty = new FakeKitty().install(exec);
  const zmx = new FakeZmx().install(exec);
  // Default: no niri (so discovery falls back to the default workspace). Tests that want niri
  // register their own handler.
  const bus = new DaemonBus();
  const config: TallyConfig = { ...defaultConfig(), conductorHost: "harness-desktop", ...overrides };
  return { exec, kitty, zmx, bus, config };
}

function makeDiscovery(rig: Rig): Discovery {
  return new Discovery({
    store: new SessionStore({ bus: rig.bus, daemonVersion: rig.config.daemonVersion }),
    bus: rig.bus,
    exec: rig.exec,
    sessions: rig.config.sessions,
    conductorHost: rig.config.conductorHost,
  });
}

// A discovery that shares one explicit store (so tests can read it back).
function discoveryWithStore(rig: Rig, sessions?: string[]): { discovery: Discovery; store: SessionStore } {
  const store = new SessionStore({ bus: rig.bus, daemonVersion: rig.config.daemonVersion });
  const discovery = new Discovery({
    store,
    bus: rig.bus,
    exec: rig.exec,
    sessions: sessions ?? rig.config.sessions,
    conductorHost: rig.config.conductorHost,
  });
  store.wireLegs();
  return { discovery, store };
}

// ---------------------------------------------------------------------------------------------
// Pane id encoding.
// ---------------------------------------------------------------------------------------------

describe("pane id encoding", () => {
  test("makePaneId / parsePaneId round-trip the composite key", () => {
    const id = makePaneId("term-0707-1530", "p2");
    expect(id).toBe("term-0707-1530:p2");
    expect(parsePaneId(id)).toEqual({ session: "term-0707-1530", pane: "p2" });
  });
});

// ---------------------------------------------------------------------------------------------
// Rollup aggregation.
// ---------------------------------------------------------------------------------------------

describe("rollup aggregation", () => {
  test("rollupForPanes counts only agent-bearing panes, by the agent's status", () => {
    const agents = new Map([
      ["a1", { id: "a1", pane_id: "s:p1", session_id: "s", kind: "pi", status: "working", detector: "hook", persistence_session_id: "s", session_ref: null, job_id: null, since: "t" }],
      ["a2", { id: "a2", pane_id: "s:p2", session_id: "s", kind: "shell", status: "idle", detector: "scrape", persistence_session_id: "s", session_ref: null, job_id: null, since: "t" }],
    ] as const);
    const panes = [
      { id: "s:p1", session_id: "s", kitty_window_id: 1, cwd: null, agent_id: "a1", is_viewer: false },
      { id: "s:p2", session_id: "s", kitty_window_id: 2, cwd: null, agent_id: "a2", is_viewer: false },
      { id: "s:p3", session_id: "s", kitty_window_id: 3, cwd: null, agent_id: null, is_viewer: false }, // bare shell — no dot
    ];
    const rollup = rollupForPanes(panes, agents as never);
    expect(rollup).toEqual({ blocked: 0, working: 1, done: 0, idle: 1 });
  });

  test("stampSessionRollups stamps per-session hints matching the legs", () => {
    const sessions = [
      { id: "s1", workspace_id: "w", persistence_session_id: "s1", backend: "zmx" as const, observed_at: "t", pane_ids: [], status_rollup: { blocked: 0, working: 0, done: 0, idle: 0 } },
      { id: "s2", workspace_id: "w", persistence_session_id: "s2", backend: "zmx" as const, observed_at: "t", pane_ids: [], status_rollup: { blocked: 0, working: 0, done: 0, idle: 0 } },
    ];
    const panes = [
      { id: "s1:p1", session_id: "s1", kitty_window_id: 1, cwd: null, agent_id: "a1", is_viewer: false },
      { id: "s2:p1", session_id: "s2", kitty_window_id: 2, cwd: null, agent_id: "a2", is_viewer: false },
    ];
    const agents = [
      { id: "a1", pane_id: "s1:p1", session_id: "s1", kind: "pi" as const, status: "blocked" as const, detector: "hook" as const, persistence_session_id: "s1", session_ref: null, job_id: null, since: "t" },
      { id: "a2", pane_id: "s2:p1", session_id: "s2", kind: "pi" as const, status: "done" as const, detector: "hook" as const, persistence_session_id: "s2", session_ref: null, job_id: null, since: "t" },
    ];
    stampSessionRollups(sessions, panes, agents);
    expect(sessions[0]!.status_rollup).toEqual({ blocked: 1, working: 0, done: 0, idle: 0 });
    expect(sessions[1]!.status_rollup).toEqual({ blocked: 0, working: 0, done: 1, idle: 0 });
  });
});

// ---------------------------------------------------------------------------------------------
// Discovery: joins from fake ls/zmx fixtures.
// ---------------------------------------------------------------------------------------------

describe("discovery join", () => {
  test("joins zmx sessions × kitty windows into panes keyed by the composite id", async () => {
    const rig = makeRig();
    rig.zmx.add("term-0707-1530");
    rig.kitty.addWindow({
      id: 7,
      title: "term-0707-1530",
      cwd: "/home/tom/work/api",
      user_vars: { [SESSION_USER_VAR]: "term-0707-1530", [PANE_USER_VAR]: "p2" },
    });
    const { discovery, store } = discoveryWithStore(rig);

    const observed: string[] = [];
    rig.bus.on("session.observed", (p) => observed.push(p.session_id));
    const created: string[] = [];
    rig.bus.on("pane.created", (p) => created.push(p.pane_id));

    await discovery.reconcile();

    const panes = store.listPanes();
    expect(panes.length).toBe(1);
    expect(panes[0]!.id).toBe("term-0707-1530:p2");
    expect(panes[0]!.kitty_window_id).toBe(7);
    expect(panes[0]!.cwd).toBe("/home/tom/work/api");
    expect(store.listSessions().map((s) => s.id)).toEqual(["term-0707-1530"]);
    expect(store.getSession("term-0707-1530")!.persistence_session_id).toBe("term-0707-1530");
    expect(observed).toEqual(["term-0707-1530"]);
    expect(created).toEqual(["term-0707-1530:p2"]);
  });

  test("binds a window to a session by title when no user-var is set", async () => {
    const rig = makeRig();
    rig.zmx.add("term-0101-0900");
    rig.kitty.addWindow({ id: 3, title: "term-0101-0900", cwd: "/x" });
    const { discovery, store } = discoveryWithStore(rig);
    await discovery.reconcile();
    expect(store.listPanes().map((p) => p.id)).toEqual(["term-0101-0900:p3"]);
  });

  test("does not admit a kitty window with no matching zmx session", async () => {
    const rig = makeRig();
    rig.zmx.add("term-known");
    rig.kitty.addWindow({ id: 9, title: "some-random-window", cwd: "/x" });
    const { discovery, store } = discoveryWithStore(rig);
    await discovery.reconcile();
    expect(store.listPanes().length).toBe(0);
    expect(store.listSessions().length).toBe(0);
  });

  test("observed_at is stable across reconciles (first-seen, not creation)", async () => {
    const rig = makeRig();
    rig.zmx.add("s");
    rig.kitty.addWindow({ id: 1, title: "s", user_vars: { [SESSION_USER_VAR]: "s" } });
    const { discovery, store } = discoveryWithStore(rig);
    await discovery.reconcile();
    const first = store.getSession("s")!.observed_at;
    await discovery.reconcile();
    expect(store.getSession("s")!.observed_at).toBe(first);
  });

  test("a vanished window emits pane.closed and, when the last, session.ended", async () => {
    const rig = makeRig();
    rig.zmx.add("s");
    rig.kitty.addWindow({ id: 1, title: "s", user_vars: { [SESSION_USER_VAR]: "s", [PANE_USER_VAR]: "p1" } });
    const { discovery, store } = discoveryWithStore(rig);
    await discovery.reconcile();
    expect(store.listPanes().length).toBe(1);

    const closed: string[] = [];
    const ended: string[] = [];
    rig.bus.on("pane.closed", (p) => closed.push(p.pane_id));
    rig.bus.on("session.ended", (p) => ended.push(p.session_id));

    // Remove the window from kitty (window closed) — re-install a kitty with no windows.
    const rig2Exec = rig.exec;
    new FakeKitty().install(rig2Exec); // replaces the kitty handler with an empty universe
    await discovery.reconcile();

    expect(closed).toEqual(["s:p1"]);
    expect(ended).toEqual(["s"]);
    expect(store.listPanes().length).toBe(0);
    expect(store.listSessions().length).toBe(0);
  });

  test("sessions glob scoping filters the zmx universe", async () => {
    const rig = makeRig();
    rig.zmx.add("term-keep-1", "term-drop-1");
    rig.kitty.addWindow({ id: 1, title: "term-keep-1", user_vars: { [SESSION_USER_VAR]: "term-keep-1" } });
    rig.kitty.addWindow({ id: 2, title: "term-drop-1", user_vars: { [SESSION_USER_VAR]: "term-drop-1" } });
    const { discovery, store } = discoveryWithStore(rig, ["term-keep-*"]);
    await discovery.reconcile();
    expect(store.listSessions().map((s) => s.id)).toEqual(["term-keep-1"]);
  });

  test("idempotent: a second reconcile with unchanged substrate emits nothing new", async () => {
    const rig = makeRig();
    rig.zmx.add("s");
    rig.kitty.addWindow({ id: 1, title: "s", user_vars: { [SESSION_USER_VAR]: "s" } });
    const { discovery, store } = discoveryWithStore(rig);
    await discovery.reconcile();
    let events = 0;
    rig.bus.on("pane.created", () => events++);
    rig.bus.on("session.observed", () => events++);
    await discovery.reconcile();
    expect(events).toBe(0);
    expect(store.listPanes().length).toBe(1);
  });

  test("focus follows the focused kitty window", async () => {
    const rig = makeRig();
    rig.zmx.add("s");
    rig.kitty.addWindow({ id: 5, title: "s", is_focused: true, user_vars: { [SESSION_USER_VAR]: "s", [PANE_USER_VAR]: "p1" } });
    const { discovery, store } = discoveryWithStore(rig);
    const focusedPanes: string[] = [];
    rig.bus.on("pane.focused", (p) => focusedPanes.push(p.pane_id));
    await discovery.reconcile();
    expect(store.getFocus().pane).toBe("s:p1");
    expect(store.getFocus().session).toBe("s");
    expect(focusedPanes).toEqual(["s:p1"]);
  });
});

// ---------------------------------------------------------------------------------------------
// Viewer marking + register_viewer.
// ---------------------------------------------------------------------------------------------

describe("viewer marking", () => {
  test("register_viewer marks a live pane is_viewer and survives reconcile", async () => {
    const rig = makeRig();
    rig.zmx.add("s");
    rig.kitty.addWindow({ id: 4, title: "s", user_vars: { [SESSION_USER_VAR]: "s", [PANE_USER_VAR]: "watch" } });
    const { discovery, store } = discoveryWithStore(rig);
    await discovery.reconcile();
    expect(store.getPane("s:watch")!.is_viewer).toBe(false);

    const res = discovery.registerViewer({ kitty_window_id: 4 });
    expect(res).toEqual({ registered: true, kitty_window_id: 4 });
    expect(store.getPane("s:watch")!.is_viewer).toBe(true);

    // Marking survives a subsequent reconcile.
    await discovery.reconcile();
    expect(store.getPane("s:watch")!.is_viewer).toBe(true);
  });

  test("register_viewer applied BEFORE the pane exists still marks it on discovery", async () => {
    const rig = makeRig();
    rig.zmx.add("s");
    rig.kitty.addWindow({ id: 8, title: "s", user_vars: { [SESSION_USER_VAR]: "s", [PANE_USER_VAR]: "v" } });
    const { discovery, store } = discoveryWithStore(rig);
    discovery.registerViewer({ kitty_window_id: 8 });
    await discovery.reconcile();
    expect(store.getPane("s:v")!.is_viewer).toBe(true);
  });

  test("register_viewer rejects malformed params", () => {
    const rig = makeRig();
    const discovery = makeDiscovery(rig);
    expect(() => discovery.registerViewer({})).toThrow();
    expect(() => discovery.registerViewer({ kitty_window_id: "x" })).toThrow();
    expect(() => discovery.registerViewer(null)).toThrow();
  });
});

// ---------------------------------------------------------------------------------------------
// Workspace tier (niri parse + default fallback).
// ---------------------------------------------------------------------------------------------

describe("workspace tier", () => {
  test("parseNiriWorkspaces reads named + indexed workspaces", () => {
    const ws = parseNiriWorkspaces(
      JSON.stringify([
        { id: 1, idx: 1, name: "harness-desktop", is_focused: true },
        { id: 2, idx: 2 },
      ]),
    );
    expect(ws).not.toBeNull();
    expect(ws!.map((w) => w.id)).toEqual(["harness-desktop", "ws-2"]);
  });

  test("parseNiriWorkspaces returns null on non-array / unparseable output", () => {
    expect(parseNiriWorkspaces("not json")).toBeNull();
    expect(parseNiriWorkspaces('{"x":1}')).toBeNull();
  });

  test("defaultWorkspace names the panel by conductorHost", () => {
    expect(defaultWorkspace("harness-desktop")).toEqual({
      id: "harness-desktop",
      label: "harness-desktop",
      focused_session: null,
    });
  });

  test("discovery falls back to the conductorHost workspace when niri is absent", async () => {
    const rig = makeRig();
    rig.zmx.add("s");
    rig.kitty.addWindow({ id: 1, title: "s", user_vars: { [SESSION_USER_VAR]: "s" } });
    const { discovery, store } = discoveryWithStore(rig);
    await discovery.reconcile();
    expect(store.listWorkspaces().map((w) => w.id)).toEqual(["harness-desktop"]);
    expect(store.getSession("s")!.workspace_id).toBe("harness-desktop");
  });

  test("discovery uses niri workspaces when present", async () => {
    const rig = makeRig();
    rig.exec.register("niri", () =>
      ok(JSON.stringify([{ id: 1, idx: 1, name: "code", is_focused: true }])),
    );
    rig.zmx.add("s");
    rig.kitty.addWindow({ id: 1, title: "s", user_vars: { [SESSION_USER_VAR]: "s" } });
    const { discovery, store } = discoveryWithStore(rig);
    await discovery.reconcile();
    expect(store.listWorkspaces().map((w) => w.id)).toEqual(["code"]);
  });
});

// ---------------------------------------------------------------------------------------------
// Snapshot composition from the single store (agents[]/jobs[] legs written via the Bus).
// ---------------------------------------------------------------------------------------------

describe("snapshot composition", () => {
  test("§2.2 frame body shape + key order from the single store", async () => {
    const rig = makeRig();
    rig.zmx.add("term-0707-1530");
    rig.kitty.addWindow({
      id: 7,
      title: "term-0707-1530",
      cwd: "/home/tom/work/api",
      is_focused: true,
      user_vars: { [SESSION_USER_VAR]: "term-0707-1530", [PANE_USER_VAR]: "p2" },
    });
    const { discovery, store } = discoveryWithStore(rig);
    await discovery.reconcile();

    // Detector writes the agents[] leg via the Bus (single-store ruling).
    const detected: AgentDetectedPayload = {
      agent_id: "ag_91be",
      pane_id: "term-0707-1530:p2",
      session_id: "term-0707-1530",
      kind: "claude-code",
      status: "working",
      detector: "hook",
      persistence_session_id: "term-0707-1530",
      session_ref: "3d9c1a2e",
      kitty_window_id: 7,
    };
    rig.bus.emit("agent.detected", detected);

    // Jobs writes the jobs[] leg via the Bus.
    const enqueued: JobEnqueuedPayload = {
      job_id: "job_0f21",
      task_uuid: "b2c4-uuid",
      class: "high",
      source: "orchestrator",
      agent_kind: "claude-code",
      invocation: "claude --resume 3d9c1a2e",
      cwd: "/home/tom/work/api",
      evidence_spec: [],
      priority: "high",
    };
    rig.bus.emit("job.enqueued", enqueued);

    const snap = store.composeSnapshot();

    // Key order matches the frozen §2.2 order.
    expect(Object.keys(snap)).toEqual([...SNAPSHOT_KEY_ORDER]);
    expect(snap.protocol).toBe(PROTOCOL_ID);
    expect(snap.protocol_version).toBe(PROTOCOL_VERSION);

    // Tree tiers.
    expect(snap.workspaces.map((w) => w.id)).toEqual(["harness-desktop"]);
    expect(snap.sessions.length).toBe(1);
    expect(snap.sessions[0]!.pane_ids).toEqual(["term-0707-1530:p2"]);
    expect(snap.panes[0]!.id).toBe("term-0707-1530:p2");
    expect(snap.panes[0]!.agent_id).toBe("ag_91be"); // detector bound the pane's agent leg

    // Legs composed from the store (detector agents[], jobs jobs[]).
    expect(snap.agents.map((a) => a.id)).toEqual(["ag_91be"]);
    expect(snap.agents[0]!.status).toBe("working");
    expect(snap.jobs.map((j) => j.job_id)).toEqual(["job_0f21"]);
    expect(snap.jobs[0]!.state).toBe("enqueued");

    // Rollup hint reflects the working agent.
    expect(snap.sessions[0]!.status_rollup).toEqual({ blocked: 0, working: 1, done: 0, idle: 0 });

    // Focus follows the focused window.
    expect(snap.focus.pane).toBe("term-0707-1530:p2");
  });

  test("agent.status_changed updates the mirrored leg; agent.released removes it and clears the pane", async () => {
    const rig = makeRig();
    rig.zmx.add("s");
    rig.kitty.addWindow({ id: 1, title: "s", user_vars: { [SESSION_USER_VAR]: "s", [PANE_USER_VAR]: "p1" } });
    const { discovery, store } = discoveryWithStore(rig);
    await discovery.reconcile();

    rig.bus.emit("agent.detected", {
      agent_id: "ag_1",
      pane_id: "s:p1",
      session_id: "s",
      kind: "pi",
      status: "working",
      detector: "hook",
      persistence_session_id: "s",
      session_ref: null,
      kitty_window_id: 1,
    } satisfies AgentDetectedPayload);
    expect(store.getPane("s:p1")!.agent_id).toBe("ag_1");

    rig.bus.emit("agent.status_changed", {
      agent_id: "ag_1",
      pane_id: "s:p1",
      session_id: "s",
      status: "done",
      prev_status: "working",
      detector: "hook",
      since: "t2",
    } satisfies AgentStatusChangedPayload);
    expect(store.readAgents()[0]!.status).toBe("done");

    rig.bus.emit("agent.released", { agent_id: "ag_1", pane_id: "s:p1", session_id: "s", reason: "exited" });
    expect(store.readAgents().length).toBe(0);
    expect(store.getPane("s:p1")!.agent_id).toBeNull();
  });

  test("a registered SnapshotSectionProvider is authoritative over the Bus mirror", () => {
    const rig = makeRig();
    const store = new SessionStore({ bus: rig.bus });
    store.wireLegs();
    rig.bus.emit("agent.detected", {
      agent_id: "mirror",
      pane_id: "s:p",
      session_id: "s",
      kind: "pi",
      status: "idle",
      detector: "scrape",
      persistence_session_id: "s",
      session_ref: null,
      kitty_window_id: 1,
    } satisfies AgentDetectedPayload);
    expect(store.readAgents().map((a) => a.id)).toEqual(["mirror"]);

    store.registerSectionProvider({
      section: "agents",
      read: () => [
        { id: "authoritative", pane_id: "s:p", session_id: "s", kind: "pi", status: "working", detector: "hook", persistence_session_id: "s", session_ref: null, job_id: null, since: "t" },
      ],
    });
    expect(store.readAgents().map((a) => a.id)).toEqual(["authoritative"]);
  });
});

// ---------------------------------------------------------------------------------------------
// SessionModel: mount surface (session.list + register_viewer RPCs, reconcile loop).
// ---------------------------------------------------------------------------------------------

describe("SessionModel mount", () => {
  function fakeMount() {
    const rpcs = new Map<string, (p: unknown) => unknown>();
    const supervised: Array<{ name: string; start: () => unknown; stop?: () => unknown }> = [];
    return {
      rpcs,
      supervised,
      registerRpc: (m: string, h: (p: unknown) => unknown) => rpcs.set(m, h),
      registerWatcher: () => {},
      registerSupervised: (l: { name: string; start: () => unknown; stop?: () => unknown }) => supervised.push(l),
    };
  }

  test("mounts register_viewer + session.list and a reconcile loop", async () => {
    const rig = makeRig();
    rig.zmx.add("term-x");
    rig.kitty.addWindow({ id: 2, title: "term-x", user_vars: { [SESSION_USER_VAR]: "term-x", [PANE_USER_VAR]: "p1" } });
    const model = new SessionModel({ bus: rig.bus, exec: rig.exec, config: rig.config });
    const mount = fakeMount();
    model.mount(mount as never);

    expect(mount.rpcs.has("session.register_viewer")).toBe(true);
    expect(mount.rpcs.has("session.list")).toBe(true);
    expect(mount.supervised.map((s) => s.name)).toContain("session-model.reconcile");

    // The supervised reconcile loop is LONG-RUNNING: its `start()` primes a fire-and-forget reconcile
    // then stays pending until stop() (a supervised loop that settled would be restart-churned). We
    // fire start() (do NOT await it — it never resolves while running) and drive the reconcile the
    // deterministic way for the assertion.
    void mount.supervised.find((s) => s.name === "session-model.reconcile")!.start();
    await model.discovery.reconcile();

    // §1.2: session.list returns a BARE array (one record per session, no `sessions` envelope).
    const list = model.handleSessionList({}) as Array<{ session: string; panes: unknown[] }>;
    expect(list.map((s) => s.session)).toEqual(["term-x"]);
    expect(list[0]!.panes.length).toBe(1);

    // Snapshot provider assembles from the same store.
    const snap = model.snapshotProvider.snapshot();
    expect(snap.sessions.map((s) => s.id)).toEqual(["term-x"]);

    model.unmount();
  });

  test("session.list projects the agent leg and honors the workspace filter", async () => {
    const rig = makeRig();
    rig.zmx.add("s");
    rig.kitty.addWindow({ id: 1, title: "s", user_vars: { [SESSION_USER_VAR]: "s", [PANE_USER_VAR]: "p1" } });
    const model = new SessionModel({ bus: rig.bus, exec: rig.exec, config: rig.config });
    const mount = fakeMount();
    model.mount(mount as never);
    // Long-running loop: fire start() (never resolves while running) and drive the first reconcile.
    void mount.supervised[0]!.start();
    await model.discovery.reconcile();

    rig.bus.emit("agent.detected", {
      agent_id: "ag_1",
      pane_id: "s:p1",
      session_id: "s",
      kind: "pi",
      status: "blocked",
      detector: "hook",
      persistence_session_id: "s",
      session_ref: null,
      kitty_window_id: 1,
    } satisfies AgentDetectedPayload);

    const list = model.handleSessionList({}) as Array<{ session: string; workspace: string; panes: Array<{ agent: { kind: string; status: string } | null }> }>;
    expect(list[0]!.panes[0]!.agent).toEqual({ kind: "pi", status: "blocked" });

    // A non-matching workspace filter yields nothing.
    const empty = model.handleSessionList({ workspace: "nope" }) as unknown[];
    expect(empty.length).toBe(0);

    // The real workspace filter matches.
    const matched = model.handleSessionList({ workspace: "harness-desktop" }) as unknown[];
    expect(matched.length).toBe(1);

    model.unmount();
  });

  test("session.list rejects malformed params", () => {
    const rig = makeRig();
    const model = new SessionModel({ bus: rig.bus, exec: rig.exec, config: rig.config });
    expect(() => model.handleSessionList({ workspace: 5 })).toThrow();
    expect(() => model.handleSessionList({ short: "x" })).toThrow();
    expect(() => model.handleSessionList([])).toThrow();
  });
});
