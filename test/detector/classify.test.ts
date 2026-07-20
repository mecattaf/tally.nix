// tally — detector classifier + region tests (IMPLEMENTATION-PLAN M2.3: fixture-grid classification
// per state for claude-code and pi, OSC fast path, predicate evaluation, region extraction).
//
// Drives the two bundled manifests against fixture grids and hand-authored per-state frames, asserts
// each of the four wire states is reached, and asserts the OSC fast path decides from `@ ls` alone
// when no higher-priority grid rule could outrank it. No vendor/ fixtures (clean-room, CLI-SURFACE §4).

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { parseManifest, type Manifest } from "../../src/detector/manifest.ts";
import { classify, classifyOscFastPath, evalPredicate, regionReader } from "../../src/detector/classify.ts";
import {
  afterLastHorizontalRule,
  promptBoxBody,
  bottomNonEmptyLines,
  extractGridRegion,
} from "../../src/detector/regions.ts";
import { oscTitle, oscProgress } from "../../src/detector/osc.ts";
import type { KittyWindow } from "../../src/kitty/rc.ts";

const REPO = join(import.meta.dir, "..", "..");
const CC: Manifest = parseManifest(readFileSync(join(REPO, "manifests", "claude-code.toml"), "utf8"));
const PI: Manifest = parseManifest(readFileSync(join(REPO, "manifests", "pi.toml"), "utf8"));

function win(over: Partial<KittyWindow> = {}): KittyWindow {
  return {
    id: 1,
    is_focused: false,
    is_active: false,
    title: "",
    cwd: "",
    foreground_processes: [],
    user_vars: {},
    tab_id: 0,
    os_window_id: 0,
    ...over,
  };
}

describe("predicate evaluation", () => {
  test("contains / regex / line_regex / any / all / not", () => {
    expect(evalPredicate({ contains: "esc" }, "press esc to interrupt")).toBe(true);
    expect(evalPredicate({ contains: "zzz" }, "abc")).toBe(false);
    expect(evalPredicate({ regex: "^a.c$" }, "abc")).toBe(true);
    expect(evalPredicate({ line_regex: "^>\\s*$" }, "transcript\n> \nmore")).toBe(true);
    expect(evalPredicate({ any: [{ contains: "x" }, { contains: "y" }] }, "y")).toBe(true);
    expect(evalPredicate({ all: [{ contains: "x" }, { contains: "y" }] }, "y")).toBe(false);
    expect(evalPredicate({ not: { contains: "x" } }, "y")).toBe(true);
  });
});

describe("grid region extraction", () => {
  test("after_last_horizontal_rule keeps only the text below the last rule", () => {
    const text = "top\n────────────\nmiddle\n────────────\nfooter line\n";
    expect(afterLastHorizontalRule(text)).toBe("footer line");
  });

  test("bottom_non_empty_lines(N) counts non-empty lines from the bottom", () => {
    const text = "a\nb\n\nc\n\nd\n";
    expect(bottomNonEmptyLines(text, 2)).toBe("c\n\nd");
  });

  test("prompt_box_body extracts the bordered box interior", () => {
    const text = "transcript\n\n╭────╮\n│ Do you want to proceed? │\n│ ❯ 1. Yes │\n╰────╯\n";
    const body = promptBoxBody(text);
    expect(body).toContain("Do you want to proceed?");
    expect(body).toContain("1. Yes");
    expect(body).not.toContain("transcript");
  });

  test("extractGridRegion routes each grid name", () => {
    expect(extractGridRegion("whole_recent", "abc")).toBe("abc");
    expect(extractGridRegion("bottom_non_empty_lines(1)", "a\nb\n")).toBe("b");
  });
});

describe("OSC region extraction", () => {
  test("osc_title reads the foreground process title", () => {
    const w = win({ foreground_processes: [{ pid: 1, cwd: "", cmdline: ["claude"], title: "⠉ Manufacturing" }] });
    expect(oscTitle(w)).toBe("⠉ Manufacturing");
  });

  test("osc_progress reads kitty's reported progress state", () => {
    expect(oscProgress(win({ osc_progress: "1;40" }))).toBe("1;40");
    expect(oscProgress(win())).toBe("");
  });
});

describe("claude-code classification per state", () => {
  test("working (fixture grid)", () => {
    const grid = readFileSync(join(REPO, "test", "fixtures", "grids", "claude-code-working.txt"), "utf8");
    const c = classify(CC, grid, win());
    expect(c.status).toBe("working");
    expect(c.matchedRule?.id).toBe("cc-esc-to-interrupt");
  });

  test("blocked (permission box)", () => {
    const grid = "transcript\n\n╭─────╮\n│ Do you want to proceed? │\n│ ❯ 1. Yes │\n│   2. No │\n╰─────╯\n";
    const c = classify(CC, grid, win());
    expect(c.status).toBe("blocked");
    expect(c.matchedRule?.id).toBe("cc-permission-box");
  });

  test("done (settled turn footer)", () => {
    const grid = "output settled\n\n> \n  ⏵⏵ auto-accept edits on (shift+tab to cycle)\n";
    const c = classify(CC, grid, win());
    expect(c.status).toBe("done");
    expect(c.matchedRule?.id).toBe("cc-turn-settled");
  });

  test("idle (nothing agent-like)", () => {
    const c = classify(CC, "welcome to claude code\nnothing happening\n", win());
    expect(c.status).toBe("idle");
    expect(c.matchedRule?.id).toBe("cc-idle-prompt");
  });
});

describe("pi classification per state", () => {
  test("working (fixture grid)", () => {
    const grid = readFileSync(join(REPO, "test", "fixtures", "grids", "pi-working.txt"), "utf8");
    const c = classify(PI, grid, win());
    expect(c.status).toBe("working");
  });

  test("blocked (permission prompt)", () => {
    const grid = "transcript\n\n│ Allow this action? │\n│ ❯ 1. Yes │\n";
    expect(classify(PI, grid, win()).status).toBe("blocked");
  });

  test("done (settled input frame)", () => {
    const grid = "assistant done\n\n╭──────────╮\n│ >        │\n╰──────────╯\n";
    expect(classify(PI, grid, win()).status).toBe("done");
  });

  test("idle (no active turn)", () => {
    expect(classify(PI, "welcome to pi\nno turn active\n", win()).status).toBe("idle");
  });
});

describe("OSC fast path", () => {
  test("decides working from an OSC-only manifest without a grid read", () => {
    // A minimal OSC-only manifest: the spinner rule is the highest priority ⇒ fast-path authoritative.
    const oscOnly = parseManifest(
      [
        'kind = "claude-code"',
        'version = "1"',
        "[[rules]]",
        'id = "spin"',
        'state = "working"',
        "priority = 80",
        'region = "osc_title"',
        'regex = "[\\\\u2800-\\\\u28ff]"',
        "[[rules]]",
        'id = "idle"',
        'state = "idle"',
        "priority = 5",
        'region = "osc_title"',
        'not = { regex = "[\\\\u2800-\\\\u28ff]" }',
      ].join("\n"),
    );
    const spinning = win({ foreground_processes: [{ pid: 1, cwd: "", cmdline: ["claude"], title: "⠉ Working" }] });
    const c = classifyOscFastPath(oscOnly, spinning);
    expect(c.status).toBe("working");
    expect(c.matchedRule?.id).toBe("spin");
  });

  test("bails (inconclusive) when a higher-priority grid rule exists", () => {
    // claude-code.toml: the blocked box (grid, priority 100) outranks the OSC spinner (80), so the
    // OSC fast path cannot decide alone — a grid read is required (correct, conservative behavior).
    const spinning = win({ foreground_processes: [{ pid: 1, cwd: "", cmdline: ["claude"], title: "⠉ Manufacturing" }] });
    const c = classifyOscFastPath(CC, spinning);
    expect(c.status).toBe("unknown");
  });
});

describe("region reader shares one grid read", () => {
  test("caches grid + OSC region text per rule", () => {
    const reader = regionReader("a\nb\n", win({ foreground_processes: [{ pid: 1, cwd: "", cmdline: ["x"], title: "T" }] }));
    const gridRule = CC.rules.find((r) => r.region === "whole_recent")!;
    const oscRule = CC.rules.find((r) => r.region === "osc_title")!;
    expect(reader(gridRule)).toBe("a\nb\n");
    expect(reader(oscRule)).toBe("T");
  });
});
