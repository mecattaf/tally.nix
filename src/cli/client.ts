// tally CLI — the NDJSON Unix-socket client (IMPLEMENTATION-PLAN M3.1; CLI-SURFACE §0, §2.1).
//
// Every CLI verb is "a thin socket request against the daemon's Unix socket" (§0). This module owns
// the one client the whole CLI shares: it connects to `$XDG_RUNTIME_DIR/tally/tally.sock`, frames
// requests per the FROZEN §2.1 wire (one UTF-8 JSON object per line, LF-terminated, no raw embedded
// newlines), correlates responses by `id`, and — for `session watch` and the `--wait` barrier —
// buffers/streams pushed event frames. Three frame kinds ride one connection, disambiguated
// structurally: request `{id, method, params}`, response `{id, result|error}`, event `{seq, event, …}`.
//
// The client speaks the wire the daemon-core server implements; it never imports daemon-core
// internals (it is the CLIENT side of the dependency, IMPLEMENTATION-PLAN M3.1 `dependsOn`). A wire
// `error` becomes a typed `RpcError` the CLI surfaces (exit code + message). The socket path is
// resolved from the injected env so tests point it at a tmp socket.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { connect, type Socket } from "node:net";
import { socketPath, type PathEnv } from "../contracts/paths";
import type { WireError, WireErrorCode } from "../contracts/errors";

/** A pushed event frame `{seq?, id?, event, …payload}` (§2.1, §2.3). Payload fields are flat. */
export interface WireEvent {
  seq?: number;
  id?: string;
  event: string;
  [k: string]: unknown;
}

/** A response frame's `error` member, projected to a thrown `RpcError`. */
export class RpcError extends Error {
  readonly code: WireErrorCode | string;
  readonly data?: Record<string, unknown>;
  readonly method: string;
  constructor(method: string, err: WireError | { code: string; message?: string; data?: Record<string, unknown> }) {
    super(err.message ?? `RPC ${method} failed with ${err.code}`);
    this.name = "RpcError";
    this.method = method;
    this.code = err.code;
    if (err.data !== undefined) this.data = err.data;
    Object.setPrototypeOf(this, RpcError.prototype);
  }
}

/** Raised when the daemon socket is not present/reachable — the CLI surfaces "is the daemon running?". */
export class DaemonUnreachable extends Error {
  readonly path: string;
  constructor(path: string, cause?: unknown) {
    super(
      `tally: cannot reach the daemon at ${path} — is it running? (start it with \`systemctl --user start tally-daemon\` or \`tally daemon run\`)`,
    );
    this.name = "DaemonUnreachable";
    this.path = path;
    if (cause !== undefined) (this as { cause?: unknown }).cause = cause;
    Object.setPrototypeOf(this, DaemonUnreachable.prototype);
  }
}

interface Pending {
  resolve: (result: unknown) => void;
  reject: (err: Error) => void;
  method: string;
  timer: ReturnType<typeof setTimeout> | null;
}

/** Options for constructing the client. */
export interface TallyClientOptions {
  /** Explicit socket path; else resolved from `env`. */
  socket?: string;
  /** Env for path resolution (tests). Defaults to `process.env`. */
  env?: PathEnv;
  /** Connect timeout (ms). */
  connectTimeoutMs?: number;
  /** Default per-request timeout (ms). */
  requestTimeoutMs?: number;
}

/**
 * The one socket client the CLI shares. Connect once, issue any number of correlated RPCs, optionally
 * subscribe to the event stream. Structurally disambiguates the three frame kinds on the single
 * connection: a frame with `event` is an event; a frame with `id` + (`result`|`error`) is a response.
 */
export class TallyClient {
  readonly socketPath: string;
  private readonly connectTimeoutMs: number;
  private readonly requestTimeoutMs: number;

  private sock: Socket | null = null;
  private buf = "";
  private nextId = 1;
  private readonly pending = new Map<string, Pending>();
  private readonly eventListeners = new Set<(e: WireEvent) => void>();
  private closed = false;
  private failure: Error | null = null;

  constructor(opts: TallyClientOptions = {}) {
    this.socketPath = opts.socket ?? socketPath((opts.env ?? (process.env as PathEnv)));
    this.connectTimeoutMs = opts.connectTimeoutMs ?? 3000;
    this.requestTimeoutMs = opts.requestTimeoutMs ?? 10000;
  }

  /** Connect to the daemon socket. Rejects with {@link DaemonUnreachable} when the socket is absent. */
  connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      const sock = connect(this.socketPath);
      this.sock = sock;
      sock.setEncoding("utf8");
      const t = setTimeout(() => {
        sock.destroy();
        reject(new DaemonUnreachable(this.socketPath, new Error("connect timeout")));
      }, this.connectTimeoutMs);
      sock.once("connect", () => {
        clearTimeout(t);
        resolve();
      });
      sock.on("data", (chunk: string) => this.onData(chunk));
      sock.on("error", (err: Error) => {
        clearTimeout(t);
        const wrapped = new DaemonUnreachable(this.socketPath, err);
        this.fail(wrapped);
        reject(wrapped);
      });
      sock.on("close", () => {
        this.fail(this.failure ?? new Error("tally: daemon connection closed"));
      });
    });
  }

  /** Whether the connection has failed/closed. */
  get isClosed(): boolean {
    return this.closed;
  }

  private onData(chunk: string): void {
    this.buf += chunk;
    let nl: number;
    while ((nl = this.buf.indexOf("\n")) !== -1) {
      const line = this.buf.slice(0, nl);
      this.buf = this.buf.slice(nl + 1);
      if (line.trim().length === 0) continue;
      let frame: unknown;
      try {
        frame = JSON.parse(line);
      } catch {
        // A malformed frame from the daemon is a protocol break — fail loudly.
        this.fail(new Error(`tally: received a non-JSON frame from the daemon: ${line.slice(0, 200)}`));
        return;
      }
      this.dispatch(frame);
    }
  }

  private dispatch(frame: unknown): void {
    if (typeof frame !== "object" || frame === null) return;
    const f = frame as Record<string, unknown>;
    // Response: has `id` and (`result` or `error`) and NOT `event`.
    if (!("event" in f) && "id" in f && ("result" in f || "error" in f)) {
      const key = String(f.id);
      const waiter = this.pending.get(key);
      if (!waiter) return;
      this.pending.delete(key);
      if (waiter.timer) clearTimeout(waiter.timer);
      if ("error" in f && f.error !== undefined) {
        waiter.reject(new RpcError(waiter.method, f.error as WireError));
      } else {
        waiter.resolve(f.result);
      }
      return;
    }
    // Event frame.
    if ("event" in f && typeof f.event === "string") {
      const ev = f as WireEvent;
      for (const l of this.eventListeners) l(ev);
    }
  }

  private fail(err: Error): void {
    if (this.closed) return;
    this.closed = true;
    this.failure = err;
    for (const p of this.pending.values()) {
      if (p.timer) clearTimeout(p.timer);
      p.reject(err);
    }
    this.pending.clear();
  }

  /**
   * Issue an RPC and await its correlated `result`. A wire `error` rejects with {@link RpcError}; a
   * closed/failed connection rejects with the failure cause.
   */
  call<R = unknown>(method: string, params?: unknown, timeoutMs?: number): Promise<R> {
    if (this.closed || !this.sock) {
      return Promise.reject(this.failure ?? new DaemonUnreachable(this.socketPath));
    }
    const id = `cli-${this.nextId++}`;
    const frame = params === undefined ? { id, method } : { id, method, params };
    const t = timeoutMs ?? this.requestTimeoutMs;
    return new Promise<R>((resolve, reject) => {
      const timer =
        t > 0
          ? setTimeout(() => {
              this.pending.delete(id);
              reject(new Error(`tally: request timed out after ${t}ms (${method})`));
            }, t)
          : null;
      this.pending.set(id, {
        resolve: (r) => resolve(r as R),
        reject,
        method,
        timer,
      });
      this.sock!.write(JSON.stringify(frame) + "\n");
    });
  }

  /** Register an event listener. Returns an unsubscribe fn. Used by `session watch` + the `--wait` barrier. */
  onEvent(listener: (e: WireEvent) => void): () => void {
    this.eventListeners.add(listener);
    return () => this.eventListeners.delete(listener);
  }

  /** Send a raw request frame WITHOUT awaiting a response (used to fire `session.ack` best-effort). */
  send(method: string, params?: unknown): void {
    if (this.closed || !this.sock) return;
    const id = `cli-${this.nextId++}`;
    const frame = params === undefined ? { id, method } : { id, method, params };
    this.sock.write(JSON.stringify(frame) + "\n");
  }

  /** Close the connection cleanly. */
  close(): void {
    if (this.closed) {
      this.sock?.destroy();
      return;
    }
    this.closed = true;
    this.sock?.end();
    this.sock?.destroy();
  }
}

/** Connect a fresh client, throwing {@link DaemonUnreachable} if the daemon is not up. */
export async function connectClient(opts: TallyClientOptions = {}): Promise<TallyClient> {
  const c = new TallyClient(opts);
  await c.connect();
  return c;
}
