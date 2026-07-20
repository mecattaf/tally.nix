// tally — the Strategy-2 CLASSIFIER (IMPLEMENTATION-PLAN M2.3; CLI-SURFACE §3.3).
//
// Given a manifest and a way to read each region's text, walk the priority-ordered rules and produce
// the classified state + the matched rule (for `agent.explain`). Two inherited herdr laws are honored
// structurally: (1) rules match INVARIANT visible controls via explicit AND/OR predicate gates (the
// `Predicate` AST) — never incidental whole-pane prose; (2) the region layer already scopes to the
// bottom-buffer / OSC regions, never the user-scrollable viewport.
//
// The classifier produces the INTERNAL status (`InternalAgentStatus`, incl. the transient `unknown`
// that never reaches the wire). The loop collapses `unknown` to last-known / `idle` (CLI-SURFACE §0)
// — this module never emits; it only decides.

import type { InternalAgentStatus } from "../contracts/agent";
import type { Manifest, Predicate, Rule } from "./manifest";
import { isOscRegion } from "./manifest";
import { extractGridRegion } from "./regions";
import { extractOscRegion } from "./osc";
import type { KittyWindow } from "../kitty/rc";

/**
 * Evaluate one predicate against a region's text. `contains` is a literal substring; `regex` matches
 * the whole region text; `line_regex` matches if ANY line matches; `any`/`all`/`not` compose. Regexes
 * are compiled per-call from the (already-validated) source; the multiline flag lets `^`/`$` anchor
 * per-line for `line_regex` (each line is tested individually, so `line_regex` needs no `m` flag).
 */
export function evalPredicate(pred: Predicate, text: string): boolean {
  if ("contains" in pred) {
    return text.includes(pred.contains);
  }
  if ("regex" in pred) {
    return new RegExp(pred.regex).test(text);
  }
  if ("line_regex" in pred) {
    const re = new RegExp(pred.line_regex);
    return text.split("\n").some((line) => re.test(line));
  }
  if ("any" in pred) {
    return pred.any.some((p) => evalPredicate(p, text));
  }
  if ("all" in pred) {
    return pred.all.every((p) => evalPredicate(p, text));
  }
  // "not"
  return !evalPredicate(pred.not, text);
}

/** A reader for one manifest region — returns the region's current text (grid or OSC). */
export type RegionTextReader = (rule: Rule) => string;

/**
 * Build a `RegionTextReader` over one grid read + the window's `@ ls` record. Grid regions slice the
 * `gridText`; OSC regions project the window's title/progress. This is the in-process region split —
 * ONE `get-text` read feeds every grid rule; OSC rules never touch it.
 */
export function regionReader(gridText: string, window: KittyWindow): RegionTextReader {
  const cache = new Map<string, string>();
  return (rule: Rule): string => {
    const key = rule.region;
    const hit = cache.get(key);
    if (hit !== undefined) return hit;
    const text = isOscRegion(rule.region)
      ? extractOscRegion(rule.region, window)
      : extractGridRegion(rule.region, gridText);
    cache.set(key, text);
    return text;
  };
}

/** The classification outcome: the decided status + the winning rule (or null if none matched). */
export interface Classification {
  /** The internal status — `unknown` when no rule matched (collapses to last-known at the loop). */
  status: InternalAgentStatus;
  /** The rule that decided the status, or null when nothing matched. */
  matchedRule: Rule | null;
}

/**
 * Classify one pane from its grid + `@ ls` record against a manifest. Walks rules in descending
 * priority (the manifest is pre-sorted) and returns the FIRST rule whose predicate matches, unless
 * that rule carries `skip_state_update` — in which case its match is noted but the walk continues to
 * the next rule for the actual state (herdr's `skip_state_update` flag semantics). When no
 * state-setting rule matches, the status is `unknown`.
 */
export function classify(manifest: Manifest, gridText: string, window: KittyWindow): Classification {
  const read = regionReader(gridText, window);
  let skippedMatch: Rule | null = null;
  for (const rule of manifest.rules) {
    const text = read(rule);
    if (!evalPredicate(rule.predicate, text)) continue;
    if (rule.skip_state_update) {
      // Records evidence but does not set state — remember the first such match for explain, continue.
      if (skippedMatch === null) skippedMatch = rule;
      continue;
    }
    return { status: rule.state, matchedRule: rule };
  }
  // No state-setting rule matched. Surface a skip-only match (if any) as the explain evidence.
  return { status: "unknown", matchedRule: skippedMatch };
}

/**
 * Whether the classifier ONLY needs OSC regions (the zero-latency fast path can decide the state
 * without a grid read). True when the highest-priority rule that could set a state is OSC-scoped AND
 * every rule of >= its priority is OSC-scoped — i.e. no grid rule can outrank an OSC decision.
 *
 * The loop uses this to try the OSC fast path FIRST (M2.3): classify against `@ ls` alone, and only
 * fall through to the throttled grid read when the OSC pass is inconclusive.
 */
export function classifyOscFastPath(manifest: Manifest, window: KittyWindow): Classification {
  // Read only OSC regions; treat every grid region as absent (empty) so grid rules cannot match here.
  const read: RegionTextReader = (rule: Rule): string =>
    isOscRegion(rule.region) ? extractOscRegion(rule.region, window) : "";
  for (const rule of manifest.rules) {
    if (rule.skip_state_update) continue;
    if (!isOscRegion(rule.region)) {
      // A grid rule of this priority could still change the answer — the fast path is only
      // authoritative if no equal-or-higher-priority grid rule exists. Since rules are sorted
      // by descending priority, encountering a grid rule before any OSC match means the fast
      // path is inconclusive (a grid read is needed to rank correctly).
      return { status: "unknown", matchedRule: null };
    }
    const text = read(rule);
    if (evalPredicate(rule.predicate, text)) {
      return { status: rule.state, matchedRule: rule };
    }
  }
  return { status: "unknown", matchedRule: null };
}
