// tally daemon-core — the RPC router (CLI-SURFACE §2.4, byte-for-byte).
//
// Dispatches request frames to handlers. The FROZEN public five are:
//   - `session.snapshot`  — connection-independent; served here from `DaemonState.assembleSnapshot`.
//   - `session.wait`      — connection-independent; served here via the wait engine.
//   - `session.subscribe` / `session.ack` / `session.unsubscribe` — CONNECTION-BOUND (they mutate the
//     connection's subscription), so they are handled in `server.ts` where the per-connection sink
//     lives. The router exposes `isConnectionBound` so the server routes them, and `negotiate` for the
//     `min_protocol`/`max_protocol` handshake.
//
// Internal-additive carriers (`queue.*`, `pane.*`, `agent.*`, `query.*`, `session.list`,
// `session.register_viewer`, `kitty.watcher_event`, `agent.hook_event`) are registered by the mounted
// modules via `DaemonMount.registerRpc`; the router holds that table and dispatches to it. An unknown
// method is `unknown_method`; a registered method whose module has not mounted is `unsupported`.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { RpcHandler } from "../contracts/bus";
import { PROTOCOL_VERSION } from "../contracts/constants";
import { TallyError, UnsupportedProtocol } from "../contracts/errors";
import { isRpcMethod, validateWaitParams } from "../contracts/wire";
import type { DaemonState } from "./state";
import { runWait, type WaitHost } from "./wait";

/** The public methods that mutate connection state and are handled by the server, not the router. */
export const CONNECTION_BOUND_METHODS = new Set<string>([
  "session.subscribe",
  "session.ack",
  "session.unsubscribe",
]);

/** The public methods the router itself serves (connection-independent). */
export const ROUTER_PUBLIC_METHODS = new Set<string>(["session.snapshot", "session.wait"]);

/** Whether a method must be routed to the server's connection-bound path. */
export function isConnectionBound(method: string): boolean {
  return CONNECTION_BOUND_METHODS.has(method);
}

/**
 * The RPC router. Owns the additive-handler table and dispatches the two connection-independent
 * public methods. The server owns the connection-bound trio and calls `dispatch` for everything else.
 */
export class RpcRouter {
  private readonly handlers = new Map<string, RpcHandler>();

  constructor(private readonly state: DaemonState) {}

  /**
   * Register an internal-additive RPC carrier (`DaemonMount.registerRpc`). Adding one is NEVER a
   * protocol bump (§2.5). A name that is not in the known inventory is still accepted (forward-compat)
   * but logged, so a typo is visible without breaking a green build.
   */
  register(method: string, handler: RpcHandler): void {
    if (!isRpcMethod(method) && !CONNECTION_BOUND_METHODS.has(method) && !ROUTER_PUBLIC_METHODS.has(method)) {
      process.stderr.write(`tally[rpc]: registering out-of-inventory method "${method}"\n`);
    }
    if (this.handlers.has(method)) {
      process.stderr.write(`tally[rpc]: replacing handler for "${method}"\n`);
    }
    this.handlers.set(method, handler);
  }

  /** Whether a method has a registered additive handler. */
  hasHandler(method: string): boolean {
    return this.handlers.has(method);
  }

  /**
   * Dispatch a NON-connection-bound method to its handler, returning the result. Throws a
   * `TallyError` the server serializes into the response `error`. `session.snapshot`/`session.wait`
   * are served here; every additive carrier is served from the registered table.
   */
  async dispatch(method: string, params: unknown): Promise<unknown> {
    if (method === "session.snapshot") {
      return this.state.assembleSnapshot();
    }
    if (method === "session.wait") {
      const host: WaitHost = {
        bus: this.state.bus,
        clock: this.state.clock,
        waitScrape: () => this.state.waitScrape,
        jobBarrier: () => this.state.jobBarrier,
      };
      return runWait(host, validateWaitParams(params));
    }
    const handler = this.handlers.get(method);
    if (handler) {
      return await handler(params);
    }
    // A known method name whose module has not mounted a handler ⇒ honest `unsupported`.
    if (isRpcMethod(method)) {
      throw new TallyError("unsupported", `method "${method}" is known but not currently served (module not mounted)`);
    }
    throw new TallyError("unknown_method", `unknown method "${method}"`);
  }
}

/**
 * Negotiate `min_protocol`/`max_protocol` against the daemon's supported version. Returns the served
 * `protocol_version` or throws `UnsupportedProtocol{supported:[…]}` (§2.5). v0 supports exactly
 * `[PROTOCOL_VERSION]`.
 */
export function negotiateProtocol(min?: number, max?: number): number {
  const supported = [PROTOCOL_VERSION];
  const lo = min ?? PROTOCOL_VERSION;
  const hi = max ?? PROTOCOL_VERSION;
  if (lo > hi) {
    throw new UnsupportedProtocol(supported);
  }
  if (PROTOCOL_VERSION < lo || PROTOCOL_VERSION > hi) {
    throw new UnsupportedProtocol(supported);
  }
  return PROTOCOL_VERSION;
}
