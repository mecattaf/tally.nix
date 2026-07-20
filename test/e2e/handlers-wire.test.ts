// test/e2e/handlers-wire.test.ts
//
// The composition-root wire pass (gate round 1): drives each of the nine cross-cutting §1 verbs —
// pane.send / pane.send_key / pane.focus / pane.capture / agent.list / agent.get / agent.read /
// query.status / query.render — through the REAL Unix socket to the REAL handler, over the ACTUAL
// `composeDaemon` composition root (not the e2e helper's partial harness). This is the test the gate
// asked for: the cli-surface golden test only pins `--help` text and never dispatches, so a missing
// daemon-side handler (method_not_found) is invisible to it — this suite catches that class of gap by
// making the live daemon answer every carrier.
//
// It composes the daemon with a FakeExec (FakeKitty + FakePls + FakeTask installed), announces one
// agent pane + one viewer pane on the daemon bus (so selector resolution has a live model), fixes the
// agent kind via the detector hook, then reaches each verb over a §2 NDJSON client and asserts the
// handler ran (the kitty write was recorded, the projection has the right shape, the viewer pane is
// refused for a detection read).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { composeDaemon, type ComposedDaemon } from "../../src/compose.ts";
import { makeTmpEnv, type TmpEnv } from "../helpers/tmp.ts";
import { connectClient, type SocketClient } from "../helpers/socket-client.ts";
import { FakeExec } from "../helpers/exec-fakes.ts";
import { FakeKitty } from "../helpers/fake-kitty.ts";
import { FakePls } from "../helpers/fake-pls.ts";
import { FakeTask } from "../helpers/fake-task.ts";
import { FakeZmx } from "../helpers/fake-zmx.ts";
import { defaultConfig } from "../../src/contracts/index.ts";
import { announcePane, tick } from "./helpers.ts";
import { runCli } from "../../src/cli/index.ts";
import { captureWriter } from "../../src/cli/output.ts";

const AGENT_WIN = 41;
const VIEWER_WIN = 42;

describe("composition-root wire — the nine cross-cutting verbs answer over the real socket", () => {
  let env: TmpEnv;
  let composed: ComposedDaemon;
  let kitty: FakeKitty;
  let client: SocketClient;
  let exec: FakeExec;

  beforeEach(async () => {
    env = makeTmpEnv();
    exec = new FakeExec();
    const pls = new FakePls();
    pls.addPool("worker-gpu", { capacity: 1, budget: 128 });
    pls.addPool("controller-gpu", { capacity: 1, budget: 128 });
    pls.install(exec);
    new FakeTask().install(exec);
    // The zmx session universe (persistence_session_id == session name). The windows declare their
    // session/pane via the `tally_session`/`tally_pane` user-vars, so discovery joins them naturally.
    new FakeZmx().add("s1").install(exec);
    kitty = new FakeKitty();
    kitty.addWindow({
      id: AGENT_WIN,
      gridText: "working…\n> \n",
      user_vars: { tally_session: "s1", tally_pane: "a" },
      foreground_processes: [{ pid: 1, cwd: "/home/tom", cmdline: ["claude"] }],
    });
    kitty.addWindow({ id: VIEWER_WIN, gridText: "tally watch\n", user_vars: { tally_session: "s1", tally_pane: "v" } });
    kitty.install(exec);

    composed = composeDaemon({ env: env.env, config: { ...defaultConfig(), heartbeatMs: 100000 }, exec });
    await composed.daemon.start();

    // Mark the viewer pane a viewer BEFORE the reconcile (the anti-loop #4 registration path), then run
    // the genuine discovery join so the STORE-backed handlers (pane.*, query.status/render) resolve
    // selectors against a real tree built from the fake zmx×kitty substrate.
    composed.model.discovery.registerViewer({ kitty_window_id: VIEWER_WIN });
    await composed.model.discovery.reconcile();

    // Announce the panes on the bus so the DETECTOR learns them (agent.* reads its own registry), then
    // fix the agent kind via the authoritative hook so `agent.*` reads see a detected agent.
    announcePane(composed.daemon.state.bus, { pane_id: "s1:a", session_id: "s1", kitty_window_id: AGENT_WIN });
    announcePane(composed.daemon.state.bus, { pane_id: "s1:v", session_id: "s1", kitty_window_id: VIEWER_WIN, is_viewer: true });
    composed.detector!.loop.applyHookEvent({ kind: "claude-code", kitty_window_id: AGENT_WIN, turn: "UserPromptSubmit" });
    await tick();

    client = await connectClient(composed.daemon.server.socketPath);
  });

  afterEach(async () => {
    client.close();
    await composed.daemon.stop();
    env.cleanup();
  });

  // ---- pane.* — the kitty-native binding over the resolved kitty_window_id. ----

  test("pane.send writes text to the resolved pane's kitty window", async () => {
    const r = await client.call<{ pane: string; kitty_window_id: number; sent: boolean }>("pane.send", { pane: "s1:a", text: "hello" });
    expect(r.pane).toBe("s1:a");
    expect(r.kitty_window_id).toBe(AGENT_WIN);
    expect(r.sent).toBe(true);
    expect(kitty.sentText.at(-1)).toEqual({ windowId: AGENT_WIN, text: "hello" });
  });

  test("pane.send_key sends the escape sequence for a named key", async () => {
    const r = await client.call<{ pane: string; key: string; sent: boolean }>("pane.send_key", { pane: "s1:a", keys: "enter" });
    expect(r.sent).toBe(true);
    expect(r.key).toBe("enter");
    expect(kitty.sentText.at(-1)).toEqual({ windowId: AGENT_WIN, text: "\r" });
  });

  test("pane.focus focuses the resolved kitty window", async () => {
    const r = await client.call<{ pane: string; kitty_window_id: number; focused: boolean }>("pane.focus", { pane: "s1:a" });
    expect(r.focused).toBe(true);
    expect(r.kitty_window_id).toBe(AGENT_WIN);
    expect(kitty.focusCalls.at(-1)).toBe(AGENT_WIN);
  });

  test("pane.capture returns the emulated grid text", async () => {
    const r = await client.call<{ pane: string; source: string; text: string }>("pane.capture", { pane: "s1:a", source: "visible", format: "text" });
    expect(r.pane).toBe("s1:a");
    expect(r.text).toContain("working");
  });

  test("pane.capture --source detection refuses a viewer pane (anti-loop #4)", async () => {
    // Assert on the wire error frame directly (the in-process socket delivers the request+response in
    // one tick, so `.request` + the error `code` is the precise probe — `.call` wraps it in a thrown
    // Error message).
    const resp = await client.request("pane.capture", { pane: "s1:v", source: "detection" });
    expect(resp.error?.code).toBe("viewer_rejected");
  });

  // ---- agent.* — read-projections of the in-daemon detector. ----

  test("agent.list projects the detected agent", async () => {
    const rows = await client.call<Array<{ id: string; pane: string; kind: string; status: string }>>("agent.list", {});
    expect(rows.length).toBe(1);
    expect(rows[0]!.pane).toBe("s1:a");
    expect(rows[0]!.kind).toBe("claude-code");
  });

  test("agent.list filters by kind + status", async () => {
    const none = await client.call<unknown[]>("agent.list", { kind: "pi" });
    expect(none.length).toBe(0);
    const some = await client.call<unknown[]>("agent.list", { kind: "claude-code" });
    expect(some.length).toBe(1);
  });

  test("agent.get returns one record for a pane selector", async () => {
    const rec = await client.call<{ pane: string; kind: string }>("agent.get", { agent_id: "s1:a" });
    expect(rec.pane).toBe("s1:a");
    expect(rec.kind).toBe("claude-code");
  });

  test("agent.read returns the detection snapshot for the agent's pane", async () => {
    const r = await client.call<{ pane: string; agent_id: string; text: string }>("agent.read", { agent_id: "s1:a" });
    expect(r.pane).toBe("s1:a");
    expect(r.text).toContain("working");
  });

  test("agent.get for an unknown selector is not_found", async () => {
    const resp = await client.request("agent.get", { agent_id: "ag_deadbeef" });
    expect(resp.error?.code).toBe("not_found");
  });

  test("agent.list --json carries the frozen §1.4 fields (session, cwd) joined from the pane", async () => {
    const rows = await client.call<Array<{ session: string; pane: string; cwd: string | null; kind: string }>>("agent.list", {});
    expect(rows.length).toBe(1);
    // The documented §1.4 keys are present (not undefined) — joined from the agent's bound pane.
    expect(rows[0]!.session).toBe("s1");
    expect(rows[0]!.cwd).toBe("/home/tom");
  });

  test("agent.get --json carries the frozen §1.4 agent_session + foreground_cwd", async () => {
    const rec = await client.call<{ agent_session: { kind: string; value: string } | null; foreground_cwd: string | null }>("agent.get", { agent_id: "s1:a" });
    // agent_session is null when no session_ref (this agent has none), foreground_cwd from the pane.
    expect(rec.foreground_cwd).toBe("/home/tom");
    expect("agent_session" in rec).toBe(true);
  });

  // ---- session.list — DRIVEN THROUGH THE REAL CLI against the composed daemon (the integration the
  // gate previously lacked: the CLI expected an array while the daemon returned {sessions:[...]}). ----

  test("`tally session list --json` works end-to-end through the CLI against the composed daemon", async () => {
    const w = captureWriter();
    const code = await runCli(["session", "list", "--json"], { writer: w, socket: composed.daemon.server.socketPath });
    expect(code).toBe(0);
    // Non-empty stdout, one JSON record per line (§1.2), each a bare session record — NOT `{}` (which
    // is what the CLI printed when the daemon returned a `{sessions:[...]}` envelope, "is not iterable").
    const lines = w.stdout.trim().split("\n").filter((l) => l.length > 0);
    expect(lines.length).toBeGreaterThanOrEqual(1);
    const first = JSON.parse(lines[0]!);
    expect(first.session).toBe("s1");
    expect(Array.isArray(first.panes)).toBe(true);
  });

  test("`tally session list` (text) works end-to-end through the CLI against the composed daemon", async () => {
    const w = captureWriter();
    const code = await runCli(["session", "list"], { writer: w, socket: composed.daemon.server.socketPath });
    expect(code).toBe(0);
    expect(w.stdout).toContain("s1");
  });

  // ---- query.* — read-time joins over the store + pls. ----

  test("query.status returns per-pool depth + protocol_version + store sessions", async () => {
    const r = await client.call<{ protocol_version: number; pools: Array<{ pool: string; budget: number }>; sessions: unknown[] }>("query.status", {});
    expect(r.protocol_version).toBe(1);
    expect(r.pools.map((p) => p.pool)).toContain("worker-gpu");
    // The announced session `s1` is in the store's session list.
    expect(r.sessions.length).toBeGreaterThanOrEqual(1);
  });

  test("query.status --pool filters to one pool", async () => {
    const r = await client.call<{ pools: Array<{ pool: string }> }>("query.status", { pool: "worker-gpu" });
    expect(r.pools.length).toBe(1);
    expect(r.pools[0]!.pool).toBe("worker-gpu");
  });

  // Issue #5: the daemon reported the jobs engine's own `queued` depth while the broker's waiting-ticket
  // count diverged wildly (queued:2 vs 93 during the 2026-07-11 pool-deadlock incident). Reproduce the
  // divergence directly against the fake broker — two `pls acquire`s on the single-capacity pool grant
  // the first and leave the second an orphaned waiting ticket the jobs engine never enqueued — and assert
  // `query.status` surfaces BOTH the engine's `queued` and the broker's `broker_queued`, flagged `diverged`.
  test("query.status surfaces broker_queued + diverged when the broker's waiting count outpaces the engine's", async () => {
    await exec.run(["pls", "acquire", "--pool", "worker-gpu", "--cost", "1", "--priority", "0"]);
    await exec.run(["pls", "acquire", "--pool", "worker-gpu", "--cost", "1", "--priority", "0"]);

    const r = await client.call<{
      pools: Array<{ pool: string; queued: number; broker_queued?: number; diverged?: boolean }>;
    }>("query.status", { pool: "worker-gpu" });

    const p = r.pools[0]!;
    // No job was ever enqueued through the engine — its own depth is 0 — while the broker's raw
    // `pls status` reports the one orphaned waiting ticket. That gap is exactly the incident's shape.
    expect(p.queued).toBe(0);
    expect(p.broker_queued).toBe(1);
    expect(p.diverged).toBe(true);
  });

  test("query.render projects the Workspace→Session→Pane tree over the store", async () => {
    const r = await client.call<{ workspaces: Array<{ workspace: string; sessions: Array<{ session: string; panes: unknown[] }> }> }>("query.render", { format: "json", scope: "sessions" });
    const allSessions = r.workspaces.flatMap((w) => w.sessions);
    const s1 = allSessions.find((s) => s.session === "s1");
    expect(s1).toBeDefined();
    // Both the agent pane and the viewer pane belong to session s1.
    expect(s1!.panes.length).toBe(2);
  });
});
