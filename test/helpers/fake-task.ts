// test/helpers/fake-task.ts
//
// A fake of the taskwarrior 3.x `task` binary faithful to its JSON export/import
// contract, backed by an in-memory (tmp-persisted) store. tally accesses
// TaskChampion by `task export` / `task import` / `task config` shell-out ONLY
// (SPEC "Canonical work store"; DECISIONS jul9) — never in-process — so this
// fake covers exactly that surface:
//
//   task rc.json.array=on export [<filter>]   -> JSON array of task objects
//   task import [<file>]                       -> upsert tasks from JSON (stdin/file)
//   task config <name> <value>                 -> set an rc/UDA config key
//   task config                                -> dump config keys
//   task _get / task <filter> count            -> convenience read helpers
//
// The store models the taskwarrior JSON shape: `uuid`, `description`,
// `status` (pending|completed|deleted), `entry`/`modified` in the compact
// `YYYYMMDDTHHMMSSZ` datetime form, `priority` (H|M|L), and every UDA as a
// top-level string key. UDAs registered via `task config uda.<name>.*` are
// tracked so `udas.ts` bootstrap-idempotence tests can assert them.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { readFileSync } from "node:fs";
import { randomUUID } from "node:crypto";
import { type FakeExec, type ExecResult, ok, fail } from "./exec-fakes.ts";

/** A taskwarrior task object as it appears in `task export` JSON. */
export interface FakeTaskRecord {
  uuid: string;
  description: string;
  status: "pending" | "completed" | "deleted" | "waiting";
  entry: string;
  modified?: string;
  end?: string;
  priority?: "H" | "M" | "L";
  /** UDAs and any other string attributes ride as extra keys. */
  [k: string]: unknown;
}

/** Compact taskwarrior datetime for `now` (YYYYMMDDTHHMMSSZ). */
export function twNow(date = new Date()): string {
  const p = (n: number, w = 2) => String(n).padStart(w, "0");
  return (
    `${date.getUTCFullYear()}${p(date.getUTCMonth() + 1)}${p(date.getUTCDate())}` +
    `T${p(date.getUTCHours())}${p(date.getUTCMinutes())}${p(date.getUTCSeconds())}Z`
  );
}

/**
 * Normalize a date field to the compact taskwarrior form on import (what real `task import` does).
 * An already-compact value is returned unchanged; an ISO-8601 value (with or without milliseconds) is
 * converted to compact UTC. This models the durable store's real normalization so tests certifying a
 * datetime survive-a-round-trip actually reflect the binary's behaviour.
 */
export function normalizeTwDate(value: string): string {
  if (/^\d{8}T\d{6}Z$/.test(value)) return value;
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return value;
  return twNow(d);
}

/**
 * A programmable taskwarrior store. Seed tasks and UDA config, then
 * `install(exec)`. Reads the store back with `.tasks()` / `.config`.
 */
export class FakeTask {
  private readonly store = new Map<string, FakeTaskRecord>();
  /** rc/config keys set via `task config ...` (incl. uda.* registration). */
  readonly config: Record<string, string> = {};
  /** Every `task import` payload, in order (for veneer-discipline assertions). */
  readonly imports: FakeTaskRecord[][] = [];

  /** Seed a task directly into the store. */
  seed(task: Partial<FakeTaskRecord> & { uuid: string; description: string }): this {
    const full: FakeTaskRecord = {
      status: "pending",
      entry: twNow(),
      modified: twNow(),
      ...task,
    };
    this.store.set(full.uuid, full);
    return this;
  }

  /** All tasks currently in the store. */
  tasks(): FakeTaskRecord[] {
    return [...this.store.values()];
  }

  /** A single task by uuid. */
  get(uuid: string): FakeTaskRecord | undefined {
    return this.store.get(uuid);
  }

  /** True when a `uda.<name>.type` config key is registered. */
  hasUda(name: string): boolean {
    return `uda.${name}.type` in this.config;
  }

  /** The registered UDA names (those with a `.type` set). */
  registeredUdas(): string[] {
    return Object.keys(this.config)
      .filter((k) => k.startsWith("uda.") && k.endsWith(".type"))
      .map((k) => k.slice("uda.".length, -".type".length));
  }

  /**
   * Apply a minimal taskwarrior filter to the store. Supports:
   *   - `status:pending` / `status:completed`
   *   - `<uuid>` (exact uuid or a uuid prefix)
   *   - `<uda>:<value>` (e.g. `trust:unreviewed`)
   * Multiple terms are ANDed. An empty filter returns pending+completed (not
   * deleted), matching taskwarrior's default report scope closely enough for
   * tests; `export` with no filter returns everything.
   */
  private applyFilter(terms: readonly string[], includeAll: boolean): FakeTaskRecord[] {
    let out = [...this.store.values()];
    if (!includeAll && terms.length === 0) {
      out = out.filter((t) => t.status !== "deleted");
    }
    for (const term of terms) {
      const colon = term.indexOf(":");
      if (colon !== -1) {
        const key = term.slice(0, colon);
        const val = term.slice(colon + 1);
        out = out.filter((t) => String(t[key] ?? "") === val);
      } else {
        // Treat as a uuid or uuid prefix.
        out = out.filter((t) => t.uuid === term || t.uuid.startsWith(term));
      }
    }
    return out;
  }

  install(exec: FakeExec): this {
    exec.register("task", (args, opts): ExecResult => {
      // Strip leading `rc.<key>=<val>` overrides (e.g. rc.json.array=on,
      // rc.confirmation=off) — record them as config but do not treat as verbs.
      const rest: string[] = [];
      for (const a of args) {
        const m = /^rc\.([^=]+)=(.*)$/.exec(a);
        if (m) {
          this.config[m[1]!] = m[2]!;
        } else {
          rest.push(a);
        }
      }

      // `task config <name> <value>` and `task config`.
      const configIdx = rest.indexOf("config");
      if (configIdx !== -1) {
        const after = rest.slice(configIdx + 1);
        if (after.length === 0) {
          // Dump config.
          const dump = Object.entries(this.config)
            .map(([k, v]) => `${k} ${v}`)
            .join("\n");
          return ok(dump + (dump ? "\n" : ""));
        }
        const name = after[0]!;
        const value = after.slice(1).join(" ");
        this.config[name] = value;
        return ok(`Config ${name} set.`);
      }

      // `task [<filter>] export`
      const exportIdx = rest.indexOf("export");
      if (exportIdx !== -1) {
        const filter = rest.slice(0, exportIdx);
        const rows = this.applyFilter(filter, /*includeAll*/ filter.length === 0);
        return ok(JSON.stringify(rows));
      }

      // `task import [<file>]` — payload from file arg or stdin.
      const importIdx = rest.indexOf("import");
      if (importIdx !== -1) {
        const fileArg = rest[importIdx + 1];
        let payload: string;
        if (fileArg && fileArg !== "-") {
          try {
            payload = readFileSync(fileArg, "utf8");
          } catch (e) {
            return fail(1, `task import: cannot read ${fileArg}: ${String(e)}`);
          }
        } else {
          const stdin = opts.stdin;
          payload = typeof stdin === "string"
            ? stdin
            : stdin
              ? new TextDecoder().decode(stdin)
              : "";
        }
        let parsed: unknown;
        try {
          // taskwarrior accepts a JSON array OR newline-delimited JSON objects.
          const trimmed = payload.trim();
          parsed = trimmed.startsWith("[")
            ? JSON.parse(trimmed)
            : trimmed
                .split("\n")
                .filter((l) => l.trim())
                .map((l) => JSON.parse(l));
        } catch (e) {
          return fail(1, `task import: bad JSON: ${String(e)}`);
        }
        const incoming = (parsed as FakeTaskRecord[]).map((t) => ({ ...t }));
        this.imports.push(incoming);
        let added = 0;
        let updated = 0;
        for (const t of incoming) {
          if (!t.uuid) {
            t.uuid = randomUUID();
          }
          // Date-typed fields must be the compact taskwarrior form (real `task import` normalizes
          // them and does NOT round-trip a fractional-second ISO). Model that: normalize (and, for a
          // strict-fidelity check, REJECT a fractional-second ISO the way real taskwarrior effectively
          // corrupts it — here we normalize so a mis-formatted write is at least caught by the shape).
          for (const field of ["entry", "modified", "end", "due"] as const) {
            const v = (t as Record<string, unknown>)[field];
            if (typeof v === "string" && v.length > 0) {
              (t as Record<string, unknown>)[field] = normalizeTwDate(v);
            }
          }
          const existed = this.store.has(t.uuid);
          const defaults: Pick<FakeTaskRecord, "status" | "entry"> = {
            status: "pending",
            entry: twNow(),
          };
          // REAL `task import` REPLACES the task for a matching uuid — it does NOT merge over the prior
          // row. Model replace-not-merge so a future PARTIAL import (dropping UDAs) is caught by tests
          // rather than silently preserved by a merge that real taskwarrior would not perform.
          const replaced: FakeTaskRecord = {
            ...defaults,
            ...t,
            modified: twNow(),
          };
          this.store.set(t.uuid, replaced);
          if (existed) updated++;
          else added++;
        }
        // taskwarrior prints one line per imported task.
        const lines = incoming.map(
          (t) => `Importing '${t.description ?? ""}' ${t.uuid}`,
        );
        return ok(lines.join("\n") + `\nImported ${added + updated} tasks.\n`);
      }

      // `task <filter> count`
      const countIdx = rest.indexOf("count");
      if (countIdx !== -1) {
        const filter = rest.slice(0, countIdx);
        return ok(String(this.applyFilter(filter, false).length) + "\n");
      }

      // `task _uuids` convenience.
      if (rest.includes("_uuids")) {
        return ok([...this.store.keys()].join("\n") + (this.store.size ? "\n" : ""));
      }

      return fail(2, `fake-task: unsupported invocation: ${rest.join(" ")}`);
    });
    return this;
  }
}
