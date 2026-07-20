// tally — the FROZEN Seam-B wire contract (CLI-SURFACE §2, byte-for-byte; IMPLEMENTATION-PLAN §3
// Wire + RPC method inventory). Frame kinds, the RPC method inventory (public five + internal
// additive carriers), the SubscribeAck with its `type:"subscription"` discriminator, the
// session.wait predicate over the FULL four-value AgentStatus, and hand-rolled runtime validators
// (no zod) the daemon uses on ingress.

import type { AgentStatus } from "./agent";
import { AGENT_STATUSES } from "./agent";
import type { EventName } from "./events";
import type { EventCategory } from "./events";
import type { EnqueueParams } from "./job";
import type { WireError } from "./errors";
import { ValidationError } from "./errors";
import { PROTOCOL_VERSION } from "./constants";

// ---------------------------------------------------------------------------------------------
// Frame kinds — NDJSON, one UTF-8 JSON object per line (CLI-SURFACE §2.1).
// ---------------------------------------------------------------------------------------------

/** A request frame `{id, method, params}` (CLI-SURFACE §2.1). */
export interface RequestFrame<P = unknown> {
  id: string | number;
  method: string;
  params?: P;
}

/** A successful response frame `{id, result}` (correlate by `id`). */
export interface ResponseOk<R = unknown> {
  id: string | number;
  result: R;
}

/** An error response frame `{id, error}`. */
export interface ResponseErr {
  id: string | number;
  error: WireError;
}

export type ResponseFrame<R = unknown> = ResponseOk<R> | ResponseErr;

/**
 * An event frame (CLI-SURFACE §2.1, §2.3). `seq` is the monotonic sequence (per `lease_epoch`);
 * `id` is the STABLE event uuid carried alongside `seq` on every replayable event
 * (IMPLEMENTATION-PLAN §3 — a first-class field, not prose). Non-replayable control frames
 * (`heartbeat`, `stream.overflow`) omit `seq`/`id`.
 */
export interface EventFrame<N extends EventName = EventName, P = unknown> {
  seq?: number;
  /** The stable event uuid for idempotent dedupe across a resume overlap (§2.1). */
  id?: string;
  event: N;
  /** The payload fields are spread flat onto the frame alongside `seq`/`id`/`event` (§2.3). */
  payload: P;
}

/** The three structurally-disambiguated frame kinds on one connection. */
export type Frame = RequestFrame | ResponseFrame | EventFrame;

/** Structural frame-kind discriminators (CLI-SURFACE §2.1 "disambiguated structurally"). */
export function isRequestFrame(f: unknown): f is RequestFrame {
  return isObj(f) && "method" in f && "id" in f;
}
export function isResponseFrame(f: unknown): f is ResponseFrame {
  return isObj(f) && "id" in f && ("result" in f || "error" in f) && !("method" in f);
}
export function isEventFrame(f: unknown): f is EventFrame {
  return isObj(f) && "event" in f && !("method" in f) && !("id" in f && ("result" in f || "error" in f));
}

// ---------------------------------------------------------------------------------------------
// RPC method inventory (IMPLEMENTATION-PLAN §3 — golden-tested by name).
// ---------------------------------------------------------------------------------------------

/** The FROZEN public five (CLI-SURFACE §2.4). */
export const PUBLIC_RPC_METHODS = [
  "session.snapshot",
  "session.subscribe",
  "session.wait",
  "session.ack",
  "session.unsubscribe",
] as const;

/** The internal-additive carriers (adding one is NEVER a protocol bump, §2.5). */
export const INTERNAL_RPC_METHODS = [
  // queue.*
  "queue.enqueue",
  "queue.cancel",
  "queue.pause",
  "queue.resume",
  "queue.drain",
  // The Seam-A `--wait` barrier served over the engine's BarrierTracker (drains already-terminal
  // deltas AND filters by task_uuid / barrier group — the CLI-side stream count could do neither).
  // Internal-additive (§2.5): adding a carrier is never a protocol bump.
  "queue.await_job",
  "queue.await_barrier",
  // pane.*
  "pane.send",
  "pane.send_key",
  "pane.focus",
  "pane.capture",
  // agent.*
  "agent.list",
  "agent.get",
  "agent.read",
  "agent.explain",
  // query.*
  "query.status",
  "query.render",
  // session.* (additive)
  "session.list",
  "session.register_viewer",
  // sensor edges
  "kitty.watcher_event",
  "agent.hook_event",
] as const;

/** Every method name the daemon serves — the golden-tested inventory. */
export const RPC_METHODS = [...PUBLIC_RPC_METHODS, ...INTERNAL_RPC_METHODS] as const;

export type PublicRpcMethod = (typeof PUBLIC_RPC_METHODS)[number];
export type InternalRpcMethod = (typeof INTERNAL_RPC_METHODS)[number];
export type RpcMethod = (typeof RPC_METHODS)[number];

const RPC_METHOD_SET = new Set<string>(RPC_METHODS);

/** Whether a method name is a known RPC method (daemon rejects unknowns with `unknown_method`). */
export function isRpcMethod(name: string): name is RpcMethod {
  return RPC_METHOD_SET.has(name);
}

// ---------------------------------------------------------------------------------------------
// session.subscribe params + the SubscribeAck (FROZEN discriminator).
// ---------------------------------------------------------------------------------------------

/** `session.subscribe` params (CLI-SURFACE §2.4). Omitting `from_seq` subscribes live from `next_seq`. */
export interface SubscribeParams {
  from_seq?: number;
  names?: EventName[];
  categories?: EventCategory[];
  include_heartbeat?: boolean;
  min_protocol?: number;
  max_protocol?: number;
}

/** The `resume` block in the subscribe ACK (CLI-SURFACE §2.1, §2.4). */
export interface ResumeInfo {
  after_seq: number;
  oldest_seq: number;
  latest_seq: number;
  next_seq: number;
  /** `true` when the requested `from_seq` was older than `oldest_seq` ⇒ client MUST re-snapshot. */
  gap: boolean;
}

/**
 * The subscribe ACK — the FIRST response to `session.subscribe` (CLI-SURFACE §2.4, FROZEN). It
 * carries the literal discriminator `type:"subscription"` (do NOT drop it) so a client can tell the
 * ACK apart from an ordinary result on the shared connection.
 */
export interface SubscribeAck {
  type: "subscription";
  subscription_id: string;
  protocol_version: number;
  epoch: number;
  resume: ResumeInfo;
}

/** The literal discriminator value the ACK MUST carry. Pinned by the golden test. */
export const SUBSCRIPTION_DISCRIMINATOR = "subscription" as const;

/** Build a well-formed SubscribeAck (used by daemon-core; keeps the discriminator un-droppable). */
export function makeSubscribeAck(args: {
  subscription_id: string;
  epoch: number;
  resume: ResumeInfo;
  protocol_version?: number;
}): SubscribeAck {
  return {
    type: SUBSCRIPTION_DISCRIMINATOR,
    subscription_id: args.subscription_id,
    protocol_version: args.protocol_version ?? PROTOCOL_VERSION,
    epoch: args.epoch,
    resume: args.resume,
  };
}

// ---------------------------------------------------------------------------------------------
// session.wait predicates (CLI-SURFACE §2.4) — the Seam-A --wait barrier.
// ---------------------------------------------------------------------------------------------

/** Terminal job outcomes a job-predicate awaits (`completed`/`failed`). */
export type JobUntil = "completed" | "failed";

/** A `job` wait predicate: barrier = enqueue-N-await-N (CLI-SURFACE §2.4). */
export interface JobPredicate {
  subject: "job";
  job_ids: string[];
  until: JobUntil[];
  count: number;
}

/**
 * An `agent` wait predicate (CLI-SURFACE §2.4). `until_status` accepts the FULL four-value
 * `AgentStatus` (IMPLEMENTATION-PLAN §3 — an additive widening REQUIRED so the frozen §1.4
 * `agent wait --status <done|blocked|idle|working>` is fully servable; a narrowing to the §2.4
 * two-value literal is a build failure).
 */
export interface AgentPredicate {
  subject: "agent";
  agent_ids: string[];
  until_status: AgentStatus;
  count: number;
}

/** A `pane_output` wait predicate — agent panes only; `is_viewer` rejected (CLI-SURFACE §2.4). */
export interface PaneOutputPredicate {
  subject: "pane_output";
  pane_id: string;
  regex: string;
}

/** The three `session.wait` predicate subjects. */
export type WaitPredicate = JobPredicate | AgentPredicate | PaneOutputPredicate;

/** `session.wait` params (CLI-SURFACE §2.4). One-shot blocking. */
export interface WaitParams {
  predicate: WaitPredicate;
  timeout_ms?: number;
}

/** `session.wait` result — the satisfying event(s), or a timeout report (CLI-SURFACE §2.4). */
export interface WaitResult {
  satisfied: unknown[];
  timed_out?: boolean;
  pending?: unknown[];
}

/** `session.ack` params (CLI-SURFACE §2.4). */
export interface AckParams {
  subscription_id: string;
  seq: number;
}

/** `session.unsubscribe` params (CLI-SURFACE §2.4). */
export interface UnsubscribeParams {
  subscription_id: string;
}

/** `session.register_viewer` params (IMPLEMENTATION-PLAN §3 — internal-additive). */
export interface RegisterViewerParams {
  kitty_window_id: number;
}

// ---------------------------------------------------------------------------------------------
// Hand-rolled runtime validators (no zod) — the daemon uses these on ingress.
// ---------------------------------------------------------------------------------------------

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function str(v: unknown, path: string): string {
  if (typeof v !== "string") throw new ValidationError(`${path} must be a string`, path);
  return v;
}
function num(v: unknown, path: string): number {
  if (typeof v !== "number" || !Number.isFinite(v)) throw new ValidationError(`${path} must be a finite number`, path);
  return v;
}
function optNum(v: unknown, path: string): number | undefined {
  return v === undefined ? undefined : num(v, path);
}
function optBool(v: unknown, path: string): boolean | undefined {
  if (v === undefined) return undefined;
  if (typeof v !== "boolean") throw new ValidationError(`${path} must be a boolean`, path);
  return v;
}
function strArray(v: unknown, path: string): string[] {
  if (!Array.isArray(v)) throw new ValidationError(`${path} must be an array`, path);
  return v.map((x, i) => str(x, `${path}[${i}]`));
}
function oneOf<T extends string>(v: unknown, allowed: readonly T[], path: string): T {
  const s = str(v, path);
  if (!(allowed as readonly string[]).includes(s)) {
    throw new ValidationError(`${path} must be one of ${allowed.join("|")}`, path);
  }
  return s as T;
}

/** Validate a decoded line as a Request frame (the daemon's ingress guard, CLI-SURFACE §2.1). */
export function validateRequestFrame(v: unknown): RequestFrame {
  if (!isObj(v)) throw new ValidationError("frame must be a JSON object", "$");
  if (!("id" in v)) throw new ValidationError("request frame requires id", "id");
  if (typeof v.id !== "string" && typeof v.id !== "number") {
    throw new ValidationError("id must be a string or number", "id");
  }
  const method = str(v.method, "method");
  return { id: v.id, method, params: v.params };
}

/** Validate `session.subscribe` params. */
export function validateSubscribeParams(v: unknown): SubscribeParams {
  if (v === undefined) return {};
  if (!isObj(v)) throw new ValidationError("subscribe params must be an object", "params");
  const out: SubscribeParams = {};
  if (v.from_seq !== undefined) out.from_seq = num(v.from_seq, "from_seq");
  if (v.names !== undefined) out.names = strArray(v.names, "names") as EventName[];
  if (v.categories !== undefined) {
    out.categories = strArray(v.categories, "categories").map((c, i) =>
      oneOf(c, ["agent", "pane", "session", "workspace", "job", "control"] as const, `categories[${i}]`),
    ) as EventCategory[];
  }
  const ih = optBool(v.include_heartbeat, "include_heartbeat");
  if (ih !== undefined) out.include_heartbeat = ih;
  const mn = optNum(v.min_protocol, "min_protocol");
  if (mn !== undefined) out.min_protocol = mn;
  const mx = optNum(v.max_protocol, "max_protocol");
  if (mx !== undefined) out.max_protocol = mx;
  return out;
}

/** Validate a `session.wait` predicate over the FULL four-value AgentStatus. */
export function validateWaitParams(v: unknown): WaitParams {
  if (!isObj(v)) throw new ValidationError("wait params must be an object", "params");
  if (!isObj(v.predicate)) throw new ValidationError("wait.predicate must be an object", "predicate");
  const p = v.predicate;
  const subject = oneOf(p.subject, ["job", "agent", "pane_output"] as const, "predicate.subject");
  let predicate: WaitPredicate;
  if (subject === "job") {
    predicate = {
      subject: "job",
      job_ids: strArray(p.job_ids, "predicate.job_ids"),
      until: strArray(p.until, "predicate.until").map((u, i) =>
        oneOf(u, ["completed", "failed"] as const, `predicate.until[${i}]`),
      ),
      count: num(p.count, "predicate.count"),
    };
  } else if (subject === "agent") {
    predicate = {
      subject: "agent",
      agent_ids: strArray(p.agent_ids, "predicate.agent_ids"),
      // FULL four-value acceptance — required for `agent wait --status idle|working` (§1.4).
      until_status: oneOf(p.until_status, AGENT_STATUSES, "predicate.until_status"),
      count: num(p.count, "predicate.count"),
    };
  } else {
    predicate = {
      subject: "pane_output",
      pane_id: str(p.pane_id, "predicate.pane_id"),
      regex: str(p.regex, "predicate.regex"),
    };
  }
  const out: WaitParams = { predicate };
  const t = optNum(v.timeout_ms, "timeout_ms");
  if (t !== undefined) out.timeout_ms = t;
  return out;
}

/** Validate `session.ack` params. */
export function validateAckParams(v: unknown): AckParams {
  if (!isObj(v)) throw new ValidationError("ack params must be an object", "params");
  return { subscription_id: str(v.subscription_id, "subscription_id"), seq: num(v.seq, "seq") };
}

/** Validate `session.unsubscribe` params. */
export function validateUnsubscribeParams(v: unknown): UnsubscribeParams {
  if (!isObj(v)) throw new ValidationError("unsubscribe params must be an object", "params");
  return { subscription_id: str(v.subscription_id, "subscription_id") };
}

/** Validate `session.register_viewer` params. */
export function validateRegisterViewerParams(v: unknown): RegisterViewerParams {
  if (!isObj(v)) throw new ValidationError("register_viewer params must be an object", "params");
  return { kitty_window_id: num(v.kitty_window_id, "kitty_window_id") };
}

/**
 * Validate Seam-A enqueue params (CLI-SURFACE §1.1a). Enforces the `invocation` XOR `argv` and
 * `cwd` XOR `worktree` disjunctions and the enum sets. The daemon uses this on `queue.enqueue`.
 */
export function validateEnqueueParams(v: unknown): EnqueueParams {
  if (!isObj(v)) throw new ValidationError("enqueue params must be an object", "params");
  const priority = oneOf(v.priority, ["high", "medium", "low"] as const, "priority");
  const source = oneOf(v.source, ["r2", "gh", "calendar", "manual", "orchestrator"] as const, "source");
  const kind = oneOf(v.kind, ["pi", "claude-code", "shell"] as const, "kind");

  const hasInvocation = v.invocation !== undefined;
  const hasArgv = v.argv !== undefined;
  if (hasInvocation === hasArgv) {
    throw new ValidationError("exactly one of invocation / argv is required", "invocation");
  }
  const hasCwd = v.cwd !== undefined;
  const hasWorktree = v.worktree !== undefined;
  if (hasCwd && hasWorktree) {
    throw new ValidationError("at most one of cwd / worktree may be set", "cwd");
  }

  const out: EnqueueParams = { priority, source, kind };
  if (hasInvocation) out.invocation = str(v.invocation, "invocation");
  if (hasArgv) out.argv = strArray(v.argv, "argv");
  if (hasCwd) out.cwd = str(v.cwd, "cwd");
  if (hasWorktree) out.worktree = str(v.worktree, "worktree");
  if (v.evidence !== undefined) out.evidence = parseEvidenceSpecs(v.evidence);
  if (v.pool !== undefined) out.pool = str(v.pool, "pool");
  if (v.model_class !== undefined) out.model_class = str(v.model_class, "model_class");
  if (v.dedup_key !== undefined) out.dedup_key = str(v.dedup_key, "dedup_key");
  if (v.session !== undefined) out.session = str(v.session, "session");
  if (v.barrier !== undefined) out.barrier = str(v.barrier, "barrier");
  if (v.wait_group !== undefined) out.wait_group = str(v.wait_group, "wait_group");
  if (v.wait_count !== undefined) out.wait_count = num(v.wait_count, "wait_count");
  const w = optBool(v.wait, "wait");
  if (w !== undefined) out.wait = w;
  if (v.timeout !== undefined) out.timeout = str(v.timeout, "timeout");
  const d = optBool(v.detach, "detach");
  if (d !== undefined) out.detach = d;
  return out;
}

// ---------------------------------------------------------------------------------------------
// The EvidenceCheck grammar (CLI-SURFACE §1.1a `--evidence <check>` repeatable).
// ---------------------------------------------------------------------------------------------

import type { EvidenceCheck } from "./job";

/**
 * Parse one `--evidence` token: `artifact:<path>` | `hash:<algo>[:<value>]` | `exit:<code>`
 * (CLI-SURFACE §1.1a; the witness-span check is implicit and NOT expressible). Both the CLI and the
 * daemon parse through this one function so the grammar cannot drift.
 */
export function parseEvidenceSpec(spec: string): EvidenceCheck {
  const idx = spec.indexOf(":");
  if (idx <= 0) throw new ValidationError(`evidence spec must be "<kind>:<value>": ${spec}`, "evidence");
  const kind = spec.slice(0, idx);
  const rest = spec.slice(idx + 1);
  switch (kind) {
    case "artifact":
      if (rest === "") throw new ValidationError("artifact evidence requires a path", "evidence");
      return { kind: "artifact", path: rest };
    case "hash": {
      // `hash:<algo>` or `hash:<algo>:<value>`.
      const sep = rest.indexOf(":");
      if (sep === -1) {
        if (rest === "") throw new ValidationError("hash evidence requires an algo", "evidence");
        return { kind: "hash", algo: rest };
      }
      const algo = rest.slice(0, sep);
      const value = rest.slice(sep + 1);
      if (algo === "") throw new ValidationError("hash evidence requires an algo", "evidence");
      return { kind: "hash", algo, value };
    }
    case "exit": {
      const code = Number(rest);
      if (!Number.isInteger(code)) throw new ValidationError(`exit evidence requires an integer code: ${spec}`, "evidence");
      return { kind: "exit", code };
    }
    default:
      throw new ValidationError(`unknown evidence kind "${kind}" (expected artifact|hash|exit)`, "evidence");
  }
}

/** Parse an array of `--evidence` tokens (or an already-structured array) into EvidenceChecks. */
export function parseEvidenceSpecs(v: unknown): EvidenceCheck[] {
  if (!Array.isArray(v)) throw new ValidationError("evidence must be an array", "evidence");
  return v.map((item, i) => {
    if (typeof item === "string") return parseEvidenceSpec(item);
    // Accept an already-structured check (idempotent re-validation).
    if (isObj(item) && typeof item.kind === "string") return item as unknown as EvidenceCheck;
    throw new ValidationError(`evidence[${i}] must be a string spec or a check object`, `evidence[${i}]`);
  });
}

/** Render an EvidenceCheck back to its canonical `<kind>:<value>` string (for witness/journald). */
export function renderEvidenceSpec(check: EvidenceCheck): string {
  switch (check.kind) {
    case "artifact":
      return `artifact:${check.path}`;
    case "hash":
      return check.value !== undefined ? `hash:${check.algo}:${check.value}` : `hash:${check.algo}`;
    case "exit":
      return `exit:${check.code}`;
  }
}
