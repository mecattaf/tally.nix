// tally — the ambient pls-lease-wrap helper (IMPLEMENTATION-PLAN M1.5 wrap.ts; SPEC "The ambient
// default the module ships", PS#5/appendix§5).
//
// The ambient default: EVERY heavy (GPU-touching) invocation → pls-lease-wrapped, so a CC-spawned
// pi subagent is tally-compatible without CC knowing tally exists. This module implements the two
// halves of that default:
//
//   1. `tally pls-wrap -- <cmd...>` — the internal CLI verb (routed here by the CLI/main dispatch):
//      acquire a pls lease, run <cmd> under it, and RELEASE on the child's process exit — the single
//      RAII release path. This is what a heavy tenant execs to become tally-compatible.
//
//   2. `renderWrapScript()` — the text of the standalone `pls-lease-wrap` shell script the nix
//      module installs on PATH (SPEC "The ambient default"). It shells `tally pls-wrap -- "$@"`, so
//      any invocation prefixed with `pls-lease-wrap` is lease-gated WITHOUT the caller importing
//      tally. tally owns pls's config, so the wrapper is tally's to ship.
//
// The wrap acquires DIRECTLY at the declared priority (SPEC "every heavy tenant acquires the pls
// lease directly") — it does not require the daemon: a direct non-tally tenant (ds4-server, the OCR
// vLLM) uses exactly this path. The lease is held for the child's whole lifetime and released once,
// on child exit. No vendor code (clean-room, CLI-SURFACE §4).

import type { Exec, Pool } from "../contracts/index";
import { PlsBroker } from "./broker";
import { LeaseManager } from "./lease";
import { PoolRegistry } from "./pools";

/** Options parsed from a `tally pls-wrap` invocation. */
export interface WrapOptions {
  /** The pool to lease (default: the heavy pool, worker-gpu). */
  pool?: Pool;
  /** Estimated VRAM-GB cost (default 1 — a conservative small hold). */
  cost: number;
  /** Declared priority (default 0). */
  priority: number;
  /** The tenant label (default `tally`; a direct non-tally tenant may override). */
  tenant?: string;
  /** The command + args to run under the lease (everything after `--`). */
  command: string[];
}

/** The default VRAM-GB cost a bare `pls-wrap` holds when the caller declares none. */
export const DEFAULT_WRAP_COST = 1;

/**
 * Parse the argv AFTER the `pls-wrap` verb into `WrapOptions`. Grammar (CLI-SURFACE internal verb;
 * main.ts help text `tally pls-wrap -- <cmd>`):
 *   [--pool <p>] [--cost <n>] [--priority <n>] [--tenant <t>] -- <cmd> [args...]
 * Everything after the first bare `--` is the command verbatim. Throws on a missing command.
 */
export function parseWrapArgs(args: readonly string[]): WrapOptions {
  let pool: Pool | undefined;
  let cost = DEFAULT_WRAP_COST;
  let priority = 0;
  let tenant: string | undefined;
  let i = 0;
  for (; i < args.length; i++) {
    const a = args[i]!;
    if (a === "--") {
      i++;
      break;
    }
    const takeValue = (name: string): string => {
      const v = args[i + 1];
      if (v === undefined) throw new Error(`tally pls-wrap: ${name} requires a value`);
      i++;
      return v;
    };
    if (a === "--pool") pool = takeValue("--pool") as Pool;
    else if (a === "--cost") cost = Number(takeValue("--cost"));
    else if (a === "--priority") priority = Number(takeValue("--priority"));
    else if (a === "--tenant") tenant = takeValue("--tenant");
    else if (a.startsWith("--pool=")) pool = a.slice("--pool=".length) as Pool;
    else if (a.startsWith("--cost=")) cost = Number(a.slice("--cost=".length));
    else if (a.startsWith("--priority=")) priority = Number(a.slice("--priority=".length));
    else if (a.startsWith("--tenant=")) tenant = a.slice("--tenant=".length);
    else throw new Error(`tally pls-wrap: unexpected argument '${a}' before '--'`);
  }
  const command = args.slice(i);
  if (command.length === 0) {
    throw new Error("tally pls-wrap: no command given (usage: tally pls-wrap [--pool p] [--cost n] [--priority n] -- <cmd>)");
  }
  if (!Number.isFinite(cost) || cost < 0) throw new Error("tally pls-wrap: --cost must be a non-negative number");
  if (!Number.isFinite(priority)) throw new Error("tally pls-wrap: --priority must be a number");
  return {
    ...(pool !== undefined ? { pool } : {}),
    cost,
    priority,
    ...(tenant !== undefined ? { tenant } : {}),
    command,
  };
}

/**
 * Run `tally pls-wrap`: acquire a pls lease, run the wrapped command under it as a streamed child,
 * and release the lease exactly once on child exit (the RAII release path). Returns the child's
 * exit code so the caller (main.ts) can mirror it as tally's exit code.
 *
 * Serialization (acquire-before-GPU): the wrap NEVER runs the child without the lease. When the pool
 * has no headroom (a competitor holds the single slot), the wrap WAITS for the pool to drain — it
 * polls the pool status at a fixed cadence and only issues the actual `acquire` once there is a free
 * slot within budget, so it never leaves an orphan queue ticket behind (a re-`acquire` loop would
 * pile up phantom waiters the broker auto-promotes). This is a genuine serialization: two
 * `pls-wrap`s on one pool run one at a time, in ask-order.
 */
export async function runWrap(
  exec: Exec,
  registry: PoolRegistry,
  opts: WrapOptions,
  hooks: { sleep: (ms: number) => Promise<void>; queuePollMs?: number } = { sleep: defaultSleep },
): Promise<number> {
  const broker = new PlsBroker(exec);
  const leases = new LeaseManager(broker, registry);
  const descriptor = opts.pool ? registry.require(opts.pool) : registry.defaultHeavyPool();
  const pool: Pool = descriptor.name;
  const queuePollMs = hooks.queuePollMs ?? 250;

  // Acquire-before-GPU: wait for pool headroom, then acquire and hold for the child's whole life.
  let lease;
  for (;;) {
    const status = await broker.status(descriptor.broker, pool);
    const hasHeadroom =
      status.held < status.capacity &&
      (status.free_cost === undefined || status.free_cost >= opts.cost);
    if (hasHeadroom) {
      const outcome = await leases.acquire(pool, {
        cost: opts.cost,
        priority: opts.priority,
        ...(opts.tenant !== undefined ? { tenant: opts.tenant } : {}),
      });
      if (outcome.kind === "granted") {
        lease = outcome.lease;
        break;
      }
      // Lost a race to another tenant between the status probe and the acquire — release any queue
      // ticket the broker minted for us and wait for the next drain (no orphan hold).
      await leases.reclaim(pool, outcome.leaseId);
    }
    // No headroom (or we lost the race) — wait and re-probe (the serialization point).
    await hooks.sleep(queuePollMs);
  }

  try {
    const child = exec.spawn(opts.command);
    // Drain stdout so the child is not blocked on a full pipe; pls-wrap is transparent, not a filter.
    void (async () => {
      try {
        for await (const _line of child.lines()) {
          // The wrapper is out-of-band: it neither interprets nor rewrites the child's stream.
        }
      } catch {
        // Stream errors surface via `exited`; nothing to do here.
      }
    })();
    const code = await child.exited;
    return code;
  } finally {
    // The SINGLE release path: the child has exited, so free the lease exactly once.
    await lease.release();
  }
}

function defaultSleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Render the standalone `pls-lease-wrap` shell script the nix module installs on PATH (SPEC "The
 * ambient default"). It forwards to `tally pls-wrap -- "$@"`, so any command prefixed with
 * `pls-lease-wrap` is lease-gated without importing tally. `tallyBin` is the store path of the tally
 * binary the module injects; defaulting to `tally` keeps the script usable when tally is on PATH.
 *
 * The script passes an explicit `--` so the wrapped command's own flags are never parsed as
 * `pls-wrap` options.
 */
export function renderWrapScript(tallyBin = "tally"): string {
  return `#!/usr/bin/env bash
# pls-lease-wrap — tally's ambient GPU-lease wrapper (SPEC "The ambient default").
# Every heavy (GPU-touching) invocation prefixed with this runs under a pls lease,
# so a subagent is tally-compatible without knowing tally exists. Owned by the tally
# module (tally owns pls's pool config, PS#5). Do not edit by hand — it is rendered.
set -euo pipefail
exec ${shellQuote(tallyBin)} pls-wrap -- "$@"
`;
}

/** Minimal POSIX single-quote escaping for a shell literal (keeps the compile dependency-free). */
function shellQuote(s: string): string {
  return `'${s.replace(/'/g, `'\\''`)}'`;
}
