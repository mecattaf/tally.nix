// tally — the module-owned cooperative-hook installer (IMPLEMENTATION-PLAN M3.2; CLI-SURFACE §5
// flag 2 CLOSED 2026-07-09; SPEC boundary "tally SHIPS the cooperative-hook installer"; DECISIONS Q5).
//
// Backs `tally hooks install [--kind claude-code|pi] [--dry-run]` and is invoked by home-manager
// activation from the nix module. It wires tally's Strategy-1 AUTHORITATIVE detector input:
//
//   claude-code — registers the `UserPromptSubmit` / `Stop` / `SessionStart` / `Notification` hooks
//     in Claude Code's settings `hooks` schema, each running `hooks/claude-code/tally-hook.ts`
//     (which posts `agent.hook_event` to the tally socket). MERGES into an existing settings.json,
//     never clobbers foreign hooks: it identifies tally's own command entries by a stable marker and
//     replaces only those, leaving every other user/tool hook intact.
//
//   pi — installs `hooks/pi/tally-session.ts` into pi's extension directory
//     (`$PI_CODING_AGENT_DIR/extensions/` or `~/.pi/agent/extensions/`), the mechanism cmux uses for
//     `cmux-session.ts` (CLI-SURFACE §3.3/§3.4). Idempotent overwrite of tally's own file only.
//
// This module imports ONLY from `src/contracts` (its declared dependency). All FS access is direct
// node:fs (no subprocess), so no Exec seam is needed. Every operation is idempotent and reports a
// plan; `--dry-run` computes the same plan without writing. Authored fresh; no vendor/ code
// (clean-room, CLI-SURFACE §4).

import {
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
  copyFileSync,
  realpathSync,
} from "node:fs";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// ---------------------------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------------------------

/** The kinds the installer can install (the two hook-exposing harnesses; `shell` has no hook). */
export type InstallKind = "claude-code" | "pi";

/** All installable kinds, canonical order (default target when `--kind` is omitted). */
export const INSTALL_KINDS: readonly InstallKind[] = ["claude-code", "pi"];

/**
 * A stable marker embedded in every tally-authored hook command / extension file, so a re-install
 * (and a cooperative merge) can find and replace ONLY tally's own entries and never a foreign hook.
 */
export const TALLY_HOOK_MARKER = "tally:cooperative-hook" as const;

/** Options for an install run. Everything is injectable so tests point at a tmp tree. */
export interface InstallOptions {
  /** Which harness(es) to install. Omit ⇒ all of `INSTALL_KINDS`. */
  kind?: InstallKind;
  /** Compute the plan without writing anything. */
  dryRun?: boolean;
  /**
   * The environment the path resolver reads (Claude/pi config dirs). Defaults to `process.env`.
   * Injected in tests so no real home directory is touched.
   */
  env?: NodeJS.ProcessEnv;
  /**
   * Absolute path to the Claude Code hook payload script (`tally-hook.ts`). The nix module passes the
   * store path; defaults to the repo-relative script next to this module.
   */
  ccHookScript?: string;
  /**
   * Absolute path to the pi extension payload (`tally-session.ts`). The nix module passes the store
   * path; defaults to the repo-relative script.
   */
  piExtensionScript?: string;
  /**
   * The interpreter that runs the Claude Code hook script (`bun` by default; the compiled binary
   * ships bun's runtime, but the hook is a standalone `.ts` run by `bun`).
   */
  runner?: string;
}

/** One filesystem action the installer performed (or would perform under `--dry-run`). */
export interface InstallAction {
  kind: InstallKind;
  /** `create` = new file written; `update` = existing file merged/changed; `noop` = already current. */
  action: "create" | "update" | "noop";
  /** The absolute path written (or that would be written). */
  path: string;
  /** A human-readable description of the change. */
  detail: string;
}

/** The full result of an install run. */
export interface InstallResult {
  dryRun: boolean;
  actions: InstallAction[];
}

// ---------------------------------------------------------------------------------------------
// Default script-path resolution (repo-relative; nix overrides with store paths)
// ---------------------------------------------------------------------------------------------

/** This module's own directory (`src/hooks/`), resolved from the module URL. */
function selfDir(): string {
  try {
    return dirname(fileURLToPath(import.meta.url));
  } catch {
    // Fallback for exotic runtimes: cwd-relative source layout.
    return resolve("src/hooks");
  }
}

/** Repo root (two levels up from `src/hooks/`). */
function repoRoot(): string {
  return resolve(selfDir(), "..", "..");
}

function defaultCcHookScript(): string {
  return join(repoRoot(), "hooks", "claude-code", "tally-hook.ts");
}

function defaultPiExtensionScript(): string {
  return join(repoRoot(), "hooks", "pi", "tally-session.ts");
}

// ---------------------------------------------------------------------------------------------
// Config-dir resolution
// ---------------------------------------------------------------------------------------------

function homeDir(env: NodeJS.ProcessEnv): string {
  const home = env.HOME;
  if (home && home.length > 0) return home;
  throw new HookInstallError("cannot resolve HOME for hook install (set $HOME)");
}

/** The Claude Code config directory: `$CLAUDE_CONFIG_DIR` else `~/.claude`. */
export function claudeConfigDir(env: NodeJS.ProcessEnv): string {
  const explicit = env.CLAUDE_CONFIG_DIR;
  if (explicit && explicit.length > 0) return explicit;
  return join(homeDir(env), ".claude");
}

/** The Claude Code settings file the hooks are merged into (`<configDir>/settings.json`). */
export function claudeSettingsPath(env: NodeJS.ProcessEnv): string {
  return join(claudeConfigDir(env), "settings.json");
}

/** The pi extensions directory: `$PI_CODING_AGENT_DIR/extensions` else `~/.pi/agent/extensions`. */
export function piExtensionsDir(env: NodeJS.ProcessEnv): string {
  const explicit = env.PI_CODING_AGENT_DIR;
  if (explicit && explicit.length > 0) return join(explicit, "extensions");
  return join(homeDir(env), ".pi", "agent", "extensions");
}

/** The installed pi extension file path. */
export function piExtensionTargetPath(env: NodeJS.ProcessEnv): string {
  return join(piExtensionsDir(env), "tally-session.ts");
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/** A hook-install failure (bad settings JSON, missing HOME, missing source script). */
export class HookInstallError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "HookInstallError";
    Object.setPrototypeOf(this, HookInstallError.prototype);
  }
}

// ---------------------------------------------------------------------------------------------
// Claude Code settings hooks schema (structural types for the merge)
// ---------------------------------------------------------------------------------------------

/** The Claude Code hook events tally registers, each mapped to a tally lifecycle post. */
export const CC_HOOK_EVENTS = ["UserPromptSubmit", "Stop", "SessionStart", "Notification"] as const;
export type CcHookEvent = (typeof CC_HOOK_EVENTS)[number];

/** One command hook inside a matcher-group (`{type:"command", command}`, plus tally's marker/env). */
interface CcCommandHook {
  type: "command";
  command: string;
  /** tally's stable marker so a re-install/merge finds only its own entries. */
  [TALLY_HOOK_MARKER]?: true;
  [k: string]: unknown;
}

/** One matcher-group: `{matcher?, hooks:[…]}` (CC's settings `hooks.<Event>[]` element shape). */
interface CcMatcherGroup {
  matcher?: string;
  hooks: CcCommandHook[];
  [k: string]: unknown;
}

/** The `hooks` section: event name → array of matcher-groups. */
type CcHooksSection = Partial<Record<string, CcMatcherGroup[]>>;

/** The (relevant slice of the) CC settings document. */
interface CcSettings {
  hooks?: CcHooksSection;
  [k: string]: unknown;
}

// ---------------------------------------------------------------------------------------------
// Install entrypoint
// ---------------------------------------------------------------------------------------------

/**
 * Install tally's cooperative hooks. Idempotent and cooperative: re-running produces `noop` actions,
 * and existing foreign hooks are preserved. Under `dryRun`, computes the plan and writes nothing.
 */
export function installHooks(opts: InstallOptions = {}): InstallResult {
  const env = opts.env ?? process.env;
  const dryRun = opts.dryRun ?? false;
  const kinds: InstallKind[] = opts.kind ? [opts.kind] : [...INSTALL_KINDS];

  const actions: InstallAction[] = [];
  for (const kind of kinds) {
    if (kind === "claude-code") {
      actions.push(installClaudeCode(env, dryRun, opts));
    } else {
      actions.push(installPi(env, dryRun, opts));
    }
  }
  return { dryRun, actions };
}

// ---------------------------------------------------------------------------------------------
// Claude Code install (cooperative settings.json merge)
// ---------------------------------------------------------------------------------------------

function installClaudeCode(env: NodeJS.ProcessEnv, dryRun: boolean, opts: InstallOptions): InstallAction {
  const settingsPath = claudeSettingsPath(env);
  const script = resolveExistingScript(opts.ccHookScript ?? defaultCcHookScript(), "claude-code hook script");
  const runner = opts.runner ?? "bun";

  const existed = existsSync(settingsPath);
  const current: CcSettings = existed ? parseSettings(settingsPath) : {};

  const merged = mergeClaudeHooks(current, script, runner);
  const before = existed ? readFileSync(settingsPath, "utf8") : "";
  const after = serializeSettings(merged);

  if (existed && before === after) {
    return { kind: "claude-code", action: "noop", path: settingsPath, detail: "hooks already current" };
  }

  const action: "create" | "update" = existed ? "update" : "create";
  const detail = existed
    ? `merged ${CC_HOOK_EVENTS.length} tally hook events into existing settings.json (foreign hooks preserved)`
    : `wrote settings.json with ${CC_HOOK_EVENTS.length} tally hook events`;

  if (!dryRun) {
    mkdirSync(dirname(settingsPath), { recursive: true });
    writeFileSync(settingsPath, after, "utf8");
  }
  return { kind: "claude-code", action, path: settingsPath, detail };
}

/** Read + parse the CC settings JSON, failing loudly on malformed JSON (never silently clobber). */
function parseSettings(path: string): CcSettings {
  let text: string;
  try {
    text = readFileSync(path, "utf8");
  } catch (e) {
    throw new HookInstallError(`cannot read Claude Code settings at ${path}: ${(e as Error).message}`);
  }
  if (text.trim().length === 0) return {};
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (e) {
    throw new HookInstallError(
      `Claude Code settings at ${path} is not valid JSON (${(e as Error).message}); refusing to clobber — fix it or move it aside`,
    );
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new HookInstallError(`Claude Code settings at ${path} must be a JSON object`);
  }
  return parsed as CcSettings;
}

/** Serialize settings back to disk form (2-space indent, trailing newline — matches CC's own writes). */
function serializeSettings(s: CcSettings): string {
  return JSON.stringify(s, null, 2) + "\n";
}

/** The tally hook command line for one CC event (runs the script with `CLAUDE_HOOK_EVENT` set). */
export function ccHookCommand(runner: string, script: string, event: CcHookEvent): string {
  // `CLAUDE_HOOK_EVENT=<Event> <runner> <script>` — the script reads the env var to know which event
  // fired, so one script serves every registered event.
  return `CLAUDE_HOOK_EVENT=${event} ${runner} ${shellQuote(script)}`;
}

/** Whether a command hook is one of tally's (identified by the marker, or the command shape). */
function isTallyHook(h: CcCommandHook): boolean {
  if (h[TALLY_HOOK_MARKER] === true) return true;
  return typeof h.command === "string" && h.command.includes("CLAUDE_HOOK_EVENT=") && h.command.includes("tally-hook");
}

/**
 * Merge tally's hooks into the CC settings, cooperatively. For each tally event: within the event's
 * matcher-group array, tally owns exactly one group (matcher-less, marker-tagged); its `hooks` are
 * rewritten to the current command. EVERY foreign matcher-group and EVERY foreign command hook is
 * preserved. Returns a new settings object (the input is not mutated).
 */
export function mergeClaudeHooks(current: CcSettings, script: string, runner: string): CcSettings {
  const next: CcSettings = { ...current };
  const hooks: CcHooksSection = { ...(current.hooks ?? {}) };

  for (const event of CC_HOOK_EVENTS) {
    const groups: CcMatcherGroup[] = [...(hooks[event] ?? [])];
    const tallyHook: CcCommandHook = {
      type: "command",
      command: ccHookCommand(runner, script, event),
      [TALLY_HOOK_MARKER]: true,
    };

    // Strip tally's own command hooks out of every existing group (so a re-install replaces, never
    // duplicates), dropping any group that becomes empty and was tally-only.
    const cleaned: CcMatcherGroup[] = [];
    for (const g of groups) {
      const foreignHooks = (g.hooks ?? []).filter((h) => !isTallyHook(h));
      if (foreignHooks.length > 0) {
        cleaned.push({ ...g, hooks: foreignHooks });
      } else if ((g.hooks ?? []).length === 0) {
        // A pre-existing empty foreign group — preserve it untouched.
        cleaned.push(g);
      }
      // else: a tally-only group — dropped, re-added canonically below.
    }

    // Append tally's single canonical (matcher-less) group.
    cleaned.push({ hooks: [tallyHook] });
    hooks[event] = cleaned;
  }

  next.hooks = hooks;
  return next;
}

// ---------------------------------------------------------------------------------------------
// pi install (extension-file copy)
// ---------------------------------------------------------------------------------------------

function installPi(env: NodeJS.ProcessEnv, dryRun: boolean, opts: InstallOptions): InstallAction {
  const source = resolveExistingScript(opts.piExtensionScript ?? defaultPiExtensionScript(), "pi extension script");
  const target = piExtensionTargetPath(env);

  const desired = readFileSync(source, "utf8");
  const existed = existsSync(target);
  if (existed) {
    const current = readFileSync(target, "utf8");
    if (current === desired) {
      return { kind: "pi", action: "noop", path: target, detail: "extension already current" };
    }
  }

  const action: "create" | "update" = existed ? "update" : "create";
  const detail = existed
    ? "updated tally pi extension (tally-session.ts)"
    : "installed tally pi extension (tally-session.ts)";

  if (!dryRun) {
    mkdirSync(dirname(target), { recursive: true });
    // Write the content rather than copyFileSync so a read-only nix store source still installs a
    // writable target (and the content-equality check above stays exact).
    writeFileSync(target, desired, "utf8");
  } else {
    // touch the source for parity with copyFileSync's read (validated above); no write in dry-run.
    void copyFileSync;
  }
  return { kind: "pi", action, path: target, detail };
}

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

/** Resolve a source script to an existing absolute path, failing loudly if it is missing. */
function resolveExistingScript(path: string, what: string): string {
  const abs = resolve(path);
  if (!existsSync(abs)) {
    throw new HookInstallError(`${what} not found at ${abs} (pass an explicit path or check the nix wiring)`);
  }
  try {
    return realpathSync(abs);
  } catch {
    return abs;
  }
}

/** Minimal POSIX single-quote shell escape for embedding a path in a command string. */
export function shellQuote(s: string): string {
  if (s.length > 0 && /^[A-Za-z0-9_@%+=:,./-]+$/.test(s)) return s;
  return `'${s.replace(/'/g, `'\\''`)}'`;
}

// ---------------------------------------------------------------------------------------------
// Human-readable plan rendering (for the CLI `tally hooks install` output)
// ---------------------------------------------------------------------------------------------

/** Render an install result as human-readable lines (the CLI text output). */
export function renderInstallResult(result: InstallResult): string {
  const prefix = result.dryRun ? "[dry-run] " : "";
  const lines = result.actions.map((a) => `${prefix}${a.kind}: ${a.action} ${a.path} — ${a.detail}`);
  return lines.join("\n");
}
