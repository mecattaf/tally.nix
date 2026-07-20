// tally — the drain surface (M2.5 triggers; SPEC "Trigger surface"; PS#16b; DECISIONS PS#1/PS#16).
//
// TWO distinct pieces, deliberately co-located because they are two faces of one `queue.drain`:
//
//  1. The IN-DAEMON drain (`Drainer.drain`): what the `queue.drain` RPC handler runs — sweep the
//     `events/` drop dir (via `EventsDir`) AND re-present pending durable TaskChampion rows into the
//     live queue (via the injected `RepresentFn`, the jobs engine's recover/re-present path). This
//     runs INSIDE the daemon, reaching the jobs engine through the ordinary enqueue / re-present
//     paths, so the one-queue invariant (PS#16b) and the single `lease_epoch` source are preserved.
//     `queue.drain` is idempotent: a double-drain enqueues each pending file once (the archive step
//     is the fence) and re-presents each durable row once (the jobs engine dedups by task_uuid).
//
//  2. The ONESHOT (`runDrainOneshot`): the `tally daemon drain` verb the module's `Persistent=true`
//     systemd timer invokes. It is a THIN SOCKET CLIENT — it connects to the tally socket and issues
//     the `queue.drain` RPC, then exits. It instantiates NO jobs engine, NO queue, NO lease client.
//     If the socket is absent it FAILS (exit non-zero); systemd retries at the next timer tick. No
//     filesystem-drain codepath ever runs outside the daemon (PS#1).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { connect } from "node:net";
import { existsSync } from "node:fs";
import type { PathEnv } from "../contracts/paths";
import { socketPath } from "../contracts/paths";
import type { EventsDir, SweepResult } from "./events-dir";

// ---------------------------------------------------------------------------------------------
// In-daemon drain — sweep events/ + re-present durable TW rows.
// ---------------------------------------------------------------------------------------------

/**
 * Re-present pending durable TaskChampion rows into the live queue (SPEC recover() "re-present, never
 * replay"; PS#9). Injected by the composition root — it is the jobs engine's re-present path (M2.2
 * `recover.ts`), which reads undeleted durable rows via the TW veneer and re-dispatches them
 * (`pi --resume`, `labor_class=recovered`), deduped by `task_uuid` so a double-drain re-presents each
 * row exactly once. Returns the number of rows re-presented. `triggers` never imports jobs — it holds
 * only this typed seam.
 */
export type RepresentFn = () => Promise<number>;

/** The result of one in-daemon drain: the events-dir sweep plus the re-present count. */
export interface DrainResult {
  /** The `events/` sweep outcome. */
  sweep: SweepResult;
  /** Files accepted (enqueued) this drain. */
  enqueued: number;
  /** Files rejected (quarantined) this drain. */
  rejected: number;
  /** Durable TW rows re-presented into the live queue this drain. */
  represented: number;
}

/** Options for the in-daemon drainer. */
export interface DrainerOptions {
  /** The events-dir sweeper (triggers-owned). */
  eventsDir: EventsDir;
  /**
   * The jobs-engine re-present path (composition-root-injected). Optional: when absent (e.g. a dev
   * rig with no durable store yet mounted) the drain still sweeps `events/`, re-presenting zero rows.
   */
  represent?: RepresentFn;
}

/**
 * The in-daemon drainer the `queue.drain` RPC handler invokes. Sweeps `events/` and re-presents
 * durable rows in one pass, both reaching the jobs engine through its ordinary paths.
 */
export class Drainer {
  private readonly eventsDir: EventsDir;
  private readonly represent: RepresentFn | undefined;

  constructor(opts: DrainerOptions) {
    this.eventsDir = opts.eventsDir;
    this.represent = opts.represent;
  }

  /** Run one drain: sweep the drop dir, then re-present pending durable rows. */
  async drain(): Promise<DrainResult> {
    const sweep = await this.eventsDir.sweep();
    const represented = this.represent ? await this.represent() : 0;
    return {
      sweep,
      enqueued: sweep.accepted,
      rejected: sweep.rejected,
      represented,
    };
  }
}

// ---------------------------------------------------------------------------------------------
// The `tally daemon drain` oneshot — a thin socket client of the `queue.drain` RPC.
// ---------------------------------------------------------------------------------------------

/** The `queue.drain` method name (the one RPC the oneshot issues). */
export const QUEUE_DRAIN_METHOD = "queue.drain" as const;

/** Options for the drain oneshot. */
export interface DrainOneshotOptions {
  env: PathEnv;
  /** Connect timeout (ms) before the oneshot gives up on an absent/slow daemon. */
  connectTimeoutMs?: number;
  /** Request timeout (ms) for the `queue.drain` round-trip. */
  requestTimeoutMs?: number;
  /** Diagnostic sink (default: stderr). */
  stderr?: (line: string) => void;
}

/**
 * The `tally daemon drain` entrypoint. Connects to the tally socket, issues one `queue.drain` request,
 * and returns a process exit code: `0` on a served drain, non-zero when the socket is absent or the
 * RPC fails. THE ONESHOT DOES NO SWEEP ITSELF — the daemon does. If the socket is missing it fails so
 * systemd retries at the next timer tick (M2.5: "socket-absent oneshot fails cleanly").
 */
export async function runDrainOneshot(opts: DrainOneshotOptions): Promise<number> {
  const path = socketPath(opts.env);
  const stderr = opts.stderr ?? ((l: string) => process.stderr.write(l + "\n"));

  // Fast, honest failure when the socket is absent — no connect attempt to a missing path.
  if (!existsSync(path)) {
    stderr(`tally: daemon socket absent at ${path}; drain skipped (will retry next timer tick)`);
    return 1;
  }

  try {
    const result = await drainOverSocket(path, {
      connectTimeoutMs: opts.connectTimeoutMs ?? 2000,
      requestTimeoutMs: opts.requestTimeoutMs ?? 30000,
    });
    stderr(`tally: drain served — ${JSON.stringify(result)}`);
    return 0;
  } catch (err) {
    stderr(`tally: drain failed: ${err instanceof Error ? err.message : String(err)}`);
    return 1;
  }
}

/**
 * Connect to the tally socket, issue one `queue.drain` request over the frozen §2 NDJSON framing, and
 * resolve with its result (or reject on error/timeout/close). A self-contained minimal client —
 * production code cannot import the test-kit socket client, and the oneshot needs only this one call.
 */
export function drainOverSocket(
  path: string,
  timeouts: { connectTimeoutMs: number; requestTimeoutMs: number },
): Promise<unknown> {
  return new Promise<unknown>((resolve, reject) => {
    let buf = "";
    let settled = false;
    const id = "drain-1";

    const connectTimer = setTimeout(() => {
      fail(new Error(`connect timeout: ${path}`));
    }, timeouts.connectTimeoutMs);

    // Guard the connect itself: connecting to a non-socket / missing path can throw synchronously on
    // some platforms rather than emitting an `error` event — catch it so the oneshot fails cleanly.
    let sock: ReturnType<typeof connect>;
    try {
      sock = connect(path);
    } catch (err) {
      clearTimeout(connectTimer);
      reject(err instanceof Error ? err : new Error(String(err)));
      return;
    }
    // A late `error` (e.g. ENOTSOCK surfaced asynchronously) must not become an uncaught exception.
    sock.on("error", (err: Error) => fail(err));
    sock.setEncoding("utf8");

    let requestTimer: ReturnType<typeof setTimeout> | null = null;

    function cleanup(): void {
      clearTimeout(connectTimer);
      if (requestTimer) clearTimeout(requestTimer);
      try {
        sock.end();
        sock.destroy();
      } catch {
        // ignore teardown races
      }
    }

    function fail(err: Error): void {
      if (settled) return;
      settled = true;
      cleanup();
      reject(err);
    }

    function done(value: unknown): void {
      if (settled) return;
      settled = true;
      cleanup();
      resolve(value);
    }

    sock.once("connect", () => {
      clearTimeout(connectTimer);
      requestTimer = setTimeout(() => {
        fail(new Error(`request timeout: ${QUEUE_DRAIN_METHOD}`));
      }, timeouts.requestTimeoutMs);
      sock.write(JSON.stringify({ id, method: QUEUE_DRAIN_METHOD, params: {} }) + "\n");
    });

    sock.on("data", (chunk: string) => {
      buf += chunk;
      let idx: number;
      while ((idx = buf.indexOf("\n")) !== -1) {
        const line = buf.slice(0, idx);
        buf = buf.slice(idx + 1);
        if (line.trim().length === 0) continue;
        let frame: { id?: unknown; result?: unknown; error?: { message?: unknown } };
        try {
          frame = JSON.parse(line) as typeof frame;
        } catch {
          fail(new Error(`bad frame (not JSON): ${line}`));
          return;
        }
        // Ignore any interleaved event frames (no `id`); correlate our one response by id.
        if (frame.id !== id) continue;
        if (frame.error) {
          const msg = typeof frame.error.message === "string" ? frame.error.message : JSON.stringify(frame.error);
          fail(new Error(`queue.drain error: ${msg}`));
          return;
        }
        done(frame.result);
        return;
      }
    });

    sock.on("close", () => fail(new Error("socket closed before drain response")));
  });
}
