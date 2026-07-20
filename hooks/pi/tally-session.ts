// tally — pi cooperative-hook extension (IMPLEMENTATION-PLAN M3.2 `hooks/pi/tally-session.ts`;
// CLI-SURFACE §3.3 Strategy-1, §3.4 pi binding, §5 flag 2 CLOSED; SPEC boundary "tally SHIPS the
// cooperative-hook installer"; DECISIONS Q5).
//
// pi auto-discovers extension modules from `~/.pi/agent/extensions/` (or `$PI_CODING_AGENT_DIR/
// extensions/`) — the mechanism cmux installs `cmux-session.ts` into; tally installs THIS analogue
// (src/hooks/installer.ts). The extension observes pi's session lifecycle and POSTs tally's
// internal-additive `agent.hook_event` RPC — the SAME param shape `src/detector/hooks.ts`
// (`validateHookEventParams` / `HookEventParams`) owns — to the tally daemon's Unix socket, carrying
// the `pi --session <id>` resume ref used by recover().
//
// ⚠ The vendored `badlogic/pi` pin is STALE (a GPU-pod manager, CLI-SURFACE §3.4) — this binds to the
// DOCUMENTED pi extension interface, never to that clone. The interface is intentionally duck-typed
// (structural, not an import from any pi package): pi calls `activate(context)`, and the context
// exposes lifecycle registration + the session id. Whatever subset of the documented surface a given
// pi build actually provides, this extension degrades gracefully (missing hooks ⇒ fewer posts, never
// a throw), because it must NEVER break a pi session.
//
// HARD RULES (mirror hooks/claude-code/tally-hook.ts + hooks/kitty/tally-watcher.py):
//   * Connect with a short timeout, fire-and-forget the post, swallow every error.
//   * Socket absent / refused / slow ⇒ silently succeed. `tally` is invisible when the daemon is off.
//   * NEVER throw into pi. Every registration / callback is wrapped.
//   * No third-party imports; stdlib (node:net) only.
//
// The daemon side is `src/detector/hooks.ts` (the detector's registered `agent.hook_event` handler),
// which owns the matching param contract.

import { connect } from "node:net";
import { join } from "node:path";

// --- socket path resolution (mirrors src/contracts/paths.ts socketPath) ---------------------------

function runtimeDir(): string {
  const base = process.env.XDG_RUNTIME_DIR;
  if (base) return base;
  const uid = typeof process.getuid === "function" ? process.getuid() : 1000;
  return `/run/user/${uid}`;
}

function socketPath(): string {
  const override = process.env.TALLY_SOCKET;
  if (override) return override;
  return join(runtimeDir(), "tally", "tally.sock");
}

// --- the fire-and-forget NDJSON post --------------------------------------------------------------

/** The `agent.hook_event` param shape (a structural mirror of src/detector/hooks.ts HookEventParams). */
export interface HookPost {
  kind: "pi";
  kitty_window_id?: number;
  lifecycle?: "running" | "idle" | "needsInput" | "unknown";
  turn?: "UserPromptSubmit" | "Stop" | "SessionStart" | "Notification";
  session_ref?: string | null;
  cwd?: string | null;
}

/**
 * Connect to the tally socket and post ONE `agent.hook_event` NDJSON request, then resolve. Every
 * failure mode is swallowed (the pi session must never block on tally). Fire-and-forget: we write one
 * line and do not await the daemon's response.
 */
export function postHookEvent(params: HookPost, timeoutMs = 250, path = socketPath()): Promise<void> {
  const frame = {
    id: `pi-hook-${process.pid}-${Date.now()}`,
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

// --- pi lifecycle → tally lifecycle/turn mapping --------------------------------------------------

/**
 * The pi session lifecycle signals this extension binds to (documented interface; CLI-SURFACE §3.4).
 * These are the semantic edges — a given pi build names them via whatever registration hooks it
 * exposes; `activate()` below wires each available one to `emit(<signal>)`.
 */
export type PiSignal = "turnStart" | "turnEnd" | "needsInput" | "sessionStart";

/** Build the `agent.hook_event` payload for a pi lifecycle signal (CLI-SURFACE §3.3 mapping). */
export function buildPayload(signal: PiSignal, sessionRef: string | null, cwd: string | null): HookPost {
  const payload: HookPost = { kind: "pi" };

  switch (signal) {
    case "turnStart":
      payload.turn = "UserPromptSubmit";
      payload.lifecycle = "running";
      break;
    case "turnEnd":
      payload.turn = "Stop";
      payload.lifecycle = "idle";
      break;
    case "needsInput":
      // pi awaits operator input (a tool approval / a question) ⇒ blocked.
      payload.turn = "Notification";
      payload.lifecycle = "needsInput";
      break;
    case "sessionStart":
      payload.turn = "SessionStart";
      break;
  }

  if (sessionRef !== null) payload.session_ref = sessionRef;
  if (cwd !== null) payload.cwd = cwd;

  const wid = readWindowId();
  if (wid !== undefined) payload.kitty_window_id = wid;

  return payload;
}

function readWindowId(): number | undefined {
  const raw = process.env.KITTY_WINDOW_ID;
  if (!raw) return undefined;
  const n = Number(raw);
  return Number.isFinite(n) ? n : undefined;
}

// --- the pi extension surface (duck-typed to the documented interface) ----------------------------

/**
 * The subset of the pi extension context this extension uses, duck-typed. `on(event, handler)` is the
 * documented lifecycle registration; `sessionId` / `session.id` is the resume ref; `cwd` is the
 * working directory. All members are optional so a partial pi build never breaks activation.
 */
export interface PiContext {
  sessionId?: string;
  session?: { id?: string };
  cwd?: string;
  on?: (event: string, handler: (...args: unknown[]) => void) => void;
}

/**
 * The documented pi lifecycle event names, mapped to tally signals. A pi build may expose any subset;
 * `activate()` registers a handler for each present one. (The mapping is the documented lifecycle
 * vocabulary — turn boundaries + a needs-input signal — bound structurally, not by importing pi.)
 */
const PI_EVENT_MAP: ReadonlyArray<readonly [string, PiSignal]> = [
  ["sessionStart", "sessionStart"],
  ["session_start", "sessionStart"],
  ["turnStart", "turnStart"],
  ["userPromptSubmit", "turnStart"],
  ["turnEnd", "turnEnd"],
  ["stop", "turnEnd"],
  ["idle", "turnEnd"],
  ["needsInput", "needsInput"],
  ["needs_input", "needsInput"],
  ["awaitingInput", "needsInput"],
  ["notification", "needsInput"],
];

/** Resolve the resume ref (`pi --session <id>`) from the context, or `null`. */
export function resolveSessionRef(ctx: PiContext): string | null {
  if (typeof ctx.sessionId === "string" && ctx.sessionId.length > 0) return ctx.sessionId;
  const sid = ctx.session?.id;
  if (typeof sid === "string" && sid.length > 0) return sid;
  return null;
}

/** Resolve the cwd from the context, else the process cwd, else `null`. */
export function resolveCwd(ctx: PiContext): string | null {
  if (typeof ctx.cwd === "string" && ctx.cwd.length > 0) return ctx.cwd;
  try {
    const cwd = process.cwd();
    return cwd.length > 0 ? cwd : null;
  } catch {
    return null;
  }
}

/**
 * Emit one tally `agent.hook_event` for a pi signal. Wrapped so a post failure never escapes into pi.
 * Exported for tests. The `post` seam is injectable so tests can capture without a real socket.
 */
export async function emit(
  signal: PiSignal,
  ctx: PiContext,
  post: (params: HookPost) => Promise<void> = postHookEvent,
): Promise<void> {
  try {
    const payload = buildPayload(signal, resolveSessionRef(ctx), resolveCwd(ctx));
    await post(payload);
  } catch {
    // Never break the pi session on a tally post failure.
  }
}

/**
 * The pi extension entrypoint. pi calls `activate(context)` on load; we register a handler for every
 * documented lifecycle event the context exposes, and post a `sessionStart` immediately so the
 * detector learns the resume ref even if pi fires no early lifecycle event. `post` is injectable for
 * tests. Returns the count of lifecycle events successfully registered (for the test's assertion).
 */
export function activate(ctx: PiContext, post: (params: HookPost) => Promise<void> = postHookEvent): number {
  let registered = 0;
  const on = ctx.on;
  if (typeof on === "function") {
    for (const [event, signal] of PI_EVENT_MAP) {
      try {
        on(event, () => {
          void emit(signal, ctx, post);
        });
        registered += 1;
      } catch {
        // A pi build that rejects an unknown event name — skip it, keep going.
      }
    }
  }
  // Announce the session immediately (carries the resume ref) — best-effort, never throws.
  void emit("sessionStart", ctx, post);
  return registered;
}

// pi imports this module and calls `activate`; there is no direct-run entrypoint. `activate` is the
// default export so pi's loader (which may look for a default or a named `activate`) finds it either
// way.
export default activate;
