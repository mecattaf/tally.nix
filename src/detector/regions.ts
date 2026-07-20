// tally — GRID region extraction (IMPLEMENTATION-PLAN M2.3; CLI-SURFACE §3.3, deep-pass A1).
//
// The grid regions scope a `kitty @ get-text` extract into the sub-slice a rule matches against.
// This is the "never key off the user-scrollable viewport" law made concrete: every region here is
// a BOTTOM-buffer slice of the emulated grid text — the live-region prose kitty already recomposed.
// The OSC regions (`osc_title`/`osc_progress`) are NOT here — they bind to `kitty @ ls` (see osc.ts).
//
// Regions operate on the plain grid text a single `get-text` read produced (revision-stamped by the
// shared ReadThrottle). Splitting into named regions in-process — rather than issuing a separate
// `get-text` per region — keeps the read budget to ONE poll per window per cadence tick (M1.6).

import { bottomLinesN, type GridRegionName } from "./manifest";

/** A horizontal-rule line: a run of box-drawing horizontals (─, U+2500) or ASCII dashes. */
const HR_LINE_RE = /^[\s]*[─━┄┅┈┉\-_=]{6,}[\s]*$/;

/** Split grid text into lines, dropping a single trailing empty line from a trailing newline. */
function toLines(text: string): string[] {
  const lines = text.split("\n");
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

/**
 * `whole_recent` — the whole recent grid text. tally reads only the on-screen / bottom-buffer extent
 * (never the scrollback the operator scrolled up into), so "recent" == the text the `get-text` read
 * returned. Used by the idle catch-all rule (the universal fallback).
 */
export function wholeRecent(text: string): string {
  return text;
}

/**
 * `after_last_horizontal_rule` — the text AFTER the last horizontal-rule line. Harnesses draw a rule
 * between the transcript and the live footer/prompt; the region below it is the current turn's live
 * region. Returns the whole text if no rule is present.
 */
export function afterLastHorizontalRule(text: string): string {
  const lines = toLines(text);
  let lastHr = -1;
  for (let i = 0; i < lines.length; i++) {
    if (HR_LINE_RE.test(lines[i]!)) lastHr = i;
  }
  if (lastHr === -1) return text;
  return lines.slice(lastHr + 1).join("\n");
}

/**
 * `prompt_box_body` — the interior of the bordered prompt/permission box a harness draws at the
 * bottom (Claude Code's "Do you want to proceed?" box; pi's rounded input frame). Extracts the run of
 * lines bounded by box-drawing corner/edge glyphs. When no box border is present it falls back to the
 * bottom slice, so a `contains` predicate still has the live footer to match on.
 */
export function promptBoxBody(text: string): string {
  const lines = toLines(text);
  // Box-drawing top/bottom borders: corners ╭╮╰╯┌┐└┘ or a border line of ─/│.
  const isBorder = (l: string): boolean =>
    /[╭╮╯╰┌┐└┘]/.test(l) ||
    /^[\s]*[│┃|][\s─━]*[│┃|]?[\s]*$/.test(l);
  // Find the LAST box region (closest to the bottom): scan for a trailing block of border-framed lines.
  let end = -1;
  for (let i = lines.length - 1; i >= 0; i--) {
    if (isBorder(lines[i]!)) {
      end = i;
      break;
    }
  }
  if (end === -1) {
    // No box border — fall back to the bottom non-empty slice so `contains` still has live text.
    return bottomNonEmptyLines(text, 12);
  }
  let start = end;
  for (let i = end; i >= 0; i--) {
    const l = lines[i]!;
    // A box body line starts with a vertical edge or is another border; stop at the first
    // line that is neither (the transcript above the box).
    if (/[│┃|]/.test(l) || isBorder(l)) {
      start = i;
    } else if (i < end) {
      break;
    }
  }
  return lines.slice(start, end + 1).join("\n");
}

/**
 * `bottom_non_empty_lines(N)` — the last N NON-EMPTY lines of the grid (blank lines skipped, not
 * counted). This is the canonical live-footer region: the spinner/affordance lines sit here. Blank
 * lines between kept lines are preserved in output; only leading/trailing counting skips blanks.
 */
export function bottomNonEmptyLines(text: string, n: number): string {
  const lines = toLines(text);
  const kept: string[] = [];
  let count = 0;
  for (let i = lines.length - 1; i >= 0 && count < n; i--) {
    const l = lines[i]!;
    kept.push(l);
    if (l.trim().length > 0) count += 1;
  }
  kept.reverse();
  // Trim leading blanks so the region begins at the first non-empty line within the window.
  while (kept.length > 0 && kept[0]!.trim().length === 0) kept.shift();
  return kept.join("\n");
}

/**
 * Extract a named GRID region's text from one grid read. Throws for a name that is not a grid region
 * (OSC regions must be routed through `osc.ts`, never here — the manifest parser already enforces the
 * split, so this is defence-in-depth).
 */
export function extractGridRegion(region: GridRegionName, gridText: string): string {
  if (region === "whole_recent") return wholeRecent(gridText);
  if (region === "after_last_horizontal_rule") return afterLastHorizontalRule(gridText);
  if (region === "prompt_box_body") return promptBoxBody(gridText);
  const n = bottomLinesN(region);
  if (n !== null) return bottomNonEmptyLines(gridText, n);
  // Unreachable given the manifest parser's region validation.
  throw new Error(`extractGridRegion: not a grid region: ${region}`);
}
