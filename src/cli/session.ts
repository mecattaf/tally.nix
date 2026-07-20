// tally CLI — the `session` noun: read/observe existing zmx sessions (IMPLEMENTATION-PLAN M3.1;
// CLI-SURFACE §1.2). Lifecycle is dotfiles-owned; tally holds only read/observe verbs.
//
//   tally session list  [--short] [--workspace <w>]     — zmx-delegated enumeration join (session.list RPC)
//   tally session watch [session | --all] (Seam B)      — snapshot-then-events over the delta stream
//
// `session watch` marks its OWN pane `is_viewer=true` (anti-loop invariant #4): before subscribing it
// reads `$KITTY_WINDOW_ID` from its environment and posts `session.register_viewer {kitty_window_id}`,
// so the detector and `pane capture --source detection` / `session.wait pane_output` exclude it. It
// then reads the §2.2 snapshot, subscribes from `snapshot.seq`, and streams events (`--format
// jsonl|tree`, `--snapshot-only`, `--since <seq>`, `--all`).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Snapshot } from "../contracts/snapshot";
import { connectClient, type TallyClient, type WireEvent } from "./client";
import type { SubscribeAck } from "../contracts/wire";
import { printError, printJson, printJsonLines, printLine, renderTree, type TreeWorkspace, type Writer } from "./output";
import { flag, hasFlag, wantsJson, type CliContext } from "./index";
import { clientOpts } from "./queue";

/** Route the `session` noun. */
export async function runSession(ctx: CliContext): Promise<number> {
  switch (ctx.verb) {
    case "list":
      return doList(ctx);
    case "watch":
      return doWatch(ctx);
    default:
      printError(ctx.writer, `unknown session verb '${ctx.verb ?? "(none)"}' (expected list|watch)`);
      return 2;
  }
}

// ---------------------------------------------------------------------------------------------
// session list.
// ---------------------------------------------------------------------------------------------

/** One row of `session list` — the §1.2 `--json` shape (Workspace→Session→Pane rollup). */
interface SessionListRow {
  session: string;
  persistence_session_id: string;
  workspace: string;
  status_rollup: Record<string, number>;
  panes: Array<{ pane: string; kitty_window_id: number; agent: { kind: string | null; status: string | null } | null }>;
}

async function doList(ctx: CliContext): Promise<number> {
  const params: { workspace?: string; short?: boolean } = {};
  const workspace = flag(ctx.args, "--workspace");
  if (workspace !== undefined) params.workspace = workspace;
  if (hasFlag(ctx.args, "--short")) params.short = true;

  const client = await connectClient(clientOpts(ctx));
  try {
    const rows = await client.call<SessionListRow[]>("session.list", params);
    if (wantsJson(ctx.args)) {
      printJsonLines(ctx.writer, rows);
    } else {
      renderListText(ctx.writer, rows, params.short === true);
    }
    return 0;
  } finally {
    client.close();
  }
}

function renderListText(w: Writer, rows: readonly SessionListRow[], short: boolean): void {
  if (rows.length === 0) {
    printLine(w, "(no live sessions)");
    return;
  }
  for (const r of rows) {
    const roll = (["blocked", "working", "done", "idle"] as const).map((k) => `${k[0]}${r.status_rollup[k] ?? 0}`).join(" ");
    printLine(w, `${r.session}  [${roll}]  ${r.workspace}`);
    if (short) continue;
    for (const p of r.panes) {
      // `agent` is null for a pane with no detected agent (a plain shell pane).
      const kind = p.agent?.kind ?? "-";
      const status = p.agent?.status ?? "-";
      printLine(w, `    ${p.pane}  win=${p.kitty_window_id}  ${kind}/${status}`);
    }
  }
}

// ---------------------------------------------------------------------------------------------
// session watch (Seam B).
// ---------------------------------------------------------------------------------------------

async function doWatch(ctx: CliContext): Promise<number> {
  const format = flag(ctx.args, "--format") ?? "jsonl";
  if (format !== "jsonl" && format !== "tree") {
    printError(ctx.writer, `unknown --format '${format}' (expected jsonl|tree)`);
    return 2;
  }
  const snapshotOnly = hasFlag(ctx.args, "--snapshot-only");
  const all = hasFlag(ctx.args, "--all");
  const sessionFilter = all ? undefined : ctx.args.positionals[0];
  const since = flag(ctx.args, "--since");

  const client = await connectClient(clientOpts(ctx));
  try {
    // Anti-loop invariant #4: mark our own pane a viewer BEFORE subscribing, so the detector never
    // scrapes tally's own watch output. `$KITTY_WINDOW_ID` is the pane we are running in.
    await registerViewer(client, ctx.env.KITTY_WINDOW_ID);

    const snapshot = await client.call<Snapshot>("session.snapshot");
    emitSnapshot(ctx.writer, snapshot, format, sessionFilter);

    if (snapshotOnly) {
      return 0;
    }

    // Subscribe from the snapshot's seq (§2.2: "subscribe from here") unless `--since` overrides.
    const fromSeq = since !== undefined ? Number(since) : snapshot.seq;
    const streamCode = await streamEvents(client, ctx.writer, {
      fromSeq: Number.isFinite(fromSeq) ? fromSeq : snapshot.seq,
      format,
      sessionFilter,
    });
    return streamCode;
  } finally {
    client.close();
  }
}

/** Post `session.register_viewer` when a `$KITTY_WINDOW_ID` is known (best-effort; harmless if not). */
async function registerViewer(client: TallyClient, kittyWindowId: string | undefined): Promise<void> {
  if (kittyWindowId === undefined) return;
  const id = Number(kittyWindowId);
  if (!Number.isInteger(id)) return;
  try {
    await client.call("session.register_viewer", { kitty_window_id: id });
  } catch {
    // A daemon that does not serve register_viewer (no session-model mounted) is not fatal to a
    // read-only watch; the anti-loop marking is best-effort at the CLI boundary.
  }
}

function emitSnapshot(w: Writer, snapshot: Snapshot, format: "jsonl" | "tree", sessionFilter: string | undefined): void {
  if (format === "tree") {
    printLine(w, renderTree(snapshotToTree(snapshot, sessionFilter)));
    return;
  }
  // jsonl: the snapshot is the seed line, then events follow.
  printJson(w, snapshot);
}

/** Project the §2.2 snapshot into the text-tree shape (Workspace→Session→Pane with status dots). */
function snapshotToTree(snapshot: Snapshot, sessionFilter: string | undefined): TreeWorkspace[] {
  const agentByPane = new Map<string, { kind: string; status: string }>();
  for (const ag of snapshot.agents) {
    agentByPane.set(ag.pane_id, { kind: ag.kind, status: ag.status });
  }
  const sessionsByWs = new Map<string, typeof snapshot.sessions>();
  for (const s of snapshot.sessions) {
    if (sessionFilter !== undefined && s.id !== sessionFilter && s.persistence_session_id !== sessionFilter) continue;
    const list = sessionsByWs.get(s.workspace_id) ?? [];
    list.push(s);
    sessionsByWs.set(s.workspace_id, list);
  }
  const out: TreeWorkspace[] = [];
  for (const ws of snapshot.workspaces) {
    const sessions = sessionsByWs.get(ws.id) ?? [];
    if (sessions.length === 0 && sessionFilter !== undefined) continue;
    out.push({
      workspace: ws.label,
      sessions: sessions.map((s) => ({
        session: s.id,
        status_rollup: { ...s.status_rollup } as Record<string, number>,
        panes: s.pane_ids.map((pid) => {
          const agent = agentByPane.get(pid);
          return {
            pane: pid,
            kind: agent?.kind ?? null,
            status: agent?.status ?? null,
          };
        }),
      })),
    });
  }
  return out;
}

interface StreamOptions {
  fromSeq: number;
  format: "jsonl" | "tree";
  sessionFilter: string | undefined;
}

/**
 * Subscribe and stream events until the connection ends or the process is interrupted. In `jsonl`
 * mode each event is one JSON line; in `tree` mode each event prints a one-line human summary
 * (the full tree is the snapshot seed — the stream is deltas). Returns 0 on a clean stream end.
 */
function streamEvents(client: TallyClient, w: Writer, opts: StreamOptions): Promise<number> {
  return new Promise<number>((resolve) => {
    const unsub = client.onEvent((ev) => {
      // Skip control frames in the projection unless the operator asked for jsonl (they are informative
      // there); a tree view suppresses heartbeats.
      if (opts.sessionFilter !== undefined && !eventMatchesSession(ev, opts.sessionFilter)) return;
      if (opts.format === "tree") {
        if (ev.event === "heartbeat") return;
        printLine(w, summarizeEvent(ev));
      } else {
        printJson(w, ev);
      }
    });

    const done = () => {
      unsub();
      resolve(0);
    };

    const onSig = () => done();
    process.once("SIGINT", onSig);
    process.once("SIGTERM", onSig);

    // A closed connection ends the stream.
    const poll = setInterval(() => {
      if (client.isClosed) {
        clearInterval(poll);
        process.removeListener("SIGINT", onSig);
        process.removeListener("SIGTERM", onSig);
        done();
      }
    }, 100);

    client.call("session.subscribe", { from_seq: opts.fromSeq }).catch(() => {
      // Subscribe failed (e.g. gap → re-snapshot needed). End the stream; the caller reconnects.
      clearInterval(poll);
      done();
    });
  });
}

/** Whether an event frame belongs to the filtered session (best-effort field probe). */
function eventMatchesSession(ev: WireEvent, session: string): boolean {
  const sid = ev.session_id ?? ev.session ?? ev.persistence_session_id;
  if (typeof sid === "string") return sid === session;
  // Events without a session field (job.*, control) always pass — they are not session-scoped.
  return true;
}

/** A one-line human summary of a delta event (the `tree` stream mode). */
function summarizeEvent(ev: WireEvent): string {
  const seq = ev.seq !== undefined ? `#${ev.seq} ` : "";
  const parts: string[] = [seq + ev.event];
  if (typeof ev.pane_id === "string") parts.push(ev.pane_id);
  if (typeof ev.status === "string") parts.push(String(ev.status));
  if (typeof ev.job_id === "string") parts.push(ev.job_id);
  if (typeof ev.task_uuid === "string") parts.push(ev.task_uuid);
  return parts.join(" ");
}

/** Exported for tests: build the subscribe ACK-independent snapshot→tree projection. */
export { snapshotToTree };
export type { SessionListRow, SubscribeAck };
