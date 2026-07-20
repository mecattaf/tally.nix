// test/hooks/installer.test.ts
//
// The cooperative-hook installer (M3.2): idempotence, cooperative merge-not-clobber on a fixture
// Claude Code settings.json, malformed-settings refusal, pi extension install, and `--dry-run`
// producing a plan without writing. Uses the tmp-tree helper so no real ~/.claude or ~/.pi is touched.

import { test, expect, afterEach } from "bun:test";
import { mkdtempSync, rmSync, mkdirSync, writeFileSync, readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  installHooks,
  mergeClaudeHooks,
  claudeSettingsPath,
  piExtensionTargetPath,
  ccHookCommand,
  TALLY_HOOK_MARKER,
  CC_HOOK_EVENTS,
  HookInstallError,
  type InstallOptions,
} from "../../src/hooks/installer";

// --- fixture tmp tree -----------------------------------------------------------------------------

interface Fixture {
  root: string;
  home: string;
  env: NodeJS.ProcessEnv;
  ccScript: string;
  piScript: string;
  cleanup(): void;
}

const cleanups: Array<() => void> = [];
afterEach(() => {
  while (cleanups.length) cleanups.pop()!();
});

function makeFixture(): Fixture {
  const root = mkdtempSync(join(tmpdir(), "tally-hooks-"));
  const home = join(root, "home");
  mkdirSync(home, { recursive: true });
  // Author real source scripts the installer resolves + copies (its own payload files).
  const srcDir = join(root, "src-scripts");
  mkdirSync(join(srcDir, "claude-code"), { recursive: true });
  mkdirSync(join(srcDir, "pi"), { recursive: true });
  const ccScript = join(srcDir, "claude-code", "tally-hook.ts");
  const piScript = join(srcDir, "pi", "tally-session.ts");
  writeFileSync(ccScript, "// tally-hook payload\n", "utf8");
  writeFileSync(piScript, "// tally-session extension payload v1\n", "utf8");

  const env: NodeJS.ProcessEnv = { HOME: home };
  const fx: Fixture = {
    root,
    home,
    env,
    ccScript,
    piScript,
    cleanup() {
      rmSync(root, { recursive: true, force: true });
    },
  };
  cleanups.push(fx.cleanup);
  return fx;
}

function baseOpts(fx: Fixture): InstallOptions {
  return { env: fx.env, ccHookScript: fx.ccScript, piExtensionScript: fx.piScript, runner: "bun" };
}

// --- Claude Code: fresh install ------------------------------------------------------------------

test("claude-code fresh install writes all four hook events, marker-tagged", () => {
  const fx = makeFixture();
  const result = installHooks({ ...baseOpts(fx), kind: "claude-code" });

  expect(result.actions).toHaveLength(1);
  const a = result.actions[0]!;
  expect(a.kind).toBe("claude-code");
  expect(a.action).toBe("create");

  const settings = JSON.parse(readFileSync(claudeSettingsPath(fx.env), "utf8"));
  for (const event of CC_HOOK_EVENTS) {
    const groups = settings.hooks[event];
    expect(Array.isArray(groups)).toBe(true);
    const tallyGroup = groups.find((g: any) => g.hooks.some((h: any) => h[TALLY_HOOK_MARKER] === true));
    expect(tallyGroup).toBeDefined();
    const hook = tallyGroup.hooks.find((h: any) => h[TALLY_HOOK_MARKER] === true);
    expect(hook.type).toBe("command");
    expect(hook.command).toBe(ccHookCommand("bun", fx.ccScript, event));
    expect(hook.command).toContain(`CLAUDE_HOOK_EVENT=${event}`);
  }
});

// --- Claude Code: idempotence ---------------------------------------------------------------------

test("claude-code install is idempotent (second run is a noop, bytes unchanged)", () => {
  const fx = makeFixture();
  installHooks({ ...baseOpts(fx), kind: "claude-code" });
  const firstBytes = readFileSync(claudeSettingsPath(fx.env), "utf8");

  const second = installHooks({ ...baseOpts(fx), kind: "claude-code" });
  expect(second.actions[0]!.action).toBe("noop");
  const secondBytes = readFileSync(claudeSettingsPath(fx.env), "utf8");
  expect(secondBytes).toBe(firstBytes);
});

test("re-install does not duplicate tally hook entries", () => {
  const fx = makeFixture();
  installHooks({ ...baseOpts(fx), kind: "claude-code" });
  installHooks({ ...baseOpts(fx), kind: "claude-code" });
  installHooks({ ...baseOpts(fx), kind: "claude-code" });

  const settings = JSON.parse(readFileSync(claudeSettingsPath(fx.env), "utf8"));
  for (const event of CC_HOOK_EVENTS) {
    const tallyHooks = settings.hooks[event].flatMap((g: any) => g.hooks).filter((h: any) => h[TALLY_HOOK_MARKER] === true);
    expect(tallyHooks).toHaveLength(1);
  }
});

// --- Claude Code: cooperative merge (never clobber foreign hooks) ---------------------------------

test("merge preserves foreign hooks on the same event and other keys", () => {
  const fx = makeFixture();
  // Pre-seed a settings.json with a foreign hook on UserPromptSubmit and unrelated top-level config.
  const settingsPath = claudeSettingsPath(fx.env);
  mkdirSync(join(fx.home, ".claude"), { recursive: true });
  const foreign = {
    model: "sonnet",
    permissions: { allow: ["Bash"] },
    hooks: {
      UserPromptSubmit: [
        { matcher: "*", hooks: [{ type: "command", command: "echo foreign-guard" }] },
      ],
      PreToolUse: [{ hooks: [{ type: "command", command: "echo other-event" }] }],
    },
  };
  writeFileSync(settingsPath, JSON.stringify(foreign, null, 2), "utf8");

  const result = installHooks({ ...baseOpts(fx), kind: "claude-code" });
  expect(result.actions[0]!.action).toBe("update");

  const settings = JSON.parse(readFileSync(settingsPath, "utf8"));
  // Foreign top-level keys survive.
  expect(settings.model).toBe("sonnet");
  expect(settings.permissions.allow).toEqual(["Bash"]);
  // A foreign event tally never touches survives untouched.
  expect(settings.hooks.PreToolUse[0].hooks[0].command).toBe("echo other-event");
  // The foreign UserPromptSubmit hook survives alongside tally's.
  const ups = settings.hooks.UserPromptSubmit;
  const allCommands = ups.flatMap((g: any) => g.hooks).map((h: any) => h.command);
  expect(allCommands).toContain("echo foreign-guard");
  expect(allCommands.some((c: string) => c.includes("CLAUDE_HOOK_EVENT=UserPromptSubmit"))).toBe(true);
});

test("mergeClaudeHooks is a pure function that does not mutate its input", () => {
  const input = { hooks: { Stop: [{ hooks: [{ type: "command", command: "keep-me" }] }] } };
  const snapshot = JSON.stringify(input);
  const out = mergeClaudeHooks(input as any, "/store/tally-hook.ts", "bun");
  expect(JSON.stringify(input)).toBe(snapshot); // input untouched
  const stopCmds = out.hooks!.Stop!.flatMap((g) => g.hooks).map((h) => h.command);
  expect(stopCmds).toContain("keep-me");
});

// --- Claude Code: malformed settings refusal -----------------------------------------------------

test("malformed settings.json is refused, not clobbered", () => {
  const fx = makeFixture();
  const settingsPath = claudeSettingsPath(fx.env);
  mkdirSync(join(fx.home, ".claude"), { recursive: true });
  writeFileSync(settingsPath, "{ this is not json ", "utf8");

  expect(() => installHooks({ ...baseOpts(fx), kind: "claude-code" })).toThrow(HookInstallError);
  // The bad file is left untouched.
  expect(readFileSync(settingsPath, "utf8")).toBe("{ this is not json ");
});

// --- pi: install + idempotence + update ----------------------------------------------------------

test("pi install copies the extension, then is a noop, then updates on content change", () => {
  const fx = makeFixture();
  const target = piExtensionTargetPath(fx.env);

  const first = installHooks({ ...baseOpts(fx), kind: "pi" });
  expect(first.actions[0]!.action).toBe("create");
  expect(existsSync(target)).toBe(true);
  expect(readFileSync(target, "utf8")).toBe(readFileSync(fx.piScript, "utf8"));

  const second = installHooks({ ...baseOpts(fx), kind: "pi" });
  expect(second.actions[0]!.action).toBe("noop");

  // Change the source payload ⇒ an update.
  writeFileSync(fx.piScript, "// tally-session extension payload v2\n", "utf8");
  const third = installHooks({ ...baseOpts(fx), kind: "pi" });
  expect(third.actions[0]!.action).toBe("update");
  expect(readFileSync(target, "utf8")).toContain("v2");
});

test("pi honors PI_CODING_AGENT_DIR override", () => {
  const fx = makeFixture();
  const piDir = join(fx.root, "custom-pi");
  const env = { ...fx.env, PI_CODING_AGENT_DIR: piDir };
  installHooks({ ...baseOpts(fx), env, kind: "pi" });
  expect(existsSync(join(piDir, "extensions", "tally-session.ts"))).toBe(true);
});

// --- dry-run: plan without writes ----------------------------------------------------------------

test("dry-run computes actions but writes nothing", () => {
  const fx = makeFixture();
  const result = installHooks({ ...baseOpts(fx), dryRun: true });
  expect(result.dryRun).toBe(true);
  // Both kinds planned.
  expect(result.actions.map((a) => a.kind).sort()).toEqual(["claude-code", "pi"]);
  expect(result.actions.every((a) => a.action === "create")).toBe(true);
  // Nothing on disk.
  expect(existsSync(claudeSettingsPath(fx.env))).toBe(false);
  expect(existsSync(piExtensionTargetPath(fx.env))).toBe(false);
});

// --- default kind = both -------------------------------------------------------------------------

test("omitting --kind installs both harnesses", () => {
  const fx = makeFixture();
  const result = installHooks(baseOpts(fx));
  expect(result.actions.map((a) => a.kind).sort()).toEqual(["claude-code", "pi"]);
  expect(existsSync(claudeSettingsPath(fx.env))).toBe(true);
  expect(existsSync(piExtensionTargetPath(fx.env))).toBe(true);
});

// --- missing source script -----------------------------------------------------------------------

test("a missing source script fails loudly", () => {
  const fx = makeFixture();
  expect(() =>
    installHooks({ ...baseOpts(fx), ccHookScript: join(fx.root, "nope.ts"), kind: "claude-code" }),
  ).toThrow(HookInstallError);
});
