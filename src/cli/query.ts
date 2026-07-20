// tally CLI — the `query` noun: read-time joins over the JSON-projection contract
// (IMPLEMENTATION-PLAN M3.1; CLI-SURFACE §1.5).
//
//   tally query status  [--pool <p>]   — per-pool lease/queue depth + protocol_version (query.status RPC; the ping)
//   tally query log     [...]          — witness ledger + journald TALLY_* as ONE filtered feed (DAEMONLESS read)
//   tally query render  [...]          — Workspace→Session→Pane tree / status / ledger projection (query.render RPC)
//   tally query standup [...]          — the four-log read-time-join digest (delegates to standup.ts; DAEMONLESS)
//
// `query status`/`query render` are socket RPCs. `query log` and `query standup` read journald +
// the ledger + the four logs DIRECTLY (no daemon RPC — CLI-SURFACE §1.5, IMPLEMENTATION-PLAN §3
// "query log / query standup read journald + ledger + the four logs directly").
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Exec, ExecOptions, ExecResult, ExecStream } from "../contracts/exec";
import { JournalReader, type JournalEntry, type ReadOptions } from "../journal/reader";
import { ledgerPath } from "../contracts/paths";
import { verifyLedgerFile } from "../witness/verify";
import { TALLY_EVENTS, type TallyEvent } from "../contracts/journal";
import { connectClient } from "./client";
import { printError, printJson, printJsonLines, printLine, renderTree, type TreeWorkspace, type Writer } from "./output";
import { flag, hasFlag, wantsJson, type CliContext } from "./index";
import { clientOpts } from "./queue";
import { runStandup } from "./standup";

/** Route the `query` noun. */
export async function runQuery(ctx: CliContext): Promise<number> {
  switch (ctx.verb) {
    case "status":
      return doStatus(ctx);
    case "log":
      return doLog(ctx);
    case "render":
      return doRender(ctx);
    case "standup":
      return runStandup(ctx);
    default:
      printError(ctx.writer, `unknown query verb '${ctx.verb ?? "(none)"}' (expected status|log|render|standup)`);
      return 2;
  }
}

// ---------------------------------------------------------------------------------------------
// query status — the ping (per-pool lease/queue depth + protocol_version).
// ---------------------------------------------------------------------------------------------

interface QueryStatusResult {
  protocol_version: number;
  pools: Array<{
    pool: string;
    held: number;
    queued: number;
    budget?: number;
    // Additive (CLI-SURFACE §2.5, not a protocol bump) — the broker's own waiting-ticket count and a
    // divergence flag, so a daemon/broker split (issue #5) reads in one command instead of hiding
    // behind the engine-only `queued` figure.
    broker_queued?: number;
    diverged?: boolean;
  }>;
  sessions: Array<{ session: string; status_rollup: Record<string, number> }>;
}

async function doStatus(ctx: CliContext): Promise<number> {
  const params: { pool?: string } = {};
  const pool = flag(ctx.args, "--pool");
  if (pool !== undefined) params.pool = pool;

  const client = await connectClient(clientOpts(ctx));
  try {
    const result = await client.call<QueryStatusResult>("query.status", params);
    if (wantsJson(ctx.args)) {
      printJson(ctx.writer, result);
    } else {
      printLine(ctx.writer, `protocol ${result.protocol_version}`);
      for (const p of result.pools) {
        const broker = p.broker_queued !== undefined ? ` broker_queued=${p.broker_queued}` : "";
        const divergedFlag = p.diverged ? " DIVERGED" : "";
        printLine(
          ctx.writer,
          `  ${p.pool}: held=${p.held} queued=${p.queued}${p.budget !== undefined ? ` budget=${p.budget}` : ""}${broker}${divergedFlag}`,
        );
      }
      for (const s of result.sessions) {
        const roll = (["blocked", "working", "done", "idle"] as const).map((k) => `${k[0]}${s.status_rollup[k] ?? 0}`).join(" ");
        printLine(ctx.writer, `  session ${s.session} [${roll}]`);
      }
    }
    return 0;
  } finally {
    client.close();
  }
}

// ---------------------------------------------------------------------------------------------
// query render — the grouping-tier projection (query.render RPC).
// ---------------------------------------------------------------------------------------------

async function doRender(ctx: CliContext): Promise<number> {
  const format = flag(ctx.args, "--format") ?? (wantsJson(ctx.args) ? "json" : "text");
  const KNOWN_FORMATS = ["text", "json", "jsonl", "tree", "jcal"];
  if (!KNOWN_FORMATS.includes(format)) {
    printError(ctx.writer, `unknown --format '${format}' (expected ${KNOWN_FORMATS.join("|")})`);
    return 2;
  }
  const scope = flag(ctx.args, "--scope") ?? "sessions";
  const KNOWN_SCOPES = ["sessions", "queue", "witness"];
  if (!KNOWN_SCOPES.includes(scope)) {
    printError(ctx.writer, `unknown --scope '${scope}' (expected ${KNOWN_SCOPES.join("|")})`);
    return 2;
  }
  const collapse = hasFlag(ctx.args, "--collapse");

  const client = await connectClient(clientOpts(ctx));
  try {
    // The daemon assembles the projection; the CLI chooses the rendering. Ask for the structured form
    // and render locally for `text`/`tree` so the daemon returns one canonical shape.
    const result = await client.call<{ workspaces?: TreeWorkspace[]; [k: string]: unknown }>("query.render", {
      format,
      scope,
      collapse,
    });
    renderProjection(ctx.writer, format, result);
    return 0;
  } finally {
    client.close();
  }
}

function renderProjection(w: Writer, format: string, result: { workspaces?: TreeWorkspace[]; [k: string]: unknown }): void {
  switch (format) {
    case "json":
      printJson(w, result);
      return;
    case "jsonl": {
      // One record per line: the workspaces (or the raw records the daemon returned).
      const records = Array.isArray(result.workspaces) ? result.workspaces : [result];
      printJsonLines(w, records);
      return;
    }
    case "tree":
      printLine(w, renderTree(result.workspaces ?? []));
      return;
    case "jcal":
      // jCal is an RFC-7265 JSON calendar array — the daemon returns it verbatim; we print as JSON.
      printJson(w, result);
      return;
    case "text":
    default:
      printLine(w, renderTree(result.workspaces ?? []));
      return;
  }
}

// ---------------------------------------------------------------------------------------------
// query log — witness ledger + journald TALLY_* as one filtered feed (DAEMONLESS).
// ---------------------------------------------------------------------------------------------

/** One `query log` feed record — the §1.5 `--json` shape (journald fields + the witness verdict). */
interface LogRecord {
  TALLY_EVENT: string;
  TALLY_TASK_UUID?: string;
  TALLY_GPU_SECONDS?: number;
  TALLY_ARTIFACT_HASH?: string;
  verdict?: string | null;
  ts?: string | null;
}

async function doLog(ctx: CliContext): Promise<number> {
  const opts: ReadOptions = {};
  const since = flag(ctx.args, "--since");
  if (since !== undefined) opts.since = since;
  const task = flag(ctx.args, "--task");
  if (task !== undefined) opts.task = task;
  const session = flag(ctx.args, "--session");
  if (session !== undefined) opts.session = session;
  const event = flag(ctx.args, "--event");
  if (event !== undefined) {
    if (!(TALLY_EVENTS as readonly string[]).includes(event)) {
      printError(ctx.writer, `--event must be one of ${TALLY_EVENTS.join("|")}`);
      return 2;
    }
    opts.event = event as TallyEvent;
  }

  const reader = new JournalReader(bunExec());
  const json = wantsJson(ctx.args);

  if (hasFlag(ctx.args, "--follow")) {
    // Tail: stream matching journald entries as they arrive. Ends on SIGINT/SIGTERM.
    const iter = reader.follow(opts);
    const stop = () => {
      void iter.return?.(undefined);
    };
    process.once("SIGINT", stop);
    process.once("SIGTERM", stop);
    try {
      for await (const entry of iter) {
        emitLogRecord(ctx.writer, entry, json);
      }
    } finally {
      process.removeListener("SIGINT", stop);
      process.removeListener("SIGTERM", stop);
    }
    return 0;
  }

  const entries = await reader.read(opts);
  for (const entry of entries) {
    emitLogRecord(ctx.writer, entry, json);
  }
  return 0;
}

function emitLogRecord(w: Writer, entry: JournalEntry, json: boolean): void {
  const f = entry.fields;
  const record: LogRecord = { TALLY_EVENT: f.TALLY_EVENT };
  if (f.TALLY_TASK_UUID !== undefined) record.TALLY_TASK_UUID = f.TALLY_TASK_UUID;
  if (f.TALLY_GPU_SECONDS !== undefined) record.TALLY_GPU_SECONDS = f.TALLY_GPU_SECONDS;
  if (f.TALLY_ARTIFACT_HASH !== undefined) record.TALLY_ARTIFACT_HASH = f.TALLY_ARTIFACT_HASH;
  // The verdict rides the journald evidence/completion events; surface it when the message carries it.
  const verdict = deriveVerdict(f.TALLY_EVENT);
  if (verdict !== undefined) record.verdict = verdict;
  record.ts = entry.realtimeUs !== null ? new Date(entry.realtimeUs / 1000).toISOString() : null;

  if (json) {
    printJson(w, record);
  } else {
    const gpu = record.TALLY_GPU_SECONDS !== undefined ? ` gpu=${record.TALLY_GPU_SECONDS}s` : "";
    printLine(w, `${record.ts ?? "-"}  ${record.TALLY_EVENT}  ${record.TALLY_TASK_UUID ?? "-"}${gpu}${verdict ? ` ${verdict}` : ""}`);
  }
}

/** The verdict a journald event implies (evidence_pass ⇒ pass; evidence_fail ⇒ clean-exit-no-artifact). */
function deriveVerdict(event: string): string | undefined {
  if (event === "evidence_pass" || event === "completed") return "pass";
  if (event === "evidence_fail") return "clean-exit-no-artifact";
  if (event === "failed") return "failed";
  return undefined;
}

/** Exported for tests: the daemonless ledger-verify convenience the render/witness paths reuse. */
export function verifyLedgerAt(env: Parameters<typeof ledgerPath>[0]): ReturnType<typeof verifyLedgerFile> {
  return verifyLedgerFile(ledgerPath(env));
}

// ---------------------------------------------------------------------------------------------
// The Bun-backed production `Exec` for the daemonless read paths (query log / standup).
// ---------------------------------------------------------------------------------------------

/**
 * A minimal Bun-backed `Exec` for the CLI's daemonless read paths (`query log`/`query standup` shell
 * `journalctl`, `git log`, `task export`). The daemon and jobs modules take an injected `Exec` from
 * the composition root; the CLI's read-only verbs run outside the daemon, so this module owns a small
 * production `Exec` shared by `query.ts` and `standup.ts`. Fully replaced by a fake in tests, which
 * inject their own `Exec` into `JournalReader`/the standup join.
 */
export function bunExec(): Exec {
  return {
    async run(argv: string[], opts: ExecOptions = {}): Promise<ExecResult> {
      const [cmd, ...rest] = argv;
      if (cmd === undefined) return { code: 127, stdout: "", stderr: "empty argv" };
      const spawnOpts: Parameters<typeof Bun.spawn>[1] = {
        stdin: opts.stdin !== undefined ? new TextEncoder().encode(typeof opts.stdin === "string" ? opts.stdin : new TextDecoder().decode(opts.stdin)) : "ignore",
        stdout: "pipe",
        stderr: "pipe",
      };
      if (opts.cwd !== undefined) spawnOpts.cwd = opts.cwd;
      if (opts.env !== undefined) spawnOpts.env = { ...process.env, ...opts.env };
      const proc = Bun.spawn([cmd, ...rest], spawnOpts);
      let timedOut = false;
      let timer: ReturnType<typeof setTimeout> | null = null;
      if (opts.timeoutMs !== undefined && opts.timeoutMs > 0) {
        timer = setTimeout(() => {
          timedOut = true;
          proc.kill();
        }, opts.timeoutMs);
      }
      const [stdout, stderr, code] = await Promise.all([
        new Response(proc.stdout).text(),
        new Response(proc.stderr).text(),
        proc.exited,
      ]);
      if (timer) clearTimeout(timer);
      const result: ExecResult = { code, stdout, stderr };
      if (timedOut) result.timedOut = true;
      return result;
    },
    spawn(argv: string[], opts: ExecOptions = {}): ExecStream {
      const [cmd, ...rest] = argv;
      const spawnOpts: Parameters<typeof Bun.spawn>[1] = { stdout: "pipe", stderr: "pipe", stdin: "pipe" };
      if (opts.cwd !== undefined) spawnOpts.cwd = opts.cwd;
      if (opts.env !== undefined) spawnOpts.env = { ...process.env, ...opts.env };
      const proc = Bun.spawn([cmd ?? "", ...rest], spawnOpts);
      const decoder = new TextDecoder();
      async function* lines(): AsyncIterableIterator<string> {
        let buf = "";
        const reader = proc.stdout.getReader();
        try {
          for (;;) {
            const { done, value } = await reader.read();
            if (done) break;
            buf += decoder.decode(value, { stream: true });
            let nl: number;
            while ((nl = buf.indexOf("\n")) !== -1) {
              yield buf.slice(0, nl);
              buf = buf.slice(nl + 1);
            }
          }
          if (buf.length > 0) yield buf;
        } finally {
          reader.releaseLock();
        }
      }
      const stream: ExecStream = {
        lines,
        async write(data: string | Uint8Array): Promise<void> {
          const stdin = proc.stdin;
          if (stdin && typeof (stdin as { write?: unknown }).write === "function") {
            (stdin as { write: (d: string | Uint8Array) => void }).write(data);
          }
        },
        kill(signal?: NodeJS.Signals | number): void {
          proc.kill(signal as number | undefined);
        },
        exited: proc.exited,
      };
      if (proc.pid !== undefined) stream.pid = proc.pid;
      return stream;
    },
  };
}
