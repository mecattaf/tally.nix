// test/helpers/socket-client.ts
//
// An NDJSON Unix-socket test client speaking the frozen CLI-SURFACE §2 wire
// framing. Layer-1 daemon-core tests (and layer-4 e2e) drive the daemon over a
// real Unix stream socket with this client: it frames requests, correlates
// responses by `id`, buffers pushed events, and exposes await-helpers for the
// snapshot / subscribe / wait handshake.
//
// Framing (§2.1): one UTF-8 JSON object per line, LF-terminated, no raw embedded
// newlines. Three frame kinds on one connection, disambiguated structurally:
//   request  {id, method, params}
//   response {id, result | error}
//   event    {seq, event, ...}   (no id)
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { connect, type Socket } from "node:net";

export interface RequestFrame {
  id: string;
  method: string;
  params?: unknown;
}

export interface ResponseFrame {
  id: string;
  result?: unknown;
  error?: { code: string; [k: string]: unknown };
}

export interface EventFrame {
  seq?: number;
  id?: string;
  event: string;
  [k: string]: unknown;
}

type AnyFrame = ResponseFrame | EventFrame;

function isResponse(f: AnyFrame): f is ResponseFrame {
  return typeof (f as ResponseFrame).id === "string" && "id" in f && !("event" in f);
}

/**
 * A minimal, promise-based NDJSON client over a Unix stream socket. Requests are
 * correlated by a monotonically-increasing `id`; events (no `id`) are buffered
 * and delivered to `onEvent` listeners and an internal queue that `nextEvent`
 * and `waitForEvent` consume.
 */
export class SocketClient {
  private sock: Socket | null = null;
  private buf = "";
  private nextId = 1;
  private readonly pending = new Map<string, {
    resolve: (r: ResponseFrame) => void;
    reject: (e: Error) => void;
  }>();
  /** Every event frame received, in arrival order. */
  readonly events: EventFrame[] = [];
  private readonly eventWaiters: Array<(e: EventFrame) => void> = [];
  private readonly eventListeners: Array<(e: EventFrame) => void> = [];
  private closed = false;
  private closeErr: Error | null = null;

  constructor(private readonly path: string) {}

  /** Connect to the socket path. Resolves once the connection is established. */
  connect(timeoutMs = 2000): Promise<void> {
    return new Promise((resolve, reject) => {
      const t = setTimeout(() => reject(new Error(`connect timeout: ${this.path}`)), timeoutMs);
      const sock = connect(this.path);
      this.sock = sock;
      sock.setEncoding("utf8");
      sock.once("connect", () => {
        clearTimeout(t);
        resolve();
      });
      sock.on("data", (chunk: string) => this.onData(chunk));
      sock.on("error", (err: Error) => {
        clearTimeout(t);
        this.fail(err);
        reject(err);
      });
      sock.on("close", () => {
        this.fail(this.closeErr ?? new Error("socket closed"));
      });
    });
  }

  private onData(chunk: string): void {
    this.buf += chunk;
    let idx: number;
    while ((idx = this.buf.indexOf("\n")) !== -1) {
      const line = this.buf.slice(0, idx);
      this.buf = this.buf.slice(idx + 1);
      if (line.trim().length === 0) continue;
      let frame: AnyFrame;
      try {
        frame = JSON.parse(line) as AnyFrame;
      } catch {
        this.fail(new Error(`bad frame (not JSON): ${line}`));
        return;
      }
      this.dispatch(frame);
    }
  }

  private dispatch(frame: AnyFrame): void {
    if (isResponse(frame)) {
      const waiter = this.pending.get(frame.id);
      if (waiter) {
        this.pending.delete(frame.id);
        waiter.resolve(frame);
      }
      return;
    }
    // Event frame.
    const ev = frame as EventFrame;
    this.events.push(ev);
    for (const l of this.eventListeners) l(ev);
    const waiter = this.eventWaiters.shift();
    if (waiter) waiter(ev);
  }

  private fail(err: Error): void {
    if (this.closed) return;
    this.closed = true;
    this.closeErr = err;
    for (const { reject } of this.pending.values()) reject(err);
    this.pending.clear();
  }

  /** Send a request and await its correlated response frame. */
  request(method: string, params?: unknown, timeoutMs = 3000): Promise<ResponseFrame> {
    if (!this.sock || this.closed) {
      return Promise.reject(this.closeErr ?? new Error("not connected"));
    }
    const id = `c${this.nextId++}`;
    const frame: RequestFrame = params === undefined ? { id, method } : { id, method, params };
    return new Promise<ResponseFrame>((resolve, reject) => {
      const t = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`request timeout: ${method}`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (r) => {
          clearTimeout(t);
          resolve(r);
        },
        reject: (e) => {
          clearTimeout(t);
          reject(e);
        },
      });
      this.sock!.write(JSON.stringify(frame) + "\n");
    });
  }

  /**
   * Convenience: send a request and return `result`, throwing if the response
   * carried an `error`.
   */
  async call<T = unknown>(method: string, params?: unknown, timeoutMs = 3000): Promise<T> {
    const resp = await this.request(method, params, timeoutMs);
    if (resp.error) {
      throw new Error(`RPC ${method} error: ${JSON.stringify(resp.error)}`);
    }
    return resp.result as T;
  }

  /** Register an event listener. Returns an unsubscribe function. */
  onEvent(listener: (e: EventFrame) => void): () => void {
    this.eventListeners.push(listener);
    return () => {
      const i = this.eventListeners.indexOf(listener);
      if (i !== -1) this.eventListeners.splice(i, 1);
    };
  }

  /** Await the next event to arrive (after this call), with a timeout. */
  nextEvent(timeoutMs = 3000): Promise<EventFrame> {
    return new Promise((resolve, reject) => {
      const t = setTimeout(() => {
        const i = this.eventWaiters.indexOf(w);
        if (i !== -1) this.eventWaiters.splice(i, 1);
        reject(new Error("nextEvent timeout"));
      }, timeoutMs);
      const w = (e: EventFrame) => {
        clearTimeout(t);
        resolve(e);
      };
      this.eventWaiters.push(w);
    });
  }

  /**
   * Await the first event (already-received or future) whose `event` name and
   * optional predicate match. Scans the buffered `events` first so a match that
   * arrived before the call is not missed.
   */
  waitForEvent(
    name: string,
    predicate?: (e: EventFrame) => boolean,
    timeoutMs = 3000,
  ): Promise<EventFrame> {
    const match = (e: EventFrame) => e.event === name && (predicate ? predicate(e) : true);
    const existing = this.events.find(match);
    if (existing) return Promise.resolve(existing);
    return new Promise((resolve, reject) => {
      const t = setTimeout(() => {
        unsub();
        reject(new Error(`waitForEvent timeout: ${name}`));
      }, timeoutMs);
      const unsub = this.onEvent((e) => {
        if (match(e)) {
          clearTimeout(t);
          unsub();
          resolve(e);
        }
      });
    });
  }

  /** All buffered events with the given name. */
  eventsNamed(name: string): EventFrame[] {
    return this.events.filter((e) => e.event === name);
  }

  /** Close the connection. */
  close(): void {
    this.closed = true;
    this.sock?.end();
    this.sock?.destroy();
  }
}

/** Connect a fresh client (convenience for tests). */
export async function connectClient(path: string, timeoutMs = 2000): Promise<SocketClient> {
  const c = new SocketClient(path);
  await c.connect(timeoutMs);
  return c;
}
