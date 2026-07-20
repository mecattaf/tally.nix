// tally — the `claude -p` launch contingency (IMPLEMENTATION-PLAN M2.2 / §1 item 9; CLI-SURFACE §3.1
// sidenote; DECISIONS Q6; SPEC "the `claude -p` launch contingency … the sole boundary exception").
//
// THE SOLE BOUNDARY CARVE-OUT. Everywhere else under src/ the out-of-band law (CLI-SURFACE §3.1/§3.2)
// forbids `kitty @ launch`: tally OBSERVES the terminal substrate, it never *starts* a window. This
// one file is the single, gated, documented exception — the only place under src/ where `kitty @
// launch` is legal (the M1.6 boundary grep-test carve-out). It exists for exactly one contingency
// (DECISIONS Q6, kept NON-DEFAULT): should Anthropic (again) externalize `claude -p` headless runs
// off the subscription meter, tally can MOCK `claude -p` scheduling with the interactive TUI —
//   1. `kitty @ launch` a `claude` window (interactive TUI, no `-p`),
//   2. wait ~10 s for the TUI to settle,
//   3. `kitty @ send-text` a short autonomous-mode kickoff (the "you're unsupervised, this is the
//      last message you get from me" prompt) — steering the interactive session into an autonomous
//      run without a print-mode meter charge.
// The scheduled run then becomes a RECOVERABLE zmx-backed session (SPEC "Recovering a tally-owned
// agent session") and tally keeps its TaskChampion row / witness line exactly as for any run.
//
// This is a CONTINGENCY built into the one binary — a flip away, NOT a default path. It fires ONLY
// when a caller opts in with `--via-terminal`; the default claude-code dispatch (claude-code.ts) is
// headless and launch-free. Because `kitty @ launch` is forbidden on `KittyRc` (rc.ts throws on the
// verb by construction), this file constructs the launch argv DIRECTLY over the injected `Exec`
// seam — that directness is the whole reason the carve-out is isolated to one named function here.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Clock, Exec } from "../contracts/index";
import { systemClock, TallyError } from "../contracts/index";
import { KittyRc, KITTY_BIN } from "../kitty/rc";

/**
 * The opt-in flag that gates this whole path. The default dispatch never sets it; only a caller that
 * explicitly requests the terminal contingency does. Named as a string constant so the gate is
 * greppable (and so the M1.6 boundary test sees `--via-terminal` present in the sole launch file).
 */
export const VIA_TERMINAL_FLAG = "--via-terminal" as const;

/** The kitty `@ launch` verb — spelled here (and ONLY here under src/) because this is the carve-out. */
const KITTY_LAUNCH_VERB = "launch" as const;

/** How long to let the interactive `claude` TUI settle before sending the kickoff (§3.1 sidenote: ~10 s). */
export const DEFAULT_SETTLE_MS = 10_000;

/**
 * The request to run a claude session VIA the interactive terminal (the contingency), rather than
 * headless. `viaTerminal` MUST be true for this module to do anything — it is the `--via-terminal`
 * gate in structured form. Everything else describes the window to open and the kickoff to send.
 */
export interface ViaTerminalRequest {
  /**
   * The gate. Only `true` triggers a launch; `false` is a no-op that returns `launched:false`, so a
   * caller can pass the request unconditionally and let the flag decide (the "flip away" ergonomics).
   */
  viaTerminal: boolean;
  /** The autonomous-mode kickoff prompt sent into the TUI after it settles (the last human message). */
  kickoff: string;
  /** Bind an existing claude session for `--resume`, or null to start a fresh interactive session. */
  sessionRef?: string | null;
  /** The DECLARED model id passed to `claude --model` (never re-picked here, PS#2), or null. */
  model?: string | null;
  /** The working directory the launched window opens in (`--cwd` / `--worktree`), or undefined. */
  cwd?: string;
  /** A window title kitty stamps on the launched window (identity back-reference), or undefined. */
  title?: string;
  /** Settle delay before the kickoff; defaults to {@link DEFAULT_SETTLE_MS}. */
  settleMs?: number;
  /** Append a carriage return to the kickoff (submit it), default true. */
  submit?: boolean;
}

/** The outcome of a via-terminal launch. */
export interface ViaTerminalResult {
  /** True when a window was actually launched (the gate was on). */
  launched: boolean;
  /** The kitty_window_id of the launched window — the pane the run's recoverable session binds to. */
  kittyWindowId: number | null;
  /** The exact `claude …` argv the launched window runs (for the witness/journald record). */
  claudeArgv: string[];
}

/**
 * True when a resolved argv/flag set opts into the terminal contingency. Kept tiny + explicit so the
 * gate is one function the whole binary shares (a caller that scanned `--via-terminal` off the CLI,
 * or a config flip, resolves to this boolean).
 */
export function isViaTerminal(flags: readonly string[]): boolean {
  return flags.includes(VIA_TERMINAL_FLAG);
}

/**
 * Build the interactive `claude` argv the launched window runs. This is the INTERACTIVE TUI form (no
 * `-p`) — the whole point of the contingency is to avoid print-mode. A declared `sessionRef` binds a
 * `--resume`; a declared model passes `--model` verbatim (declared, never re-picked, PS#2).
 */
export function buildClaudeArgv(req: ViaTerminalRequest): string[] {
  const argv = ["claude"];
  if (req.sessionRef !== undefined && req.sessionRef !== null) {
    argv.push("--resume", req.sessionRef);
  }
  if (req.model !== undefined && req.model !== null) {
    argv.push("--model", req.model);
  }
  return argv;
}

/**
 * Build the `kitty @ launch` argv that opens the interactive `claude` window. THIS is the sole
 * `kitty @ launch` construction in all of src/ (the carve-out). `--type=os-window` gives the agent
 * its own window (the recoverable session's home); `--cwd` and `--title` carry the identity; the
 * `claude …` argv is the window's command. Constructed directly (not via `KittyRc`, which forbids
 * the verb) — the directness is contained entirely to this one function.
 */
export function buildLaunchArgv(req: ViaTerminalRequest, claudeArgv: string[], bin: string = KITTY_BIN): string[] {
  // The ONE `kitty @ launch` invocation permitted under src/ (M1.6 carve-out; --via-terminal gated).
  const argv = [bin, "@", KITTY_LAUNCH_VERB, "--type=os-window"];
  if (req.cwd !== undefined) argv.push("--cwd", req.cwd);
  if (req.title !== undefined) argv.push("--title", req.title);
  // Kitty runs everything after the flags as the window's command.
  argv.push(...claudeArgv);
  return argv;
}

/**
 * Parse the `kitty_window_id` kitty prints on `@ launch` (a bare integer on stdout). Throws on a
 * non-numeric response (a launch that did not yield a window id is a hard failure — the run has no
 * pane to bind to).
 */
export function parseLaunchedWindowId(stdout: string): number {
  const trimmed = stdout.trim();
  const id = Number.parseInt(trimmed, 10);
  if (!Number.isInteger(id) || String(id) !== trimmed) {
    throw new TallyError("internal", `kitty @ launch did not return a window id (got: ${JSON.stringify(trimmed)})`);
  }
  return id;
}

/**
 * The via-terminal contingency runner. Owns the ONE `kitty @ launch` under src/. Reuses `KittyRc`
 * for the sanctioned follow-up writes (`send-text`/`focus-window`) — only the launch itself is the
 * carve-out. Clock-injected so the ~10 s settle is deterministic in tests.
 */
export class ClaudePContingency {
  private readonly rc: KittyRc;

  constructor(
    private readonly exec: Exec,
    private readonly clock: Clock = systemClock,
    private readonly bin: string = KITTY_BIN,
  ) {
    this.rc = new KittyRc(exec, bin);
  }

  /**
   * Launch the interactive `claude` window, wait for it to settle, and send the autonomous-mode
   * kickoff. Returns the launched window id (the pane the recoverable session binds to). When the
   * `--via-terminal` gate is OFF (`viaTerminal:false`) this is a no-op returning `launched:false`, so
   * a caller can hand the request in unconditionally and let the flag decide (the "flip away" path).
   */
  async run(req: ViaTerminalRequest): Promise<ViaTerminalResult> {
    const claudeArgv = buildClaudeArgv(req);
    if (!req.viaTerminal) {
      // The gate is off: the default headless path (claude-code.ts) owns this run; we launch nothing.
      return { launched: false, kittyWindowId: null, claudeArgv };
    }

    // 1. `kitty @ launch` the interactive TUI (the carve-out). Constructed directly, not via KittyRc.
    const launchArgv = buildLaunchArgv(req, claudeArgv, this.bin);
    const launch = await this.exec.run(launchArgv);
    if (launch.code !== 0) {
      throw new TallyError("internal", `kitty @ launch (--via-terminal contingency) failed (exit ${launch.code}): ${launch.stderr.trim()}`);
    }
    const kittyWindowId = parseLaunchedWindowId(launch.stdout);

    // 2. Let the interactive TUI settle before steering it (§3.1 sidenote: ~10 s).
    const settleMs = req.settleMs ?? DEFAULT_SETTLE_MS;
    if (settleMs > 0) await this.clock.sleep(settleMs);

    // 3. Send the autonomous-mode kickoff through the ONE sanctioned write path (`kitty @ send-text`),
    //    reused from KittyRc — only the launch above is the carve-out.
    const submit = req.submit ?? true;
    await this.rc.sendText(kittyWindowId, req.kickoff, { enter: submit });

    return { launched: true, kittyWindowId, claudeArgv };
  }

  /** Focus the launched window (tunnel-in affordance) — a thin passthrough to the sanctioned verb. */
  async focus(kittyWindowId: number): Promise<void> {
    await this.rc.focusWindow(kittyWindowId);
  }
}
