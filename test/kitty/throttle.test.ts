// tally — read-throttle tests (IMPLEMENTATION-PLAN M1.6: throttle coalescing).
//
// Asserts the shared kitty-read budget: min-interval throttling per window, working-vs-idle cadence
// (flag 4), concurrent-read coalescing (the detector poll + `pane capture` never double-hit kitty),
// and the monotonic per-window `revision` carried by `pane.output_matched`.

import { describe, expect, test } from "bun:test";
import { ReadThrottle, DEFAULT_DETECTOR_CADENCE } from "../../src/kitty/throttle.ts";
import type { Clock } from "../../src/contracts/exec.ts";

/** A deterministic clock the tests advance by hand. */
function fakeClock(): Clock & { advance(ms: number): void } {
  let t = 1_000_000;
  return {
    now: () => t,
    nowIso: () => new Date(t).toISOString(),
    sleep: () => Promise.resolve(),
    setTimer: () => () => {},
    setInterval: () => () => {},
    advance(ms: number) {
      t += ms;
    },
  };
}

describe("ReadThrottle min-interval", () => {
  test("coalesces a second read inside the interval (no fresh get-text)", async () => {
    const clock = fakeClock();
    let reads = 0;
    const throttle = new ReadThrottle(async () => `read-${++reads}`, clock);

    const a = await throttle.read(1);
    expect(a.coalesced).toBe(false);
    expect(a.text).toBe("read-1");
    expect(a.revision).toBe(1);

    // Immediately again, under the (idle=10s) interval ⇒ cached, no new read.
    const b = await throttle.read(1);
    expect(b.coalesced).toBe(true);
    expect(b.text).toBe("read-1");
    expect(b.revision).toBe(1);
    expect(reads).toBe(1);
  });

  test("re-reads once the interval elapses, bumping revision", async () => {
    const clock = fakeClock();
    let reads = 0;
    const throttle = new ReadThrottle(async () => `read-${++reads}`, clock);

    await throttle.read(1);
    clock.advance(DEFAULT_DETECTOR_CADENCE.idle_poll_ms + 1);
    const c = await throttle.read(1);
    expect(c.coalesced).toBe(false);
    expect(c.text).toBe("read-2");
    expect(c.revision).toBe(2);
    expect(reads).toBe(2);
  });

  test("working panes poll faster than idle panes (flag 4 cadence)", async () => {
    const clock = fakeClock();
    const throttle = new ReadThrottle(async () => "x", clock);

    throttle.setStatus(1, "working");
    expect(throttle.intervalFor("working")).toBe(DEFAULT_DETECTOR_CADENCE.working_poll_ms);
    expect(throttle.intervalFor("idle")).toBe(DEFAULT_DETECTOR_CADENCE.idle_poll_ms);

    await throttle.read(1);
    // Advance past the working interval but NOT the idle interval.
    clock.advance(DEFAULT_DETECTOR_CADENCE.working_poll_ms + 1);
    expect(throttle.isDue(1)).toBe(true); // due under working cadence
    throttle.setStatus(1, "idle");
    expect(throttle.isDue(1)).toBe(false); // not yet due under idle cadence
  });
});

describe("ReadThrottle coalescing concurrent reads", () => {
  test("two concurrent reads of one window share ONE get-text", async () => {
    const clock = fakeClock();
    let reads = 0;
    let release!: (v: string) => void;
    const gate = new Promise<string>((r) => (release = r));
    const throttle = new ReadThrottle(async () => {
      reads++;
      return gate; // both callers await the same in-flight promise
    }, clock);

    const p1 = throttle.read(1);
    const p2 = throttle.read(1);
    release("grid");
    const [r1, r2] = await Promise.all([p1, p2]);

    expect(reads).toBe(1); // only ONE actual read issued
    expect(r1.text).toBe("grid");
    expect(r2.text).toBe("grid");
    // Exactly one of them (or both) is coalesced; the revision advanced exactly once.
    expect(throttle.revisionOf(1)).toBe(1);
  });
});

describe("ReadThrottle forceRead (pane capture)", () => {
  test("bypasses the min interval but still coalesces + bumps revision", async () => {
    const clock = fakeClock();
    let reads = 0;
    const throttle = new ReadThrottle(async () => `r${++reads}`, clock);

    await throttle.read(1); // primes, revision 1
    // A capture immediately after: forceRead bypasses the interval ⇒ fresh read.
    const cap = await throttle.forceRead(1);
    expect(cap.coalesced).toBe(false);
    expect(cap.revision).toBe(2);
    expect(reads).toBe(2);
  });
});

describe("ReadThrottle lifecycle", () => {
  test("forget resets a window's revision", async () => {
    const clock = fakeClock();
    const throttle = new ReadThrottle(async () => "x", clock);
    await throttle.read(1);
    expect(throttle.revisionOf(1)).toBe(1);
    throttle.forget(1);
    expect(throttle.revisionOf(1)).toBe(0);
  });

  test("a fresh window is always due", () => {
    const clock = fakeClock();
    const throttle = new ReadThrottle(async () => "x", clock);
    expect(throttle.isDue(99)).toBe(true);
  });
});
