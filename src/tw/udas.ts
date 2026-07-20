// tally — UDA vocabulary bootstrap (IMPLEMENTATION-PLAN M1.3 `udas.ts`).
//
// Bootstraps the tally UDA vocabulary idempotently via `task config` (with `rc.confirmation=off`,
// supplied by client.ts's RC_OVERRIDES). The vocabulary is the FROZEN table in
// src/contracts/task.ts (`TALLY_UDAS`) — the single source of truth this bootstrap and the row
// (de)serializer both read. UDAs ride TaskChampion config: "zero new tally code, TaskChampion
// syncs the field for free" (SPEC "The trust review UDA").
//
// Idempotence: taskwarrior's `task config <name> <value>` is itself idempotent (setting a key to
// its current value is a no-op), and we additionally skip a key whose value already matches the
// current config dump — so a second bootstrap issues NO `task config` writes at all. This is what
// the bootstrap-idempotence test asserts.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { TALLY_UDAS, type UdaSpec } from "../contracts/task";
import type { TaskClient } from "./client";

/**
 * The set of `task config` key/value pairs one UDA spec expands to. taskwarrior models a UDA as:
 *   uda.<name>.type   = string|numeric|date
 *   uda.<name>.label  = <prose>            (optional)
 *   uda.<name>.values = a,b,c              (enumerated string UDAs only)
 * (IMPLEMENTATION-PLAN M1.3; taskwarrior 3.x UDA config schema.)
 */
export function udaConfigPairs(spec: UdaSpec): Array<[string, string]> {
  const pairs: Array<[string, string]> = [[`uda.${spec.name}.type`, spec.type]];
  if (spec.label !== undefined) {
    pairs.push([`uda.${spec.name}.label`, spec.label]);
  }
  if (spec.values !== undefined && spec.values.length > 0) {
    pairs.push([`uda.${spec.name}.values`, spec.values.join(",")]);
  }
  return pairs;
}

/** Every `task config` key/value pair the full tally vocabulary expands to, in declaration order. */
export function allUdaConfigPairs(specs: readonly UdaSpec[] = TALLY_UDAS): Array<[string, string]> {
  return specs.flatMap((s) => udaConfigPairs(s));
}

/** The result of a bootstrap run — which keys were written vs already present (for observability/tests). */
export interface BootstrapResult {
  /** Keys that were `task config`-written this run (absent or mismatched before). */
  readonly written: string[];
  /** Keys already present with the correct value — skipped. */
  readonly skipped: string[];
}

/**
 * Bootstrap the tally UDA vocabulary. Reads the current `task config` dump ONCE, then writes only
 * the keys that are missing or hold a different value. A fully-provisioned store yields an all-skip
 * result with zero writes (idempotence). Returns the write/skip breakdown.
 */
export async function bootstrapUdas(
  client: TaskClient,
  specs: readonly UdaSpec[] = TALLY_UDAS,
): Promise<BootstrapResult> {
  const current = await client.dumpConfig();
  const written: string[] = [];
  const skipped: string[] = [];

  for (const [key, value] of allUdaConfigPairs(specs)) {
    if (current[key] === value) {
      skipped.push(key);
      continue;
    }
    await client.config(key, value);
    written.push(key);
  }

  return { written, skipped };
}

/**
 * True when every tally UDA key is already registered with its declared value in the given config
 * dump — the "already bootstrapped" predicate a caller can check before deciding to bootstrap.
 */
export function isBootstrapped(
  config: Record<string, string>,
  specs: readonly UdaSpec[] = TALLY_UDAS,
): boolean {
  return allUdaConfigPairs(specs).every(([key, value]) => config[key] === value);
}
