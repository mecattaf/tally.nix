// test/helpers/fake-journalctl.ts
//
// A fake of `journalctl -t tally -o json`, the read half of the journald
// TALLY_* path (SPEC "journald TALLY_* event schema"; FS§4 + PS#11). Under the
// ruled emission path (`StandardOutput=journal` stdout capture) the structured
// TALLY_* fields ride as a single-line JSON `MESSAGE` payload beneath
// `SYSLOG_IDENTIFIER=tally`. `journalctl -o json` prints one JSON object per
// line; this fake reproduces exactly that so `journal/reader.ts` can re-hydrate
// the TALLY_* fields by parsing the MESSAGE payload.
//
// Seed entries with `emit(fields)` (the same TALLY_* record `emit.ts` writes),
// then `install(exec)`. Supports the flags tally uses: `-t <ident>` /
// `--identifier`, `-o json`, `--since <t>`, and `-f`/`--follow` (the fake
// returns the current buffer and exits — a test drives follow by seeding more
// entries between calls).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { type FakeExec, type ExecResult, ok, fail, parseArgs } from "./exec-fakes.ts";

/** The TALLY_* structured record (a subset/superset is fine; extra keys pass). */
export interface TallyFields {
  TALLY_EVENT: string;
  TALLY_TASK_UUID?: string;
  TALLY_CLASS?: string;
  TALLY_SOURCE?: string;
  TALLY_AGENT?: string;
  TALLY_SESSION_REF?: string;
  TALLY_UNIT?: string;
  TALLY_EXIT_CODE?: number | string;
  TALLY_GPU_SECONDS?: number | string;
  TALLY_ARTIFACT_HASH?: string;
  TALLY_EVIDENCE?: string;
  TALLY_ATTEMPT?: number | string;
  TALLY_LEASE_EPOCH?: number | string;
  TALLY_LABOR_CLASS?: string;
  MESSAGE?: string;
  [k: string]: unknown;
}

interface JournalEntry {
  realtimeUs: number; // __REALTIME_TIMESTAMP microseconds
  identifier: string;
  fields: TallyFields;
}

/**
 * A programmable journald. `emit()` appends one structured entry; `install()`
 * serves them as `journalctl -o json` lines. The MESSAGE line carries the full
 * TALLY_* record as a JSON string (the stdout-capture convention), and the
 * top-level entry mirrors SYSLOG_IDENTIFIER + __REALTIME_TIMESTAMP so a reader
 * can filter by `-t tally` and `--since`.
 */
export class FakeJournalctl {
  private readonly entries: JournalEntry[] = [];
  private clockUs = Date.UTC(2026, 6, 9, 12, 0, 0) * 1000;

  /**
   * Append one event. `fields` is the TALLY_* record; a human MESSAGE is
   * synthesized if absent. Each call advances the fake clock by 1s so `--since`
   * ordering is deterministic. `at` (ISO string) overrides the timestamp.
   */
  emit(fields: TallyFields, at?: string): this {
    const realtimeUs = at ? Date.parse(at) * 1000 : (this.clockUs += 1_000_000);
    const message = fields.MESSAGE ?? `${fields.TALLY_EVENT} ${fields.TALLY_TASK_UUID ?? ""}`.trim();
    this.entries.push({
      realtimeUs,
      identifier: "tally",
      fields: { ...fields, MESSAGE: message },
    });
    return this;
  }

  /** Number of buffered entries. */
  count(): number {
    return this.entries.length;
  }

  /**
   * Render one journald `-o json` line for an entry: the stdout-capture shape
   * where the full TALLY_* record rides inside MESSAGE as a JSON string, and the
   * structured fields ALSO appear as top-level keys (journald preserves both
   * when a program prints structured data — the reader may read either).
   */
  private renderLine(e: JournalEntry): string {
    const obj: Record<string, unknown> = {
      __REALTIME_TIMESTAMP: String(e.realtimeUs),
      SYSLOG_IDENTIFIER: e.identifier,
      // stdout capture: MESSAGE is the single-line JSON payload of TALLY_* fields.
      MESSAGE: JSON.stringify(e.fields),
    };
    // Mirror the TALLY_* fields at top level too (journald keeps custom fields).
    for (const [k, v] of Object.entries(e.fields)) {
      if (k === "MESSAGE") continue;
      obj[k] = typeof v === "string" ? v : String(v);
    }
    return JSON.stringify(obj);
  }

  install(exec: FakeExec): this {
    exec.register("journalctl", (args): ExecResult => {
      const parsed = parseArgs(args);
      // Identifier filter: -t / --identifier tally.
      const ident = parsed.value("t") ?? parsed.value("identifier");
      if (ident !== undefined && ident !== "tally") {
        // A non-tally identifier yields nothing.
        return ok("");
      }
      // Output format must be json for the reader (support -o json / --output json).
      const output = parsed.value("o") ?? parsed.value("output");
      if (output !== undefined && output !== "json") {
        return fail(2, `fake-journalctl: only -o json supported, got '${output}'`);
      }
      // --since filter.
      let entries = [...this.entries].sort((a, b) => a.realtimeUs - b.realtimeUs);
      const since = parsed.value("since") ?? parsed.value("S");
      if (since) {
        const sinceUs = Date.parse(since) * 1000;
        if (!Number.isNaN(sinceUs)) {
          entries = entries.filter((e) => e.realtimeUs >= sinceUs);
        }
      }
      // -f / --follow: the fake returns the current buffer then "exits" (a test
      // drives follow by seeding entries and re-invoking).
      const lines = entries.map((e) => this.renderLine(e));
      return ok(lines.join("\n") + (lines.length ? "\n" : ""));
    });
    return this;
  }
}
