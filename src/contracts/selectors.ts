// tally — the three never-conflated keys, the pane composite id, and the selector grammar
// (CLI-SURFACE §0, §1.3 `<sel>`; IMPLEMENTATION-PLAN §3 Keys). The CLI's one resolver parses these
// shapes; this file owns the encoding/parse so every module agrees on the grammar.

/**
 * The three keys, never conflated (CLI-SURFACE §0). Encoded as branded string aliases so a call
 * site that mixes them up is a review smell, not a silent bug.
 */

/** The zmx session handle — a dotfiles timestamp name like `term-0707-1530`. The `session` leg. */
export type PersistenceSessionId = string;

/** The kitty-native binding; the focus/tunnel-in key. The `pane` leg. */
export type KittyWindowId = number;

/** The harness JSONL id (`pi`/`cc` `--resume`, content-plane join). A DIFFERENT key from the zmx id. */
export type SessionRef = string;

/** A composite pane id `"<session>:<pane>"` (CLI-SURFACE §0). */
export type PaneId = string;

/** Build a pane composite id from its session and pane parts. */
export function makePaneId(session: string, pane: string): PaneId {
  return `${session}:${pane}`;
}

/**
 * Parse a composite pane id back into its parts. Returns null if the shape is not
 * `"<session>:<pane>"` (exactly one colon separating two non-empty parts is the canonical form; a
 * session name itself never contains a colon under the dotfiles `term-MMDD-HHMMSS` scheme).
 */
export function parsePaneId(id: string): { session: string; pane: string } | null {
  const idx = id.indexOf(":");
  if (idx <= 0 || idx >= id.length - 1) return null;
  const session = id.slice(0, idx);
  const pane = id.slice(idx + 1);
  if (pane.includes(":")) return null;
  return { session, pane };
}

/** The kinds a `<sel>` can resolve to (CLI-SURFACE §1.3). */
export type SelectorKind = "pane" | "agent" | "session";

/**
 * A parsed selector (CLI-SURFACE §1.3 / §1.4): a `<session>:<pane>` composite, an `agent_id`, or a
 * bare `pane`/`session` token. Resolution to a concrete pane is the CLI resolver's job; this is the
 * syntactic classification only.
 */
export type Selector =
  | { kind: "pane"; session: string; pane: string; raw: string }
  | { kind: "agent"; agent_id: string; raw: string }
  | { kind: "bare"; token: string; raw: string };

/** The prefix that marks an agent-id selector token (agent ids are minted as `ag_<hex>`). */
export const AGENT_ID_PREFIX = "ag_" as const;

/**
 * Classify a raw selector token syntactically (CLI-SURFACE §1.3). A `<session>:<pane>` composite
 * parses to `pane`; an `ag_`-prefixed token to `agent`; anything else is a `bare` token the CLI
 * resolver disambiguates against the live model (a session name or a pane short-name).
 */
export function parseSelector(raw: string): Selector {
  const trimmed = raw.trim();
  if (trimmed.startsWith(AGENT_ID_PREFIX)) {
    return { kind: "agent", agent_id: trimmed, raw };
  }
  const parts = parsePaneId(trimmed);
  if (parts) {
    return { kind: "pane", session: parts.session, pane: parts.pane, raw };
  }
  return { kind: "bare", token: trimmed, raw };
}
