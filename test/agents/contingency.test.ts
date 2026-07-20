// test/agents/contingency.test.ts
//
// The `--via-terminal` claude -p launch contingency (M2.2 / §1 item 9; DECISIONS Q6; CLI-SURFACE
// §3.1 sidenote) — the SOLE boundary carve-out. These tests drive the gated launch path against the
// dedicated `FakeKittyWithLaunch` fake (the only fake that permits `kitty @ launch`), asserting:
//   - the gate: `viaTerminal:false` launches NOTHING;
//   - the flow: launch the interactive TUI → settle → send the autonomous kickoff;
//   - the argv shape (interactive claude, no `-p`; declared model/session carried, never re-picked);
//   - the recoverable-session id is returned;
//   - a failed launch is a hard error.
//
// Authored fresh for tally; no vendor/ fixtures (clean-room, CLI-SURFACE §4).

import { describe, expect, test } from "bun:test";
import { FakeExec } from "../helpers/exec-fakes.ts";
import { FakeKittyWithLaunch } from "../helpers/fake-kitty.ts";
import {
  ClaudePContingency,
  buildClaudeArgv,
  buildLaunchArgv,
  isViaTerminal,
  parseLaunchedWindowId,
  VIA_TERMINAL_FLAG,
  type ViaTerminalRequest,
} from "../../src/agents/claude-p-contingency.ts";

/** A Clock whose sleep resolves immediately and records the requested delays (deterministic settle). */
function fakeClock(): { clock: import("../../src/contracts/index").Clock; slept: number[] } {
  const slept: number[] = [];
  const clock: import("../../src/contracts/index").Clock = {
    now: () => 0,
    nowIso: () => "1970-01-01T00:00:00.000Z",
    sleep: async (ms: number) => {
      slept.push(ms);
    },
    setTimer: () => () => {},
    setInterval: () => () => {},
  };
  return { clock, slept };
}

function rig() {
  const exec = new FakeExec();
  const kitty = new FakeKittyWithLaunch();
  kitty.install(exec);
  const { clock, slept } = fakeClock();
  return { exec, kitty, clock, slept, contingency: new ClaudePContingency(exec, clock) };
}

describe("claude -p via-terminal contingency", () => {
  test("isViaTerminal detects the opt-in flag", () => {
    expect(isViaTerminal([VIA_TERMINAL_FLAG])).toBe(true);
    expect(isViaTerminal(["--wait", "--json"])).toBe(false);
    expect(VIA_TERMINAL_FLAG).toBe("--via-terminal");
  });

  test("the gate: viaTerminal:false launches NOTHING", async () => {
    const { kitty, contingency } = rig();
    const res = await contingency.run({ viaTerminal: false, kickoff: "go" });
    expect(res.launched).toBe(false);
    expect(res.kittyWindowId).toBeNull();
    expect(kitty.launches.length).toBe(0);
    // The default headless path owns the run; no window opened, no text sent.
    expect(kitty.sentText.length).toBe(0);
  });

  test("the flow: launch interactive TUI → settle → send autonomous kickoff", async () => {
    const { kitty, slept, contingency } = rig();
    const req: ViaTerminalRequest = {
      viaTerminal: true,
      kickoff: "today you're unsupervised in autonomous mode — last message from me",
      cwd: "/home/tom/work",
      title: "tally:pi-session",
      settleMs: 10_000,
    };
    const res = await contingency.run(req);

    // One launch happened (the carve-out fired) and returned a window id.
    expect(kitty.launches.length).toBe(1);
    expect(res.launched).toBe(true);
    expect(res.kittyWindowId).toBe(9001);

    // The launched window runs the INTERACTIVE claude (no -p).
    expect(res.claudeArgv).toEqual(["claude"]);

    // We waited ~10 s before steering.
    expect(slept).toEqual([10_000]);

    // The autonomous kickoff was sent into the launched window, submitted (CR appended).
    expect(kitty.sentText.length).toBe(1);
    expect(kitty.sentText[0]!.windowId).toBe(9001);
    expect(kitty.sentText[0]!.text).toContain("autonomous mode");
  });

  test("buildLaunchArgv is the interactive TUI (no -p), carrying cwd/title", () => {
    const req: ViaTerminalRequest = { viaTerminal: true, kickoff: "go", cwd: "/w", title: "t" };
    const claudeArgv = buildClaudeArgv(req);
    const launch = buildLaunchArgv(req, claudeArgv);
    expect(launch).toEqual(["kitty", "@", "launch", "--type=os-window", "--cwd", "/w", "--title", "t", "claude"]);
    expect(launch).not.toContain("-p");
  });

  test("declared model + session are carried verbatim (never re-picked, PS#2)", () => {
    const argv = buildClaudeArgv({
      viaTerminal: true,
      kickoff: "go",
      sessionRef: "sess-abc",
      model: "anthropic/claude-opus-4",
    });
    expect(argv).toEqual(["claude", "--resume", "sess-abc", "--model", "anthropic/claude-opus-4"]);
  });

  test("the launched window id is parsed off kitty @ launch stdout", () => {
    expect(parseLaunchedWindowId("9042\n")).toBe(9042);
    expect(() => parseLaunchedWindowId("not-a-window")).toThrow();
  });

  test("a failed kitty @ launch is a hard error", async () => {
    const exec = new FakeExec();
    // A kitty that fails the launch verb.
    exec.register("kitty", (args) => {
      if (args[0] === "@" && args[1] === "launch") return { code: 1, stdout: "", stderr: "no display" };
      return { code: 0, stdout: "", stderr: "" };
    });
    const { clock } = fakeClock();
    const contingency = new ClaudePContingency(exec, clock);
    await expect(contingency.run({ viaTerminal: true, kickoff: "go" })).rejects.toThrow(/launch/);
  });
});
