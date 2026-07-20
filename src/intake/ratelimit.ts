// tally — gh intake rate-limit headroom + backoff (IMPLEMENTATION-PLAN M2.4; octo.nvim surface scan
// §5.7 / §6 "check /rate_limit like octo does"). The poller checks headroom before a cycle and backs
// off when a bucket is exhausted, so the intake never hammers a throttled API.
//
// This is pure decision logic over a `RateLimitSnapshot` + a `Clock`; it performs no I/O. It reads
// `gh.rateLimit()` snapshots and the `RateLimitExceeded` throw (a mid-cycle 403) and computes the
// next poll delay. The reset epoch drives the backoff so tally waits exactly until the window
// reopens rather than guessing.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Clock } from "../contracts/exec";
import type { RateLimitBucket, RateLimitSnapshot } from "./gh";

/** The minimum remaining headroom, per bucket, below which a poll cycle is deferred (scan §5.7). */
export const MIN_HEADROOM = 10;

/** The floor/ceiling for a computed backoff, in ms — never busy-spin, never wait absurdly long. */
export const MIN_BACKOFF_MS = 1_000;
export const MAX_BACKOFF_MS = 15 * 60_000; // 15 minutes

/** A rate-limit decision: whether a poll may proceed, and how long to wait when it may not. */
export interface RateDecision {
  /** True when every relevant bucket has headroom above {@link MIN_HEADROOM}. */
  ok: boolean;
  /** When `ok` is false, the ms to wait before retrying (until the earliest bucket reset). */
  backoffMs: number;
  /** The bucket that gated the decision (for diagnostics), or undefined when `ok`. */
  limiting?: keyof RateLimitSnapshot;
}

/** Compute the ms until a bucket's reset epoch, bounded to [MIN, MAX]. */
export function backoffUntilReset(bucket: RateLimitBucket, clock: Clock): number {
  const resetMs = bucket.reset * 1000;
  const delta = resetMs - clock.now();
  if (!Number.isFinite(delta) || delta <= 0) return MIN_BACKOFF_MS;
  return Math.min(MAX_BACKOFF_MS, Math.max(MIN_BACKOFF_MS, delta));
}

/**
 * Decide whether a poll cycle may proceed given a rate-limit snapshot. The poll uses the REST
 * (`core`) bucket for `/notifications` and the `search`/`graphql` buckets for the search + probe;
 * the decision gates on the tightest of the three. When a bucket is below {@link MIN_HEADROOM}, the
 * cycle is deferred until that bucket's reset.
 */
export function decideFromSnapshot(snap: RateLimitSnapshot, clock: Clock): RateDecision {
  const buckets: Array<[keyof RateLimitSnapshot, RateLimitBucket]> = [
    ["core", snap.core],
    ["graphql", snap.graphql],
    ["search", snap.search],
  ];
  let worst: { name: keyof RateLimitSnapshot; bucket: RateLimitBucket } | undefined;
  for (const [name, bucket] of buckets) {
    if (bucket.remaining < MIN_HEADROOM) {
      if (worst === undefined || bucket.reset < worst.bucket.reset) {
        worst = { name, bucket };
      }
    }
  }
  if (worst === undefined) return { ok: true, backoffMs: 0 };
  return {
    ok: false,
    backoffMs: backoffUntilReset(worst.bucket, clock),
    limiting: worst.name,
  };
}

/**
 * The backoff to apply after a mid-cycle {@link RateLimitExceeded} throw (a 403 the snapshot did not
 * anticipate). Without a fresh reset epoch we fall back to a fixed conservative wait — the next cycle
 * re-checks the snapshot and computes an exact reset-based backoff.
 */
export const EXCEEDED_BACKOFF_MS = 60_000;

/**
 * A tiny stateful tracker the poller holds: it remembers the last decision so a caller can query
 * "am I currently backing off?" without re-fetching. Purely in-memory; reset on daemon restart.
 */
export class RateLimitGate {
  private nextAllowedAt = 0;

  constructor(private readonly clock: Clock) {}

  /** Record that the poller must wait `ms` before the next cycle. */
  backoff(ms: number): void {
    this.nextAllowedAt = Math.max(this.nextAllowedAt, this.clock.now() + ms);
  }

  /** Apply a rate-limit-exceeded throw's fixed backoff. */
  onExceeded(): void {
    this.backoff(EXCEEDED_BACKOFF_MS);
  }

  /** Ms remaining before the next cycle may run (0 when clear). */
  waitMs(): number {
    const delta = this.nextAllowedAt - this.clock.now();
    return delta > 0 ? delta : 0;
  }

  /** True when the gate is currently open (no active backoff). */
  isOpen(): boolean {
    return this.waitMs() === 0;
  }
}
