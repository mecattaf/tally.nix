// tally — OSC region binding (IMPLEMENTATION-PLAN M2.3; CLI-SURFACE §3.3, deep-pass A1 correction).
//
// The OSC regions bind to `kitty @ ls` — the `foreground_processes[].title` field (the terminal/OS
// title an agent writes, e.g. Claude Code's braille-spinner glyph) and kitty's reported OSC 9;4
// progress escape state — NEVER `kitty @ get-text`. They are the zero-latency Strategy-2 fast path:
// checked BEFORE the grid read (M2.3), riding a verb family (`@ ls`) tally already polls, so no extra
// read budget is spent and no format/protocol change is needed.
//
// A KittyWindow (from sensors/rc.ts `parseLsTree`) already carries these fields; this module only
// projects them into the region text a manifest rule matches against.

import type { KittyWindow } from "../kitty/rc";
import type { OscRegionName } from "./manifest";

/**
 * The OSC region text extracted from a window's `@ ls` record:
 * - `osc_title`  → the foreground process's OSC title (the LAST foreground process's title, the one
 *   in the foreground of the pane); empty string when no title is reported.
 * - `osc_progress` → kitty's reported OSC 9;4 progress escape state; empty when kitty reports none.
 *
 * The manifest split guarantees only these two names route here (regions.ts owns the grid names).
 */
export function extractOscRegion(region: OscRegionName, window: KittyWindow): string {
  if (region === "osc_title") return oscTitle(window);
  return oscProgress(window);
}

/**
 * The `osc_title` region text: the OSC title of the pane's foreground process. Uses the LAST
 * foreground process (the one actually in the foreground of the pty), falling back to the window
 * title. This is where Claude Code's braille spinner glyph lands.
 */
export function oscTitle(window: KittyWindow): string {
  const fps = window.foreground_processes;
  for (let i = fps.length - 1; i >= 0; i--) {
    const t = fps[i]!.title;
    if (typeof t === "string" && t.length > 0) return t;
  }
  return window.title ?? "";
}

/**
 * The `osc_progress` region text: kitty's reported OSC 9;4 progress escape state, or empty string. An
 * OSC 9;4 sequence is `state;percent` where state ∈ {0=clear, 1=set, 2=error, 3=indeterminate,
 * 4=paused}; the manifest rule matches the raw reported string (e.g. `"1;40"`).
 */
export function oscProgress(window: KittyWindow): string {
  return window.osc_progress ?? "";
}
