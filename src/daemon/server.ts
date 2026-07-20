// tally daemon-core — the Bun unix-socket server (CLI-SURFACE §2.1, byte-for-byte).
//
// One Unix-domain stream socket at `$XDG_RUNTIME_DIR/tally/tally.sock`, mode 0600, local-only. NDJSON
// framing: one UTF-8 JSON object per line, request/response/event interleaved on one connection. This
// module owns the TRANSPORT: it decodes frames, validates requests, routes the connection-bound
// subscription trio itself (they mutate per-connection state), delegates every other method to the
// `RpcRouter`, fans stamped events out to subscribers, and drives the idle heartbeat.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { chmodSync, mkdirSync, rmSync } from "node:fs";
import { dirname } from "node:path";
import type { Socket } from "bun";
import { SOCKET_MODE } from "../contracts/constants";
import { socketPath, type PathEnv } from "../contracts/paths";
import { TallyError, ValidationError } from "../contracts/errors";
import type { WireError } from "../contracts/errors";
import {
  validateRequestFrame,
  validateSubscribeParams,
  validateAckParams,
  validateUnsubscribeParams,
  makeSubscribeAck,
} from "../contracts/wire";
import type { HeartbeatPayload } from "../contracts/events";
import { LineDecoder, encodeFrame } from "./framing";
import { DaemonState } from "./state";
import { RpcRouter, isConnectionBound, negotiateProtocol } from "./rpc";
import { resolveFilter, type FrameSink, type Subscription } from "./subscriptions";
import { Heartbeat } from "./heartbeat";

/**
 * Sentinel a connection-bound handler returns to signal it has ALREADY written its own response frame
 * (so `onFrame` must not write again). `session.subscribe` uses it: it writes the ACK synchronously so
 * the ACK precedes the replay frames on the wire (§2.4).
 */
const ALREADY_RESPONDED = Symbol("tally.alreadyResponded");

/** Per-connection state carried in `socket.data`. */
interface ConnData {
  decoder: LineDecoder;
  /** The connection's active subscription (at most one; the frozen surface is one per connection). */
  subscription: Subscription | null;
  /** Set once the socket is closed, so late writes are dropped. */
  closed: boolean;
}

/**
 * The daemon-core socket server. Constructed with a `DaemonState` and an `RpcRouter`; `listen` binds
 * the socket, `stop` tears it down. The composition root (`index.ts`) wires the mounted modules'
 * handlers into the router before `listen`.
 */
export class DaemonServer {
  private listener: ReturnType<typeof Bun.listen<ConnData>> | null = null;
  private readonly heartbeat: Heartbeat;
  private readonly path: string;

  constructor(
    private readonly state: DaemonState,
    private readonly router: RpcRouter,
    env: PathEnv,
  ) {
    this.path = socketPath(env);
    this.heartbeat = new Heartbeat(
      state.clock,
      {
        latestSeq: () => this.state.ring.latestSeq,
        nowIso: () => this.state.clock.nowIso(),
        emitHeartbeat: (payload: HeartbeatPayload) => this.state.emitControl("heartbeat", payload),
      },
      state.config.heartbeatMs,
    );
  }

  /** The bound socket path. */
  get socketPath(): string {
    return this.path;
  }

  /** Bind the socket (fresh-binding over any stale file), mode 0600, and arm the heartbeat. */
  async listen(): Promise<void> {
    mkdirSync(dirname(this.path), { recursive: true });
    // A stale socket file blocks listen(); best-effort unlink.
    try {
      rmSync(this.path, { force: true });
    } catch {
      // ignore
    }
    const self = this;
    this.listener = Bun.listen<ConnData>({
      unix: this.path,
      socket: {
        open(socket) {
          socket.data = { decoder: new LineDecoder(), subscription: null, closed: false };
        },
        data(socket, chunk) {
          self.onData(socket, chunk);
        },
        close(socket) {
          self.onClose(socket);
        },
        error(socket, error) {
          process.stderr.write(`tally[server]: socket error: ${error instanceof Error ? error.message : String(error)}\n`);
          self.onClose(socket);
        },
      },
    });
    // Mode 0600 — single operator, local-only (§2.1).
    try {
      chmodSync(this.path, SOCKET_MODE);
    } catch {
      // ignore — some filesystems reject chmod on a socket; the dir is already user-scoped.
    }
    this.heartbeat.start();
  }

  /** Stop the heartbeat, stop accepting, and remove the socket file. */
  stop(): void {
    this.heartbeat.stop();
    this.listener?.stop(true);
    this.listener = null;
    try {
      rmSync(this.path, { force: true });
    } catch {
      // ignore
    }
  }

  // -------------------------------------------------------------------------------------------
  // Per-connection I/O.
  // -------------------------------------------------------------------------------------------

  private onData(socket: Socket<ConnData>, chunk: Buffer): void {
    const data = socket.data;
    // Feed raw BYTES to the decoder (no per-chunk UTF-8 decode) so a multibyte codepoint split across
    // socket chunks is never corrupted. Iterate the generator INCREMENTALLY: each frame yielded before
    // a later malformed line in the same chunk is dispatched before the decode throw is observed, so a
    // pipelined valid request is never silently dropped because a subsequent frame was bad.
    const gen = data.decoder.push(chunk);
    for (;;) {
      let step: IteratorResult<{ raw: string; value: unknown }, void>;
      try {
        step = gen.next();
      } catch (err) {
        // Framing failure (bad JSON, or a frame over FRAME_CAP): report and — for an oversized frame —
        // close, since the stream is now unsynchronized. Frames yielded before this point were already
        // dispatched in prior loop iterations.
        const wire = toWireError(err);
        this.write(socket, { id: null, error: wire });
        if (wire.code === "frame_too_large") {
          socket.end();
        }
        return;
      }
      if (step.done) return;
      void this.onFrame(socket, step.value.value);
    }
  }

  private async onFrame(socket: Socket<ConnData>, value: unknown): Promise<void> {
    let id: string | number | null = null;
    try {
      const req = validateRequestFrame(value);
      id = req.id;
      const result = await this.handleRequest(socket, req.method, req.params, id);
      // `session.subscribe` writes its OWN ACK synchronously (so the ACK precedes the replay frames on
      // the wire, §2.4 "First response is an ACK") and signals that by returning ALREADY_RESPONDED;
      // every other handler returns its result and we serialize the response here.
      if (result === ALREADY_RESPONDED) return;
      this.write(socket, { id, result });
    } catch (err) {
      this.write(socket, { id, error: toWireError(err) });
    }
  }

  /**
   * Handle one validated request. The connection-bound subscription trio is served here (it mutates
   * `socket.data.subscription`); everything else goes to the router.
   */
  private async handleRequest(socket: Socket<ConnData>, method: string, params: unknown, id: string | number | null): Promise<unknown> {
    if (isConnectionBound(method)) {
      switch (method) {
        case "session.subscribe":
          return this.handleSubscribe(socket, params, id);
        case "session.ack":
          return this.handleAck(params);
        case "session.unsubscribe":
          return this.handleUnsubscribe(socket, params);
      }
    }
    return this.router.dispatch(method, params);
  }

  /**
   * `session.subscribe` — negotiate protocol, compute the resume window, register the subscription,
   * return the ACK (with the FROZEN `type:"subscription"` discriminator), then replay retained events
   * strictly after the resume point. One subscription per connection: a re-subscribe replaces it.
   */
  private handleSubscribe(socket: Socket<ConnData>, rawParams: unknown, id: string | number | null): unknown {
    const params = validateSubscribeParams(rawParams);
    negotiateProtocol(params.min_protocol, params.max_protocol); // throws UnsupportedProtocol on miss

    // Replace any prior subscription on this connection.
    if (socket.data.subscription) {
      this.state.subscriptions.remove(socket.data.subscription.id);
      socket.data.subscription = null;
    }

    const resume = this.state.ring.resume(params.from_seq);
    const sink: FrameSink = {
      write: (line: string) => this.rawWrite(socket, line),
      close: () => socket.end(),
    };
    const filterArgs: Parameters<typeof resolveFilter>[0] = {
      include_heartbeat: params.include_heartbeat ?? true,
    };
    if (params.names) filterArgs.names = params.names;
    if (params.categories) filterArgs.categories = params.categories;
    const sub = this.state.subscriptions.create({
      filter: resolveFilter(filterArgs),
      sink,
      encode: (frame) => encodeFrame(frame),
    });
    socket.data.subscription = sub;

    const ack = makeSubscribeAck({
      subscription_id: sub.id,
      epoch: this.state.epoch,
      resume: {
        after_seq: resume.after_seq,
        oldest_seq: resume.oldest_seq,
        latest_seq: resume.latest_seq,
        next_seq: resume.next_seq,
        gap: resume.gap,
      },
    });

    // Write the ACK SYNCHRONOUSLY here (before returning), then deliver the replay SYNCHRONOUSLY right
    // after it. This guarantees ACK → replay → live ordering within one synchronous block (§2.4: "First
    // response is an ACK"). If the ACK were left for `onFrame`'s post-await continuation, that write
    // would be enqueued as a microtask AFTER a queued replay microtask, putting replay frames on the
    // wire before the ACK — the exact defect this replaces. We return ALREADY_RESPONDED so `onFrame`
    // does not write the ACK a second time.
    this.write(socket, { id, result: ack });
    if (resume.replay.length > 0) {
      for (const ev of resume.replay) {
        const outcome = sub.deliver(ev);
        if (outcome === "overflowed") {
          sub.overflow(this.state.ring.oldestSeq, this.state.ring.latestSeq);
          this.state.subscriptions.remove(sub.id);
          socket.data.subscription = null;
          break;
        }
      }
    }
    return ALREADY_RESPONDED;
  }

  /** `session.ack {subscription_id, seq}` — advance the subscriber cursor. */
  private handleAck(rawParams: unknown): unknown {
    const params = validateAckParams(rawParams);
    const sub = this.state.subscriptions.get(params.subscription_id);
    if (!sub) {
      throw new TallyError("unknown_subscription", `no such subscription ${params.subscription_id}`, {
        subscription_id: params.subscription_id,
      });
    }
    sub.ack(params.seq);
    return { acked: params.seq };
  }

  /** `session.unsubscribe {subscription_id}` — close the push stream; the socket stays open for RPCs. */
  private handleUnsubscribe(socket: Socket<ConnData>, rawParams: unknown): unknown {
    const params = validateUnsubscribeParams(rawParams);
    const removed = this.state.subscriptions.remove(params.subscription_id);
    if (socket.data.subscription && socket.data.subscription.id === params.subscription_id) {
      socket.data.subscription = null;
    }
    if (!removed) {
      throw new TallyError("unknown_subscription", `no such subscription ${params.subscription_id}`, {
        subscription_id: params.subscription_id,
      });
    }
    return { unsubscribed: params.subscription_id };
  }

  private onClose(socket: Socket<ConnData>): void {
    if (socket.data) {
      socket.data.closed = true;
      if (socket.data.subscription) {
        this.state.subscriptions.remove(socket.data.subscription.id);
        socket.data.subscription = null;
      }
    }
  }

  // -------------------------------------------------------------------------------------------
  // Writers.
  // -------------------------------------------------------------------------------------------

  /** Encode + write a response/event frame; swallows write-after-close. */
  private write(socket: Socket<ConnData>, frame: unknown): void {
    try {
      this.rawWrite(socket, encodeFrame(frame));
    } catch (err) {
      // A frame we ourselves produced overran the cap — a daemon bug; surface a minimal error instead.
      if (err instanceof TallyError && err.code === "frame_too_large") {
        this.rawWrite(
          socket,
          JSON.stringify({ id: null, error: { code: "internal", message: "response frame exceeded cap" } }) + "\n",
        );
      } else {
        throw err;
      }
    }
  }

  /** Write a pre-encoded line to the socket. Returns false if the connection is closed. */
  private rawWrite(socket: Socket<ConnData>, line: string): boolean {
    if (socket.data?.closed) return false;
    try {
      socket.write(line);
      return true;
    } catch {
      return false;
    }
  }
}

/** Project any thrown value onto a wire `error` object. */
function toWireError(err: unknown): WireError {
  if (err instanceof TallyError) return err.toWire();
  if (err instanceof ValidationError) return err.toWire();
  if (err instanceof Error) return { code: "internal", message: err.message };
  return { code: "internal", message: String(err) };
}

/** Convenience: build a `DaemonServer` from state + a fresh router. */
export function makeServer(state: DaemonState, env: PathEnv): { server: DaemonServer; router: RpcRouter } {
  const router = new RpcRouter(state);
  const server = new DaemonServer(state, router, env);
  return { server, router };
}
