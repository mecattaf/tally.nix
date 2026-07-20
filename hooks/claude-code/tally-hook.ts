#!/usr/bin/env bun
// tally — Claude Code cooperative hook payload (IMPLEMENTATION-PLAN M3.2 `hooks/claude-code/
// tally-hook.ts`; CLI-SURFACE §3.3 Strategy-1, §5 flag 2 CLOSED; SPEC boundary "tally SHIPS the
// cooperative-hook installer"; DECISIONS Q5).
//
// Claude Code runs this script on the hook events tally registers (`UserPromptSubmit`, `Stop`,
// `SessionStart`, `Notification`) via the generated CC settings `hooks` schema (see
// src/hooks/installer.ts). On each invocation Claude Code passes the hook JSON on STDIN and the
// hook-event name via the `CLAUDE_HOOK_EVENT` environment variable (with the stdin `hook_event_name`
// as the fallback). This script maps that into tally's internal-additive `agent.hook_event` RPC —
// the SAME param shape `src/detector/hooks.ts` (`validateHookEventParams` / `HookEventParams`) owns —
// and POSTs it as one NDJSON frame to the tally daemon's Unix socket.
//
// This is Strategy-1 AUTHORITATIVE detector input: the lifecycle a harness reports beats the scrape
// fallback. The mapping (CLI-SURFACE §3.3):
//   UserPromptSubmit → turn open  + lifecycle running  (a turn started ⇒ working)
//   Stop             → turn close + lifecycle idle      (the turn settled ⇒ idle)
//   SessionStart     → turn none  + (carries session_ref / cwd, no lifecycle change on its own)
//   Notification     → turn none  + lifecycle needsInput (CC needs attention/permission ⇒ blocked)
//
// HARD RULES (mirror the kitty watcher discipline, hooks/kitty/tally-watcher.py; CLI-SURFACE §3.1 /
// §5 flag 2): the harness must NEVER block on tally.
//   * Connect with a short timeout, fire-and-forget the post, swallow every error.
//   * Socket absent / refused / slow ⇒ silently succeed. `tally` is invisible when the daemon is off.
//   * ALWAYS exit 0 fast. A non-zero exit or a hang would stall Claude Code's turn — forbidden.
//   * No third-party imports; stdlib (node:net / Bun) only, so it runs standalone under `bun`.
//
// The daemon side is `src/detector/hooks.ts` (`validateHookEventParams` / the detector's registered
// `agent.hook_event` handler), which owns the matching param contract.

import { connect } from "node:net";
import { join } from "node:path";

// --- socket path resolution (mirrors src/contracts/paths.ts socketPath + the kitty watcher) -------

function runtimeDir(): string {
  const base = process.env.XDG_RUNTIME_DIR;
  if (base) return base;
  const uid = typeof process.getuid === "function" ? process.getuid() : 1000;
  return `/run/user/${uid}`;
}

function socketPath(): string {
  // An explicit override wins (tests / non-standard layouts), else the XDG runtime path.
  const override = process.env.TALLY_SOCKET;
  if (override) return override;
  return join(runtimeDir(), "tally", "tally.sock");
}

// --- the fire-and-forget NDJSON post --------------------------------------------------------------

/** The `agent.hook_event` param shape (a structural mirror of src/detector/hooks.ts HookEventParams). */
export interface HookPost {
  kind: "claude-code";
  kitty_window_id?: number;
  lifecycle?: "running" | "idle" | "needsInput" | "unknown";
  turn?: "UserPromptSubmit" | "Stop" | "SessionStart" | "Notification";
  session_ref?: string | null;
  cwd?: string | null;
}

/**
 * Connect to the tally socket and post ONE `agent.hook_event` NDJSON request, then resolve. Every
 * failure mode (no socket, refused, timeout, broken pipe) is swallowed — the harness must never block
 * or fail on tally. We write a single line and do not wait to read the daemon's response (ingestion is
 * one-way; the hook does not need the ack).
 */
export function postHookEvent(params: HookPost, timeoutMs = 250, path = socketPath()): Promise<void> {
  const frame = {
    id: `cc-hook-${process.pid}-${Date.now()}`,
    method: "agent.hook_event",
    params,
  };
  const line = JSON.stringify(frame) + "\n";
  return new Promise<void>((resolve) => {
    let settled = false;
    const done = () => {
      if (settled) return;
      settled = true;
      try {
        sock.destroy();
      } catch {
        // ignore
      }
      resolve();
    };
    const sock = connect(path);
    const timer = setTimeout(done, timeoutMs);
    timer.unref?.();
    sock.on("error", () => {
      clearTimeout(timer);
      done();
    });
    sock.on("connect", () => {
      sock.write(line, () => {
        clearTimeout(timer);
        done();
      });
    });
  });
}

// --- Claude Code hook-event → tally lifecycle/turn mapping ----------------------------------------

/** The Claude Code hook events tally registers (CLI-SURFACE §3.3). */
export type CcHookEvent = "UserPromptSubmit" | "Stop" | "SessionStart" | "Notification";

const CC_HOOK_EVENTS: ReadonlySet<string> = new Set([
  "UserPromptSubmit",
  "Stop",
  "SessionStart",
  "Notification",
]);

/** Coerce a raw string to a known CC hook event, or `null`. */
export function asCcHookEvent(name: string | undefined): CcHookEvent | null {
  if (name && CC_HOOK_EVENTS.has(name)) return name as CcHookEvent;
  return null;
}

/**
 * Build the `agent.hook_event` payload for a Claude Code hook event. `stdin` is the parsed hook JSON
 * Claude Code passes (may be `{}`), from which we lift the resume ref (`session_id`) and `cwd`. The
 * lifecycle/turn mapping is CLI-SURFACE §3.3; the daemon's `lifecycleToStatus`/`turnGate` complete it.
 */
export function buildPayload(event: CcHookEvent, stdin: Record<string, unknown>): HookPost {
  const payload: HookPost = { kind: "claude-code", turn: event };

  switch (event) {
    case "UserPromptSubmit":
      // Turn start ⇒ the agent is actively working.
      payload.lifecycle = "running";
      break;
    case "Stop":
      // Turn end ⇒ the agent settled / awaits the next prompt.
      payload.lifecycle = "idle";
      break;
    case "Notification":
      // CC fires Notification when it needs attention / a permission ⇒ blocked (needsInput).
      payload.lifecycle = "needsInput";
      break;
    case "SessionStart":
      // Identity only; carries the resume ref + cwd, no lifecycle change on its own.
      break;
  }

  // The Claude Code session id is the `claude --resume <id>` handle used by recover().
  const sessionRef = readString(stdin, "session_id");
  if (sessionRef !== undefined) payload.session_ref = sessionRef;

  // Best-effort cwd join key: the hook JSON's `cwd`, else the environment.
  const cwd = readString(stdin, "cwd") ?? process.env.CLAUDE_PROJECT_DIR ?? process.cwd();
  if (cwd) payload.cwd = cwd;

  // The kitty window id, when the hook runs inside a kitty pane, locates the pane for the detector.
  const wid = readWindowId();
  if (wid !== undefined) payload.kitty_window_id = wid;

  return payload;
}

function readString(obj: Record<string, unknown>, key: string): string | undefined {
  const v = obj[key];
  return typeof v === "string" && v.length > 0 ? v : undefined;
}

function readWindowId(): number | undefined {
  const raw = process.env.KITTY_WINDOW_ID;
  if (!raw) return undefined;
  const n = Number(raw);
  return Number.isFinite(n) ? n : undefined;
}

// --- stdin read ------------------------------------------------------------------------------------

/** Read all of stdin as text, tolerating no stdin (returns ""). Swallows any read error. */
async function readStdin(): Promise<string> {
  try {
    const chunks: Uint8Array[] = [];
    for await (const chunk of process.stdin) {
      chunks.push(chunk as Uint8Array);
    }
    return Buffer.concat(chunks).toString("utf8");
  } catch {
    return "";
  }
}

/** Parse the hook stdin JSON defensively; a non-object / parse failure yields `{}`. */
export function parseStdin(text: string): Record<string, unknown> {
  const trimmed = text.trim();
  if (trimmed.length === 0) return {};
  try {
    const parsed = JSON.parse(trimmed) as unknown;
    if (parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)) {
      return parsed as Record<string, unknown>;
    }
    return {};
  } catch {
    return {};
  }
}

// --- entrypoint ------------------------------------------------------------------------------------

/**
 * Resolve the hook event name from the environment (`CLAUDE_HOOK_EVENT`) or the stdin
 * `hook_event_name`, whichever is a known CC hook event. The installer wires `CLAUDE_HOOK_EVENT`
 * per registration so the one script serves every registered event.
 */
export function resolveEvent(stdin: Record<string, unknown>): CcHookEvent | null {
  return (
    asCcHookEvent(process.env.CLAUDE_HOOK_EVENT) ??
    asCcHookEvent(readString(stdin, "hook_event_name"))
  );
}

/** The main hook body — post the event, then always exit 0. Exported for tests. */
export async function main(): Promise<void> {
  const text = await readStdin();
  const stdin = parseStdin(text);
  const event = resolveEvent(stdin);
  // An unrecognized event is a silent no-op (never fail the harness).
  if (event === null) return;
  const payload = buildPayload(event, stdin);
  await postHookEvent(payload);
}

// Only run when invoked directly (not when imported by a test).
if (import.meta.main) {
  // Best-effort: never let a throw escape into Claude Code. Always exit 0.
  main()
    .catch(() => {})
    .finally(() => process.exit(0));
}
