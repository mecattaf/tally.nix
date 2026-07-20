// tally daemon-core — the lease-epoch source (PS#9 / PS#21; CLI-SURFACE §2.5).
//
// `lease_epoch` is ONE monotone series (PS#21 as amended for issue #9: "the pls lease generation,
// backstopped by a persisted counter file" — daemon-incremented). It has two writers of two FILES
// but a single ordering:
//   - the shim's `$XDG_STATE_HOME/tally/pls-generation` — bumped on every pls GRANT (the primary
//     source; nix/pls-shim.py is its sole writer, and it floors at OUR `epoch` file);
//   - daemon-core's `$XDG_STATE_HOME/tally/epoch` — bumped on every daemon (re)start (the backstop;
//     this module is its SOLE writer, and it floors at the shim's `pls-generation` file).
// Each bump on either side is strictly greater than the max of BOTH files, so boot fences and grant
// generations interleave into one totally-ordered series — a witness line's per-grant value and a
// snapshot header's boot value are always comparable (issue #4: before this convergence the two
// files ran as independent counters, e.g. boot fence 4/5/6 vs grants 16…30, mixing two meanings
// under the one wire name).
//
// The snapshot/ACK header epoch changes ONLY on daemon (re)start; `seq` is monotonic only WITHIN an
// epoch. This module is the ONLY increment owner of the `epoch` file (issue #9: a systemd
// `ExecStartPre` used to ALSO bump it before the daemon read it, so a single restart consumed two
// epoch values). The daemon must be able to bump-and-persist the counter on its own boot whether
// launched by systemd or run outside it (dev rig, tests, `tally daemon run` by hand), so this module
// both READS the files and ATOMICALLY bumps its own — always choosing an epoch strictly greater
// than every previously-observed source, so no restart ever reuses an epoch (a boot that merely
// ADOPTED the last grant generation would leave a zombie holding that exact generation unfenced by
// recover()'s strict `<` comparison), and the persisted file always equals the announced epoch.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { existsSync, mkdirSync, readFileSync, writeFileSync, renameSync } from "node:fs";
import { dirname } from "node:path";
import { epochPath, plsGenerationPath, type PathEnv } from "../contracts/paths";

/** How the current epoch was arrived at — for diagnostics/logging (never on the wire). */
export type EpochSource = "pls-generation" | "counter-file" | "genesis";

/** The resolved epoch plus how it was derived. */
export interface ResolvedEpoch {
  epoch: number;
  source: EpochSource;
}

/** Read a counter file tolerantly: `0` when absent/empty/corrupt (never throw at boot). */
function readCounterFile(path: string): number {
  if (!existsSync(path)) return 0;
  try {
    const raw = readFileSync(path, "utf8").trim();
    if (raw === "") return 0;
    const n = Number.parseInt(raw, 10);
    return Number.isFinite(n) && n >= 0 ? n : 0;
  } catch {
    return 0;
  }
}

/** Read the persisted counter value, or `0` when the file is absent/empty/corrupt (genesis). */
export function readEpochCounter(env: PathEnv): number {
  return readCounterFile(epochPath(env));
}

/**
 * Read the shim's grant-generation counter (`pls-generation`), or `0` when absent/corrupt. READ
 * only — the shim is that file's sole writer (the mirror of daemon-core solely writing `epoch`).
 */
export function readPlsGenerationCounter(env: PathEnv): number {
  return readCounterFile(plsGenerationPath(env));
}

/** Atomically persist a counter value (temp-then-rename so a torn write cannot corrupt it). */
export function writeEpochCounter(env: PathEnv, value: number): void {
  const path = epochPath(env);
  mkdirSync(dirname(path), { recursive: true });
  const tmp = `${path}.tmp.${process.pid}`;
  writeFileSync(tmp, `${value}\n`, "utf8");
  renameSync(tmp, path);
}

/**
 * Resolve the lease epoch for THIS daemon boot and persist it monotonically. The chosen epoch is
 * STRICTLY greater than the persisted counter, the shim's `pls-generation` file, and any supplied
 * live pls generation — so a restart never reuses or lowers an epoch (the fence invariant, PS#9),
 * and every previously-issued grant generation IS fenced by recover()'s strict `<` (an epoch merely
 * EQUAL to the last grant would leave a zombie holding that grant unfenced). This is the ONLY place
 * the `epoch` file is incremented, so the persisted value always equals the epoch this boot
 * announces — and because the shim's next grant floors at this file in turn, boot fences and grant
 * generations form the single monotone `lease_epoch` series PS#21 rules (issue #4).
 *
 * `plsGeneration` is an optional extra floor for a caller that already holds a live broker-reported
 * generation; the shim's counter file is always consulted regardless, so no boot path needs it.
 */
export function bumpEpoch(env: PathEnv, plsGeneration?: number): ResolvedEpoch {
  const counter = readEpochCounter(env);
  const supplied = plsGeneration !== undefined && Number.isFinite(plsGeneration) ? plsGeneration : 0;
  const plsFloor = Math.max(readPlsGenerationCounter(env), supplied);
  const next = Math.max(counter, plsFloor) + 1;
  writeEpochCounter(env, next);
  const source: EpochSource = plsFloor > counter ? "pls-generation" : counter === 0 ? "genesis" : "counter-file";
  return { epoch: next, source };
}
