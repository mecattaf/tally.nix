// tally — the in-daemon seams: the typed `Bus` (pub/sub), the `DaemonMount`/`RpcRegistrar` boot
// registration seam, and the `WaitScrapeProvider` seam (IMPLEMENTATION-PLAN §3 Seams, risk 9).
//
// These resolve the cross-layer joins WITHOUT a layer-2 module importing its siblings: the
// detector writes the `agents[]` leg into the single store via the Bus; jobs writes `jobs[]`;
// `daemon-core/wait.ts` reads `pane_output` via `WaitScrapeProvider`; `main.ts` mounts every
// in-daemon module's handlers/loops via `DaemonMount`.

import type { EventName, EventPayloadMap } from "./events";
import type { PaneRead } from "./events";

/**
 * A typed in-daemon event envelope. `seq`/`id` are stamped by daemon-core's replay ring when the
 * event goes to the wire — on the internal bus they are optional (an internal producer emits the
 * name + payload; daemon-core assigns wire identity).
 */
export interface BusEvent<N extends EventName = EventName> {
  event: N;
  payload: EventPayloadMap[N];
}

/** An unsubscribe handle returned by `Bus.on`. */
export type Unsubscribe = () => void;

/**
 * The typed in-daemon pub/sub bus (IMPLEMENTATION-PLAN §3 Seams). Internal producers (detector,
 * jobs, session-model, sensors) publish name+payload; daemon-core subscribes to fan events onto the
 * wire (stamping `seq`/`id`) and the single store subscribes to compose snapshot legs. A subscriber
 * MUST tolerate unknown event names (forward-compat).
 */
export interface Bus {
  /** Publish an event. Synchronous fan-out to current subscribers. */
  emit<N extends EventName>(event: N, payload: EventPayloadMap[N]): void;
  /** Subscribe to one event name. Returns an unsubscribe handle. */
  on<N extends EventName>(event: N, handler: (payload: EventPayloadMap[N]) => void): Unsubscribe;
  /** Subscribe to every event (daemon-core's wire fan-out uses this). */
  onAny(handler: (e: BusEvent) => void): Unsubscribe;
}

// ---------------------------------------------------------------------------------------------
// DaemonMount / RpcRegistrar — the boot-time registration seam (risk mount mechanism).
// ---------------------------------------------------------------------------------------------

/** An RPC handler: takes validated params, returns a result (or throws a `TallyError`). */
export type RpcHandler = (params: unknown) => Promise<unknown> | unknown;

/** A directory-watcher handler: called with each changed path under a watched directory. */
export type WatcherHandler = (path: string) => Promise<void> | void;

/**
 * A supervised loop registered on daemon-core's `supervise.ts` cadence/restart host
 * (IMPLEMENTATION-PLAN M1.1 supervise.ts). `start` runs the loop; `name` labels it for restart
 * isolation; `stop` (optional) tears it down on shutdown.
 */
export interface SupervisedLoop {
  name: string;
  start(): Promise<void> | void;
  stop?(): Promise<void> | void;
}

/**
 * The boot-time registration seam (IMPLEMENTATION-PLAN §3 Seams `DaemonMount`/`RpcRegistrar`). The
 * composition root (`main.ts`) calls each in-daemon module's `mount(daemon)` with an object
 * implementing this, so a mounted module never imports daemon-core internals nor is imported by a
 * sibling to get wired.
 */
export interface DaemonMount {
  /** Register an additive RPC carrier (`queue.drain`, `kitty.watcher_event`, `agent.hook_event`, …). */
  registerRpc(method: string, handler: RpcHandler): void;
  /** Register a directory watcher (triggers' `events/` sweep). */
  registerWatcher(path: string, handler: WatcherHandler): void;
  /** Register a supervised loop (detector, gh poller). */
  registerSupervised(loop: SupervisedLoop): void;
}

/** A module that mounts itself onto the daemon at boot. `main.ts` calls `mount(daemon)` for each. */
export interface DaemonModule {
  mount(daemon: DaemonMount): void;
}

// ---------------------------------------------------------------------------------------------
// WaitScrapeProvider — the seam daemon-core/wait.ts uses for a `session.wait pane_output` read.
// ---------------------------------------------------------------------------------------------

/** A request for an on-demand pane_output regex read (CLI-SURFACE §2.4 `pane_output`). */
export interface PaneOutputWaitRequest {
  pane_id: string;
  /** The regex the read must match (source pattern; the provider compiles it). */
  regex: string;
  /** Optional millisecond deadline the provider honors. */
  timeout_ms?: number;
}

/** The result of a satisfied pane_output wait — the matched line + the read that produced it. */
export interface PaneOutputWaitResult {
  pane_id: string;
  session_id: string;
  matched_line: string;
  read: PaneRead;
}

/**
 * The seam `daemon-core/wait.ts` calls to satisfy a `session.wait pane_output` predicate
 * (IMPLEMENTATION-PLAN §3 Seams `WaitScrapeProvider`, risk 9). The DETECTOR loop registers the
 * implementation and fulfills the read via throttled `kitty @ get-text`, REJECTING `is_viewer`
 * panes at the seam (anti-loop invariant #4). `wait.ts` depends only on this interface — it never
 * imports sensors/detector. The provider's match is emitted as `pane.output_matched` by the
 * detector; `wait.ts` consumes that same event as the RPC result.
 */
export interface WaitScrapeProvider {
  /**
   * Resolve when the pane's output matches `regex`, or reject with `ViewerRejected` if the pane is a
   * viewer, or with a `TallyError{code:"timeout"}` on deadline. The returned event is what
   * `session.wait` reports as its result.
   */
  awaitPaneOutput(req: PaneOutputWaitRequest): Promise<PaneOutputWaitResult>;
}

// ---------------------------------------------------------------------------------------------
// JobBarrierProvider — the seam daemon-core/wait.ts uses to satisfy a `session.wait {subject:job}`.
// ---------------------------------------------------------------------------------------------

/** One satisfied terminal-job delta the barrier surfaces (shape mirrors the wire `job.*` payloads). */
export interface JobBarrierDelta {
  job_id: string;
  task_uuid: string | null;
  state: "completed" | "failed" | "evidence_fail";
  verdict: string;
}

/** The result of a job-barrier await: the deltas that satisfied it, and whether it timed out. */
export interface JobBarrierResult {
  satisfied: JobBarrierDelta[];
  timed_out: boolean;
  /** Job ids still pending on a timeout (for the wait's `pending` field). */
  pending: string[];
}

/**
 * The seam `daemon-core/wait.ts` calls to satisfy a `session.wait {subject:job}` predicate over the
 * jobs engine's BarrierTracker (IMPLEMENTATION-PLAN §3; SPEC "enqueue-N-await-N"). The JOBS ENGINE
 * registers the implementation; `wait.ts` depends only on this interface (never imports the engine).
 * Crucially the tracker DRAINS ALREADY-RECORDED TERMINALS FIRST, so a `session.wait` issued after the
 * job(s) already finished resolves immediately instead of hanging on future-only bus events.
 */
export interface JobBarrierProvider {
  /** Await `count` of `jobIds` reaching a terminal outcome (completed/failed). */
  awaitJobIds(jobIds: readonly string[], count: number, timeoutMs?: number): Promise<JobBarrierResult>;
}
