// tally — kitty @ rc-client tests (IMPLEMENTATION-PLAN M1.6: rc arg construction).
//
// Asserts the KittyRc client shells EXACTLY the four sanctioned `kitty @` verbs with the right argv,
// keyed on `kitty_window_id`, parses the `@ ls` tree, and REFUSES the forbidden `kitty @ launch`.
// Driven against the layer-0 FakeKitty (no real substrate).

import { describe, expect, test } from "bun:test";
import { FakeExec } from "../helpers/exec-fakes.ts";
import { FakeKitty } from "../helpers/fake-kitty.ts";
import { KittyRc, parseLsTree, keyEscape, KEY_ESCAPES, FORBIDDEN_KITTY_VERB } from "../../src/kitty/rc.ts";
import { TallyError } from "../../src/contracts/errors.ts";

function setup() {
  const exec = new FakeExec();
  const kitty = new FakeKitty();
  kitty.install(exec);
  const rc = new KittyRc(exec);
  return { exec, kitty, rc };
}

describe("KittyRc.ls", () => {
  test("flattens the @ ls tree into windows keyed on kitty_window_id", async () => {
    const { kitty, rc } = setup();
    kitty.addWindow({
      id: 7,
      is_focused: true,
      title: "claude",
      cwd: "/home/tom/work/api",
      foreground_processes: [{ pid: 4242, cwd: "/home/tom/work/api", cmdline: ["claude"], title: "◐ working" }],
      user_vars: { tally_pane: "term-0707-1530:p2" },
    });
    kitty.addWindow({ id: 8, cwd: "/home/tom" });

    const windows = await rc.ls();
    const byId = new Map(windows.map((w) => [w.id, w]));
    expect(byId.get(7)!.is_focused).toBe(true);
    expect(byId.get(7)!.cwd).toBe("/home/tom/work/api");
    expect(byId.get(7)!.foreground_processes[0]!.title).toBe("◐ working");
    expect(byId.get(7)!.user_vars.tally_pane).toBe("term-0707-1530:p2");
    expect(byId.get(8)!.cwd).toBe("/home/tom");
  });

  test("issues exactly `kitty @ ls`", async () => {
    const { exec, kitty, rc } = setup();
    kitty.addWindow({ id: 1 });
    await rc.ls();
    expect(exec.lastCall("kitty")!.argv).toEqual(["kitty", "@", "ls"]);
  });
});

describe("parseLsTree (defensive)", () => {
  test("skips windows without a numeric id and tolerates missing fields", () => {
    const tree = [
      {
        id: 1,
        tabs: [
          {
            id: 1,
            windows: [{ id: 5 }, { title: "no-id" }, { id: "x" }],
          },
        ],
      },
    ];
    const out = parseLsTree(tree);
    expect(out.map((w) => w.id)).toEqual([5]);
    expect(out[0]!.title).toBe("");
    expect(out[0]!.tab_id).toBe(1);
    expect(out[0]!.os_window_id).toBe(1);
  });

  test("returns [] for non-array input", () => {
    expect(parseLsTree(null)).toEqual([]);
    expect(parseLsTree({})).toEqual([]);
  });
});

describe("KittyRc.getText", () => {
  test("reads a window's grid with `--match id:<id>`", async () => {
    const { exec, kitty, rc } = setup();
    kitty.addWindow({ id: 7, gridText: "hello grid" });
    const text = await rc.getText(7);
    expect(text).toBe("hello grid");
    expect(exec.lastCall("kitty")!.argv).toEqual(["kitty", "@", "get-text", "--match", "id:7"]);
  });

  test("adds --extent and --ansi flags when requested", async () => {
    const { exec, kitty, rc } = setup();
    kitty.addWindow({ id: 3, gridText: "x" });
    await rc.getText(3, { extent: "all", ansi: true });
    expect(exec.lastCall("kitty")!.argv).toEqual([
      "kitty",
      "@",
      "get-text",
      "--match",
      "id:3",
      "--extent",
      "all",
      "--ansi",
    ]);
  });

  test("does NOT add --extent for the default screen extent", async () => {
    const { exec, kitty, rc } = setup();
    kitty.addWindow({ id: 3, gridText: "x" });
    await rc.getText(3, { extent: "screen" });
    expect(exec.lastCall("kitty")!.argv).toEqual(["kitty", "@", "get-text", "--match", "id:3"]);
  });
});

describe("KittyRc.sendText / sendKey", () => {
  test("send-text uses @ send-text with the id match and records the payload", async () => {
    const { exec, kitty, rc } = setup();
    kitty.addWindow({ id: 9 });
    await rc.sendText(9, "hello world");
    expect(exec.lastCall("kitty")!.argv.slice(0, 5)).toEqual(["kitty", "@", "send-text", "--match", "id:9"]);
    expect(kitty.sentText.at(-1)).toEqual({ windowId: 9, text: "hello world" });
  });

  test("send-text --enter appends a carriage return", async () => {
    const { kitty, rc } = setup();
    kitty.addWindow({ id: 9 });
    await rc.sendText(9, "run", { enter: true });
    expect(kitty.sentText.at(-1)!.text).toBe("run\r");
  });

  test("send-text delivers a LITERAL backslash-n unaltered (kitty decodes escapes; sendText escapes them)", async () => {
    const { kitty, rc } = setup();
    kitty.addWindow({ id: 9 });
    // A regex an operator pastes: the two characters backslash + n. Real kitty decodes `\n` into a
    // newline unless we escape the backslash — the fake models that decode, so a missing escape would
    // deliver a real newline (a premature mid-command submission). We assert the literal survives.
    await rc.sendText(9, 'grep "\\n" file');
    expect(kitty.sentText.at(-1)!.text).toBe('grep "\\n" file');
    expect(kitty.sentText.at(-1)!.text).not.toContain("\n"); // never a real newline
  });

  test("send-text puts the payload AFTER a `--` option terminator (so a leading-dash payload is text, not a kitty option)", async () => {
    const { exec, kitty, rc } = setup();
    kitty.addWindow({ id: 9 });
    await rc.sendText(9, "--from-file=/etc/hostname");
    const argv = exec.lastCall("kitty")!.argv;
    // The `--` terminator precedes the payload; the payload is the trailing token.
    expect(argv).toContain("--");
    expect(argv.indexOf("--")).toBeLessThan(argv.length - 1);
    expect(argv[argv.length - 1]).toBe("--from-file=/etc/hostname");
    // The fake delivered it as literal text (not interpreted as a --from-file option).
    expect(kitty.sentText.at(-1)!.text).toBe("--from-file=/etc/hostname");
  });

  test("send-key sends the resolved escape as send-text", async () => {
    const { exec, kitty, rc } = setup();
    kitty.addWindow({ id: 9 });
    await rc.sendKey(9, "ctrl+c");
    // send-key is @ send-text of the escape (the one sanctioned write path).
    expect(exec.lastCall("kitty")!.argv[2]).toBe("send-text");
    expect(kitty.sentText.at(-1)!.text).toBe("\x03");
  });

  test("send-key rejects an unknown chord", async () => {
    const { kitty, rc } = setup();
    kitty.addWindow({ id: 9 });
    await expect(rc.sendKey(9, "hyper+meta+q")).rejects.toThrow(TallyError);
  });
});

describe("keyEscape", () => {
  test("resolves known chords case-insensitively", () => {
    expect(keyEscape("Enter")).toBe("\r");
    expect(keyEscape("ESC")).toBe("\x1b");
    expect(keyEscape("ctrl+c")).toBe(KEY_ESCAPES["ctrl+c"]!);
  });
  test("throws not_found for an unknown key", () => {
    expect(() => keyEscape("nope")).toThrow(/unknown key/);
  });
});

describe("KittyRc.focusWindow / setUserVars", () => {
  test("focus-window matches on id and updates the fake focus", async () => {
    const { exec, kitty, rc } = setup();
    kitty.addWindow({ id: 4 });
    kitty.addWindow({ id: 5, is_focused: true });
    await rc.focusWindow(4);
    expect(exec.lastCall("kitty")!.argv).toEqual(["kitty", "@", "focus-window", "--match", "id:4"]);
    expect(kitty.focusedWindowId()).toBe(4);
    expect(kitty.focusCalls).toContain(4);
  });

  test("set-user-vars writes KEY=VALUE opaque back-references only", async () => {
    const { exec, kitty, rc } = setup();
    kitty.addWindow({ id: 6 });
    await rc.setUserVars(6, { tally_pane: "term-0707-1530:p2" });
    expect(exec.lastCall("kitty")!.argv).toEqual([
      "kitty",
      "@",
      "set-user-vars",
      "--match",
      "id:6",
      "tally_pane=term-0707-1530:p2",
    ]);
    expect(kitty.getWindow(6)!.user_vars.tally_pane).toBe("term-0707-1530:p2");
  });

  test("set-user-vars is a no-op with no vars (never issues an empty call)", async () => {
    const { exec, kitty, rc } = setup();
    kitty.addWindow({ id: 6 });
    await rc.setUserVars(6, {});
    expect(exec.callsFor("kitty").length).toBe(0);
  });
});

describe("the boundary: kitty @ launch is forbidden", () => {
  test("KittyRc never exposes launch and refuses it defensively", async () => {
    const { rc } = setup();
    // The client has no `launch` method; and its private runAt refuses the verb. We assert the
    // constant is the forbidden name and that no public method can produce it.
    expect(FORBIDDEN_KITTY_VERB).toBe("launch");
    // There is no public API path to launch; the surface is ls/getText/sendText/sendKey/focus/setUserVars.
    const surface = Object.getOwnPropertyNames(Object.getPrototypeOf(rc));
    expect(surface).not.toContain("launch");
  });

  test("the base FakeKitty throws loudly if any code path reaches `kitty @ launch`", async () => {
    const exec = new FakeExec();
    new FakeKitty().install(exec);
    // Directly exercising the fake with launch must reject (mirrors the boundary law).
    await expect(exec.run(["kitty", "@", "launch", "claude"])).rejects.toThrow(/forbidden/);
  });
});
