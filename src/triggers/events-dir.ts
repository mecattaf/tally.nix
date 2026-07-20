// tally — the `events/` drop-directory ingress (M2.5 triggers; SPEC "Trigger surface"; PS#16b;
// CLI-SURFACE §1.1a "the `events/` drop dir … is a client of this one verb").
//
// One of the three ingress paths, **one queue, no path privileged**. Each file dropped under
// `$XDG_STATE_HOME/tally/events/` is exactly ONE Seam-A enqueue payload (the same params
// `tally enqueue` accepts). The sweeper validates each file with the shared
// `validateEnqueueParams` narrower (so the events dir cannot drift from the frozen Seam-A grammar),
// enqueues it through the injected `EnqueueFn` — the ORDINARY in-daemon enqueue path, never a second
// queue — then archives the file to `events/done/`. A malformed file (bad JSON, failing validation)
// is quarantined to `events/rejected/` and a diagnostic line is emitted (journald under the daemon's
// `StandardOutput=journal` / `SyslogIdentifier=tally` capture).
//
// This module runs IN-DAEMON only: `queue.drain` (the sweep) and the `events/` fs.watch both call
// `EventsDir.sweep`. No filesystem-drain codepath ever runs outside the daemon (PS#1) — the events
// dir is an ingress that produces ordinary in-daemon enqueues, never a queue.
//
// The drop file's own `source` field is honored verbatim (no path is privileged — a file's
// provenance is whatever it declares: `r2` | `calendar` | `manual` | …); the events dir NEVER
// rewrites provenance.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  statSync,
} from "node:fs";
import { basename, join } from "node:path";
import type { PathEnv } from "../contracts/paths";
import { eventsDir, eventsDoneDir, eventsRejectedDir } from "../contracts/paths";
import type { EnqueueParams, EnqueueResult } from "../contracts/job";
import { validateEnqueueParams } from "../contracts/wire";
import { TallyError } from "../contracts/errors";

/**
 * The ordinary Seam-A enqueue entrypoint the events dir drives — the SAME function the
 * `queue.enqueue` RPC wraps (the jobs engine, M2.2), injected by the composition root (`main.ts`) so
 * the events dir reaches the jobs engine through the one enqueue path without importing it. Producing
 * an in-daemon enqueue, never a queue (PS#1/PS#16b).
 */
export type EnqueueFn = (params: EnqueueParams) => Promise<EnqueueResult>;

/**
 * A diagnostic notice sink. Production writes to stdout (captured by `StandardOutput=journal` under
 * `SyslogIdentifier=tally`, so a rejected drop file lands in journald as ruled); tests inject a
 * collector. A rejection is NOT a job-lifecycle `TALLY_EVENT`, so it rides as a plain structured
 * diagnostic line rather than through the journald event vocabulary.
 */
export type NoticeSink = (line: string) => void;

/** The default notice sink: one structured line to stdout, LF-terminated, captured by journald. */
export const stdoutNotice: NoticeSink = (line: string): void => {
  process.stdout.write(line + "\n");
};

/** The outcome of processing one drop file. */
export interface DropOutcome {
  /** The original drop-file path. */
  file: string;
  /** `accepted` = enqueued + archived to done/; `rejected` = quarantined to rejected/. */
  status: "accepted" | "rejected";
  /** On accept, the enqueue result the jobs engine returned. */
  result?: EnqueueResult;
  /** On reject, the reason (bad JSON / validation failure / enqueue error). */
  reason?: string;
  /** The path the file was moved to (done/ or rejected/). */
  archivedTo?: string;
}

/** The aggregate result of one sweep. */
export interface SweepResult {
  /** Every drop file processed this sweep, in filename-sorted order. */
  outcomes: DropOutcome[];
  accepted: number;
  rejected: number;
}

/** Options for the events-dir sweeper. */
export interface EventsDirOptions {
  env: PathEnv;
  /** The ordinary Seam-A enqueue path (jobs engine), injected by the composition root. */
  enqueue: EnqueueFn;
  /** Diagnostic sink for rejected files (default: stdout → journald). */
  notice?: NoticeSink;
}

/** File suffixes the sweeper treats as candidate drop files. */
const DROP_SUFFIXES = [".json"] as const;

/**
 * The `events/` drop-directory sweeper. Owns validation + archival; delegates the actual admission to
 * the injected `EnqueueFn`. Both the `queue.drain` handler and the `events/` fs.watch call `sweep`.
 * Concurrent sweeps are serialized so a watch edge landing mid-drain cannot double-process a file
 * (each file is claimed by an atomic rename before enqueue, so the archive step is the idempotency
 * fence — a second sweep finds the file already gone).
 */
export class EventsDir {
  private readonly env: PathEnv;
  private readonly enqueue: EnqueueFn;
  private readonly notice: NoticeSink;
  private readonly dir: string;
  private readonly doneDir: string;
  private readonly rejectedDir: string;
  /** Serializes overlapping sweeps (watch edge + timer drain) onto one tail. */
  private sweeping: Promise<SweepResult> | null = null;

  constructor(opts: EventsDirOptions) {
    this.env = opts.env;
    this.enqueue = opts.enqueue;
    this.notice = opts.notice ?? stdoutNotice;
    this.dir = eventsDir(this.env);
    this.doneDir = eventsDoneDir(this.env);
    this.rejectedDir = eventsRejectedDir(this.env);
  }

  /** The watched drop directory (`$XDG_STATE_HOME/tally/events/`). */
  get path(): string {
    return this.dir;
  }

  /** Ensure the drop dir and its done/ + rejected/ subdirs exist (idempotent). */
  ensureDirs(): void {
    mkdirSync(this.dir, { recursive: true });
    mkdirSync(this.doneDir, { recursive: true });
    mkdirSync(this.rejectedDir, { recursive: true });
  }

  /**
   * Sweep every pending drop file: validate, enqueue, archive. Overlapping calls share one tail so a
   * watch edge arriving mid-drain does not race the timer drain. Returns per-file outcomes.
   */
  sweep(): Promise<SweepResult> {
    if (this.sweeping) {
      // Chain a fresh sweep after the in-flight one so a late file is not stranded.
      const next = this.sweeping.then(() => this.runSweep());
      this.sweeping = next;
      return next;
    }
    const run = this.runSweep().finally(() => {
      if (this.sweeping === run) this.sweeping = null;
    });
    this.sweeping = run;
    return run;
  }

  private async runSweep(): Promise<SweepResult> {
    this.ensureDirs();
    const outcomes: DropOutcome[] = [];
    for (const name of this.listDropFiles()) {
      const file = join(this.dir, name);
      // Skip anything that vanished (already claimed by a concurrent sweep / a watch edge).
      if (!existsSync(file)) continue;
      outcomes.push(await this.processFile(file));
    }
    let accepted = 0;
    let rejected = 0;
    for (const o of outcomes) {
      if (o.status === "accepted") accepted++;
      else rejected++;
    }
    return { outcomes, accepted, rejected };
  }

  /**
   * Process one drop file: read, JSON-parse, validate as Seam-A params, enqueue, archive to done/.
   * Any failure quarantines the file to rejected/ + emits a diagnostic and returns a rejected outcome
   * — a malformed file never poisons the queue and never blocks the rest of the sweep.
   */
  async processFile(file: string): Promise<DropOutcome> {
    let raw: string;
    try {
      raw = readFileSync(file, "utf8");
    } catch (err) {
      return this.reject(file, `unreadable: ${errMsg(err)}`);
    }

    let parsed: unknown;
    try {
      parsed = JSON.parse(raw) as unknown;
    } catch (err) {
      return this.reject(file, `invalid JSON: ${errMsg(err)}`);
    }

    let params: EnqueueParams;
    try {
      params = validateEnqueueParams(parsed);
    } catch (err) {
      const detail = err instanceof TallyError ? err.message : errMsg(err);
      return this.reject(file, `invalid enqueue params: ${detail}`);
    }

    let result: EnqueueResult;
    try {
      result = await this.enqueue(params);
    } catch (err) {
      // An enqueue failure is a real admission error, not a malformed file — still quarantine so the
      // file is not re-swept forever, but label the reason distinctly for the operator.
      return this.reject(file, `enqueue failed: ${errMsg(err)}`);
    }

    const archivedTo = this.archive(file, this.doneDir);
    const outcome: DropOutcome = { file, status: "accepted", result };
    if (archivedTo !== null) outcome.archivedTo = archivedTo;
    return outcome;
  }

  /** Quarantine a file to rejected/ and emit a diagnostic notice. */
  private reject(file: string, reason: string): DropOutcome {
    const archivedTo = this.archive(file, this.rejectedDir);
    this.notice(
      JSON.stringify({
        SYSLOG_IDENTIFIER: "tally",
        TALLY_TRIGGER: "events-dir",
        TALLY_REJECT_REASON: reason,
        MESSAGE: `tally: rejected drop file ${basename(file)}: ${reason}`,
      }),
    );
    const outcome: DropOutcome = { file, status: "rejected", reason };
    if (archivedTo !== null) outcome.archivedTo = archivedTo;
    return outcome;
  }

  /**
   * Atomically move a processed file into an archive subdir. On a name collision (a re-dropped file
   * with the same name) a monotonic suffix is appended so nothing is silently overwritten. Returns
   * the destination path, or null if the source vanished (already archived by a concurrent sweep).
   */
  private archive(file: string, destDir: string): string | null {
    const base = basename(file);
    let dest = join(destDir, base);
    let n = 1;
    while (existsSync(dest)) {
      dest = join(destDir, `${base}.${n}`);
      n++;
    }
    try {
      renameSync(file, dest);
      return dest;
    } catch {
      // The file vanished between the sweep listing and the rename (a concurrent claim); treat as
      // already handled.
      return null;
    }
  }

  /**
   * List the pending drop files (top-level `*.json`), filename-sorted for deterministic order. The
   * `done/` and `rejected/` subdirs and any non-file entries are skipped.
   */
  private listDropFiles(): string[] {
    if (!existsSync(this.dir)) return [];
    let entries: string[];
    try {
      entries = readdirSync(this.dir);
    } catch {
      return [];
    }
    return entries
      .filter((name) => this.isDropCandidate(name))
      .sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  }

  private isDropCandidate(name: string): boolean {
    if (name.startsWith(".")) return false;
    if (!DROP_SUFFIXES.some((s) => name.endsWith(s))) return false;
    const full = join(this.dir, name);
    try {
      return statSync(full).isFile();
    } catch {
      return false;
    }
  }
}

/** Extract a message string from an unknown thrown value. */
function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
