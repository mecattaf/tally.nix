// tally — TaskChampion shell-out client (IMPLEMENTATION-PLAN M1.3).
//
// The thin durable veneer over taskwarrior 3.x / TaskChampion. Access is `task export` /
// `task import` / `task config` **shell-out ONLY** (DECISIONS jul9: never in-process, never
// Rust-inside-Bun FFI). 30–80 ms per call is fine — the veneer is never on a hot loop
// (heartbeats/leases/evidence never touch TW; SPEC veneer discipline).
//
// Every subprocess call rides the injectable `Exec` seam (src/contracts/exec.ts), so this module
// is fully testable against the layer-0 fake `task` binary with no real taskwarrior present.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Exec, ExecResult } from "../contracts/exec";
import type { Clock } from "../contracts/exec";
import { systemClock } from "../contracts/exec";
import { TallyError } from "../contracts/errors";
import type { TaskRow } from "../contracts/task";

/** The taskwarrior binary name (resolved from PATH; the nix module pins taskwarrior 3.x). */
export const TASK_BIN = "task";

/**
 * `rc.*` overrides applied on EVERY invocation. `rc.json.array=on` makes `export` emit a JSON
 * array (stable to parse); `rc.confirmation=off` makes `config`/`import` non-interactive
 * (IMPLEMENTATION-PLAN M1.3 — `rc.confirmation=off` on the UDA bootstrap); `rc.recurrence=off`
 * keeps the veneer from ever materializing recurring children behind tally's back; `rc.gc=off`
 * keeps uuids stable across a mutate→export cycle so the op-log delta (oplog.ts) lines up.
 */
export const RC_OVERRIDES: readonly string[] = [
  "rc.json.array=on",
  "rc.confirmation=off",
  "rc.recurrence=off",
  "rc.gc=off",
];

/** Default per-call timeout — generous relative to the 30–80 ms nominal, tight enough to fail fast. */
export const TASK_TIMEOUT_MS = 15000;

/**
 * A shell-out failure from the `task` binary. Carries the argv and captured stderr so the veneer's
 * callers (and the jobs engine) can log the exact failing invocation. Projects to the wire as an
 * `internal` error — a taskwarrior fault is never a client-params fault.
 */
export class TaskShellError extends TallyError {
  readonly argv: readonly string[];
  readonly exitCode: number;
  readonly stderr: string;

  constructor(argv: readonly string[], result: ExecResult) {
    super(
      "internal",
      `task ${argv.join(" ")} exited ${result.code}: ${result.stderr.trim() || "(no stderr)"}`,
      { argv: [...argv], code: result.code },
    );
    this.name = "TaskShellError";
    this.argv = [...argv];
    this.exitCode = result.code;
    this.stderr = result.stderr;
    Object.setPrototypeOf(this, TaskShellError.prototype);
  }
}

/** Options for constructing a {@link TaskClient}. */
export interface TaskClientOptions {
  /** The injectable subprocess seam — the ONLY way this client shells out. */
  exec: Exec;
  /** Injectable clock (unused for timing today, reserved for retry/backoff); defaults to system. */
  clock?: Clock;
  /** Binary name / path; defaults to `task`. */
  bin?: string;
  /** Per-call timeout ms; defaults to {@link TASK_TIMEOUT_MS}. */
  timeoutMs?: number;
}

/**
 * The low-level shell-out client. Owns the three sanctioned verbs (`export`, `import`, `config`)
 * and nothing else. Higher-level veneer logic (UDA bootstrap, row admission/serialization, op-log
 * shadow derivation) composes this client — it never re-implements the shell-out.
 */
export class TaskClient {
  private readonly exec: Exec;
  private readonly bin: string;
  private readonly timeoutMs: number;
  readonly clock: Clock;

  constructor(opts: TaskClientOptions) {
    this.exec = opts.exec;
    this.bin = opts.bin ?? TASK_BIN;
    this.timeoutMs = opts.timeoutMs ?? TASK_TIMEOUT_MS;
    this.clock = opts.clock ?? systemClock;
  }

  /**
   * Build the full argv: `task <rc overrides...> <args...>`. The rc overrides lead so a caller
   * filter/verb never shadows them.
   */
  private argv(args: readonly string[]): string[] {
    return [this.bin, ...RC_OVERRIDES, ...args];
  }

  /** Run a `task` invocation, throwing {@link TaskShellError} on a non-zero exit. */
  private async run(args: readonly string[], stdin?: string): Promise<ExecResult> {
    const argv = this.argv(args);
    const result = await this.exec.run(argv, {
      timeoutMs: this.timeoutMs,
      ...(stdin !== undefined ? { stdin } : {}),
    });
    if (result.code !== 0 || result.timedOut) {
      throw new TaskShellError(argv, result);
    }
    return result;
  }

  /**
   * `task [<filter>...] export` → parse the JSON array of rows. A filter is any taskwarrior filter
   * expression already split into argv tokens (e.g. `["status:pending"]`, `["trust:unreviewed"]`,
   * or a bare uuid). An empty filter exports the full store.
   */
  async export(filter: readonly string[] = []): Promise<TaskRow[]> {
    const result = await this.run([...filter, "export"]);
    return this.parseExport(result.stdout, filter);
  }

  /** Export a single row by uuid, or `undefined` when absent. */
  async exportOne(uuid: string): Promise<TaskRow | undefined> {
    const rows = await this.export([uuid]);
    // A uuid filter can match nothing (deleted/never-existed) — return the exact match if present.
    return rows.find((r) => r.uuid === uuid) ?? rows[0];
  }

  /**
   * Parse `task export` stdout into rows. taskwarrior with `rc.json.array=on` emits a JSON array;
   * we also tolerate the newline-delimited-objects form (older `rc.json.array=off`) for safety.
   */
  private parseExport(stdout: string, filter: readonly string[]): TaskRow[] {
    const trimmed = stdout.trim();
    if (trimmed.length === 0) return [];
    let parsed: unknown;
    try {
      parsed = trimmed.startsWith("[")
        ? JSON.parse(trimmed)
        : trimmed
            .split("\n")
            .filter((l) => l.trim().length > 0)
            .map((l) => JSON.parse(l));
    } catch (e) {
      throw new TaskShellError(this.argv([...filter, "export"]), {
        code: 0,
        stdout,
        stderr: `unparseable export JSON: ${String(e)}`,
      });
    }
    if (!Array.isArray(parsed)) {
      throw new TaskShellError(this.argv([...filter, "export"]), {
        code: 0,
        stdout,
        stderr: "export did not yield a JSON array",
      });
    }
    return parsed.map((r) => this.narrowRow(r));
  }

  /**
   * Hand-rolled narrowing of one exported record into a {@link TaskRow} (IMPLEMENTATION-PLAN:
   * hand-rolled validation, no zod). We require `uuid`, `description`, `status`; everything else
   * (native columns + UDAs + foreign passthrough attributes) rides through untouched so the
   * veneer never clobbers what it does not manage.
   */
  private narrowRow(raw: unknown): TaskRow {
    if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
      throw new TaskShellError([this.bin, "export"], {
        code: 0,
        stdout: JSON.stringify(raw),
        stderr: "export row is not an object",
      });
    }
    const obj = raw as Record<string, unknown>;
    const uuid = obj.uuid;
    const description = obj.description;
    const status = obj.status;
    if (typeof uuid !== "string" || uuid.length === 0) {
      throw new TaskShellError([this.bin, "export"], {
        code: 0,
        stdout: JSON.stringify(raw),
        stderr: "export row missing a string uuid",
      });
    }
    if (typeof description !== "string") {
      throw new TaskShellError([this.bin, "export"], {
        code: 0,
        stdout: JSON.stringify(raw),
        stderr: `export row ${uuid} missing a string description`,
      });
    }
    if (
      status !== "pending" &&
      status !== "waiting" &&
      status !== "completed" &&
      status !== "deleted"
    ) {
      throw new TaskShellError([this.bin, "export"], {
        code: 0,
        stdout: JSON.stringify(raw),
        stderr: `export row ${uuid} has unknown status ${String(status)}`,
      });
    }
    // Spread preserves every native column, UDA, and foreign attribute (merge-not-clobber).
    return { ...obj, uuid, description, status } as TaskRow;
  }

  /**
   * `task import` a batch of rows via stdin as a JSON array. taskwarrior upserts by uuid, so this
   * is the create-or-update path. Returns the imported rows unchanged (they carry the uuids the
   * caller passed; a caller may generate a uuid up-front so the import is a pure upsert).
   */
  async import(rows: readonly TaskRow[]): Promise<TaskRow[]> {
    if (rows.length === 0) return [];
    const payload = JSON.stringify(rows);
    await this.run(["import", "-"], payload);
    return [...rows];
  }

  /** Import a single row. */
  async importOne(row: TaskRow): Promise<TaskRow> {
    const [imported] = await this.import([row]);
    return imported ?? row;
  }

  /**
   * `task config <name> <value>` — set one rc/UDA config key. `rc.confirmation=off` (already in
   * {@link RC_OVERRIDES}) makes this non-interactive. Idempotent at the taskwarrior level: setting
   * a key to its current value is a no-op.
   */
  async config(name: string, value: string): Promise<void> {
    await this.run(["config", name, value]);
  }

  /**
   * `task config` (no args) → the full `key value` dump, parsed into a record. Used by the UDA
   * bootstrap to skip already-registered keys (bootstrap idempotence).
   */
  async dumpConfig(): Promise<Record<string, string>> {
    const result = await this.run(["config"]);
    const out: Record<string, string> = {};
    for (const line of result.stdout.split("\n")) {
      const trimmed = line.trim();
      if (trimmed.length === 0) continue;
      const sp = trimmed.indexOf(" ");
      if (sp === -1) {
        out[trimmed] = "";
      } else {
        out[trimmed.slice(0, sp)] = trimmed.slice(sp + 1);
      }
    }
    return out;
  }

  /** `task <filter> count` → the integer count. A cheap existence/size probe. */
  async count(filter: readonly string[] = []): Promise<number> {
    const result = await this.run([...filter, "count"]);
    const n = Number.parseInt(result.stdout.trim(), 10);
    return Number.isFinite(n) ? n : 0;
  }
}
