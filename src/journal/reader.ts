// tally — journald TALLY_* read path (reader half of M1.4; SPEC "journald TALLY_* event schema",
// FS§4 + PS#11; IMPLEMENTATION-PLAN §3 Journald). The read half of `query log` and the standup join.
//
// `reader.ts` shells `journalctl -t tally -o json [--since …] [-f]` through the injectable `Exec`
// seam and re-hydrates the structured TALLY_* fields. Under the ruled stdout-capture emission path
// the structured fields ride inside the `MESSAGE` of each journald record as a single-line JSON
// payload (see `emit.ts`); this reader therefore parses the MESSAGE payload back into a `TallyFields`
// record. journald ALSO preserves custom top-level fields when a program prints structured data, so
// the reader tolerates either shape: it prefers the JSON `MESSAGE` payload and falls back to the
// top-level TALLY_* keys, coercing the string-typed numeric fields back to numbers.
//
// This module never writes; it is pure ingest. It performs journalctl access ONLY through `Exec`,
// so it is fully testable against the layer-0 `FakeJournalctl`. Malformed lines are skipped (a torn
// journald line is not proof — the ledger is the load-bearing record; journald is observability).

import {
  TALLY_EVENTS,
  type Exec,
  type TallyEvent,
  type TallyFields,
} from "../contracts/index.ts";

/**
 * One re-hydrated journald entry: the structured TALLY_* fields plus the record timestamp
 * (microseconds since the epoch, from journald's `__REALTIME_TIMESTAMP`). The timestamp lets the
 * standup join order entries and the `query log` feed present a chronological tail.
 */
export interface JournalEntry {
  fields: TallyFields;
  /** `__REALTIME_TIMESTAMP` in microseconds, or null when journald omitted it. */
  realtimeUs: number | null;
}

/** Filters for a `query log` read — the `--task/--session/--event/--since/--follow` surface. */
export interface ReadOptions {
  /** `--since` — an ISO timestamp or a journalctl relative string; passed through to journalctl. */
  since?: string;
  /** `--task` — filter to one task UUID (client-side, after re-hydration). */
  task?: string;
  /** `--session` — filter to one session_ref (client-side). */
  session?: string;
  /** `--event` — filter to one TALLY_EVENT (client-side). */
  event?: TallyEvent;
  /** Millisecond timeout for the underlying journalctl run. */
  timeoutMs?: number;
}

/** The fixed journalctl identifier the daemon emits under (SPEC: `SyslogIdentifier=tally`). */
const IDENTIFIER = "tally";

/** The numeric TALLY_* fields, coerced from string when re-hydrated off the top-level journald keys. */
const NUMERIC_FIELDS = [
  "TALLY_EXIT_CODE",
  "TALLY_GPU_SECONDS",
  "TALLY_ATTEMPT",
  "TALLY_LEASE_EPOCH",
] as const;

/** All structured TALLY_* keys the reader lifts off the top-level journald object (fallback path). */
const TALLY_KEYS = [
  "TALLY_EVENT",
  "TALLY_TASK_UUID",
  "TALLY_CLASS",
  "TALLY_SOURCE",
  "TALLY_AGENT",
  "TALLY_SESSION_REF",
  "TALLY_UNIT",
  "TALLY_EXIT_CODE",
  "TALLY_GPU_SECONDS",
  "TALLY_ARTIFACT_HASH",
  "TALLY_EVIDENCE",
  "TALLY_ATTEMPT",
  "TALLY_LEASE_EPOCH",
  "TALLY_LABOR_CLASS",
] as const;

/**
 * Build the journalctl argv for a one-shot (non-follow) read. `-t tally -o json` is fixed; `--since`
 * is passed through when present.
 */
export function buildArgv(opts: ReadOptions): string[] {
  const argv = ["journalctl", "-t", IDENTIFIER, "-o", "json"];
  if (opts.since !== undefined) {
    argv.push("--since", opts.since);
  }
  return argv;
}

/**
 * Build the journalctl argv for a follow (`-f`) read — the `query log --follow` tail.
 */
export function buildFollowArgv(opts: ReadOptions): string[] {
  return [...buildArgv(opts), "-f"];
}

/** Coerce a re-hydrated value to a finite number, or return undefined when it is not numeric. */
function toNumber(value: unknown): number | undefined {
  if (typeof value === "number") return Number.isFinite(value) ? value : undefined;
  if (typeof value === "string" && value.length > 0) {
    const n = Number(value);
    return Number.isFinite(n) ? n : undefined;
  }
  return undefined;
}

/** A raw record is anything with a string TALLY_EVENT we recognize; everything else is skipped. */
function isTallyEvent(value: unknown): value is TallyEvent {
  return typeof value === "string" && (TALLY_EVENTS as readonly string[]).includes(value);
}

/**
 * Re-hydrate a `TallyFields` record from a source object of raw values (either the parsed MESSAGE
 * payload or the top-level journald object). Numeric fields are coerced from string; unknown keys
 * are dropped. Returns null when the source has no recognizable `TALLY_EVENT`.
 */
function hydrateFields(src: Record<string, unknown>): TallyFields | null {
  const event = src["TALLY_EVENT"];
  if (!isTallyEvent(event)) return null;

  const fields: Record<string, unknown> = {
    SYSLOG_IDENTIFIER: "tally",
    TALLY_EVENT: event,
  };

  for (const key of TALLY_KEYS) {
    if (key === "TALLY_EVENT") continue;
    const raw = src[key];
    if (raw === undefined || raw === null) continue;
    if ((NUMERIC_FIELDS as readonly string[]).includes(key)) {
      const n = toNumber(raw);
      if (n !== undefined) {
        fields[key] = n;
      }
      continue;
    }
    if (typeof raw === "string") {
      fields[key] = raw;
    } else {
      // A non-string, non-numeric field: stringify defensively so nothing is lost.
      fields[key] = String(raw);
    }
  }

  const message = src["MESSAGE"];
  fields.MESSAGE = typeof message === "string" ? message : `${event} ${fields.TALLY_TASK_UUID ?? ""}`.trim();
  return fields as unknown as TallyFields;
}

/**
 * Parse one `journalctl -o json` line into a `JournalEntry`, or null when the line is not a valid
 * tally entry. The reader prefers the structured TALLY_* fields carried inside the JSON `MESSAGE`
 * payload (the stdout-capture convention emit.ts writes); if `MESSAGE` is not itself a JSON object
 * of TALLY_* fields, it falls back to the top-level journald keys. A torn/unparseable line is
 * skipped (journald is observability, not proof).
 */
export function parseLine(line: string): JournalEntry | null {
  const trimmed = line.trim();
  if (trimmed.length === 0) return null;

  let obj: Record<string, unknown>;
  try {
    const parsed: unknown = JSON.parse(trimmed);
    if (typeof parsed !== "object" || parsed === null) return null;
    obj = parsed as Record<string, unknown>;
  } catch {
    // Torn or non-JSON journald line — skip.
    return null;
  }

  const realtimeUs = toNumber(obj["__REALTIME_TIMESTAMP"]) ?? null;

  // Preferred path: MESSAGE is a JSON object carrying the full TALLY_* record (stdout capture).
  const rawMessage = obj["MESSAGE"];
  if (typeof rawMessage === "string" && rawMessage.startsWith("{")) {
    try {
      const payload: unknown = JSON.parse(rawMessage);
      if (typeof payload === "object" && payload !== null) {
        const fields = hydrateFields(payload as Record<string, unknown>);
        if (fields !== null) return { fields, realtimeUs };
      }
    } catch {
      // MESSAGE was not a JSON payload after all — fall through to the top-level keys.
    }
  }

  // Fallback path: the structured fields ride as top-level journald keys.
  const fields = hydrateFields(obj);
  if (fields === null) return null;
  return { fields, realtimeUs };
}

/**
 * Apply the client-side `--task/--session/--event` filters to a re-hydrated entry. `--since` is
 * pushed down to journalctl and not re-applied here.
 */
function matches(entry: JournalEntry, opts: ReadOptions): boolean {
  const f = entry.fields;
  if (opts.task !== undefined && f.TALLY_TASK_UUID !== opts.task) return false;
  if (opts.session !== undefined && f.TALLY_SESSION_REF !== opts.session) return false;
  if (opts.event !== undefined && f.TALLY_EVENT !== opts.event) return false;
  return true;
}

/**
 * The journald reader. Holds the injected `Exec`; every journalctl invocation flows through it, so
 * the reader is fully testable against `FakeJournalctl`. One instance per daemon/CLI process.
 */
export class JournalReader {
  private readonly exec: Exec;

  constructor(exec: Exec) {
    this.exec = exec;
  }

  /**
   * One-shot read: run `journalctl -t tally -o json [--since …]`, parse every line, apply the
   * client-side filters, and return the entries in journald (chronological) order. A non-zero
   * journalctl exit with empty output yields an empty list (no tally entries yet); a non-zero exit
   * WITH stderr is surfaced as an error.
   */
  async read(opts: ReadOptions = {}): Promise<JournalEntry[]> {
    const argv = buildArgv(opts);
    const runOpts = opts.timeoutMs !== undefined ? { timeoutMs: opts.timeoutMs } : {};
    const result = await this.exec.run(argv, runOpts);
    if (result.code !== 0 && result.stdout.trim().length === 0) {
      const detail = result.stderr.trim();
      if (detail.length > 0) {
        throw new Error(`journalctl read failed (code ${result.code}): ${detail}`);
      }
      // Non-zero with no stdout and no stderr: treat as "no entries".
      return [];
    }
    return this.parseAndFilter(result.stdout, opts);
  }

  /**
   * Follow read (`query log --follow`): spawn `journalctl -f`, yielding each matching entry as it
   * arrives. Consumers iterate the async generator; breaking out of the loop kills the underlying
   * stream. Malformed lines are skipped silently.
   */
  async *follow(opts: ReadOptions = {}): AsyncIterableIterator<JournalEntry> {
    const argv = buildFollowArgv(opts);
    const runOpts = opts.timeoutMs !== undefined ? { timeoutMs: opts.timeoutMs } : {};
    const stream = this.exec.spawn(argv, runOpts);
    try {
      for await (const line of stream.lines()) {
        const entry = parseLine(line);
        if (entry === null) continue;
        if (matches(entry, opts)) yield entry;
      }
    } finally {
      stream.kill();
    }
  }

  /** Split raw journalctl stdout into entries, parse, and apply the client-side filters. */
  private parseAndFilter(stdout: string, opts: ReadOptions): JournalEntry[] {
    const out: JournalEntry[] = [];
    for (const line of stdout.split("\n")) {
      if (line.trim().length === 0) continue;
      const entry = parseLine(line);
      if (entry === null) continue;
      if (matches(entry, opts)) out.push(entry);
    }
    return out;
  }
}
