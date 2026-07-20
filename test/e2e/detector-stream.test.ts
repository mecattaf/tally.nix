// test/e2e/detector-stream.test.ts
//
// The detector fixture pass (IMPLEMENTATION-PLAN M4.1 case 5): fake grids drive blocked/working/done
// transitions onto ONE stream that a `session watch` client and an `agent wait` both consume.
//
// A live daemon with the REAL detector loop mounted (supervised, and registered as the
// WaitScrapeProvider). We announce an agent pane on the daemon bus, apply the authoritative hook to
// fix the kind, then swap the fake kitty grid + tick the loop to drive working → blocked → done. A §2
// subscriber (the `session watch` role) sees every `agent.status_changed`/convenience frame on the
// wire, and a concurrent `session.wait {subject:agent, until_status:"blocked"}` (the `agent wait`
// primitive) resolves off the same stream — proving one detector spine feeds both consumers.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { afterEach, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import {
  announcePane,
  bootDaemonHarness,
  tick,
  type DaemonHarness,
} from "./helpers.ts";
import { parseManifest } from "../../src/detector/manifest.ts";
import type { ManifestSet } from "../../src/detector/loop.ts";
import type { Clock } from "../../src/contracts/index.ts";

/**
 * A hand-advanced clock: `sleep` resolves immediately and `now()` only moves on `advance()`. The
 * detector's per-window read throttle is keyed on this clock, so advancing past the cadence is what
 * makes a fresh `get-text` read (and thus a re-classification) due on the next `tick()`. Timers are
 * no-ops (the daemon heartbeat + fallback poll stay quiet, so assertions read a clean stream).
 */
function fakeClock(): Clock & { advance(ms: number): void } {
  let t = 1_000_000;
  return {
    now: () => t,
    nowIso: () => new Date(t).toISOString(),
    sleep: () => Promise.resolve(),
    setTimer: () => () => {},
    setInterval: () => () => {},
    advance(ms: number) {
      t += ms;
    },
  };
}

const CADENCE_JUMP = 20_000; // > idle_poll_ms (10s) ⇒ a fresh throttled read is always due

const REPO = join(import.meta.dir, "..", "..");
let MANIFESTS: ManifestSet;

const WORKING_GRID = () => readFileSync(join(REPO, "test", "fixtures", "grids", "claude-code-working.txt"), "utf8");
const BLOCKED_GRID = () => readFileSync(join(REPO, "test", "fixtures", "grids", "claude-code-blocked.txt"), "utf8");
// A settled turn: the auto-accept footer + a bare prompt, and — critically — NO "esc to interrupt"
// and NO "Do you want" (so the done rule's `not` gates pass over the working/blocked rules).
const DONE_GRID = "● Done. Updated the handler.\n\n>\n\n  ⏵⏵ auto-accept edits on (shift+tab to cycle)\n";

beforeAll(() => {
  MANIFESTS = {
    "claude-code": parseManifest(readFileSync(join(REPO, "manifests", "claude-code.toml"), "utf8")),
    pi: parseManifest(readFileSync(join(REPO, "manifests", "pi.toml"), "utf8")),
  };
});

describe("detector stream — grids drive blocked/working/done onto one wire (M4.1 case 5)", () => {
  let dh: DaemonHarness;
  let clock: ReturnType<typeof fakeClock>;
  beforeEach(async () => {
    clock = fakeClock();
    dh = await bootDaemonHarness({ withDetector: true, manifests: MANIFESTS, clock });
  });
  afterEach(async () => {
    await dh.stop();
    dh.cleanup();
  });

  test("a `session watch` subscriber sees working → blocked → done on the agent spine", async () => {
    const watcher = await dh.client();
    await watcher.call("session.subscribe", {});

    const detector = dh.detector!;
    const WIN = 30;
    dh.kitty.addWindow({ id: WIN, gridText: WORKING_GRID(), foreground_processes: [{ pid: 1, cwd: "/home/tom", cmdline: ["claude"] }] });
    announcePane(dh.daemon.state.bus, { pane_id: "ws:0", session_id: "ws", kitty_window_id: WIN });

    // Hook fixes the kind + opens the turn; the working grid classifies working.
    detector.applyHookEvent({ kind: "claude-code", kitty_window_id: WIN, turn: "UserPromptSubmit" });
    clock.advance(CADENCE_JUMP);
    await detector.tick();
    expect(detector.statusOf("ws:0")).toBe("working");

    // Swap to the blocked (permission-box) grid; an open turn + a due read let the scraper re-classify.
    dh.kitty.setGrid(WIN, BLOCKED_GRID());
    detector.applyHookEvent({ kind: "claude-code", kitty_window_id: WIN, turn: "UserPromptSubmit" });
    clock.advance(CADENCE_JUMP);
    await detector.tick();
    expect(detector.statusOf("ws:0")).toBe("blocked");

    // Swap to the settled grid ⇒ done.
    dh.kitty.setGrid(WIN, DONE_GRID);
    detector.applyHookEvent({ kind: "claude-code", kitty_window_id: WIN, turn: "UserPromptSubmit" });
    clock.advance(CADENCE_JUMP);
    await detector.tick();
    expect(detector.statusOf("ws:0")).toBe("done");

    // Let the bus → wire fan-out settle, then read the subscriber's stream. The agent status spine is
    // carried by `agent.detected` (first sight) + `agent.status_changed` (transitions); read both, in
    // wire arrival order, as the one status timeline the subscriber sees.
    await tick(20);
    const spine = watcher.events
      .filter((e) => e.event === "agent.detected" || e.event === "agent.status_changed")
      .map((e) => (e as Record<string, unknown>).status as string);

    // All three states reached the wire.
    expect(spine).toContain("working");
    expect(spine).toContain("blocked");
    expect(spine).toContain("done");
    // working was seen before the pane ever blocked; the terminal state on the wire is `done`.
    expect(spine.indexOf("working")).toBeLessThan(spine.indexOf("blocked"));
    expect(spine[spine.length - 1]).toBe("done");

    // The convenience frames fired for their transitions (§2.3).
    expect(watcher.eventsNamed("agent.blocked").length).toBeGreaterThanOrEqual(1);
    expect(watcher.eventsNamed("agent.done").length).toBeGreaterThanOrEqual(1);
    // Every wire status is one of the four (unknown never reaches the wire, CLI-SURFACE §0).
    for (const s of spine) expect(["blocked", "working", "done", "idle"]).toContain(s);
  });

  test("`agent wait --status blocked` (session.wait agent) resolves off the SAME detector stream", async () => {
    const detector = dh.detector!;
    const WIN = 31;
    const AGENT_ID = "agent:claude-code:aw:0";
    dh.kitty.addWindow({ id: WIN, gridText: WORKING_GRID() });
    announcePane(dh.daemon.state.bus, { pane_id: "aw:0", session_id: "aw", kitty_window_id: WIN });
    detector.applyHookEvent({ kind: "claude-code", kitty_window_id: WIN, turn: "UserPromptSubmit" });
    clock.advance(CADENCE_JUMP);
    await detector.tick();
    expect(detector.statusOf("aw:0")).toBe("working");

    // A client waits for the agent to become blocked (the `agent wait` primitive over the four-value
    // AgentStatus — IMPLEMENTATION-PLAN §3: `until_status` accepts all four).
    const waiter = await dh.client();
    const waitPromise = waiter.call<{ satisfied: unknown[] }>(
      "session.wait",
      { predicate: { subject: "agent", agent_ids: [AGENT_ID], until_status: "blocked", count: 1 } },
      5000,
    );
    await tick(20); // let the wait subscribe before the transition fires

    // Drive the pane into `blocked` — the detector emits agent.status_changed(blocked) onto the bus,
    // which the wait consumes as its satisfying result.
    dh.kitty.setGrid(WIN, BLOCKED_GRID());
    detector.applyHookEvent({ kind: "claude-code", kitty_window_id: WIN, turn: "UserPromptSubmit" });
    clock.advance(CADENCE_JUMP);
    await detector.tick();

    const result = await waitPromise;
    expect(result.satisfied.length).toBe(1);
    const sat = result.satisfied[0] as Record<string, unknown>;
    expect(sat.agent_id).toBe(AGENT_ID);
    expect(sat.status).toBe("blocked");
  });

  test("a viewer pane is never scraped and never enters the agent stream (anti-loop invariant #4)", async () => {
    const detector = dh.detector!;
    const WIN = 32;
    dh.kitty.addWindow({ id: WIN, gridText: WORKING_GRID() });
    announcePane(dh.daemon.state.bus, { pane_id: "vw:0", session_id: "vw", kitty_window_id: WIN, is_viewer: true });
    detector.applyHookEvent({ kind: "claude-code", kitty_window_id: WIN, turn: "UserPromptSubmit" });
    clock.advance(CADENCE_JUMP);
    await detector.tick();

    // The viewer pane has no classified status and is absent from the agents[] leg.
    expect(detector.statusOf("vw:0")).toBeNull();
    expect(detector.read().find((a) => a.pane_id === "vw:0")).toBeUndefined();
  });
});
