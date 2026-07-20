#!/usr/bin/env bun
/**
 * tally — the single entry point for the one Bun-compiled binary (daemon + CLI).
 *
 * `argv[1] === "daemon"` boots the daemon (layer 1); anything else dispatches to the CLI
 * (layer 3). Both are wired through a tiny **entrypoint registry** so that layer 0 compiles
 * and runs standalone: until the daemon-core (M1.1) and cli (M3.1) modules register their
 * real entrypoints, `main.ts` provides self-contained fallbacks that satisfy the
 * BUILD-SEQUENCE step-1 acceptance shape — `tally --help` prints the frozen §1 verb tree,
 * `tally daemon run` opens the Unix socket at `$XDG_RUNTIME_DIR/tally/tally.sock` and answers
 * a stub `session.snapshot` over NDJSON framing.
 *
 * The registry is the composition seam. `main.ts` NEVER imports daemon-core or cli internals
 * directly (that would break the disjoint-ownership + layered-build discipline); instead the
 * real modules attach themselves by calling `registerEntrypoint("daemon" | "cli", fn)` from a
 * composition root that layer 1/3 wire in later. This keeps scaffold (layer 0) importing
 * nothing from its siblings while leaving one obvious, typed insertion point for them.
 *
 * Owned by module M0.1 `scaffold`. Do not add real RPC surface here — that is daemon-core's.
 */

import { StringDecoder } from "node:string_decoder";

export const DAEMON_VERSION = "0.1.0";
export const PROTOCOL_VERSION = 1;

// The entrypoint registry lives in a leaf module (`registry.ts`) so the self-registering CLI can read a
// fully-initialized registry through the cyclic import graph (main → entrypoints → cli → registry). See
// `registry.ts` for the temporal-dead-zone rationale. `main.ts` re-exports the seam so existing
// importers (`cli/index.ts`) keep working unchanged.
export {
  registerEntrypoint,
  resolveEntrypoint,
  type Entrypoint,
  type EntrypointRole,
} from "./registry";
import { resolveEntrypoint, type EntrypointRole } from "./registry";

/** The frozen §1 verb tree (CLI-SURFACE §1). Printed by `tally --help` / `tally help`. */
const HELP_TEXT = `tally ${DAEMON_VERSION} — agent-session orchestration (one binary: daemon + CLI)

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

function printHelp(): number {
  process.stdout.write(HELP_TEXT);
  return 0;
}

function printVersion(argv: string[]): number {
  const asJson = argv.includes("--json");
  if (asJson) {
    process.stdout.write(
      JSON.stringify({ daemon_version: DAEMON_VERSION, protocol_version: PROTOCOL_VERSION }) + "\n",
    );
  } else {
    process.stdout.write(`tally ${DAEMON_VERSION} (protocol ${PROTOCOL_VERSION})\n`);
  }
  return 0;
}

/** Resolve the socket path per CLI-SURFACE §2.1: `$XDG_RUNTIME_DIR/tally/tally.sock`. */
export function socketPath(env: Record<string, string | undefined> = process.env): string {
  const runtimeDir = env["XDG_RUNTIME_DIR"] ?? `/run/user/${process.getuid?.() ?? 1000}`;
  return `${runtimeDir}/tally/tally.sock`;
}

/**
 * Layer-0 fallback daemon. Opens the Unix socket with NDJSON framing and answers exactly the
 * step-1 acceptance surface: a stub `session.snapshot` request/response and `--help`-adjacent
 * introspection. daemon-core (M1.1) replaces this wholesale via `registerEntrypoint("daemon")`.
 * Kept minimal on purpose: it proves the transport shape without pretending to be the real RPC.
 */
async function fallbackDaemon(argv: string[]): Promise<number> {
  const sub = argv[0];
  if (sub === "drain") {
    // The real drain is a thin socket client (M2.5 / M3.1). At layer 0 there is no daemon to
    // reach in the general case; report the not-yet-wired state honestly rather than pretend.
    process.stderr.write("tally: daemon drain requires a running daemon (not wired at layer 0)\n");
    return 1;
  }
  if (sub !== undefined && sub !== "run") {
    process.stderr.write(`tally: unknown daemon subcommand '${sub}' (expected 'run' or 'drain')\n`);
    return 2;
  }

  const path = socketPath();
  const dir = path.slice(0, path.lastIndexOf("/"));
  await Bun.$`mkdir -p ${dir}`.quiet().nothrow();
  // Fresh-bind: a stale socket file blocks listen(). Best-effort unlink.
  await Bun.$`rm -f ${path}`.quiet().nothrow();

  const stubSnapshot = () => ({
    protocol: "tally.delta",
    protocol_version: PROTOCOL_VERSION,
    daemon_version: DAEMON_VERSION,
    lease_epoch: 0,
    seq: 0,
    ts: new Date().toISOString(),
    focus: null,
    workspaces: [],
    sessions: [],
    panes: [],
    agents: [],
    jobs: [],
  });

  const server = Bun.listen<{ buf: string; decoder: StringDecoder }>({
    unix: path,
    socket: {
      open(socket) {
        socket.data = { buf: "", decoder: new StringDecoder("utf8") };
      },
      data(socket, chunk) {
        // A stateful StringDecoder retains a partial multibyte codepoint across chunks, so a codepoint
        // split at a socket boundary is never corrupted (matches the daemon-core framing contract).
        socket.data.buf += socket.data.decoder.write(chunk);
        let nl: number;
        while ((nl = socket.data.buf.indexOf("\n")) !== -1) {
          const line = socket.data.buf.slice(0, nl);
          socket.data.buf = socket.data.buf.slice(nl + 1);
          if (line.length === 0) continue;
          let req: { id?: unknown; method?: unknown };
          try {
            req = JSON.parse(line) as { id?: unknown; method?: unknown };
          } catch {
            socket.write(JSON.stringify({ id: null, error: { code: "parse_error", message: "invalid JSON frame" } }) + "\n");
            continue;
          }
          const id = req.id ?? null;
          if (req.method === "session.snapshot") {
            socket.write(JSON.stringify({ id, result: stubSnapshot() }) + "\n");
          } else {
            socket.write(
              JSON.stringify({
                id,
                error: { code: "method_not_found", message: `method '${String(req.method)}' not served by the layer-0 stub daemon` },
              }) + "\n",
            );
          }
        }
      },
    },
  });

  // Best-effort permissions: single-operator local socket (§2.1 mode 0600).
  await Bun.$`chmod 600 ${path}`.quiet().nothrow();

  process.stderr.write(`tally: stub daemon listening on ${path} (layer-0 fallback; answers session.snapshot)\n`);

  await new Promise<void>((resolve) => {
    const stop = () => {
      server.stop(true);
      resolve();
    };
    process.on("SIGINT", stop);
    process.on("SIGTERM", stop);
  });
  return 0;
}

/**
 * Layer-0 fallback CLI. Handles the universally-available introspection verbs
 * (`--help`/`help`, `--version`) so the binary is useful at layer 0; every other verb reports
 * that the CLI surface is not yet wired. cli (M3.1) replaces this via
 * `registerEntrypoint("cli")`.
 */
function fallbackCli(argv: string[]): number {
  const first = argv[0];
  if (first === undefined || first === "--help" || first === "-h" || first === "help") {
    return printHelp();
  }
  if (first === "--version" || first === "-v" || first === "version") {
    return printVersion(argv);
  }
  if (argv.includes("--help") || argv.includes("-h")) {
    return printHelp();
  }
  process.stderr.write(
    `tally: '${first}' is not wired at layer 0 (the CLI surface lands in M3.1). Try 'tally --help'.\n`,
  );
  return 127;
}

/**
 * Dispatch `argv` (the full process argv). Both the compiled binary and `bun src/main.ts`
 * present `process.argv` as `[runtimeOrSelfPath, scriptOrSelfPath, ...userArgs]` — verified
 * identical in both modes — so the first real user token is `argv[2]`.
 * Exported so tests can drive it without spawning a subprocess.
 */
export async function run(argv: string[]): Promise<number> {
  const tokens = argv.slice(2);
  const role: EntrypointRole = tokens[0] === "daemon" ? "daemon" : "cli";
  const rest = role === "daemon" ? tokens.slice(1) : tokens;

  const registered = resolveEntrypoint(role);
  if (registered) return await registered(rest);

  return role === "daemon" ? await fallbackDaemon(rest) : fallbackCli(rest);
}

// Import the entrypoint wiring for its side effects: `./entrypoints` runs `registerCli()` (the CLI
// self-registration) and `registerEntrypoint("daemon", runComposedDaemon)` — so the compiled binary
// ships the fully-composed daemon + real CLI surface, superseding the layer-0 fallbacks above. This is
// the composition seam: `main.ts` never statically imports daemon-core/cli internals; the wiring
// module does. Placed AFTER the fallback definitions so the registry is populated before `run` reads it
// (ES module imports are hoisted + evaluated before this module's body runs, so the registration is in
// place by the time `import.meta.main` dispatches).
import "./entrypoints";

// Execute only when run as the program entry, never when imported by a test.
if (import.meta.main) {
  run(process.argv)
    .then((code) => {
      process.exitCode = code;
    })
    .catch((err: unknown) => {
      process.stderr.write(`tally: fatal: ${err instanceof Error ? err.stack ?? err.message : String(err)}\n`);
      process.exitCode = 1;
    });
}
