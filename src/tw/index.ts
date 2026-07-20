// tally — the TaskChampion veneer barrel (IMPLEMENTATION-PLAN M1.3).
//
// The thin durable veneer over taskwarrior 3.x / TaskChampion, `task export`/`import`/`config`
// shell-out only (DECISIONS jul9). Composes the low-level client (client.ts), the idempotent UDA
// bootstrap (udas.ts), durable-row admission + (de)serialization (rows.ts), and the `prev_*`
// op-log shadow derivation (oplog.ts) into one facade the jobs engine (M2.2) and intake-gh (M2.4)
// consume. The store is single and authoritative on the conductor — no sync, no replication
// (SPEC "Conductor-receiver").

import type { Clock } from "../contracts/exec";
import { systemClock } from "../contracts/exec";
import {
  admitsDurableRow,
  type AdmissionInput,
  type PrevShadow,
  type TaskRow,
  type Trust,
} from "../contracts/task";
import type { EnqueueParams } from "../contracts/job";

import { TaskClient, type TaskClientOptions, TaskShellError } from "./client";
import { bootstrapUdas, type BootstrapResult } from "./udas";
import {
  admissionForEnqueue,
  buildRow,
  cancelRow,
  completeRow,
  overlayManaged,
  setTrust,
  toTwDatetime,
  type RowSeed,
} from "./rows";
import { capture, derive, withShadow, hasShadow, type PreImage } from "./oplog";

export * from "./client";
export * from "./udas";
export * from "./rows";
export * from "./oplog";

/** Options for constructing the {@link TaskChampion} veneer facade. */
export interface TaskChampionOptions extends TaskClientOptions {}

/**
 * The TaskChampion veneer facade — the single object the jobs engine holds. Wraps the shell-out
 * client and exposes the veneer-shaped operations: UDA bootstrap, admission-gated durable-row
 * create, completion (with `trust:unreviewed`), cancel, trust flips, and shadow-carrying mutation.
 * Every write is admission-gated and veneer-clean (rows.ts guards).
 */
export class TaskChampion {
  readonly client: TaskClient;
  private readonly clock: Clock;

  constructor(opts: TaskChampionOptions) {
    this.client = new TaskClient(opts);
    this.clock = opts.clock ?? systemClock;
  }

  /**
   * The taskwarrior COMPACT datetime for "now" (`YYYYMMDDTHHMMSSZ`, UTC, no fractional seconds) — the
   * canonical form `task import`/`task export` round-trips. The clock's ISO-8601 carries milliseconds
   * (`.512Z`); feeding that fractional form to real taskwarrior 3 on a non-UTC host mis-parses the
   * instant (dropping Z-handling → local time → an hours offset) and never round-trips. Normalizing to
   * the compact UTC form here keeps every durable date field (entry/modified/end) correct on any host.
   */
  private nowIso(): string {
    return toTwDatetime(this.clock.nowIso());
  }

  /** Bootstrap the UDA vocabulary idempotently. Call once at daemon boot. */
  async bootstrap(): Promise<BootstrapResult> {
    return bootstrapUdas(this.client);
  }

  /**
   * Whether a Seam-A enqueue earns a durable row. Live-orchestrator-spawned units (source
   * `orchestrator`) get no row unless `overrides` declares durability (e.g. a standing drain row).
   */
  admits(params: Pick<EnqueueParams, "source">, overrides: Partial<AdmissionInput> = {}): boolean {
    return admitsDurableRow(admissionForEnqueue(params, overrides));
  }

  /**
   * Create a durable row for an admitted job and `import` it. Returns the created row. The caller
   * MUST have already checked {@link admits}; a live-orchestrator unit has no row and never calls
   * this (its record is JSONL + witness).
   */
  async createRow(seed: RowSeed): Promise<TaskRow> {
    const row = buildRow(seed, () => this.nowIso());
    return this.client.importOne(row);
  }

  /** Export the current image of a row by uuid, or undefined when absent/deleted. */
  async getRow(uuid: string): Promise<TaskRow | undefined> {
    return this.client.exportOne(uuid);
  }

  /** Export a filtered set of rows (e.g. `["status:pending"]`, `["trust:unreviewed"]`). */
  async query(filter: readonly string[] = []): Promise<TaskRow[]> {
    return this.client.export(filter);
  }

  /**
   * Complete a durable row: capture the pre-image, flip to `completed`, write `trust:unreviewed`
   * and the labor class, `import`, and return the post row plus the `prev_*` shadow the wire event
   * carries. `trust` NEVER blocks future work — it describes past work (SPEC "The trust review UDA").
   */
  async complete(
    uuid: string,
    opts: { laborClass?: TaskRow["labor_class"]; trust?: Trust } = {},
  ): Promise<{ row: TaskRow; shadow: PrevShadow }> {
    return withShadow(this.client, uuid, async (pre: PreImage) => {
      if (pre.row === undefined) {
        throw new TaskShellError(["task", "export", uuid], {
          code: 0,
          stdout: "",
          stderr: `cannot complete unknown row ${uuid}`,
        });
      }
      const next = completeRow(pre.row, opts, () => this.nowIso());
      return this.client.importOne(next);
    });
  }

  /** Cancel a durable row (mark `deleted`); returns the post row + shadow. Deleted rows are not re-presented. */
  async cancel(uuid: string): Promise<{ row: TaskRow; shadow: PrevShadow }> {
    return withShadow(this.client, uuid, async (pre: PreImage) => {
      if (pre.row === undefined) {
        throw new TaskShellError(["task", "export", uuid], {
          code: 0,
          stdout: "",
          stderr: `cannot cancel unknown row ${uuid}`,
        });
      }
      const next = cancelRow(pre.row, () => this.nowIso());
      return this.client.importOne(next);
    });
  }

  /** Flip a row's `trust` (review/recall). Returns the post row + shadow. */
  async review(uuid: string, trust: Trust): Promise<{ row: TaskRow; shadow: PrevShadow }> {
    return withShadow(this.client, uuid, async (pre: PreImage) => {
      if (pre.row === undefined) {
        throw new TaskShellError(["task", "export", uuid], {
          code: 0,
          stdout: "",
          stderr: `cannot review unknown row ${uuid}`,
        });
      }
      const next = setTrust(pre.row, trust, () => this.nowIso());
      return this.client.importOne(next);
    });
  }

  /**
   * Overlay tally-managed fields onto an existing row without clobbering foreign attributes
   * (merge-not-clobber), capturing the `prev_*` shadow. Used for a re-dispatch (fresh→recovered
   * labor class, bumped attempt/lease_epoch) that must survive across recover().
   */
  async patchManaged(
    uuid: string,
    update: Partial<TaskRow>,
  ): Promise<{ row: TaskRow; shadow: PrevShadow }> {
    return withShadow(this.client, uuid, async (pre: PreImage) => {
      if (pre.row === undefined) {
        throw new TaskShellError(["task", "export", uuid], {
          code: 0,
          stdout: "",
          stderr: `cannot patch unknown row ${uuid}`,
        });
      }
      const next = overlayManaged(pre.row, { ...update, modified: this.nowIso() });
      return this.client.importOne(next);
    });
  }
}

export { capture, derive, hasShadow };
