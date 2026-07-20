// tally — Strategy-1 cooperative-hook state (IMPLEMENTATION-PLAN M2.3; CLI-SURFACE §3.3).
//
// AUTHORITATIVE where available. The installed hooks (layer 3: hooks/claude-code/tally-hook.ts,
// hooks/pi/tally-session.ts) POST an `agent.hook_event` NDJSON frame to the tally socket; daemon-core
// routes it to the detector's registered RPC handler (via the DaemonMount seam). This module owns the
// `agent.hook_event` param shape (an internal-additive carrier — no param type lives in wire.ts by
// design; the producing module defines it) and the lifecycle→status mapping.
//
// Lifecycle map (CLI-SURFACE §3.3): running→working, idle→idle, needsInput→blocked,
// unknown→scrape-fallback (the detector holds/keeps scraping). Turn boundaries gate the scraper:
// `UserPromptSubmit` = turn start, `Stop` = turn end (a settled turn need not be scraped every tick).
// Hooks also carry the resume/session ref used by recover().

import { ValidationError } from "../contracts/errors";
import type { AgentKind, InternalAgentStatus } from "../contracts/agent";

/**
 * The cooperative-harness lifecycle enum the hooks report (CLI-SURFACE §3.3): the four-value
 * lifecycle mapped into tally's status vocabulary. `unknown` triggers the scrape fallback.
 */
export type HookLifecycle = "running" | "idle" | "needsInput" | "unknown";

/**
 * The turn-boundary events the harness hooks post (CLI-SURFACE §3.3 turn gating). `UserPromptSubmit`
 * opens a turn (the scraper runs at active cadence); `Stop` closes it. `SessionStart`/`Notification`
 * are the Claude Code hook events that carry identity / needs-input signals.
 */
export type HookTurnEvent = "UserPromptSubmit" | "Stop" | "SessionStart" | "Notification";

/**
 * The `agent.hook_event` RPC params (IMPLEMENTATION-PLAN §3 — internal-additive carrier; this module
 * owns the shape). Posted by the installed cooperative hook. `pane_id` OR `kitty_window_id` locates
 * the pane; at least one MUST be present. `lifecycle` maps to a status; `turn` gates the scraper;
 * `session_ref` is the harness resume id carried for recover().
 */
export interface HookEventParams {
  kind: AgentKind;
  /** The pane composite id, when the hook knows it (from `$KITTY_WINDOW_ID` → pane resolution). */
  pane_id?: string;
  /** The kitty window id, when the hook posts it directly (from `$KITTY_WINDOW_ID`). */
  kitty_window_id?: number;
  /** The lifecycle the harness reports (maps to a status). Optional when only `turn` is posted. */
  lifecycle?: HookLifecycle;
  /** A turn-boundary event (gates the scraper). Optional when only `lifecycle` is posted. */
  turn?: HookTurnEvent;
  /** The harness resume/session ref (`pi --session <id>` / `claude --resume <id>`). */
  session_ref?: string | null;
  /** The working directory the hook reports (best-effort join key). */
  cwd?: string | null;
}

const KIND_SET: ReadonlySet<string> = new Set(["pi", "claude-code", "shell"]);
const LIFECYCLE_SET: ReadonlySet<string> = new Set(["running", "idle", "needsInput", "unknown"]);
const TURN_SET: ReadonlySet<string> = new Set(["UserPromptSubmit", "Stop", "SessionStart", "Notification"]);

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * Hand-rolled validation of an `agent.hook_event` frame (no zod — plan rule). Rejects a frame with
 * neither a `pane_id` nor a `kitty_window_id`, and a frame carrying neither `lifecycle` nor `turn`.
 */
export function validateHookEventParams(v: unknown): HookEventParams {
  if (!isObj(v)) throw new ValidationError("agent.hook_event params must be an object", "params");

  if (typeof v.kind !== "string" || !KIND_SET.has(v.kind)) {
    throw new ValidationError("agent.hook_event.kind must be pi|claude-code|shell", "kind");
  }
  const out: HookEventParams = { kind: v.kind as AgentKind };

  if (v.pane_id !== undefined) {
    if (typeof v.pane_id !== "string") throw new ValidationError("pane_id must be a string", "pane_id");
    out.pane_id = v.pane_id;
  }
  if (v.kitty_window_id !== undefined) {
    if (typeof v.kitty_window_id !== "number" || !Number.isFinite(v.kitty_window_id)) {
      throw new ValidationError("kitty_window_id must be a number", "kitty_window_id");
    }
    out.kitty_window_id = v.kitty_window_id;
  }
  if (out.pane_id === undefined && out.kitty_window_id === undefined) {
    throw new ValidationError("agent.hook_event needs a pane_id or kitty_window_id", "pane_id");
  }

  if (v.lifecycle !== undefined) {
    if (typeof v.lifecycle !== "string" || !LIFECYCLE_SET.has(v.lifecycle)) {
      throw new ValidationError("lifecycle must be running|idle|needsInput|unknown", "lifecycle");
    }
    out.lifecycle = v.lifecycle as HookLifecycle;
  }
  if (v.turn !== undefined) {
    if (typeof v.turn !== "string" || !TURN_SET.has(v.turn)) {
      throw new ValidationError("turn must be UserPromptSubmit|Stop|SessionStart|Notification", "turn");
    }
    out.turn = v.turn as HookTurnEvent;
  }
  if (out.lifecycle === undefined && out.turn === undefined) {
    throw new ValidationError("agent.hook_event needs a lifecycle or a turn event", "lifecycle");
  }

  if (v.session_ref !== undefined) {
    if (v.session_ref !== null && typeof v.session_ref !== "string") {
      throw new ValidationError("session_ref must be a string or null", "session_ref");
    }
    out.session_ref = v.session_ref;
  }
  if (v.cwd !== undefined) {
    if (v.cwd !== null && typeof v.cwd !== "string") {
      throw new ValidationError("cwd must be a string or null", "cwd");
    }
    out.cwd = v.cwd;
  }
  return out;
}

/**
 * Map a hook lifecycle to tally's internal status. `unknown` → `unknown` (the scrape fallback: the
 * loop keeps scraping / holds last-known). The three concrete values are authoritative.
 */
export function lifecycleToStatus(lifecycle: HookLifecycle): InternalAgentStatus {
  switch (lifecycle) {
    case "running":
      return "working";
    case "idle":
      return "idle";
    case "needsInput":
      return "blocked";
    case "unknown":
      return "unknown";
  }
}

/**
 * The turn-gate decision a turn event implies. `UserPromptSubmit` opens a turn ⇒ the scraper runs at
 * ACTIVE cadence; `Stop` closes it ⇒ the scraper idles. `SessionStart`/`Notification` do not change
 * the turn gate (they carry identity / needs-input signals handled via `lifecycle`).
 */
export function turnGate(turn: HookTurnEvent): "open" | "close" | "none" {
  if (turn === "UserPromptSubmit") return "open";
  if (turn === "Stop") return "close";
  return "none";
}

/** Whether a Claude Code `Notification` hook event signals the agent needs operator input (blocked). */
export function notificationImpliesBlocked(): boolean {
  // Claude Code fires `Notification` when it needs attention / permission — treat as needsInput.
  return true;
}
