// tally — the kitty `@` remote-control client (IMPLEMENTATION-PLAN M1.6 `rc.ts`; CLI-SURFACE §3.1).
//
// THE OUT-OF-BAND LAW (PS#15a, CLI-SURFACE §3.1): tally NEVER interposes on the pty byte stream.
// Every read is a side-channel poll of kitty's emulated grid via `kitty @ get-text`, and every
// write goes through kitty internals (`@ send-text`/`@ focus-window`), keyed on `kitty_window_id`.
// This module shells the FOUR sanctioned verbs only:
//
//   kitty @ ls                        — inventory (windows, cwd, foreground_processes incl. title
//                                        for the OSC regions, user-vars, focus)
//   kitty @ get-text --match id:<id>  — the throttled grid read (extent flags map to grid regions)
//   kitty @ send-text (+ key escapes) — `pane send` / `send-key` via kitty internals
//   kitty @ focus-window --match id:<id> — the tunnel-in/focus affordance
//   kitty @ set-user-vars --match id:<id> — at most ONE opaque identity back-reference (never status)
//
// `kitty @ launch` is FORBIDDEN and ABSENT from this surface (the boundary law): window/pane
// creation is niri/dotfiles' territory. The sole `kitty @ launch` carve-out in all of src/ is
// src/agents/claude-p-contingency.ts (M2.2), excluded by the M1.6 boundary grep-test — it is not
// here. This file names `kitty @ launch` only in comments/asserts, never invokes it.

import type { Exec, ExecOptions } from "../contracts/exec";
import { TallyError } from "../contracts/errors";

/** The kitty binary basename. Injectable only for tests that want to point elsewhere. */
export const KITTY_BIN = "kitty" as const;

/**
 * The `kitty @ launch` verb — declared here ONLY so the boundary is explicit and greppable. It is
 * FORBIDDEN on the sensors surface; `rc.ts` never constructs an argv containing it. Any attempt to
 * route it through this client throws (defence-in-depth alongside the grep-test and the fake).
 */
export const FORBIDDEN_KITTY_VERB = "launch" as const;

/** One foreground process inside a kitty window (drives the OSC-title region — CLI-SURFACE §3.3). */
export interface ForegroundProcess {
  pid: number;
  cwd: string;
  cmdline: string[];
  /** The OSC title kitty reports for this process (the `osc_title` region source). */
  title?: string;
}

/**
 * One kitty window flattened from the `@ ls` tree = one tally pane leg (keyed on `kitty_window_id`).
 * Only the fields tally's sensors/detector/session-model consume are surfaced.
 */
export interface KittyWindow {
  id: number;
  is_focused: boolean;
  is_active: boolean;
  title: string;
  cwd: string;
  foreground_processes: ForegroundProcess[];
  /** kitty user-vars — the opaque identity back-reference tally may write (never status). */
  user_vars: Record<string, string>;
  /** OSC progress escape state, if kitty reports it (the `osc_progress` region source). */
  osc_progress?: string;
  /** The tab id the window lives under (from the `@ ls` tree). */
  tab_id: number;
  /** The OS-window id the window lives under (from the `@ ls` tree). */
  os_window_id: number;
}

/** The extent of a `kitty @ get-text` read — maps herdr's GRID regions onto kitty's `--extent`. */
export type GetTextExtent =
  | "screen" // the visible screen (default)
  | "all" // scrollback + screen
  | "selection" // current selection
  | "first_cmd_output_on_screen"
  | "last_cmd_output"
  | "last_visited_cmd_output";

/** Options for a `kitty @ get-text` grid read (CLI-SURFACE §3.1; the throttled read). */
export interface GetTextOptions {
  /** Extent flag — defaults to the visible screen. OSC regions do NOT ride this (they use `ls`). */
  extent?: GetTextExtent;
  /** `--ansi`: keep escape codes (the `pane capture --format ansi` path). Default plain text. */
  ansi?: boolean;
  /** Per-call timeout override (ms). */
  timeoutMs?: number;
}

/** Options for a `kitty @ send-text` write. */
export interface SendTextOptions {
  /** Append a carriage return after the text (`pane send --enter`). */
  enter?: boolean;
  timeoutMs?: number;
}

/**
 * The named keys/chords `pane send-key` accepts, mapped to the escape sequence kitty's `send-text`
 * transmits. `send-key` is `send-text` of the escape — kitty has no separate key verb we rely on,
 * and modeling it as text keeps the ONE write path (`@ send-text`) the boundary law sanctions.
 */
export const KEY_ESCAPES: Readonly<Record<string, string>> = {
  enter: "\r",
  return: "\r",
  tab: "\t",
  esc: "\x1b",
  escape: "\x1b",
  space: " ",
  backspace: "\x7f",
  up: "\x1b[A",
  down: "\x1b[B",
  right: "\x1b[C",
  left: "\x1b[D",
  home: "\x1b[H",
  end: "\x1b[F",
  "ctrl+c": "\x03",
  "ctrl+d": "\x04",
  "ctrl+z": "\x1a",
  "ctrl+l": "\x0c",
  "ctrl+u": "\x15",
  "ctrl+a": "\x01",
  "ctrl+e": "\x05",
  "ctrl+w": "\x17",
} as const;

/**
 * Resolve a `send-key` chord (case-insensitive) to its escape sequence.
 * Throws `not_found` for an unknown key so the CLI surfaces a clear error rather than sending junk.
 */
export function keyEscape(key: string): string {
  const norm = key.trim().toLowerCase();
  const esc = KEY_ESCAPES[norm];
  if (esc === undefined) {
    throw new TallyError("not_found", `unknown key/chord "${key}" (known: ${Object.keys(KEY_ESCAPES).join(", ")})`);
  }
  return esc;
}

/** A `--match id:<n>` selector value for the kitty-native window binding. */
function matchId(windowId: number): string {
  return `id:${windowId}`;
}

/**
 * A raw `kitty @ ls` OS-window/tab/window node — the shape kitty emits and the fake mirrors. Parsed
 * defensively (every field optional at ingress) into the flat `KittyWindow[]` tally consumes.
 */
interface RawLsWindow {
  id?: unknown;
  is_focused?: unknown;
  is_active?: unknown;
  title?: unknown;
  cwd?: unknown;
  foreground_processes?: unknown;
  user_vars?: unknown;
  env?: unknown;
  osc_progress?: unknown;
}
interface RawLsTab {
  id?: unknown;
  windows?: unknown;
}
interface RawLsOSWindow {
  id?: unknown;
  tabs?: unknown;
}

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}
function asNumber(v: unknown, fallback: number): number {
  return typeof v === "number" && Number.isFinite(v) ? v : fallback;
}
function asBool(v: unknown): boolean {
  return v === true;
}
function asString(v: unknown, fallback: string): string {
  return typeof v === "string" ? v : fallback;
}

function parseForegroundProcess(v: unknown): ForegroundProcess {
  const o = isObj(v) ? v : {};
  const cmdline = Array.isArray(o.cmdline) ? o.cmdline.filter((x): x is string => typeof x === "string") : [];
  const proc: ForegroundProcess = {
    pid: asNumber(o.pid, 0),
    cwd: asString(o.cwd, ""),
    cmdline,
  };
  if (typeof o.title === "string") proc.title = o.title;
  return proc;
}

/**
 * Flatten a raw `kitty @ ls` JSON tree (OS windows → tabs → windows) into the flat `KittyWindow[]`
 * tally keys on `kitty_window_id`. Unknown fields are ignored (forward-compat); malformed nodes are
 * skipped rather than aborting the whole inventory.
 */
export function parseLsTree(raw: unknown): KittyWindow[] {
  if (!Array.isArray(raw)) return [];
  const out: KittyWindow[] = [];
  for (const osw of raw as RawLsOSWindow[]) {
    if (!isObj(osw)) continue;
    const osWindowId = asNumber(osw.id, 0);
    const tabs = Array.isArray(osw.tabs) ? (osw.tabs as RawLsTab[]) : [];
    for (const tab of tabs) {
      if (!isObj(tab)) continue;
      const tabId = asNumber(tab.id, 0);
      const windows = Array.isArray(tab.windows) ? (tab.windows as RawLsWindow[]) : [];
      for (const w of windows) {
        if (!isObj(w)) continue;
        const id = asNumber(w.id, NaN);
        if (!Number.isFinite(id)) continue; // a window without an id is unusable as a key.
        const fgRaw = Array.isArray(w.foreground_processes) ? w.foreground_processes : [];
        const userVars = isObj(w.user_vars) ? w.user_vars : isObj(w.env) ? {} : {};
        const flat: KittyWindow = {
          id,
          is_focused: asBool(w.is_focused),
          is_active: asBool(w.is_active),
          title: asString(w.title, ""),
          cwd: asString(w.cwd, ""),
          foreground_processes: fgRaw.map(parseForegroundProcess),
          user_vars: Object.fromEntries(
            Object.entries(userVars).filter((e): e is [string, string] => typeof e[1] === "string"),
          ),
          tab_id: tabId,
          os_window_id: osWindowId,
        };
        if (typeof w.osc_progress === "string") flat.osc_progress = w.osc_progress;
        out.push(flat);
      }
    }
  }
  return out;
}

/**
 * The kitty remote-control client. Takes an injected `Exec` — the ONLY way it shells out — so it is
 * testable against the layer-0 `FakeKitty`. Every method keys on `kitty_window_id`.
 */
export class KittyRc {
  constructor(
    private readonly exec: Exec,
    private readonly bin: string = KITTY_BIN,
  ) {}

  /** Run one `kitty @ <verb> ...` argv, throwing a `TallyError` on a non-zero exit. */
  private async runAt(atArgs: string[], opts?: ExecOptions): Promise<string> {
    // Defence-in-depth: never construct the forbidden verb, even if a caller passes it.
    if (atArgs[0] === FORBIDDEN_KITTY_VERB) {
      throw new TallyError(
        "unsupported",
        "kitty @ launch is forbidden on the sensors surface (CLI-SURFACE §3.1 boundary); " +
          "window creation is niri/dotfiles' — the sole carve-out is src/agents/claude-p-contingency.ts",
      );
    }
    const argv = [this.bin, "@", ...atArgs];
    const res = await this.exec.run(argv, opts);
    if (res.code !== 0) {
      throw new TallyError("internal", `kitty @ ${atArgs[0] ?? "?"} failed (exit ${res.code}): ${res.stderr.trim()}`);
    }
    return res.stdout;
  }

  /**
   * `kitty @ ls` — the window inventory, flattened to `KittyWindow[]` keyed on `kitty_window_id`.
   * The `foreground_processes[].title` here is the `osc_title` region source (§3.3) — read via `ls`,
   * NEVER via `get-text`.
   */
  async ls(opts?: ExecOptions): Promise<KittyWindow[]> {
    const stdout = await this.runAt(["ls"], opts);
    let parsed: unknown;
    try {
      parsed = JSON.parse(stdout);
    } catch (e) {
      throw new TallyError("internal", `kitty @ ls returned non-JSON: ${(e as Error).message}`);
    }
    return parseLsTree(parsed);
  }

  /**
   * `kitty @ get-text --match id:<id>` — the throttled grid read (the ONE read path the detector and
   * `pane capture` share; the OSC regions do NOT ride this — see `ls`). Extent flags scope the grid
   * region; `--ansi` keeps escapes for `pane capture --format ansi`.
   */
  async getText(windowId: number, opts: GetTextOptions = {}): Promise<string> {
    const at = ["get-text", "--match", matchId(windowId)];
    if (opts.extent !== undefined && opts.extent !== "screen") {
      at.push("--extent", opts.extent);
    }
    if (opts.ansi) at.push("--ansi");
    const execOpts = opts.timeoutMs !== undefined ? { timeoutMs: opts.timeoutMs } : undefined;
    return this.runAt(at, execOpts);
  }

  /**
   * `kitty @ send-text` — write text into a pane via kitty internals (`pane send` / `agent send`).
   * `--enter` appends a carriage return. Text rides as a trailing positional (kitty also accepts
   * stdin; the positional form is what the fake records). tally NEVER interposes on the pty stream.
   */
  async sendText(windowId: number, text: string, opts: SendTextOptions = {}): Promise<void> {
    // kitty's `send-text` TEXT argument follows PYTHON ESCAPING RULES (`\n`, `\e`, `\\`, …are decoded
    // before transmission). To deliver the caller's text LITERALLY — a regex like `grep "\n"`, a printf
    // format, a Windows path, a diff hunk — we escape backslashes so kitty decodes `\\` back to `\` and
    // never expands `\n` into a real newline (which would submit an altered/early command).
    const literal = text.replace(/\\/g, "\\\\");
    // Append the CR as an actual control byte (never the two-char `\r`) so `--enter` submits.
    const payload = opts.enter ? literal + "\r" : literal;
    // The POSIX `--` option terminator MUST precede the user-controlled payload: without it, kitty's
    // arg parser treats a payload that looks like an option (e.g. `--from-file=/path`, `--help`, or a
    // diff hunk `--- a/file`) AS a kitty option — `--from-file` in particular would make kitty read an
    // arbitrary local file into the pane (a file-read → cross-pane-injection primitive). With `--`, the
    // payload is always sent as literal text.
    const at = ["send-text", "--match", matchId(windowId), "--", payload];
    const execOpts = opts.timeoutMs !== undefined ? { timeoutMs: opts.timeoutMs } : undefined;
    await this.runAt(at, execOpts);
  }

  /**
   * `pane send-key` — send one named key/chord as its escape sequence through `@ send-text` (the ONE
   * sanctioned write path). Resolves the chord via `keyEscape`, throwing `not_found` for unknowns.
   */
  async sendKey(windowId: number, key: string, opts?: { timeoutMs?: number }): Promise<void> {
    const esc = keyEscape(key);
    // `--` guard for defense-in-depth (the escape table is fixed today, but keep the write path uniform).
    const at = ["send-text", "--match", matchId(windowId), "--", esc];
    const execOpts = opts?.timeoutMs !== undefined ? { timeoutMs: opts.timeoutMs } : undefined;
    await this.runAt(at, execOpts);
  }

  /**
   * `kitty @ focus-window --match id:<id>` — the tunnel-in/focus affordance. Focus keys on
   * `kitty_window_id`, never on tab/split identity. Cross-terminal MOVES belong to niri, not tally.
   */
  async focusWindow(windowId: number, opts?: ExecOptions): Promise<void> {
    await this.runAt(["focus-window", "--match", matchId(windowId)], opts);
  }

  /**
   * `kitty @ set-user-vars --match id:<id>` — write AT MOST an opaque identity back-reference (e.g.
   * the pane's composite key), NEVER a status mirror (CLI-SURFACE §5 flag 1; agent kind/status is a
   * delta-stream-only fact so CUBS and the dotfiles picker read one source). Each entry is `KEY=VALUE`.
   */
  async setUserVars(windowId: number, vars: Record<string, string>, opts?: ExecOptions): Promise<void> {
    const kvs = Object.entries(vars).map(([k, v]) => `${k}=${v}`);
    if (kvs.length === 0) return;
    await this.runAt(["set-user-vars", "--match", matchId(windowId), ...kvs], opts);
  }
}
