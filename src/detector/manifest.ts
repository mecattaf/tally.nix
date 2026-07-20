// tally — the Strategy-2 scrape MANIFEST parser (IMPLEMENTATION-PLAN M2.3; CLI-SURFACE §3.3, §4).
//
// The clean-room herdr *format* reference (zero code lifted, CLI-SURFACE §4): a per-harness TOML
// state-manifest of `[[rules]]`, each with an `id`, a target four-state `state`, an integer
// `priority` (highest wins), a named `region`, match predicates (`contains`/`regex`/`line_regex`/
// `any`/`all`/`not`), and the `visible_*`/`skip_state_update` flags. This module parses the TOML
// (via smol-toml) and HAND-VALIDATES it into the typed `Manifest`/`Rule` shapes the classifier and
// region extractor consume — no zod, per the plan's hand-rolled-validation rule.
//
// Region split by mechanism (CLI-SURFACE §3.3, deep-pass A1): the GRID regions scope a
// `kitty @ get-text` read; the OSC regions bind to `kitty @ ls foreground_processes[].title` + OSC
// progress escapes — see `regions.ts` / `osc.ts`. This file only classifies a region NAME as
// grid-vs-osc so the loop knows which read mechanism a rule needs.

import { parse as parseToml } from "smol-toml";
import { ValidationError } from "../contracts/errors";
import type { AgentStatus, AgentKind } from "../contracts/agent";
import { AGENT_STATUSES } from "../contracts/agent";

// ---------------------------------------------------------------------------------------------
// Region vocabulary (CLI-SURFACE §3.3 herdr region reference, split by mechanism).
// ---------------------------------------------------------------------------------------------

/** The GRID regions — scoped via a `kitty @ get-text` extract (`regions.ts`). */
export type GridRegionName =
  | "whole_recent"
  | "after_last_horizontal_rule"
  | "prompt_box_body"
  | `bottom_non_empty_lines(${number})`;

/** The OSC regions — bound to `kitty @ ls` title + OSC progress escapes (`osc.ts`), never get-text. */
export type OscRegionName = "osc_title" | "osc_progress";

/** A region name is either grid or OSC — the parser rejects anything else. */
export type RegionName = GridRegionName | OscRegionName;

/** The two OSC region names, the golden set. */
export const OSC_REGIONS = ["osc_title", "osc_progress"] as const satisfies readonly OscRegionName[];

/** The fixed grid region names (excluding the parameterized `bottom_non_empty_lines(N)`). */
export const FIXED_GRID_REGIONS = [
  "whole_recent",
  "after_last_horizontal_rule",
  "prompt_box_body",
] as const;

const OSC_REGION_SET: ReadonlySet<string> = new Set(OSC_REGIONS);
const FIXED_GRID_REGION_SET: ReadonlySet<string> = new Set(FIXED_GRID_REGIONS);

/** The `bottom_non_empty_lines(N)` shape — captures the integer N. */
const BOTTOM_LINES_RE = /^bottom_non_empty_lines\((\d+)\)$/;

/** True if a region name is an OSC region (bound to `kitty @ ls`, never `get-text`). */
export function isOscRegion(region: string): region is OscRegionName {
  return OSC_REGION_SET.has(region);
}

/** True if a region name is a grid region (scoped via `kitty @ get-text`). */
export function isGridRegion(region: string): region is GridRegionName {
  return FIXED_GRID_REGION_SET.has(region) || BOTTOM_LINES_RE.test(region);
}

/** Parse the `N` out of `bottom_non_empty_lines(N)`, or null if the name is not that shape. */
export function bottomLinesN(region: string): number | null {
  const m = BOTTOM_LINES_RE.exec(region);
  return m ? Number(m[1]) : null;
}

/** Validate that a region name is one of the recognized grid/OSC regions. */
export function assertRegionName(region: string, path: string): RegionName {
  if (isOscRegion(region) || isGridRegion(region)) return region as RegionName;
  throw new ValidationError(
    `unknown region "${region}" (grid: whole_recent|after_last_horizontal_rule|prompt_box_body|` +
      `bottom_non_empty_lines(N); osc: osc_title|osc_progress)`,
    path,
  );
}

// ---------------------------------------------------------------------------------------------
// Predicate AST (CLI-SURFACE §3.3 predicate set: contains / regex / line_regex / any / all / not).
// ---------------------------------------------------------------------------------------------

/** A leaf substring predicate: the region text must contain this exact substring. */
export interface ContainsPredicate {
  contains: string;
}
/** A leaf regex predicate: the region text (whole) must match this regex. */
export interface RegexPredicate {
  regex: string;
}
/** A leaf line-regex predicate: at least one LINE of the region text must match this regex. */
export interface LineRegexPredicate {
  line_regex: string;
}
/** A boolean OR over sub-predicates — matches if ANY child matches. */
export interface AnyPredicate {
  any: Predicate[];
}
/** A boolean AND over sub-predicates — matches only if ALL children match. */
export interface AllPredicate {
  all: Predicate[];
}
/** A boolean NOT of one sub-predicate. */
export interface NotPredicate {
  not: Predicate;
}

/** The predicate union — the AST the classifier evaluates against a region's text. */
export type Predicate =
  | ContainsPredicate
  | RegexPredicate
  | LineRegexPredicate
  | AnyPredicate
  | AllPredicate
  | NotPredicate;

/** One scrape rule (CLI-SURFACE §3.3): a region + predicate → a target four-state status. */
export interface Rule {
  id: string;
  state: AgentStatus;
  priority: number;
  region: RegionName;
  /** The rule's match predicate (a top-level rule carries exactly one — see `combineRulePredicate`). */
  predicate: Predicate;
  /** herdr flags — retained for `agent.explain` + the two inherited laws (documentation of intent). */
  visible_working?: boolean;
  visible_blocker?: boolean;
  visible_idle?: boolean;
  /** When set, a rule that matches records its evidence but does NOT change the state (herdr flag). */
  skip_state_update?: boolean;
}

/** A parsed manifest for one harness kind. */
export interface Manifest {
  /** The `agent.kind` this manifest classifies (`claude-code` | `pi` | ...). */
  kind: AgentKind;
  /** The manifest version string (surfaced via `agent.explain` as the manifest source/version). */
  version: string;
  /** Rules, sorted by descending `priority` (highest wins) — the classifier walks them in order. */
  rules: Rule[];
}

// ---------------------------------------------------------------------------------------------
// Hand-rolled validation (no zod — plan rule). Every field checked; unknown keys ignored.
// ---------------------------------------------------------------------------------------------

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

const AGENT_KIND_SET: ReadonlySet<string> = new Set(["pi", "claude-code", "shell"]);
const STATUS_SET: ReadonlySet<string> = new Set(AGENT_STATUSES);

/** The predicate keys a single predicate object may carry. Exactly one operator per object. */
const PREDICATE_KEYS = ["contains", "regex", "line_regex", "any", "all", "not"] as const;

function reqString(v: unknown, path: string): string {
  if (typeof v !== "string") throw new ValidationError(`${path} must be a string`, path);
  return v;
}

/** Parse one predicate object into the typed AST, validating it carries exactly one operator. */
export function parsePredicate(v: unknown, path: string): Predicate {
  if (!isObj(v)) throw new ValidationError(`${path} must be a table`, path);
  const present = PREDICATE_KEYS.filter((k) => v[k] !== undefined);
  if (present.length === 0) {
    throw new ValidationError(
      `${path} must carry exactly one predicate operator (${PREDICATE_KEYS.join("|")})`,
      path,
    );
  }
  if (present.length > 1) {
    throw new ValidationError(
      `${path} carries multiple predicate operators (${present.join(", ")}); wrap them in "all"/"any"`,
      path,
    );
  }
  const key = present[0]!;
  switch (key) {
    case "contains":
      return { contains: reqString(v.contains, `${path}.contains`) };
    case "regex": {
      const src = reqString(v.regex, `${path}.regex`);
      assertValidRegex(src, `${path}.regex`);
      return { regex: src };
    }
    case "line_regex": {
      const src = reqString(v.line_regex, `${path}.line_regex`);
      assertValidRegex(src, `${path}.line_regex`);
      return { line_regex: src };
    }
    case "any":
      return { any: parsePredicateArray(v.any, `${path}.any`) };
    case "all":
      return { all: parsePredicateArray(v.all, `${path}.all`) };
    case "not":
      return { not: parsePredicate(v.not, `${path}.not`) };
  }
}

function parsePredicateArray(v: unknown, path: string): Predicate[] {
  if (!Array.isArray(v)) throw new ValidationError(`${path} must be an array of predicates`, path);
  if (v.length === 0) throw new ValidationError(`${path} must not be empty`, path);
  return v.map((p, i) => parsePredicate(p, `${path}[${i}]`));
}

function assertValidRegex(src: string, path: string): void {
  try {
    // eslint-disable-next-line no-new
    new RegExp(src);
  } catch (e) {
    throw new ValidationError(`${path} is not a valid regex: ${(e as Error).message}`, path);
  }
}

/**
 * Combine the predicate keys present DIRECTLY on a `[[rules]]` table into one predicate. A rule may
 * carry any predicate operators at its top level (herdr rules commonly write `contains = "..."`
 * beside `all = [...]`); they are AND-combined (all must hold), matching herdr's rule semantics where
 * every predicate on the rule is a conjunct.
 */
function combineRulePredicate(rule: Record<string, unknown>, path: string): Predicate {
  const parts: Predicate[] = [];
  for (const key of PREDICATE_KEYS) {
    if (rule[key] === undefined) continue;
    // Build a single-operator object so parsePredicate validates each in isolation.
    parts.push(parsePredicate({ [key]: rule[key] }, path));
  }
  if (parts.length === 0) {
    throw new ValidationError(
      `${path} carries no match predicate (need one of ${PREDICATE_KEYS.join("|")})`,
      path,
    );
  }
  return parts.length === 1 ? parts[0]! : { all: parts };
}

/** Parse one `[[rules]]` table into a typed `Rule`. */
function parseRule(v: unknown, path: string): Rule {
  if (!isObj(v)) throw new ValidationError(`${path} must be a table`, path);

  const id = reqString(v.id, `${path}.id`);
  const stateStr = reqString(v.state, `${path}.state`);
  if (!STATUS_SET.has(stateStr)) {
    throw new ValidationError(
      `${path}.state must be one of ${AGENT_STATUSES.join("|")} (got "${stateStr}")`,
      `${path}.state`,
    );
  }
  const state = stateStr as AgentStatus;

  if (typeof v.priority !== "number" || !Number.isInteger(v.priority)) {
    throw new ValidationError(`${path}.priority must be an integer`, `${path}.priority`);
  }
  const priority = v.priority;

  const region = assertRegionName(reqString(v.region, `${path}.region`), `${path}.region`);
  const predicate = combineRulePredicate(v, path);

  const rule: Rule = { id, state, priority, region, predicate };
  if (v.visible_working !== undefined) rule.visible_working = asBool(v.visible_working, `${path}.visible_working`);
  if (v.visible_blocker !== undefined) rule.visible_blocker = asBool(v.visible_blocker, `${path}.visible_blocker`);
  if (v.visible_idle !== undefined) rule.visible_idle = asBool(v.visible_idle, `${path}.visible_idle`);
  if (v.skip_state_update !== undefined) rule.skip_state_update = asBool(v.skip_state_update, `${path}.skip_state_update`);
  return rule;
}

function asBool(v: unknown, path: string): boolean {
  if (typeof v !== "boolean") throw new ValidationError(`${path} must be a boolean`, path);
  return v;
}

/**
 * Parse + validate a manifest from a parsed-TOML object. Rules are sorted by DESCENDING priority so
 * the classifier can take the first matching rule (highest priority wins; ties keep declaration
 * order, which is stable via a stable sort key).
 */
export function buildManifest(raw: unknown): Manifest {
  if (!isObj(raw)) throw new ValidationError("manifest must be a TOML table", "$");
  const kindStr = reqString(raw.kind, "kind");
  if (!AGENT_KIND_SET.has(kindStr)) {
    throw new ValidationError(`kind must be one of pi|claude-code|shell (got "${kindStr}")`, "kind");
  }
  const kind = kindStr as AgentKind;

  // `version` may be authored as a bare number or a string in TOML — normalize to string.
  const version =
    raw.version === undefined
      ? "0"
      : typeof raw.version === "number"
        ? String(raw.version)
        : reqString(raw.version, "version");

  const rulesRaw = raw.rules;
  if (!Array.isArray(rulesRaw)) throw new ValidationError("manifest must declare [[rules]]", "rules");
  if (rulesRaw.length === 0) throw new ValidationError("manifest must declare at least one rule", "rules");

  const rules = rulesRaw.map((r, i) => parseRule(r, `rules[${i}]`));

  // Reject duplicate rule ids so `agent.explain` never reports an ambiguous matched-rule id.
  const seen = new Set<string>();
  for (const r of rules) {
    if (seen.has(r.id)) throw new ValidationError(`duplicate rule id "${r.id}"`, "rules");
    seen.add(r.id);
  }

  // Stable descending-priority sort: annotate with original index, sort, strip.
  const sorted = rules
    .map((rule, index) => ({ rule, index }))
    .sort((a, b) => (b.rule.priority - a.rule.priority) || (a.index - b.index))
    .map((x) => x.rule);

  return { kind, version, rules: sorted };
}

/** Parse a manifest from its TOML source text. */
export function parseManifest(tomlText: string): Manifest {
  let parsed: unknown;
  try {
    parsed = parseToml(tomlText);
  } catch (e) {
    throw new ValidationError(`manifest TOML parse failed: ${(e as Error).message}`, "$");
  }
  return buildManifest(parsed);
}
