// tally — the typed `gh` CLI client for the gh intake read surface (IMPLEMENTATION-PLAN M2.4;
// octo.nvim surface scan §5–6). Direct `gh api` shell-out through the injectable `Exec` seam;
// auth is the ambient authenticated `gh` — tally NEVER manages credentials (DECISIONS Q8).
//
// This module owns ONLY the read half the intake polls (scan §6 "read/poll half only"): the
// notification inbox, the GraphQL search primitive, the cheap per-item `updatedAt` probe, and the
// `/rate_limit` headroom check. The mutation surface (scan §2) is future work and is deliberately
// absent. Every response is parsed + hand-validated (no zod — keep the compile lean); a malformed
// payload throws a `ValidationError` rather than propagating an untyped shape downstream.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4). The GraphQL query text is
// authored here against the documented GitHub GraphQL schema, not lifted from octo.nvim.

import type { Exec, ExecResult } from "../contracts/exec";
import { TallyError, ValidationError } from "../contracts/errors";

/** A single-request millisecond timeout for a `gh api` call — a poll must never hang the loop. */
export const GH_CALL_TIMEOUT_MS = 20_000;

/**
 * A notification-inbox reason (`GET /notifications` `reason` field; scan §5.1). The first three are
 * first-class signals; the rest are lower-priority or informational. The union stays open (a raw
 * string) because GitHub may add reasons — the classifier tolerates unknowns.
 */
export type NotificationReason =
  | "mention"
  | "review_requested"
  | "assign"
  | "author"
  | "subscribed"
  | "team_mention"
  | "comment"
  | "state_change"
  | "ci_activity"
  | (string & {});

/** The subject type of a notification (scan §5.1). */
export type SubjectType = "Issue" | "PullRequest" | "Discussion" | "Release" | (string & {});

/** A thread-level subscription state (scan §5.8 mute tiers). */
export interface SubscriptionState {
  subscribed: boolean;
  ignored: boolean;
  /** `"unsubscribed"` when the thread was explicitly unsubscribed; else null. */
  reason: string | null;
}

/** One notification-inbox thread, parsed from `GET /notifications` (scan §5.1). */
export interface Notification {
  /** The notification thread id (NOT the subject node id). */
  id: string;
  unread: boolean;
  reason: NotificationReason;
  updated_at: string;
  last_read_at: string | null;
  subject: {
    title: string;
    /** The REST API url of the subject (used to derive a hydration endpoint). */
    url: string | null;
    latest_comment_url: string | null;
    type: SubjectType;
  };
  repository: {
    full_name: string;
    name: string;
    owner: string;
  };
  subscription_url: string | null;
  /** Present when the API inlined the subscription tier (some responses do; else undefined). */
  subscription?: SubscriptionState;
}

/** A GraphQL search node — an Issue or PullRequest matched by a `search` qualifier (scan §1.11). */
export interface SearchNode {
  /** The GraphQL node id — the STABLE dedup key (scan §6 "dedup on node id"). */
  id: string;
  typename: "Issue" | "PullRequest" | (string & {});
  number: number;
  title: string;
  url: string;
  updatedAt: string;
  state: string;
  isDraft: boolean;
  reviewDecision: string | null;
  author: string | null;
  nameWithOwner: string;
  labels: string[];
  assignees: string[];
  reviewRequests: string[];
  statusCheckRollup: string | null;
}

/** One page of a GraphQL search result. */
export interface SearchPage {
  issueCount: number;
  hasNextPage: boolean;
  endCursor: string | null;
  nodes: SearchNode[];
}

/** Rate-limit headroom for one resource bucket (`GET /rate_limit`; scan §5.7). */
export interface RateLimitBucket {
  remaining: number;
  limit: number;
  /** Unix epoch seconds at which the window resets. */
  reset: number;
}

/** The rate-limit buckets tally cares about (core REST + graphql + search). */
export interface RateLimitSnapshot {
  core: RateLimitBucket;
  graphql: RateLimitBucket;
  search: RateLimitBucket;
}

// ---------------------------------------------------------------------------------------------
// Parsing / validation helpers (hand-rolled; no zod).
// ---------------------------------------------------------------------------------------------

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function asString(v: unknown, path: string): string {
  if (typeof v !== "string") throw new ValidationError(`gh: ${path} must be a string`, path);
  return v;
}

function asStringOrNull(v: unknown): string | null {
  return typeof v === "string" ? v : null;
}

function asBool(v: unknown): boolean {
  return v === true;
}

function asNumber(v: unknown, path: string): number {
  if (typeof v !== "number" || !Number.isFinite(v)) {
    throw new ValidationError(`gh: ${path} must be a finite number`, path);
  }
  return v;
}

/** Parse the JSON stdout of a `gh api` call, or throw a `ValidationError` naming the endpoint. */
function parseJson(res: ExecResult, endpoint: string): unknown {
  if (res.stdout.trim() === "") {
    throw new ValidationError(`gh api ${endpoint}: empty response`, endpoint);
  }
  try {
    return JSON.parse(res.stdout);
  } catch {
    throw new ValidationError(`gh api ${endpoint}: response was not valid JSON`, endpoint);
  }
}

/** Collect the `.nodes[].name` (labels) / `.nodes[].login` (users) list from a GraphQL connection. */
function connectionLogins(conn: unknown, key: "login" | "name", nested?: string): string[] {
  if (!isObject(conn) || !Array.isArray(conn.nodes)) return [];
  const out: string[] = [];
  for (const node of conn.nodes) {
    if (!isObject(node)) continue;
    let holder: unknown = node;
    if (nested) holder = node[nested];
    if (isObject(holder) && typeof holder[key] === "string") out.push(holder[key] as string);
  }
  return out;
}

// ---------------------------------------------------------------------------------------------
// The GraphQL search query (authored fresh against the documented schema).
// ---------------------------------------------------------------------------------------------

/**
 * The GraphQL search query text. Selects octo's core field set (scan §6): `reviewDecision`,
 * `statusCheckRollup`, `isDraft`, `labels`, `assignees`, `reviewRequests`, plus the stable node
 * `id` and `updatedAt` (the two-phase probe key). One query serves both Issues and PullRequests via
 * inline fragments. `$q` is the search qualifier string; `$after` pages.
 */
export const SEARCH_QUERY = `query($q: String!, $after: String) {
  search(query: $q, type: ISSUE, first: 50, after: $after) {
    issueCount
    pageInfo { hasNextPage endCursor }
    nodes {
      __typename
      ... on Issue {
        id number title url updatedAt state
        author { login }
        repository { nameWithOwner }
        labels(first: 20) { nodes { name } }
        assignees(first: 20) { nodes { login } }
      }
      ... on PullRequest {
        id number title url updatedAt state isDraft reviewDecision
        author { login }
        repository { nameWithOwner }
        labels(first: 20) { nodes { name } }
        assignees(first: 20) { nodes { login } }
        reviewRequests(first: 20) { nodes { requestedReviewer { ... on User { login } } } }
        statusCheckRollup { state }
      }
    }
  }
}`;

// ---------------------------------------------------------------------------------------------
// The client.
// ---------------------------------------------------------------------------------------------

/** Options for the {@link GhClient}. */
export interface GhClientOptions {
  exec: Exec;
  /** The `gh` binary name/path. Defaults to `"gh"` (resolved from PATH; auth is ambient, Q8). */
  bin?: string;
  /** Per-call timeout in ms. */
  timeoutMs?: number;
}

/**
 * The typed `gh` CLI client for the intake read surface. Every method shells `gh api …` through the
 * injectable `Exec` and parses + validates the response into a typed shape. A non-zero exit that
 * carries a rate-limit message is surfaced as a `TallyError{code:"timeout"}`-adjacent
 * `RateLimitExceeded` so the poller can back off; any other non-zero exit throws a `GhError`.
 */
export class GhClient {
  private readonly exec: Exec;
  private readonly bin: string;
  private readonly timeoutMs: number;

  constructor(opts: GhClientOptions) {
    this.exec = opts.exec;
    this.bin = opts.bin ?? "gh";
    this.timeoutMs = opts.timeoutMs ?? GH_CALL_TIMEOUT_MS;
  }

  /** Run one `gh api` invocation, mapping a rate-limit non-zero exit onto {@link RateLimitExceeded}. */
  private async api(args: string[], endpoint: string): Promise<ExecResult> {
    const res = await this.exec.run([this.bin, "api", ...args], { timeoutMs: this.timeoutMs });
    if (res.code !== 0) {
      const msg = `${res.stderr}\n${res.stdout}`;
      if (/rate limit/i.test(msg) || /API rate limit exceeded/i.test(msg)) {
        throw new RateLimitExceeded(endpoint, msg.trim());
      }
      throw new GhError(endpoint, res.code, msg.trim());
    }
    return res;
  }

  /**
   * Poll the notification inbox (`GET /notifications`; scan §5.1, §6 primitive 1). `--paginate`
   * concatenates pages (scan §6 "--paginate semantics on lists"). Returns every parsed thread; the
   * poller applies the reason filter + mute respect (signals.ts), never this transport.
   */
  async notifications(): Promise<Notification[]> {
    const res = await this.api(["/notifications", "--paginate"], "/notifications");
    return this.parseNotifications(res, "/notifications");
  }

  /** Parse a `GET /notifications` response body into typed threads. */
  parseNotifications(res: ExecResult, endpoint: string): Notification[] {
    const body = parseJson(res, endpoint);
    // `--paginate` on an array endpoint yields a single concatenated JSON array.
    if (!Array.isArray(body)) {
      throw new ValidationError(`gh api ${endpoint}: expected an array of notifications`, endpoint);
    }
    return body.map((raw, i) => this.parseNotification(raw, `${endpoint}[${i}]`));
  }

  private parseNotification(raw: unknown, path: string): Notification {
    if (!isObject(raw)) throw new ValidationError(`${path} must be an object`, path);
    const subject = isObject(raw.subject) ? raw.subject : {};
    const repo = isObject(raw.repository) ? raw.repository : {};
    const owner = isObject(repo.owner) ? repo.owner : {};
    const notif: Notification = {
      id: asString(raw.id, `${path}.id`),
      unread: asBool(raw.unread),
      reason: asString(raw.reason, `${path}.reason`) as NotificationReason,
      updated_at: typeof raw.updated_at === "string" ? raw.updated_at : "",
      last_read_at: asStringOrNull(raw.last_read_at),
      subject: {
        title: typeof subject.title === "string" ? subject.title : "",
        url: asStringOrNull(subject.url),
        latest_comment_url: asStringOrNull(subject.latest_comment_url),
        type: (typeof subject.type === "string" ? subject.type : "") as SubjectType,
      },
      repository: {
        full_name: typeof repo.full_name === "string" ? repo.full_name : "",
        name: typeof repo.name === "string" ? repo.name : "",
        owner: typeof owner.login === "string" ? owner.login : "",
      },
      subscription_url: asStringOrNull(raw.subscription_url),
    };
    if (isObject(raw.subscription)) {
      notif.subscription = {
        subscribed: asBool(raw.subscription.subscribed),
        ignored: asBool(raw.subscription.ignored),
        reason: asStringOrNull(raw.subscription.reason),
      };
    }
    return notif;
  }

  /**
   * Run one GraphQL `search` page for a qualifier string (`review-requested:@me is:open`, etc.;
   * scan §6 primitive 2). Pass `after` to page. Returns the parsed page; the poller loops on
   * `hasNextPage`/`endCursor`.
   */
  async search(qualifier: string, after?: string): Promise<SearchPage> {
    const args = ["graphql", "-f", `query=${SEARCH_QUERY}`, "-F", `q=${qualifier}`];
    if (after !== undefined) args.push("-F", `after=${after}`);
    const res = await this.api(args, "graphql:search");
    return this.parseSearch(res, "graphql:search");
  }

  /** Parse a GraphQL `search` response body into a typed page. */
  parseSearch(res: ExecResult, endpoint: string): SearchPage {
    const body = parseJson(res, endpoint);
    if (!isObject(body)) throw new ValidationError(`${endpoint}: expected an object`, endpoint);
    const data = isObject(body.data) ? body.data : undefined;
    const search = data && isObject(data.search) ? data.search : undefined;
    if (!search) {
      throw new ValidationError(`${endpoint}: response missing data.search`, endpoint);
    }
    const pageInfo = isObject(search.pageInfo) ? search.pageInfo : {};
    const nodesRaw = Array.isArray(search.nodes) ? search.nodes : [];
    return {
      issueCount: typeof search.issueCount === "number" ? search.issueCount : nodesRaw.length,
      hasNextPage: asBool(pageInfo.hasNextPage),
      endCursor: asStringOrNull(pageInfo.endCursor),
      nodes: nodesRaw.map((n, i) => this.parseSearchNode(n, `${endpoint}.nodes[${i}]`)),
    };
  }

  private parseSearchNode(raw: unknown, path: string): SearchNode {
    if (!isObject(raw)) throw new ValidationError(`${path} must be an object`, path);
    const author = isObject(raw.author) ? raw.author : undefined;
    const repo = isObject(raw.repository) ? raw.repository : undefined;
    const rollup = isObject(raw.statusCheckRollup) ? raw.statusCheckRollup : undefined;
    return {
      id: asString(raw.id, `${path}.id`),
      typename: (typeof raw.__typename === "string" ? raw.__typename : "") as SearchNode["typename"],
      number: typeof raw.number === "number" ? raw.number : 0,
      title: typeof raw.title === "string" ? raw.title : "",
      url: typeof raw.url === "string" ? raw.url : "",
      updatedAt: typeof raw.updatedAt === "string" ? raw.updatedAt : "",
      state: typeof raw.state === "string" ? raw.state : "",
      isDraft: asBool(raw.isDraft),
      reviewDecision: asStringOrNull(raw.reviewDecision),
      author: author && typeof author.login === "string" ? author.login : null,
      nameWithOwner: repo && typeof repo.nameWithOwner === "string" ? repo.nameWithOwner : "",
      labels: connectionLogins(raw.labels, "name"),
      assignees: connectionLogins(raw.assignees, "login"),
      reviewRequests: connectionLogins(raw.reviewRequests, "login", "requestedReviewer"),
      statusCheckRollup: rollup && typeof rollup.state === "string" ? rollup.state : null,
    };
  }

  /**
   * The cheap per-tracked-item `updatedAt` probe (scan §5.7 / §6 primitive 3). Fetches ONLY the
   * `updatedAt` of one node by its GraphQL id — octo's two-phase change-detection primitive: the
   * poller calls this before any full hydration, and only re-hydrates on a delta.
   */
  async probeUpdatedAt(nodeId: string): Promise<string | null> {
    const query = `query($id: ID!) {
  node(id: $id) {
    ... on Issue { updatedAt }
    ... on PullRequest { updatedAt }
  }
}`;
    const res = await this.api(
      ["graphql", "-f", `query=${query}`, "-F", `id=${nodeId}`],
      "graphql:updatedAt",
    );
    const body = parseJson(res, "graphql:updatedAt");
    if (!isObject(body) || !isObject(body.data)) return null;
    const node = body.data.node;
    if (isObject(node) && typeof node.updatedAt === "string") return node.updatedAt;
    return null;
  }

  /**
   * Check rate-limit headroom (`GET /rate_limit`; scan §5.7). The poller checks this before a poll
   * cycle and backs off when a bucket is exhausted — exactly as octo does.
   */
  async rateLimit(): Promise<RateLimitSnapshot> {
    const res = await this.api(["/rate_limit"], "/rate_limit");
    return this.parseRateLimit(res, "/rate_limit");
  }

  /** Parse a `GET /rate_limit` response into the three buckets tally uses. */
  parseRateLimit(res: ExecResult, endpoint: string): RateLimitSnapshot {
    const body = parseJson(res, endpoint);
    if (!isObject(body)) throw new ValidationError(`${endpoint}: expected an object`, endpoint);
    const resources = isObject(body.resources) ? body.resources : {};
    const bucket = (name: string): RateLimitBucket => {
      const b = isObject(resources[name]) ? (resources[name] as Record<string, unknown>) : {};
      return {
        remaining: asNumber(b.remaining ?? 0, `${endpoint}.${name}.remaining`),
        limit: asNumber(b.limit ?? 0, `${endpoint}.${name}.limit`),
        reset: asNumber(b.reset ?? 0, `${endpoint}.${name}.reset`),
      };
    };
    return { core: bucket("core"), graphql: bucket("graphql"), search: bucket("search") };
  }
}

/** A `gh api` call failed with a non-zero, non-rate-limit exit. */
export class GhError extends TallyError {
  readonly endpoint: string;
  readonly exitCode: number;
  constructor(endpoint: string, exitCode: number, detail: string) {
    super("internal", `gh api ${endpoint} failed (exit ${exitCode}): ${detail}`, {
      endpoint,
      exitCode,
    });
    this.name = "GhError";
    this.endpoint = endpoint;
    this.exitCode = exitCode;
    Object.setPrototypeOf(this, GhError.prototype);
  }
}

/** A `gh api` call was rejected for rate-limit exhaustion — the poller backs off on this. */
export class RateLimitExceeded extends TallyError {
  readonly endpoint: string;
  constructor(endpoint: string, detail: string) {
    super("timeout", `gh api ${endpoint} rate limited: ${detail}`, { endpoint });
    this.name = "RateLimitExceeded";
    this.endpoint = endpoint;
    Object.setPrototypeOf(this, RateLimitExceeded.prototype);
  }
}
