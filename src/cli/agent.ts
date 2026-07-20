// tally CLI — the `agent` noun: read-projections of the in-daemon detector, scoped to agent panes
// (IMPLEMENTATION-PLAN M3.1; CLI-SURFACE §1.4). `agent.start` is deliberately ABSENT — starting an
// agent IS `tally enqueue` (§4 divergence 1).
//
//   tally agent list    [--status <s>] [--kind <k>]      — agent.list RPC (table projection)
//   tally agent get     <sel>                            — agent.get RPC (one record in full)
//   tally agent read    <sel> [--source <s>] [--format]  — agent.read RPC (detection snapshot)
//   tally agent explain <sel>                            — agent.explain RPC (why blocked/working)
//   tally agent wait    <sel> [--status] [--timeout] [--count]  — the barrier primitive (session.wait)
//   tally agent send    <sel> <text> [--enter]           — steering → pane.send (daemon resolves agent→pane)
//   tally agent focus   <sel>                            — pane.focus (daemon resolves agent→pane)
//
// `agent wait` routes through `session.wait` over the FULL four-value AgentStatus; `agent send`/
// `agent focus` route through `pane.send`/`pane.focus` (the daemon resolves the agent selector to its
// pane). No separate `agent.wait`/`agent.send`/`agent.focus` wire method exists (IMPLEMENTATION-PLAN
// §3 RPC inventory).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { AGENT_STATUSES, AGENT_KINDS, type AgentStatus } from "../contracts/agent";
import type { AgentRecord } from "../contracts/agent";
import type { WaitResult } from "../contracts/wire";
import { connectClient, type TallyClient } from "./client";
import { printError, printJson, printJsonLines, printLine } from "./output";
import { flag, hasFlag, wantsJson, type CliContext } from "./index";
import { clientOpts, parseDuration } from "./queue";

/** Route the `agent` noun. */
export async function runAgent(ctx: CliContext): Promise<number> {
  switch (ctx.verb) {
    case "list":
      return doList(ctx);
    case "get":
      return doGet(ctx);
    case "read":
      return doRead(ctx);
    case "explain":
      return doExplain(ctx);
    case "wait":
      return doWait(ctx);
    case "send":
      return doSend(ctx);
    case "focus":
      return doFocus(ctx);
    default:
      printError(ctx.writer, `unknown agent verb '${ctx.verb ?? "(none)"}' (expected list|get|read|explain|wait|send|focus)`);
      return 2;
  }
}

function selectorOr(ctx: CliContext): string | null {
  const raw = ctx.args.positionals[0];
  if (raw === undefined || raw.length === 0) {
    printError(ctx.writer, `agent ${ctx.verb} requires a <selector>`);
    return null;
  }
  return raw;
}

// ---------------------------------------------------------------------------------------------
// list / get / read / explain — direct RPC read-projections.
// ---------------------------------------------------------------------------------------------

async function doList(ctx: CliContext): Promise<number> {
  const params: { status?: string; kind?: string } = {};
  const status = flag(ctx.args, "--status");
  if (status !== undefined) {
    if (!(AGENT_STATUSES as readonly string[]).includes(status)) {
      printError(ctx.writer, `--status must be one of ${AGENT_STATUSES.join("|")}`);
      return 2;
    }
    params.status = status;
  }
  const kind = flag(ctx.args, "--kind");
  if (kind !== undefined) {
    if (!(AGENT_KINDS as readonly string[]).includes(kind)) {
      printError(ctx.writer, `--kind must be one of ${AGENT_KINDS.join("|")}`);
      return 2;
    }
    params.kind = kind;
  }
  const client = await connectClient(clientOpts(ctx));
  try {
    const rows = await client.call<Array<Record<string, unknown>>>("agent.list", params);
    if (wantsJson(ctx.args)) {
      printJsonLines(ctx.writer, rows);
    } else if (rows.length === 0) {
      printLine(ctx.writer, "(no detected agents)");
    } else {
      for (const r of rows) {
        printLine(
          ctx.writer,
          `${String(r.pane ?? r.pane_id ?? "-")}  ${String(r.kind ?? "-")}/${String(r.status ?? "-")}  ref=${String(r.session_ref ?? "-")}`,
        );
      }
    }
    return 0;
  } finally {
    client.close();
  }
}

async function doGet(ctx: CliContext): Promise<number> {
  const sel = selectorOr(ctx);
  if (sel === null) return 2;
  const client = await connectClient(clientOpts(ctx));
  try {
    const rec = await client.call<Record<string, unknown>>("agent.get", { agent_id: sel });
    if (wantsJson(ctx.args)) printJson(ctx.writer, rec);
    else printLine(ctx.writer, `${String(rec.pane ?? sel)}  ${String(rec.kind ?? "-")}/${String(rec.status ?? "-")}`);
    return 0;
  } finally {
    client.close();
  }
}

async function doRead(ctx: CliContext): Promise<number> {
  const sel = selectorOr(ctx);
  if (sel === null) return 2;
  const params: { agent_id: string; source?: string; format?: string } = { agent_id: sel };
  const source = flag(ctx.args, "--source");
  if (source !== undefined) params.source = source;
  const format = flag(ctx.args, "--format");
  if (format !== undefined) params.format = format;

  const client = await connectClient(clientOpts(ctx));
  try {
    const rec = await client.call<{ pane: string; source: string; text: string }>("agent.read", params);
    if (wantsJson(ctx.args)) printJson(ctx.writer, rec);
    else ctx.writer.out(rec.text.endsWith("\n") ? rec.text : rec.text + "\n");
    return 0;
  } finally {
    client.close();
  }
}

async function doExplain(ctx: CliContext): Promise<number> {
  const sel = selectorOr(ctx);
  if (sel === null) return 2;
  const client = await connectClient(clientOpts(ctx));
  try {
    const rec = await client.call<Record<string, unknown>>("agent.explain", { agent_id: sel });
    if (wantsJson(ctx.args)) printJson(ctx.writer, rec);
    else
      printLine(
        ctx.writer,
        `${String(rec.pane ?? sel)}  state=${String(rec.state ?? "-")}  rule=${String(rec.matched_rule ?? "-")}  strategy=${String(rec.strategy ?? "-")}`,
      );
    return 0;
  } finally {
    client.close();
  }
}

// ---------------------------------------------------------------------------------------------
// wait — the exposed barrier primitive (routes through session.wait).
// ---------------------------------------------------------------------------------------------

async function doWait(ctx: CliContext): Promise<number> {
  const sel = selectorOr(ctx);
  if (sel === null) return 2;
  const status = (flag(ctx.args, "--status") ?? "done") as AgentStatus;
  if (!(AGENT_STATUSES as readonly string[]).includes(status)) {
    printError(ctx.writer, `--status must be one of ${AGENT_STATUSES.join("|")}`);
    return 2;
  }
  const countRaw = flag(ctx.args, "--count");
  let count = 1;
  if (countRaw !== undefined) {
    const n = Number(countRaw);
    if (!Number.isInteger(n) || n <= 0) {
      printError(ctx.writer, `--count must be a positive integer: ${countRaw}`);
      return 2;
    }
    count = n;
  }
  const timeout = flag(ctx.args, "--timeout");
  const timeoutMs = timeout !== undefined ? parseDuration(timeout) : undefined;

  const client = await connectClient(clientOpts(ctx));
  const startMs = Date.now();
  try {
    // Resolve the selector to a concrete agent_id + its bound pane (session.wait's agent predicate keys
    // on agent_ids; the frozen §1.4 `--json` `pane` field is the pane composite key, NOT the agent id).
    const resolved = await resolveAgent(client, sel);
    const agentId = resolved.agentId;

    const params: { predicate: { subject: "agent"; agent_ids: string[]; until_status: AgentStatus; count: number }; timeout_ms?: number } = {
      predicate: { subject: "agent", agent_ids: [agentId], until_status: status, count },
    };
    if (timeoutMs !== undefined) params.timeout_ms = timeoutMs;

    const result = await client.call<WaitResult>("session.wait", params, timeoutMs !== undefined ? timeoutMs + 2000 : 0);
    const waitedMs = Date.now() - startMs;
    const reached = result.timed_out !== true;
    if (wantsJson(ctx.args)) {
      // Frozen §1.4 shape: {pane, status, reached, waited_ms}. `pane` is the pane key (not the agent
      // id, which would only resolve via the agent-fallback path); the agent id rides an additive field.
      printJson(ctx.writer, { pane: resolved.pane ?? agentId, agent_id: agentId, status, reached: reached === true, waited_ms: waitedMs, timed_out: result.timed_out === true, satisfied: result.satisfied });
    } else {
      printLine(ctx.writer, reached ? `${agentId} reached ${status} (${waitedMs}ms)` : `${agentId} timed out waiting for ${status}`);
    }
    return reached ? 0 : 1;
  } finally {
    client.close();
  }
}

/**
 * Resolve a selector to an agent_id AND its bound pane key. An `ag_`-prefixed token is already an id
 * (we still fetch the row to learn the pane); otherwise `agent.get` resolves both. Falls back to the
 * raw selector as the id when the row cannot be fetched.
 */
async function resolveAgent(client: TallyClient, sel: string): Promise<{ agentId: string; pane: string | null }> {
  try {
    const rec = await client.call<AgentRecord & { id?: string; agent_id?: string; pane?: string; pane_id?: string }>("agent.get", { agent_id: sel });
    const id = rec.id ?? rec.agent_id ?? (sel.startsWith("ag_") ? sel : sel);
    const pane = rec.pane ?? rec.pane_id ?? null;
    return { agentId: id, pane };
  } catch {
    return { agentId: sel, pane: null };
  }
}

// ---------------------------------------------------------------------------------------------
// send / focus — route through pane.send / pane.focus (the daemon resolves agent → pane).
// ---------------------------------------------------------------------------------------------

async function doSend(ctx: CliContext): Promise<number> {
  const sel = selectorOr(ctx);
  if (sel === null) return 2;
  let text = ctx.args.positionals[1];
  if (text === undefined) {
    printError(ctx.writer, "agent send requires <text>");
    return 2;
  }
  if (hasFlag(ctx.args, "--enter")) text = text + "\r";
  const client = await connectClient(clientOpts(ctx));
  try {
    const result = await client.call<{ pane: string; kind?: string; sent: boolean }>("pane.send", { pane: sel, text });
    if (wantsJson(ctx.args)) printJson(ctx.writer, { pane: result.pane, kind: result.kind ?? null, sent: result.sent });
    else printLine(ctx.writer, `steered ${result.pane}`);
    return 0;
  } finally {
    client.close();
  }
}

async function doFocus(ctx: CliContext): Promise<number> {
  const sel = selectorOr(ctx);
  if (sel === null) return 2;
  const client = await connectClient(clientOpts(ctx));
  try {
    const result = await client.call<{ pane: string; kitty_window_id: number; focused: boolean }>("pane.focus", { pane: sel });
    if (wantsJson(ctx.args)) printJson(ctx.writer, result);
    else printLine(ctx.writer, `focused ${result.pane} (win=${result.kitty_window_id})`);
    return 0;
  } finally {
    client.close();
  }
}

/** Exported for tests: the writer-free status validity check. */
export function isAgentStatus(s: string): s is AgentStatus {
  return (AGENT_STATUSES as readonly string[]).includes(s);
}
