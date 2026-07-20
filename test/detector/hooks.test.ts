// tally — detector Strategy-1 hook tests (IMPLEMENTATION-PLAN M2.3: hook lifecycle map + turn gating).
//
// Asserts the `agent.hook_event` param validation (hand-rolled, no zod), the lifecycle→status map
// (running→working, idle→idle, needsInput→blocked, unknown→scrape-fallback), and the turn gate
// (UserPromptSubmit=open, Stop=close).

import { describe, expect, test } from "bun:test";
import {
  validateHookEventParams,
  lifecycleToStatus,
  turnGate,
} from "../../src/detector/hooks.ts";
import { ValidationError } from "../../src/contracts/errors.ts";

describe("hook lifecycle → status map", () => {
  test("running→working, idle→idle, needsInput→blocked, unknown→unknown", () => {
    expect(lifecycleToStatus("running")).toBe("working");
    expect(lifecycleToStatus("idle")).toBe("idle");
    expect(lifecycleToStatus("needsInput")).toBe("blocked");
    expect(lifecycleToStatus("unknown")).toBe("unknown");
  });
});

describe("turn gate", () => {
  test("UserPromptSubmit opens, Stop closes, others no-op", () => {
    expect(turnGate("UserPromptSubmit")).toBe("open");
    expect(turnGate("Stop")).toBe("close");
    expect(turnGate("SessionStart")).toBe("none");
    expect(turnGate("Notification")).toBe("none");
  });
});

describe("agent.hook_event validation", () => {
  test("accepts a full frame", () => {
    const p = validateHookEventParams({
      kind: "claude-code",
      kitty_window_id: 7,
      lifecycle: "running",
      turn: "UserPromptSubmit",
      session_ref: "abc123",
      cwd: "/home/tom/proj",
    });
    expect(p.kind).toBe("claude-code");
    expect(p.kitty_window_id).toBe(7);
    expect(p.lifecycle).toBe("running");
    expect(p.session_ref).toBe("abc123");
  });

  test("accepts a turn-only frame", () => {
    const p = validateHookEventParams({ kind: "pi", pane_id: "s:0", turn: "Stop" });
    expect(p.turn).toBe("Stop");
  });

  test("rejects a bad kind", () => {
    expect(() => validateHookEventParams({ kind: "gpt", pane_id: "s:0", turn: "Stop" })).toThrow(ValidationError);
  });

  test("rejects a frame with neither pane_id nor kitty_window_id", () => {
    expect(() => validateHookEventParams({ kind: "pi", turn: "Stop" })).toThrow(ValidationError);
  });

  test("rejects a frame with neither lifecycle nor turn", () => {
    expect(() => validateHookEventParams({ kind: "pi", pane_id: "s:0" })).toThrow(ValidationError);
  });

  test("rejects a bad lifecycle / turn value", () => {
    expect(() => validateHookEventParams({ kind: "pi", pane_id: "s:0", lifecycle: "busy" })).toThrow(ValidationError);
    expect(() => validateHookEventParams({ kind: "pi", pane_id: "s:0", turn: "Reset" })).toThrow(ValidationError);
  });

  test("accepts a null session_ref", () => {
    const p = validateHookEventParams({ kind: "shell", pane_id: "s:0", lifecycle: "idle", session_ref: null });
    expect(p.session_ref).toBeNull();
  });
});
