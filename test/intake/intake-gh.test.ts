// test/intake/intake-gh.test.ts
//
// Tests for the gh intake module (M2.4). Covers the brief's demands against the fake gh:
//   - reason filtering (only configured signal reasons become signals);
//   - mute respect (UNSUBSCRIBED/IGNORED subjects skipped);
//   - two-phase probe (no re-map without an updatedAt delta);
//   - row dedup (re-polls never duplicate rows, deduped on node id);
//   - rate-limit backoff (remaining==0 defers the cycle);
//   - OFF-by-default (disabled config ⇒ inert poller, no gh calls);
//   - the poller mounts on the supervise host (DaemonMount.registerSupervised).
//
// Uses the layer-0 testkit fakes (FakeExec, FakeGh, FakeTask). Authored fresh for tally; no vendor/
// code (clean-room, CLI-SURFACE §4).

import { describe, expect, test } from "bun:test";

import { FakeExec } from "../helpers/exec-fakes.ts";
import { FakeGh } from "../helpers/fake-gh.ts";
import { FakeTask } from "../helpers/fake-task.ts";

import type { Clock } from "../../src/contracts/exec.ts";
import type { DaemonMount, RpcHandler, SupervisedLoop, WatcherHandler } from "../../src/contracts/bus.ts";
import type { IntakeGhConfig } from "../../src/contracts/config.ts";
import { JournalEmitter } from "../../src/journal/index.ts";
import { TaskChampion } from "../../src/tw/index.ts";

import {
  GhClient,
  RateLimitExceeded,
  GhError,
  classifyReason,
  isMuted,
  signalFromNotification,
  defaultSignalPolicy,
  dedupeSignals,
  seedForSignal,
  stableUuid,
  dedupKeyFor,
  SignalMapper,
  RateLimitGate,
  decideFromSnapshot,
  backoffUntilReset,
  GhPoller,
  IntakeGh,
  POLLER_NAME,
  type Signal,
  type Notification,
  type SubscriptionState,
} from "../../src/intake/index.ts";

// ---------------------------------------------------------------------------------------------
// Harness helpers.
// ---------------------------------------------------------------------------------------------

/** A deterministic, advanceable clock. */
function makeClock(startIso = "2026-07-09T12:00:00.000Z"): Clock & { advance(ms: number): void; fire(): void } {
  let t = Date.parse(startIso);
  let intervalFn: (() => void) | null = null;
  return {
    now: () => t,
    nowIso: () => new Date(t).toISOString(),
    async sleep() {},
    setTimer: (_ms, fn) => {
      fn();
      return () => {};
    },
    setInterval: (_ms, fn) => {
      intervalFn = fn;
      return () => {
        intervalFn = null;
      };
    },
    advance(ms: number) {
      t += ms;
    },
    fire() {
      if (intervalFn) intervalFn();
    },
  };
}

/** A recording DaemonMount for the mount test. */
function recordingMount() {
  const supervised: SupervisedLoop[] = [];
  const rpcs: string[] = [];
  const watchers: string[] = [];
  const mount: DaemonMount = {
    registerRpc(method: string, _h: RpcHandler) {
      rpcs.push(method);
    },
    registerWatcher(path: string, _h: WatcherHandler) {
      watchers.push(path);
    },
    registerSupervised(loop: SupervisedLoop) {
      supervised.push(loop);
    },
  };
  return { mount, supervised, rpcs, watchers };
}

/** Wire a full intake harness: FakeExec+FakeGh+FakeTask, a veneer, a journal collector, a poller. */
function harness(config: Partial<IntakeGhConfig> = {}, opts: { intervalMs?: number } = {}) {
  const exec = new FakeExec();
  const gh = new FakeGh().install(exec);
  const task = new FakeTask();
  task.install(exec);
  const clock = makeClock();
  const tw = new TaskChampion({ exec, clock });
  const journalLines: string[] = [];
  const journal = new JournalEmitter((l) => journalLines.push(l));
  const cfg: IntakeGhConfig = { enable: true, sources: [], ...config };
  const poller = new GhPoller({
    config: cfg,
    gh: { exec },
    tw,
    journal,
    clock,
    intervalMs: opts.intervalMs ?? 60_000,
    log: () => {},
  });
  return { exec, gh, task, clock, tw, journal, journalLines, poller, cfg };
}

// ---------------------------------------------------------------------------------------------
// gh client — parsing + rate-limit mapping.
// ---------------------------------------------------------------------------------------------

describe("GhClient — parsing", () => {
  test("parses notifications into typed threads (fixture)", async () => {
    const exec = new FakeExec();
    new FakeGh().install(exec);
    const client = new GhClient({ exec });
    const notifs = await client.notifications();
    expect(notifs.length).toBe(5);
    const first = notifs[0]!;
    expect(first.reason).toBe("review_requested");
    expect(first.subject.type).toBe("PullRequest");
    expect(first.repository.full_name).toBe("mecattaf/tally");
    // The muted (ignored) subject carries an inlined subscription.
    const ignored = notifs.find((n) => n.id === "1004")!;
    expect(ignored.subscription?.ignored).toBe(true);
    const unsub = notifs.find((n) => n.id === "1005")!;
    expect(unsub.subscription?.reason).toBe("unsubscribed");
  });

  test("parses graphql search into typed nodes (fixture)", async () => {
    const exec = new FakeExec();
    new FakeGh().install(exec);
    const client = new GhClient({ exec });
    const page = await client.search("review-requested:@me is:open");
    expect(page.issueCount).toBe(3);
    expect(page.hasNextPage).toBe(false);
    const pr = page.nodes.find((n) => n.number === 128)!;
    expect(pr.id).toBe("PR_kwABC128");
    expect(pr.reviewDecision).toBe("REVIEW_REQUIRED");
    expect(pr.labels).toContain("witness");
    expect(pr.reviewRequests).toContain("mecattaf");
    expect(pr.statusCheckRollup).toBe("SUCCESS");
  });

  test("parses rate-limit snapshot", async () => {
    const exec = new FakeExec();
    new FakeGh().setRateLimit({ remaining: 42, limit: 5000, reset: 1234 }).install(exec);
    const client = new GhClient({ exec });
    const snap = await client.rateLimit();
    expect(snap.core.remaining).toBe(42);
    expect(snap.graphql.remaining).toBe(42);
    expect(snap.search.reset).toBe(1234);
  });

  test("maps a rate-limit non-zero exit onto RateLimitExceeded", async () => {
    const exec = new FakeExec();
    new FakeGh().setRateLimit({ remaining: 0, limit: 5000, reset: 0 }).install(exec);
    const client = new GhClient({ exec });
    await expect(client.notifications()).rejects.toBeInstanceOf(RateLimitExceeded);
  });

  test("a genuine gh failure is a GhError, not a rate-limit", async () => {
    const exec = new FakeExec();
    // A gh that fails every call with a non-rate-limit error.
    exec.register("gh", () => ({ code: 1, stdout: "", stderr: "server error 500" }));
    const client = new GhClient({ exec });
    await expect(client.notifications()).rejects.toBeInstanceOf(GhError);
  });
});

// ---------------------------------------------------------------------------------------------
// signals — classification, reason filtering, mute respect.
// ---------------------------------------------------------------------------------------------

describe("signals — classification + filtering", () => {
  const policy = defaultSignalPolicy();

  test("classifyReason maps the first-class reasons", () => {
    expect(classifyReason("review_requested")).toBe("review_requested");
    expect(classifyReason("mention")).toBe("mention");
    expect(classifyReason("team_mention")).toBe("mention");
    expect(classifyReason("assign")).toBe("assign");
    expect(classifyReason("author")).toBe("author");
    expect(classifyReason("ci_activity")).toBe("other");
  });

  test("reason filtering: a non-signal reason yields no signal", () => {
    const base: Notification = {
      id: "x",
      unread: true,
      reason: "subscribed",
      updated_at: "2026-07-09T09:00:00Z",
      last_read_at: null,
      subject: { title: "t", url: "https://api.github.com/repos/o/r/issues/1", latest_comment_url: null, type: "Issue" },
      repository: { full_name: "o/r", name: "r", owner: "o" },
      subscription_url: null,
    };
    expect(signalFromNotification(base, policy)).toBeNull();
    // Flip the reason to a first-class one → a signal appears.
    const sig = signalFromNotification({ ...base, reason: "review_requested" }, policy);
    expect(sig).not.toBeNull();
    expect(sig!.class).toBe("review_requested");
  });

  test("mute respect: ignored / unsubscribed subjects are dropped", () => {
    const ignored: SubscriptionState = { subscribed: false, ignored: true, reason: null };
    const unsub: SubscriptionState = { subscribed: false, ignored: false, reason: "unsubscribed" };
    const active: SubscriptionState = { subscribed: true, ignored: false, reason: null };
    expect(isMuted(ignored)).toBe(true);
    expect(isMuted(unsub)).toBe(true);
    expect(isMuted(active)).toBe(false);
    expect(isMuted(undefined)).toBe(false);

    const n: Notification = {
      id: "1005",
      unread: true,
      reason: "review_requested",
      updated_at: "2026-07-09T06:10:00Z",
      last_read_at: null,
      subject: { title: "muted PR", url: "https://api.github.com/repos/o/r/pulls/55", latest_comment_url: null, type: "PullRequest" },
      repository: { full_name: "o/r", name: "r", owner: "o" },
      subscription_url: null,
      subscription: unsub,
    };
    expect(signalFromNotification(n, policy)).toBeNull();
  });

  test("dedupeSignals keeps the highest-urgency class per node id", () => {
    const a: Signal = {
      node_id: "N1", class: "mention", subject_type: "Issue", title: "t", repo: "o/r",
      number: 1, url: "", updated_at: "2026-07-09T09:00:00Z", origin: "notification",
    };
    const b: Signal = { ...a, class: "review_requested", origin: "search" };
    const out = dedupeSignals([a, b]);
    expect(out.length).toBe(1);
    expect(out[0]!.class).toBe("review_requested");
  });
});

// ---------------------------------------------------------------------------------------------
// map — dedup + stable uuid.
// ---------------------------------------------------------------------------------------------

describe("map — dedup + stable uuid", () => {
  const policy = defaultSignalPolicy();
  const sig: Signal = {
    node_id: "PR_kwABC128", class: "review_requested", subject_type: "PullRequest",
    title: "Fix witness chain", repo: "mecattaf/tally", number: 128,
    url: "https://github.com/mecattaf/tally/pull/128", updated_at: "2026-07-09T09:15:00Z", origin: "search",
  };

  test("stableUuid is deterministic + RFC-4122 shaped", () => {
    const key = dedupKeyFor(sig);
    const u1 = stableUuid(key);
    const u2 = stableUuid(key);
    expect(u1).toBe(u2);
    expect(u1).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  });

  test("seedForSignal maps class→priority and source=gh", () => {
    const seed = seedForSignal(sig, policy);
    expect(seed.source).toBe("gh");
    expect(seed.priority).toBe("high"); // review_requested → high
    expect(seed.dedup_key).toBe("gh:PR_kwABC128");
    expect(seed.kind).toBe("shell");
  });

  test("mapper creates once, then reports existing on re-map (dedup)", async () => {
    const exec = new FakeExec();
    const task = new FakeTask();
    task.install(exec);
    const tw = new TaskChampion({ exec, clock: makeClock() });
    const mapper = new SignalMapper(tw, policy);

    const first = await mapper.map(sig);
    expect(first.status).toBe("created");
    const uuid = first.uuid;

    const second = await mapper.map(sig);
    expect(second.status).toBe("existing");
    expect(second.uuid).toBe(uuid);

    // Exactly one row landed in the store.
    const rows = await tw.query([`dedup_key:${dedupKeyFor(sig)}`]);
    expect(rows.length).toBe(1);
  });

  test("mapAll de-conflicts duplicates within a batch", async () => {
    const exec = new FakeExec();
    const task = new FakeTask();
    task.install(exec);
    const tw = new TaskChampion({ exec, clock: makeClock() });
    const mapper = new SignalMapper(tw, policy);
    const outcomes = await mapper.mapAll([sig, { ...sig, origin: "notification" }]);
    // Same node id twice ⇒ one outcome.
    expect(outcomes.length).toBe(1);
    expect(outcomes[0]!.status).toBe("created");
  });
});

// ---------------------------------------------------------------------------------------------
// ratelimit — decision + backoff.
// ---------------------------------------------------------------------------------------------

describe("ratelimit — decision + backoff", () => {
  test("decideFromSnapshot defers when a bucket is below headroom", () => {
    const clock = makeClock();
    const resetSec = Math.floor(clock.now() / 1000) + 30; // 30s out
    const snap = {
      core: { remaining: 0, limit: 5000, reset: resetSec },
      graphql: { remaining: 5000, limit: 5000, reset: 0 },
      search: { remaining: 5000, limit: 5000, reset: 0 },
    };
    const d = decideFromSnapshot(snap, clock);
    expect(d.ok).toBe(false);
    expect(d.limiting).toBe("core");
    expect(d.backoffMs).toBeGreaterThan(0);
  });

  test("decideFromSnapshot proceeds with headroom", () => {
    const clock = makeClock();
    const snap = {
      core: { remaining: 5000, limit: 5000, reset: 0 },
      graphql: { remaining: 5000, limit: 5000, reset: 0 },
      search: { remaining: 5000, limit: 5000, reset: 0 },
    };
    expect(decideFromSnapshot(snap, clock).ok).toBe(true);
  });

  test("backoffUntilReset is bounded to [MIN, MAX]", () => {
    const clock = makeClock();
    // A reset in the past ⇒ MIN floor.
    expect(backoffUntilReset({ remaining: 0, limit: 5000, reset: 0 }, clock)).toBeGreaterThanOrEqual(1000);
  });

  test("RateLimitGate opens after the backoff elapses", () => {
    const clock = makeClock();
    const gate = new RateLimitGate(clock);
    expect(gate.isOpen()).toBe(true);
    gate.backoff(5000);
    expect(gate.isOpen()).toBe(false);
    clock.advance(5001);
    expect(gate.isOpen()).toBe(true);
  });
});

// ---------------------------------------------------------------------------------------------
// poller — the two-phase supervised loop.
// ---------------------------------------------------------------------------------------------

describe("GhPoller — cycles", () => {
  test("OFF by default: a disabled poller makes NO gh calls and lands no rows", async () => {
    const h = harness({ enable: false });
    const result = await h.poller.runCycle();
    expect(result.ran).toBe(false);
    // No gh subprocess was invoked.
    expect(h.exec.callsFor("gh").length).toBe(0);
    const rows = await h.tw.query([]);
    expect(rows.length).toBe(0);
  });

  test("a full cycle lands review_requested / mention / assign rows, mutes the noisy ones", async () => {
    const h = harness();
    const result = await h.poller.runCycle();
    expect(result.ran).toBe(true);

    // Fixture: notifications 1001 (review_requested), 1002 (mention), 1003 (assign) qualify;
    // 1004 (subscribed+ignored) and 1005 (unsubscribed) are dropped by reason/mute. Search adds
    // PR 128 + issues 131/117. All are landed, deduped on node id.
    const rows = await h.tw.query([]);
    expect(rows.length).toBeGreaterThan(0);
    // Every landed row is source=gh.
    for (const r of rows) expect(r.source).toBe("gh");

    // The muted PR #55 (node key derived from its subject url) never lands.
    const mutedKey = "gh:mecattaf/tally#55";
    const mutedRows = await h.tw.query([`dedup_key:${mutedKey}`]);
    expect(mutedRows.length).toBe(0);

    // A journald `enqueued` line was emitted per newly-created row.
    const enqueuedLines = h.journalLines.filter((l) => l.includes('"TALLY_EVENT":"enqueued"'));
    expect(enqueuedLines.length).toBe(rows.length);
    for (const l of enqueuedLines) expect(l).toContain('"TALLY_SOURCE":"gh"');
  });

  test("two-phase probe: a second cycle with no updatedAt delta re-maps nothing", async () => {
    const h = harness();
    const first = await h.poller.runCycle();
    expect(first.outcomes.length).toBeGreaterThan(0);
    const rowsAfterFirst = (await h.tw.query([])).length;

    // Re-run with identical fixtures (same updatedAt) → the delta filter drops every candidate.
    const second = await h.poller.runCycle();
    expect(second.signals.length).toBe(0);
    expect(second.outcomes.length).toBe(0);

    // No new rows; the store is unchanged (dedup + two-phase both hold).
    const rowsAfterSecond = (await h.tw.query([])).length;
    expect(rowsAfterSecond).toBe(rowsAfterFirst);
  });

  test("two-phase probe: a changed updatedAt re-surfaces the subject", async () => {
    const h = harness();
    await h.poller.runCycle();
    const before = (await h.tw.query([])).length;

    // Bump the updatedAt of one search node → it passes the delta filter again. Because the row
    // uuid is stable (dedup key), the re-map is idempotent: it reports `existing`, no duplicate row.
    h.gh.setSearch({
      data: {
        search: {
          issueCount: 1,
          pageInfo: { hasNextPage: false, endCursor: null },
          nodes: [
            {
              __typename: "PullRequest",
              id: "PR_kwABC128",
              number: 128,
              title: "Fix witness chain restart recovery",
              url: "https://github.com/mecattaf/tally/pull/128",
              updatedAt: "2026-07-10T00:00:00Z", // CHANGED
              isDraft: false,
              state: "OPEN",
              reviewDecision: "REVIEW_REQUIRED",
              author: { login: "contributor-a" },
              repository: { nameWithOwner: "mecattaf/tally" },
              labels: { nodes: [] },
              assignees: { nodes: [] },
              reviewRequests: { nodes: [] },
              statusCheckRollup: null,
            },
          ],
        },
      },
    });
    // Empty the notifications so only the changed search node surfaces.
    h.gh.setNotifications([]);

    const second = await h.poller.runCycle();
    // The changed node surfaced (delta passed) but did not create a NEW row.
    expect(second.signals.some((s) => s.node_id === "PR_kwABC128")).toBe(true);
    expect(second.outcomes.every((o) => o.status === "existing")).toBe(true);
    const after = (await h.tw.query([])).length;
    expect(after).toBe(before); // no duplicate row
  });

  test("rate-limit backoff: exhausted headroom defers the cycle (no rows, no notification poll)", async () => {
    const h = harness();
    h.gh.setRateLimit({ remaining: 0, limit: 5000, reset: Math.floor(h.clock.now() / 1000) + 30 });
    const result = await h.poller.runCycle();
    expect(result.ran).toBe(false);
    expect(result.rateLimited).toBe(true);
    // Only the /rate_limit probe ran — no notifications / search hydration.
    expect(h.gh.calls.some((c) => c.kind === "rate_limit")).toBe(true);
    expect(h.gh.calls.some((c) => c.kind === "notifications")).toBe(false);
    expect(h.gh.calls.some((c) => c.kind === "graphql")).toBe(false);
    const rows = await h.tw.query([]);
    expect(rows.length).toBe(0);
  });

  test("rate-limit gate: while backing off, the next cycle short-circuits before any gh call", async () => {
    const h = harness();
    h.gh.setRateLimit({ remaining: 0, limit: 5000, reset: Math.floor(h.clock.now() / 1000) + 300 });
    await h.poller.runCycle(); // sets the gate
    const callsAfterFirst = h.gh.calls.length;
    const second = await h.poller.runCycle(); // gate closed → no gh call at all
    expect(second.rateLimited).toBe(true);
    expect(h.gh.calls.length).toBe(callsAfterFirst); // no additional gh calls
  });
});

// ---------------------------------------------------------------------------------------------
// mount — the DaemonMount seam.
// ---------------------------------------------------------------------------------------------

describe("IntakeGh — daemon mount", () => {
  test("mount registers the poller as a supervised loop", () => {
    const exec = new FakeExec();
    new FakeGh().install(exec);
    const task = new FakeTask();
    task.install(exec);
    const clock = makeClock();
    const tw = new TaskChampion({ exec, clock });
    const intake = new IntakeGh({
      config: { enable: false, sources: [] },
      exec,
      tw,
      clock,
      journal: new JournalEmitter(() => {}),
      log: () => {},
    });
    const { mount, supervised, rpcs, watchers } = recordingMount();
    intake.mount(mount);

    // Exactly one supervised loop, named intake-gh; no RPC / watcher registrations (the poller is a
    // pure supervised loop — it does not carry an RPC method or a directory watcher).
    expect(supervised.length).toBe(1);
    expect(supervised[0]!.name).toBe(POLLER_NAME);
    expect(rpcs.length).toBe(0);
    expect(watchers.length).toBe(0);
  });

  test("a disabled poller is an inert no-op loop: start() stays pending until stop(), does no gh work", async () => {
    const h = harness({ enable: false });
    // A disabled poller is a LONG-RUNNING loop that consumes nothing: its start() promise stays PENDING
    // (the supervise host must see it as still-running, not settled — a settled start() would be
    // re-invoked in an endless restart loop). It settles ONLY on stop(). It shells no `gh`.
    const started = h.poller.start();
    let settled = false;
    void started.then(() => {
      settled = true;
    });
    await Promise.resolve(); // let any microtask-resolution flush
    expect(settled).toBe(false); // still pending — the loop is running (idle), not settled
    expect(h.exec.callsFor("gh").length).toBe(0);
    h.poller.stop();
    await expect(started).resolves.toBeUndefined(); // stop() settles the loop cleanly
    expect(h.exec.callsFor("gh").length).toBe(0);
  });
});
