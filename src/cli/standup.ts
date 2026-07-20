// tally CLI — `query standup`: the four-log read-time join (IMPLEMENTATION-PLAN M3.1; CLI-SURFACE
// §1.5; SPEC "The four-log read-time join").
//
// The digest is a READ-TIME join over four logs, keyed on `session_ref` / `TALLY_TASK_UUID`:
//   1. `task export`                       — the TaskChampion durable rows (trust, priority, status);
//   2. `journalctl -t tally -o json`       — the journald TALLY_* lifecycle feed (observability);
//   3. `git log`                           — scoped to PUBLIC proof-of-work (the visible commits);
//   4. the harness JSONL                   — POINTED AT: path + existence + ids only, NEVER copied.
// The witness ledger (`$XDG_DATA_HOME/tally/witness.jsonl`) supplies the load-bearing verdict +
// GPU-seconds per task (the canonical proof; journald is observability). Output shape:
// `{window, completed[], in_flight[], reused, gate_fails, cancelled[]}` in `text|json|md`
// (`cancelled` is additive, issue #7 — no protocol bump, §2.5).
//
// No daemon RPC: standup reads the four logs + the ledger directly (CLI-SURFACE §1.5). Every
// subprocess flows through the injected `Exec` so the join is testable against fixture logs.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Exec } from "../contracts/exec";
import { JournalReader, type JournalEntry } from "../journal/reader";
import { ledgerPath } from "../contracts/paths";
import { parseRecord } from "../witness/record";
import { countsTowardCanonicalGpuSeconds, type WitnessRecord } from "../contracts/witness";
import { existsSync, readFileSync } from "node:fs";
import { printError, printJson, printLine, type Writer } from "./output";
import { flag, hasFlag, wantsJson, type CliContext } from "./index";
import { bunExec } from "./query";

/** One completed unit in the standup digest. */
export interface CompletedEntry {
  task_uuid: string | null;
  gpu_seconds: number | null;
  verdict: string;
  session_ref: string | null;
}

/** One in-flight unit (dispatched/started but no terminal delta). */
export interface InFlightEntry {
  task_uuid: string | null;
  session_ref: string | null;
  state: string;
  last_event_ts: string | null;
}

/** The full standup digest (SPEC "The four-log read-time join"). */
export interface StandupDigest {
  window: { since: string | null; until: string };
  completed: CompletedEntry[];
  in_flight: InFlightEntry[];
  reused: number;
  gate_fails: CompletedEntry[];
  /** Cancelled rows — an additive bucket (issue #7): a cancellation is not success-shaped. */
  cancelled: CompletedEntry[];
}

/** Options for the join (injectable for tests). */
export interface StandupOptions {
  since?: string;
  source?: string;
  exec?: Exec;
  /** Ledger path override (else XDG-resolved). */
  ledger?: string;
  env?: Parameters<typeof ledgerPath>[0];
}

/** Route `query standup`. `exec` is injectable for tests; production uses the Bun-backed default. */
export async function runStandup(ctx: CliContext, exec?: Exec): Promise<number> {
  const format = flag(ctx.args, "--format") ?? (wantsJson(ctx.args) ? "json" : "text");
  if (format !== "text" && format !== "json" && format !== "md") {
    printError(ctx.writer, `unknown --format '${format}' (expected text|json|md)`);
    return 2;
  }

  // `--stale-hours` is RECOMMENDED-ADOPT with the explicit ruling PENDING (CLI-SURFACE §1.5) — do
  // not build as ruled. Silently swallowing it let an operator believe they had filtered (issue #7);
  // warn loudly on stderr instead. Warn, not exit 2 — the digest still renders (the lax posture).
  if (hasFlag(ctx.args, "--stale-hours")) {
    printError(ctx.writer, "--stale-hours is not implemented — ruling pending (CLI-SURFACE §1.5); flag ignored");
  }

  const opts: StandupOptions = { env: ctx.env };
  if (exec !== undefined) opts.exec = exec;
  const since = flag(ctx.args, "--since");
  if (since !== undefined) opts.since = since;
  const source = flag(ctx.args, "--source");
  if (source !== undefined) opts.source = source;
  if (ctx.socket !== undefined) {
    // socket is irrelevant to the daemonless join; ignored intentionally.
  }

  const digest = await buildStandup(opts);
  emitStandup(ctx.writer, digest, format);
  return 0;
}

/**
 * Build the standup digest by joining the four logs + the witness ledger. The witness ledger is the
 * load-bearing verdict/GPU-seconds source; journald supplies the lifecycle spine (which tasks
 * started, which are still in flight); `task export` supplies the durable-row provenance/session_ref.
 * The harness JSONL is pointed at (path/ids only) — never read into the digest.
 */
export async function buildStandup(opts: StandupOptions = {}): Promise<StandupDigest> {
  const exec = opts.exec ?? bunExec();
  const until = new Date().toISOString();

  // --- (2) journald TALLY_* feed ---
  const reader = new JournalReader(exec);
  const readOpts = opts.since !== undefined ? { since: opts.since } : {};
  let journal: JournalEntry[] = [];
  try {
    journal = await reader.read(readOpts);
  } catch {
    journal = [];
  }

  // --- witness ledger (load-bearing verdict + gpu-seconds) ---
  const lp = opts.ledger ?? ledgerPath(opts.env ?? (process.env as Parameters<typeof ledgerPath>[0]));
  const ledger = readLedgerRecords(lp);

  // Index witness records by task_uuid (the last/terminal record wins per task).
  const witnessByTask = new Map<string, WitnessRecord>();
  for (const rec of ledger) {
    if (rec.task_uuid !== null) witnessByTask.set(rec.task_uuid, rec);
  }

  // --- (1) TaskChampion rows (session_ref/provenance/trust) ---
  const rowsBySession = await readTaskRows(exec, opts.source);

  // --- (4) git log — public proof-of-work (existence only; not joined into records) ---
  // Read but do not fail if git is absent; the standup remains valid without it.
  void (await readGitLog(exec, opts.since).catch(() => [] as string[]));

  // --- join: walk the journald spine, resolve terminal state via the witness ledger ---
  const perTask = new Map<string, { state: string; lastTs: string | null; sessionRef: string | null; sawEvidenceFail: boolean }>();
  for (const entry of journal) {
    const f = entry.fields;
    const task = f.TALLY_TASK_UUID;
    if (task === undefined) continue;
    if (opts.source !== undefined && f.TALLY_SOURCE !== opts.source) continue;
    const ts = entry.realtimeUs !== null ? new Date(entry.realtimeUs / 1000).toISOString() : null;
    const prev = perTask.get(task) ?? { state: "enqueued", lastTs: null, sessionRef: null, sawEvidenceFail: false };
    prev.state = f.TALLY_EVENT;
    prev.lastTs = ts ?? prev.lastTs;
    if (f.TALLY_SESSION_REF !== undefined) prev.sessionRef = f.TALLY_SESSION_REF;
    // `evidence_fail` is the canonical gate-fail marker (PS#21 forensics) — remember it even though
    // later lifecycle events (failed/witness_emitted) overwrite `state`.
    if (f.TALLY_EVENT === "evidence_fail") prev.sawEvidenceFail = true;
    perTask.set(task, prev);
  }

  const completed: CompletedEntry[] = [];
  const gate_fails: CompletedEntry[] = [];
  const cancelled: CompletedEntry[] = [];
  const in_flight: InFlightEntry[] = [];
  let reused = 0;

  const TERMINAL = new Set(["completed", "failed", "evidence_pass", "evidence_fail", "witness_emitted"]);

  // Prefer the witness ledger as the terminal source of truth; fall back to the journald spine.
  const seenTasks = new Set<string>();
  for (const [task, spine] of perTask) {
    seenTasks.add(task);
    const wit = witnessByTask.get(task);
    const sessionRef = spine.sessionRef ?? rowsBySession.get(task) ?? (wit ? null : null);
    if (wit) {
      const entry: CompletedEntry = {
        task_uuid: task,
        gpu_seconds: wit.gpu_seconds,
        verdict: wit.verdict,
        session_ref: sessionRef,
      };
      if (wit.labor_class === "reused") reused++;
      // Bucket routing (issue #7): cancelled gets its OWN bucket (a cancellation is not success);
      // gate_fails is the UNION of the ledger's `clean-exit-no-artifact` verdicts and tasks whose
      // journald spine carries an `evidence_fail` event — a recovered/exit-nonzero evidence fail is
      // witnessed as plain `failed`, the gate detail lives only in journald (PS#21 forensics).
      if (wit.verdict === "cancelled") cancelled.push(entry);
      else if (wit.verdict === "clean-exit-no-artifact" || spine.sawEvidenceFail) gate_fails.push(entry);
      else completed.push(entry);
      continue;
    }
    if (TERMINAL.has(spine.state)) {
      // A terminal journald event without a witness line (rowless observability) — completed, unless
      // the spine carries the gate-fail marker (then it is a gate fail, not a success-shaped row).
      const verdict = spine.state === "failed" || spine.sawEvidenceFail ? "failed" : "pass";
      const entry: CompletedEntry = { task_uuid: task, gpu_seconds: null, verdict, session_ref: sessionRef };
      if (spine.sawEvidenceFail) gate_fails.push(entry);
      else completed.push(entry);
    } else {
      in_flight.push({ task_uuid: task, session_ref: sessionRef, state: spine.state, last_event_ts: spine.lastTs });
    }
  }

  // Witness lines for tasks absent from the journald window (e.g. journald pruned) — fold them in.
  for (const [task, wit] of witnessByTask) {
    if (seenTasks.has(task)) continue;
    const entry: CompletedEntry = { task_uuid: task, gpu_seconds: wit.gpu_seconds, verdict: wit.verdict, session_ref: null };
    if (wit.labor_class === "reused") reused++;
    if (wit.verdict === "cancelled") cancelled.push(entry);
    else if (wit.verdict === "clean-exit-no-artifact") gate_fails.push(entry);
    else completed.push(entry);
  }

  return {
    window: { since: opts.since ?? null, until },
    completed,
    in_flight,
    reused,
    gate_fails,
    cancelled,
  };
}

/** Read + parse the witness ledger daemonlessly (each valid line → a WitnessRecord). */
function readLedgerRecords(path: string): WitnessRecord[] {
  if (!existsSync(path)) return [];
  const out: WitnessRecord[] = [];
  for (const line of readFileSync(path, "utf8").split("\n")) {
    if (line.trim().length === 0) continue;
    let raw: unknown;
    try {
      raw = JSON.parse(line);
    } catch {
      continue; // torn line — journald/ledger observability, not fatal to a read-time join.
    }
    const res = parseRecord(raw);
    if (res.ok) out.push(res.record);
  }
  return out;
}

/** Read the TaskChampion durable rows via `task export`, mapping task_uuid → session_ref. */
async function readTaskRows(exec: Exec, source: string | undefined): Promise<Map<string, string | null>> {
  const map = new Map<string, string | null>();
  const argv = ["task", "export"];
  let result;
  try {
    result = await exec.run(argv, { timeoutMs: 5000 });
  } catch {
    return map;
  }
  if (result.code !== 0 || result.stdout.trim().length === 0) return map;
  let rows: unknown;
  try {
    rows = JSON.parse(result.stdout);
  } catch {
    return map;
  }
  if (!Array.isArray(rows)) return map;
  for (const r of rows) {
    if (typeof r !== "object" || r === null) continue;
    const row = r as Record<string, unknown>;
    const uuid = typeof row.uuid === "string" ? row.uuid : null;
    if (uuid === null) continue;
    if (source !== undefined && row.source !== source) continue;
    map.set(uuid, typeof row.session_ref === "string" ? row.session_ref : null);
  }
  return map;
}

/** Read the public git proof-of-work log (scoped, existence-only). Returns short commit lines. */
async function readGitLog(exec: Exec, since: string | undefined): Promise<string[]> {
  const argv = ["git", "log", "--oneline", "--no-color"];
  if (since !== undefined) argv.push(`--since=${since}`);
  let result;
  try {
    result = await exec.run(argv, { timeoutMs: 5000 });
  } catch {
    return [];
  }
  if (result.code !== 0) return [];
  return result.stdout.split("\n").filter((l) => l.trim().length > 0);
}

// ---------------------------------------------------------------------------------------------
// Rendering.
// ---------------------------------------------------------------------------------------------

function emitStandup(w: Writer, digest: StandupDigest, format: "text" | "json" | "md"): void {
  switch (format) {
    case "json":
      printJson(w, digest);
      return;
    case "md":
      printLine(w, renderMd(digest));
      return;
    case "text":
    default:
      printLine(w, renderText(digest));
      return;
  }
}

function renderText(d: StandupDigest): string {
  const lines: string[] = [];
  lines.push(`standup ${d.window.since ?? "(all time)"} → ${d.window.until}`);
  lines.push(`completed: ${d.completed.length}  in-flight: ${d.in_flight.length}  reused: ${d.reused}  gate-fails: ${d.gate_fails.length}  cancelled: ${d.cancelled.length}`);
  for (const c of d.completed) {
    lines.push(`  + ${c.task_uuid ?? "(no-row)"}  ${c.verdict}  ${c.gpu_seconds ?? 0}s  ref=${c.session_ref ?? "-"}`);
  }
  for (const g of d.gate_fails) {
    lines.push(`  ! ${g.task_uuid ?? "(no-row)"}  ${g.verdict}  ref=${g.session_ref ?? "-"}`);
  }
  for (const x of d.cancelled) {
    lines.push(`  x ${x.task_uuid ?? "(no-row)"}  ${x.verdict}  ref=${x.session_ref ?? "-"}`);
  }
  for (const f of d.in_flight) {
    lines.push(`  * ${f.task_uuid ?? "(no-row)"}  ${f.state}  ref=${f.session_ref ?? "-"}`);
  }
  return lines.join("\n");
}

function renderMd(d: StandupDigest): string {
  const lines: string[] = [];
  lines.push(`## Standup — ${d.window.until}`);
  lines.push("");
  lines.push(`- completed: **${d.completed.length}**, in-flight: **${d.in_flight.length}**, reused: **${d.reused}**, gate-fails: **${d.gate_fails.length}**, cancelled: **${d.cancelled.length}**`);
  if (d.completed.length > 0) {
    lines.push("");
    lines.push("### Completed");
    for (const c of d.completed) lines.push(`- \`${c.task_uuid ?? "(no-row)"}\` — ${c.verdict}, ${c.gpu_seconds ?? 0}s (ref \`${c.session_ref ?? "-"}\`)`);
  }
  if (d.gate_fails.length > 0) {
    lines.push("");
    lines.push("### Gate fails");
    for (const g of d.gate_fails) lines.push(`- \`${g.task_uuid ?? "(no-row)"}\` — ${g.verdict}`);
  }
  if (d.cancelled.length > 0) {
    lines.push("");
    lines.push("### Cancelled");
    for (const x of d.cancelled) lines.push(`- \`${x.task_uuid ?? "(no-row)"}\` — ${x.verdict}`);
  }
  if (d.in_flight.length > 0) {
    lines.push("");
    lines.push("### In flight");
    for (const f of d.in_flight) lines.push(`- \`${f.task_uuid ?? "(no-row)"}\` — ${f.state}`);
  }
  return lines.join("\n");
}

/** Exported for tests: whether a witness record counts toward canonical GPU-seconds (re-export). */
export { countsTowardCanonicalGpuSeconds };
