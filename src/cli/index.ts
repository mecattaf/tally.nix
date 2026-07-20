// tally CLI — the frozen §1 verb tree dispatcher + entrypoint (IMPLEMENTATION-PLAN M3.1;
// CLI-SURFACE §0, §1). Every verb is a thin socket request (`--json` everywhere; JSON is the
// contract, text a convenience), verb-prefix `tally <noun> <verb>` with the single top-level alias
// `tally enqueue`. This module owns: the argv tokenizer + flag parser, the noun/verb routing table,
// `--help`/`--version`, and the `runCli` entrypoint the composition root registers via
// `registerEntrypoint("cli", …)` in `src/main.ts`.
//
// The noun handlers live in the sibling files (queue/session/pane/agent/query/standup/witness-cmd/
// internal); each is a pure function of a parsed `CliContext` → exit code, so the whole surface is
// testable without spawning a process. Imports only from `src/contracts` + this module's declared
// `dependsOn` (daemon-core client side is our own `client.ts`; jobs/witness/journal/taskchampion for
// the daemonless read paths).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { registerEntrypoint } from "../registry";
import type { PathEnv } from "../contracts/paths";
import { processWriter, printError, type Writer } from "./output";
import { RpcError, DaemonUnreachable } from "./client";
import { runQueue, runEnqueueAlias } from "./queue";
import { runSession } from "./session";
import { runPane } from "./pane";
import { runAgent } from "./agent";
import { runQuery } from "./query";
import { runWitnessCmd } from "./witness-cmd";
import { runInternal } from "./internal";

/** Informational binary version surfaced by `--version` when the daemon is not reached. */
export const DAEMON_VERSION_FALLBACK = "0.1.0";

/** The frozen §1 verb tree (CLI-SURFACE §1). Printed by `tally --help` / `tally help`. */
export const HELP_TEXT = `tally ${DAEMON_VERSION_FALLBACK} — agent-session orchestration (one binary: daemon + CLI)

Usage: tally <noun> <verb> [args] [--json]
       tally enqueue ...            (top-level alias of \`tally queue enqueue\`)
       tally <command> --help

The JSON projection (\`--json\`) is the contract; text output is a convenience.

queue — control plane (Seam A)
  tally enqueue                    admit one spawn-tracked-agent-job (alias of queue enqueue)
    --priority <high|medium|low>   --source <r2|gh|calendar|manual|orchestrator>
    --kind <pi|claude-code|shell>  --invocation "<cmd>" | -- <argv...>
    --cwd <path> | --worktree <branch>
    --evidence <artifact:<path>|hash:<algo>|exit:<code>>   (repeatable)
    --pool <worker-gpu|controller-gpu|sub:<acct>|api>      --model-class <class>
    --dedup-key <key>              --session <persistence_session_id>
    --barrier <gid> | --wait-group <gid> | --wait-count <N> | --wait | --timeout <dur> | --detach
  tally queue enqueue ...          canonical form of the above
  tally queue cancel <uuid|selector> [--force]
  tally queue pause  [pool | --all]
  tally queue resume [pool | --all]

session — read/observe existing zmx sessions (lifecycle is dotfiles-owned)
  tally session list  [--short] [--workspace <w>]
  tally session watch [session | --all] [--snapshot-only] [--since <seq>] [--format <jsonl|tree>]   (Seam B)

pane — kitty-native binding (keyed on kitty_window_id)
  tally pane send     <sel> <text> [--enter]
  tally pane send-key <sel> <key>
  tally pane focus    <sel>
  tally pane capture  <sel> [--source <visible|recent|detection>] [--lines <n>] [--format <text|ansi>]

agent — read-projections of the in-daemon detector (agent panes only; no \`agent start\`)
  tally agent list    [--status <s>] [--kind <k>]
  tally agent get     <sel>
  tally agent read    <sel> [--source <s>] [--format <f>]
  tally agent explain <sel>
  tally agent wait    <sel> [--status <done|blocked|idle|working>] [--timeout <dur>] [--count <n>]
  tally agent send    <sel> <text> [--enter]
  tally agent focus   <sel>

query — read-time joins over the JSON-projection contract
  tally query status  [--pool <p>]
  tally query log     [--task <u>] [--session <s>] [--event <e>] [--since <t>] [--follow]
  tally query render  [--format <text|json|jsonl|tree|jcal>] [--scope <sessions|queue|witness>] [--collapse]
  tally query standup [--since <t>] [--source <s>] [--format <text|json|md>]

witness
  tally witness verify [--ledger <path>]

internal
  tally daemon run                 run the daemon (systemd ExecStart)
  tally daemon drain               thin socket client — trigger the events/ drain (queue.drain RPC)
  tally pls-wrap -- <cmd>          run <cmd> under a pls GPU lease (ambient default)
  tally hooks install [--kind <claude-code|pi>] [--dry-run]

  tally --help                     show this verb tree
  tally --version                  print daemon/protocol version
`;

// ---------------------------------------------------------------------------------------------
// Argv tokenizer + flag parser (hand-rolled — no dependency, IMPLEMENTATION-PLAN "no new deps").
// ---------------------------------------------------------------------------------------------

/**
 * A parsed argv: positionals (in order), a flag map (`--flag value` or bare boolean `--flag`), and
 * the raw tail after a literal `--` (the `enqueue -- <argv…>` passthrough). `--flag=value` and
 * `--flag value` both parse; a repeated flag accumulates into an array.
 */
export interface ParsedArgs {
  positionals: string[];
  flags: Map<string, string[]>;
  /** Tokens after a literal `--` (the leaf-worker argv for `enqueue`). Undefined when no `--` seen. */
  passthrough?: string[];
}

/** Boolean flags that never consume a following token (they are presence-only). */
const BOOLEAN_FLAGS = new Set<string>([
  "--json",
  "--all",
  "--short",
  "--snapshot-only",
  "--collapse",
  "--follow",
  "--enter",
  "--wait",
  "--detach",
  "--force",
  "--dry-run",
  "--via-terminal",
  "--help",
  "-h",
  "--version",
  "-v",
]);

/**
 * Tokenize an argv slice into positionals + flags. A literal `--` ends flag/positional parsing and
 * captures the remaining tokens as `passthrough` (the enqueue leaf-worker argv). `--flag=value`
 * splits on the first `=`; `--flag value` consumes the next token unless `--flag` is a known boolean.
 */
export function parseArgs(argv: readonly string[]): ParsedArgs {
  const positionals: string[] = [];
  const flags = new Map<string, string[]>();
  let passthrough: string[] | undefined;

  const push = (name: string, value: string) => {
    const arr = flags.get(name);
    if (arr) arr.push(value);
    else flags.set(name, [value]);
  };

  for (let i = 0; i < argv.length; i++) {
    const tok = argv[i]!;
    if (tok === "--") {
      passthrough = argv.slice(i + 1);
      break;
    }
    if (tok.startsWith("--") && tok.length > 2) {
      const eq = tok.indexOf("=");
      if (eq !== -1) {
        push(tok.slice(0, eq), tok.slice(eq + 1));
        continue;
      }
      if (BOOLEAN_FLAGS.has(tok)) {
        push(tok, "true");
        continue;
      }
      // Value flag: consume the next token if it exists and is not another flag / `--`.
      const next = argv[i + 1];
      if (next !== undefined && next !== "--" && !next.startsWith("--")) {
        push(tok, next);
        i++;
      } else {
        // A value flag with no value present — record its presence (validation happens per-verb).
        push(tok, "true");
      }
      continue;
    }
    if (tok === "-h" || tok === "-v") {
      push(tok, "true");
      continue;
    }
    positionals.push(tok);
  }

  const out: ParsedArgs = { positionals, flags };
  if (passthrough !== undefined) out.passthrough = passthrough;
  return out;
}

/** First value of a flag, or undefined. */
export function flag(args: ParsedArgs, name: string): string | undefined {
  const arr = args.flags.get(name);
  return arr ? arr[arr.length - 1] : undefined;
}

/** All values of a repeatable flag (e.g. `--evidence`). */
export function flagAll(args: ParsedArgs, name: string): string[] {
  return args.flags.get(name) ?? [];
}

/** Whether a boolean flag is present. */
export function hasFlag(args: ParsedArgs, name: string): boolean {
  return args.flags.has(name);
}

/** Whether `--json` (the contract projection) was requested. */
export function wantsJson(args: ParsedArgs): boolean {
  return args.flags.has("--json");
}

// ---------------------------------------------------------------------------------------------
// The CLI context — everything a verb handler needs, injectable for tests.
// ---------------------------------------------------------------------------------------------

/** The shared context passed to every verb handler. */
export interface CliContext {
  /** The noun (`queue`/`session`/…) or the top-level alias verb (`enqueue`). */
  noun: string;
  /** The verb under the noun (`enqueue`/`list`/…), or undefined for a noun-only invocation. */
  verb: string | undefined;
  /** The parsed args of everything AFTER the noun+verb. */
  args: ParsedArgs;
  /** The output sink. */
  writer: Writer;
  /**
   * The environment (path/socket resolution + `$KITTY_WINDOW_ID` for viewer registration + any keys
   * the dispatched internal verbs read, e.g. `$CLAUDE_CONFIG_DIR`/`$HOME` for `hooks install`).
   */
  env: PathEnv & { KITTY_WINDOW_ID?: string } & Record<string, string | undefined>;
  /** Explicit socket path override (tests point this at a tmp socket). */
  socket?: string;
}

/** A verb handler resolves to a process exit code. */
export type VerbHandler = (ctx: CliContext) => number | Promise<number>;

// ---------------------------------------------------------------------------------------------
// Help / version.
// ---------------------------------------------------------------------------------------------

function printHelp(w: Writer): number {
  w.out(HELP_TEXT);
  return 0;
}

function printVersion(w: Writer, asJson: boolean): number {
  if (asJson) {
    w.out(JSON.stringify({ daemon_version: DAEMON_VERSION_FALLBACK, protocol_version: 1 }) + "\n");
  } else {
    w.out(`tally ${DAEMON_VERSION_FALLBACK} (protocol 1)\n`);
  }
  return 0;
}

// ---------------------------------------------------------------------------------------------
// Dispatch.
// ---------------------------------------------------------------------------------------------

/** The noun set (CLI-SURFACE §1) plus the top-level alias + internal verbs. */
const NOUNS = new Set(["queue", "session", "pane", "agent", "query", "witness"]);
const INTERNAL = new Set(["daemon", "pls-wrap", "hooks"]);

/**
 * Run the CLI over the post-dispatch token list (everything after `tally`). Returns the process exit
 * code. Fully injectable: `writer`/`env`/`socket` default to the process, but tests pass fakes.
 */
export async function runCli(
  tokens: readonly string[],
  opts: { writer?: Writer; env?: CliContext["env"]; socket?: string } = {},
): Promise<number> {
  const writer = opts.writer ?? processWriter;
  const env = opts.env ?? (process.env as CliContext["env"]);

  const first = tokens[0];

  // Global help / version (before any noun).
  if (first === undefined || first === "--help" || first === "-h" || first === "help") {
    return printHelp(writer);
  }
  if (first === "--version" || first === "-v" || first === "version") {
    return printVersion(writer, tokens.includes("--json"));
  }

  // A trailing `--help` anywhere prints the top-level tree (the §1 verb tree; step-1 acceptance).
  const wantsHelp = tokens.includes("--help") || tokens.includes("-h");

  try {
    // Top-level alias: `tally enqueue …` === `tally queue enqueue …` (CLI-SURFACE §0).
    if (first === "enqueue") {
      if (wantsHelp) return printHelp(writer);
      const ctx: CliContext = buildCtx("queue", "enqueue", tokens.slice(1), writer, env, opts.socket);
      return await runEnqueueAlias(ctx);
    }

    if (NOUNS.has(first)) {
      // `--help` is intercepted before any verb dispatch/validation, whether it trails the noun
      // (`tally queue --help`) or the noun+verb (`tally queue enqueue --help`) — the verb tree is
      // the only help text this surface has, so every subcommand shows the same page rather than
      // falling through into argument validation it can't satisfy.
      if (wantsHelp) return printHelp(writer);
      const verb = tokens[1];
      const ctx = buildCtx(first, verb, tokens.slice(2), writer, env, opts.socket);
      return await dispatchNoun(ctx);
    }

    if (INTERNAL.has(first)) {
      if (wantsHelp) return printHelp(writer);
      const verb = tokens[1];
      const ctx = buildCtx(first, verb, tokens.slice(2), writer, env, opts.socket);
      return await runInternal(ctx);
    }

    printError(writer, `'${first}' is not a tally command. Try 'tally --help'.`);
    return 127;
  } catch (err) {
    return handleError(writer, err);
  }
}

function buildCtx(
  noun: string,
  verb: string | undefined,
  rest: readonly string[],
  writer: Writer,
  env: CliContext["env"],
  socket: string | undefined,
): CliContext {
  const ctx: CliContext = { noun, verb, args: parseArgs(rest), writer, env };
  if (socket !== undefined) ctx.socket = socket;
  return ctx;
}

/** Route a `<noun> <verb>` to its handler group. */
async function dispatchNoun(ctx: CliContext): Promise<number> {
  switch (ctx.noun) {
    case "queue":
      return runQueue(ctx);
    case "session":
      return runSession(ctx);
    case "pane":
      return runPane(ctx);
    case "agent":
      return runAgent(ctx);
    case "query":
      return runQuery(ctx);
    case "witness":
      return runWitnessCmd(ctx);
    default:
      printError(ctx.writer, `unknown noun '${ctx.noun}'`);
      return 127;
  }
}

/** Map a thrown error to an exit code + a stderr line. The one place error → exit-code lives. */
export function handleError(writer: Writer, err: unknown): number {
  if (err instanceof DaemonUnreachable) {
    printError(writer, err.message.replace(/^tally:\s*/, ""));
    return 3;
  }
  if (err instanceof RpcError) {
    const suffix = err.data && Object.keys(err.data).length > 0 ? ` (${JSON.stringify(err.data)})` : "";
    printError(writer, `${err.method}: ${err.message}${suffix} [${err.code}]`);
    // Distinguish "known but not served" / validation from transport for scripts.
    if (err.code === "invalid_params") return 2;
    if (err.code === "not_found") return 4;
    if (err.code === "viewer_rejected") return 5;
    return 1;
  }
  if (err instanceof Error) {
    printError(writer, err.message);
    return 1;
  }
  printError(writer, String(err));
  return 1;
}

// ---------------------------------------------------------------------------------------------
// Entrypoint registration (the composition seam; `main.ts` dispatches to this for the CLI role).
// ---------------------------------------------------------------------------------------------

/**
 * The `registerEntrypoint("cli", …)` payload: `main.ts` hands us the post-role token slice (it
 * already stripped `argv[0..1]`). We run the CLI and return the exit code.
 */
export function registerCli(): void {
  registerEntrypoint("cli", (argv: string[]) => runCli(argv));
}

// Self-register on import so the composition root only needs to `import "./cli"` (or the barrel).
registerCli();
