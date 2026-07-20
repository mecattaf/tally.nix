// test/helpers/fake-kitty.ts
//
// A scripted fake of the `kitty @` remote-control surface, registered on a
// FakeExec. Covers exactly the four sanctioned verbs tally uses (CLI-SURFACE
// §3.1), keyed on `kitty_window_id`:
//
//   kitty @ ls                       -> the window/OS-window inventory JSON tree
//   kitty @ get-text --match id:<id> -> the emulated grid text (the throttled read)
//   kitty @ send-text [--match ...]  -> recorded (no effect on the model)
//   kitty @ focus-window --match ... -> recorded, updates focus
//   kitty @ set-user-vars --match .. -> records one opaque back-ref user-var
//
// `kitty @ launch` is DELIBERATELY UNSUPPORTED: the fake throws if any test or
// module ever asks for it, mirroring the boundary law (kitty @ launch is
// forbidden everywhere under src/ except the single carve-out file). This makes
// an accidental launch a loud test failure, not a silent one.
//
// Authored fresh for tally; no vendor/ fixtures (clean-room, CLI-SURFACE §4).

import { type FakeExec, type ExecResult, ok, fail, okJson, parseArgs } from "./exec-fakes.ts";

/**
 * Decode the Python-style escape sequences real `kitty @ send-text` interprets in its TEXT argument
 * (\n \r \t \e \0 \\ \xHH). Modelled here so the fake matches the real binary's behaviour — a caller
 * that fails to escape a literal backslash would see it expand (e.g. `\n` → newline), which a test can
 * now catch. Unknown escapes pass through literally (kitty's own leniency).
 */
export function decodePythonEscapes(s: string): string {
  return s.replace(/\\(x[0-9a-fA-F]{2}|.)/g, (_m, seq: string) => {
    if (seq[0] === "x") return String.fromCharCode(parseInt(seq.slice(1), 16));
    switch (seq) {
      case "n": return "\n";
      case "r": return "\r";
      case "t": return "\t";
      case "e": return "\x1b";
      case "0": return "\0";
      case "\\": return "\\";
      default: return "\\" + seq;
    }
  });
}

/** A foreground process inside a window (drives the OSC-title region). */
export interface FakeForegroundProcess {
  pid: number;
  cwd: string;
  cmdline: string[];
  /** The OSC title kitty reports for this process (osc_title region source). */
  title?: string;
}

/** One kitty window = one tally pane. */
export interface FakeKittyWindow {
  id: number;
  is_focused: boolean;
  is_active?: boolean;
  title: string;
  cwd: string;
  foreground_processes: FakeForegroundProcess[];
  /** kitty user-vars (`kitty @ set-user-vars` writes here). */
  user_vars: Record<string, string>;
  /** The emulated grid text `kitty @ get-text` returns for this window. */
  gridText: string;
  /** OSC progress escape state, if any (osc_progress region source). */
  osc_progress?: string;
}

/** A kitty tab (grouping is not used by tally, but ls emits the shape). */
export interface FakeKittyTab {
  id: number;
  title: string;
  is_focused: boolean;
  windows: FakeKittyWindow[];
}

/** An OS window (kitty @ ls top level). */
export interface FakeKittyOSWindow {
  id: number;
  is_focused: boolean;
  tabs: FakeKittyTab[];
}

/**
 * A programmable kitty model. Add windows, then `install(exec)` to register the
 * `kitty` handler on a FakeExec. Mutate windows between calls to simulate the
 * grid changing (the detector's scrape loop reads the current gridText).
 */
export class FakeKitty {
  private readonly windows = new Map<number, FakeKittyWindow>();
  /** Recorded send-text payloads, in order. */
  readonly sentText: Array<{ windowId: number | "all"; text: string }> = [];
  /** Recorded send-key / key-escape payloads. */
  readonly sentKeys: Array<{ windowId: number | "all"; text: string }> = [];
  /** Recorded focus-window calls. */
  readonly focusCalls: number[] = [];

  /** Add or replace a window. Returns it for chaining. */
  addWindow(w: Partial<FakeKittyWindow> & { id: number }): FakeKittyWindow {
    const full: FakeKittyWindow = {
      id: w.id,
      is_focused: w.is_focused ?? false,
      is_active: w.is_active ?? w.is_focused ?? false,
      title: w.title ?? `window-${w.id}`,
      cwd: w.cwd ?? "/home/tom",
      foreground_processes: w.foreground_processes ?? [
        { pid: 1000 + w.id, cwd: w.cwd ?? "/home/tom", cmdline: ["fish"] },
      ],
      user_vars: w.user_vars ?? {},
      gridText: w.gridText ?? "",
      ...(w.osc_progress !== undefined ? { osc_progress: w.osc_progress } : {}),
    };
    this.windows.set(full.id, full);
    return full;
  }

  /** Replace a window's grid text (simulate the screen changing). */
  setGrid(windowId: number, text: string): void {
    const w = this.windows.get(windowId);
    if (!w) throw new Error(`FakeKitty.setGrid: no window ${windowId}`);
    w.gridText = text;
  }

  /** Set a foreground-process title (the osc_title region source). */
  setTitle(windowId: number, title: string): void {
    const w = this.windows.get(windowId);
    if (!w) throw new Error(`FakeKitty.setTitle: no window ${windowId}`);
    if (w.foreground_processes.length === 0) {
      w.foreground_processes.push({ pid: 1000 + windowId, cwd: w.cwd, cmdline: ["proc"] });
    }
    w.foreground_processes[w.foreground_processes.length - 1]!.title = title;
  }

  /** The current focused window id, or undefined. */
  focusedWindowId(): number | undefined {
    for (const w of this.windows.values()) if (w.is_focused) return w.id;
    return undefined;
  }

  getWindow(id: number): FakeKittyWindow | undefined {
    return this.windows.get(id);
  }

  /** Build the `kitty @ ls` JSON tree from the current windows. */
  private lsTree(): FakeKittyOSWindow[] {
    const windows = [...this.windows.values()];
    return [
      {
        id: 1,
        is_focused: true,
        tabs: [
          {
            id: 1,
            title: "tab-1",
            is_focused: true,
            windows,
          },
        ],
      },
    ];
  }

  /** Resolve the window id from a `--match id:<n>` (or `id=<n>`) flag value. */
  private matchWindowId(matchExpr: string | undefined): number | "all" | undefined {
    if (matchExpr === undefined) return "all";
    const m = /^id[:=](\d+)$/.exec(matchExpr.trim());
    if (m) return Number(m[1]);
    // Unsupported match forms return undefined => handler fails loudly.
    return undefined;
  }

  /** Register the `kitty` handler on the FakeExec. */
  install(exec: FakeExec): this {
    exec.register("kitty", (args): ExecResult => this.dispatch(args));
    return this;
  }

  /**
   * Dispatch one `kitty @ <verb> ...` argument vector. Subclasses override to
   * intercept specific verbs (e.g. the launch carve-out) and delegate the rest
   * back here via `super.dispatch(args)`.
   */
  protected dispatch(args: readonly string[]): ExecResult {
    // The tally surface always invokes `kitty @ <verb> ...`.
    const at = args[0];
    if (at !== "@") return fail(2, `fake-kitty: expected '@', got '${at}'`);
    const verb = args[1];
    const rest = args.slice(2);
    const parsed = parseArgs(rest);
    switch (verb) {
      case "ls":
        return okJson(this.lsTree());
      case "get-text": {
        const target = this.matchWindowId(parsed.value("match"));
        if (target === "all" || target === undefined) {
          return fail(1, "fake-kitty get-text: --match id:<n> required");
        }
        const w = this.windows.get(target);
        if (!w) return fail(1, `fake-kitty get-text: no window ${target}`);
        return ok(w.gridText);
      }
      case "send-text": {
        const target = this.matchWindowId(parsed.value("match"));
        if (target === undefined) return fail(1, "fake-kitty send-text: bad --match");
        // Text is the trailing positional (kitty also accepts it via stdin). REAL kitty decodes Python
        // escape sequences in this argument (\n \r \t \e \\ \xHH) before transmission — the fake models
        // that faithfully so a divergence (e.g. an unescaped backslash expanding to a newline) is
        // catchable by tests. Production KittyRc.sendText escapes backslashes to survive this decode.
        const raw = parsed.positionals.length > 0 ? parsed.positionals.join(" ") : "";
        const text = decodePythonEscapes(raw);
        this.sentText.push({ windowId: target, text });
        return ok();
      }
      case "send-key": {
        const target = this.matchWindowId(parsed.value("match"));
        if (target === undefined) return fail(1, "fake-kitty send-key: bad --match");
        this.sentKeys.push({ windowId: target, text: parsed.positionals.join(" ") });
        return ok();
      }
      case "focus-window": {
        const target = this.matchWindowId(parsed.value("match"));
        if (typeof target !== "number") return fail(1, "fake-kitty focus-window: bad --match");
        if (!this.windows.has(target)) return fail(1, `no window ${target}`);
        for (const w of this.windows.values()) w.is_focused = w.id === target;
        this.focusCalls.push(target);
        return ok();
      }
      case "set-user-vars": {
        const target = this.matchWindowId(parsed.value("match"));
        if (typeof target !== "number") return fail(1, "fake-kitty set-user-vars: bad --match");
        const w = this.windows.get(target);
        if (!w) return fail(1, `no window ${target}`);
        // Each positional is KEY=VALUE.
        for (const kv of parsed.positionals) {
          const eq = kv.indexOf("=");
          if (eq !== -1) w.user_vars[kv.slice(0, eq)] = kv.slice(eq + 1);
        }
        return ok();
      }
      case "launch":
        return this.onLaunch(rest);
      default:
        return fail(2, `fake-kitty: unsupported verb '${verb}'`);
    }
  }

  /**
   * The boundary hook. The base fake FORBIDS `kitty @ launch` (throws loudly),
   * mirroring the tally boundary law (CLI-SURFACE §3.1): launch is forbidden
   * everywhere under src/ except the single carve-out. Subclasses (the gated
   * contingency fake) override this to record launches instead.
   */
  protected onLaunch(_launchArgs: readonly string[]): ExecResult {
    throw new Error(
      "fake-kitty: `kitty @ launch` is forbidden by the tally boundary " +
        "(CLI-SURFACE §3.1). Only src/agents/claude-p-contingency.ts may launch; " +
        "test it through FakeKittyWithLaunch.",
    );
  }
}

/**
 * A dedicated fake for the ONE carve-out: the `--via-terminal` contingency in
 * src/agents/claude-p-contingency.ts, the sole place `kitty @ launch` is legal
 * (M2.2 / §1 item 9). Tests of that gated path install this instead of FakeKitty
 * so launches are recorded rather than thrown — the boundary stays loud
 * everywhere else while the gated path remains testable.
 */
export class FakeKittyWithLaunch extends FakeKitty {
  readonly launches: Array<{ args: string[] }> = [];

  protected override onLaunch(launchArgs: readonly string[]): ExecResult {
    this.launches.push({ args: [...launchArgs] });
    // kitty @ launch prints the new window id on stdout.
    return ok(String(9000 + this.launches.length));
  }
}
