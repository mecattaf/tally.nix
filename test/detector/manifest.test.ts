// tally — detector manifest parser tests (IMPLEMENTATION-PLAN M2.3: manifest parsing + rule precedence).
//
// Asserts the clean-room herdr-FORMAT TOML manifest parses into the typed Rule/Manifest shapes, that
// rules sort by descending priority (highest wins), that the predicate AST (contains/regex/line_regex/
// any/all/not) validates hand-rolled (no zod), and that the two bundled manifests (claude-code, pi)
// load and carry the four target states. No vendor/ fixtures (clean-room, CLI-SURFACE §4).

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import {
  parseManifest,
  buildManifest,
  parsePredicate,
  assertRegionName,
  isOscRegion,
  isGridRegion,
  bottomLinesN,
} from "../../src/detector/manifest.ts";
import { ValidationError } from "../../src/contracts/errors.ts";

const REPO = join(import.meta.dir, "..", "..");

describe("manifest region vocabulary", () => {
  test("classifies grid vs OSC regions", () => {
    expect(isOscRegion("osc_title")).toBe(true);
    expect(isOscRegion("osc_progress")).toBe(true);
    expect(isOscRegion("whole_recent")).toBe(false);
    expect(isGridRegion("whole_recent")).toBe(true);
    expect(isGridRegion("after_last_horizontal_rule")).toBe(true);
    expect(isGridRegion("prompt_box_body")).toBe(true);
    expect(isGridRegion("bottom_non_empty_lines(6)")).toBe(true);
    expect(isGridRegion("osc_title")).toBe(false);
  });

  test("parses bottom_non_empty_lines(N)", () => {
    expect(bottomLinesN("bottom_non_empty_lines(12)")).toBe(12);
    expect(bottomLinesN("whole_recent")).toBeNull();
  });

  test("rejects an unknown region name", () => {
    expect(() => assertRegionName("nonsense", "r.region")).toThrow(ValidationError);
  });
});

describe("predicate parsing (hand-rolled, no zod)", () => {
  test("parses each leaf + composite operator", () => {
    expect(parsePredicate({ contains: "x" }, "p")).toEqual({ contains: "x" });
    expect(parsePredicate({ regex: "^a" }, "p")).toEqual({ regex: "^a" });
    expect(parsePredicate({ line_regex: "b$" }, "p")).toEqual({ line_regex: "b$" });
    expect(parsePredicate({ any: [{ contains: "a" }] }, "p")).toEqual({ any: [{ contains: "a" }] });
    expect(parsePredicate({ all: [{ contains: "a" }] }, "p")).toEqual({ all: [{ contains: "a" }] });
    expect(parsePredicate({ not: { contains: "a" } }, "p")).toEqual({ not: { contains: "a" } });
  });

  test("rejects a predicate with zero operators", () => {
    expect(() => parsePredicate({}, "p")).toThrow(ValidationError);
  });

  test("rejects a predicate carrying multiple operators", () => {
    expect(() => parsePredicate({ contains: "a", regex: "b" }, "p")).toThrow(ValidationError);
  });

  test("rejects an invalid regex", () => {
    expect(() => parsePredicate({ regex: "(" }, "p")).toThrow(ValidationError);
  });

  test("rejects a non-string contains", () => {
    expect(() => parsePredicate({ contains: 3 }, "p")).toThrow(ValidationError);
  });
});

describe("manifest build + validation", () => {
  test("sorts rules by descending priority (ties keep declaration order)", () => {
    const m = buildManifest({
      kind: "claude-code",
      version: "1",
      rules: [
        { id: "low", state: "idle", priority: 5, region: "whole_recent", contains: "x" },
        { id: "hi", state: "working", priority: 80, region: "whole_recent", contains: "y" },
        { id: "mid-a", state: "done", priority: 30, region: "whole_recent", contains: "a" },
        { id: "mid-b", state: "blocked", priority: 30, region: "whole_recent", contains: "b" },
      ],
    });
    expect(m.rules.map((r) => r.id)).toEqual(["hi", "mid-a", "mid-b", "low"]);
  });

  test("AND-combines multiple top-level predicates on one rule", () => {
    const m = buildManifest({
      kind: "pi",
      version: "1",
      rules: [{ id: "r", state: "working", priority: 1, region: "whole_recent", contains: "a", regex: "b" }],
    });
    expect(m.rules[0]!.predicate).toEqual({ all: [{ contains: "a" }, { regex: "b" }] });
  });

  test("rejects a bad state / non-integer priority / missing predicate / duplicate id", () => {
    const base = { kind: "pi", version: "1" };
    expect(() => buildManifest({ ...base, rules: [{ id: "r", state: "spinning", priority: 1, region: "whole_recent", contains: "a" }] })).toThrow(ValidationError);
    expect(() => buildManifest({ ...base, rules: [{ id: "r", state: "idle", priority: 1.5, region: "whole_recent", contains: "a" }] })).toThrow(ValidationError);
    expect(() => buildManifest({ ...base, rules: [{ id: "r", state: "idle", priority: 1, region: "whole_recent" }] })).toThrow(ValidationError);
    expect(() => buildManifest({ ...base, rules: [
      { id: "dup", state: "idle", priority: 1, region: "whole_recent", contains: "a" },
      { id: "dup", state: "idle", priority: 2, region: "whole_recent", contains: "b" },
    ] })).toThrow(ValidationError);
  });

  test("rejects a manifest with no rules / bad kind", () => {
    expect(() => buildManifest({ kind: "pi", version: "1", rules: [] })).toThrow(ValidationError);
    expect(() => buildManifest({ kind: "bogus", version: "1", rules: [{ id: "r", state: "idle", priority: 1, region: "whole_recent", contains: "a" }] })).toThrow(ValidationError);
  });

  test("normalizes a numeric version to a string", () => {
    const m = buildManifest({ kind: "pi", version: 2, rules: [{ id: "r", state: "idle", priority: 1, region: "whole_recent", contains: "a" }] });
    expect(m.version).toBe("2");
  });

  test("rejects non-TOML garbage", () => {
    expect(() => parseManifest("this is = not [valid")).toThrow(ValidationError);
  });
});

describe("the two bundled manifests parse", () => {
  test("claude-code.toml", () => {
    const m = parseManifest(readFileSync(join(REPO, "manifests", "claude-code.toml"), "utf8"));
    expect(m.kind).toBe("claude-code");
    const states = new Set(m.rules.map((r) => r.state));
    expect(states).toEqual(new Set(["blocked", "working", "done", "idle"]));
    // Priority ordering: blocked box outranks the working spinner outranks done outranks idle.
    const byId = new Map(m.rules.map((r) => [r.id, r.priority]));
    expect(byId.get("cc-permission-box")!).toBeGreaterThan(byId.get("cc-esc-to-interrupt")!);
    expect(byId.get("cc-esc-to-interrupt")!).toBeGreaterThan(byId.get("cc-turn-settled")!);
    expect(byId.get("cc-turn-settled")!).toBeGreaterThan(byId.get("cc-idle-prompt")!);
  });

  test("pi.toml", () => {
    const m = parseManifest(readFileSync(join(REPO, "manifests", "pi.toml"), "utf8"));
    expect(m.kind).toBe("pi");
    const states = new Set(m.rules.map((r) => r.state));
    expect(states).toEqual(new Set(["blocked", "working", "done", "idle"]));
  });
});
