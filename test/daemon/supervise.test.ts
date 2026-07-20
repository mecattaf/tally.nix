// daemon-core supervise: restart-isolation for in-daemon loops (PS#15a).

import { describe, expect, test } from "bun:test";
import { Supervisor } from "../../src/daemon/supervise";
import { systemClock } from "../../src/contracts/exec";
import type { SupervisedLoop } from "../../src/contracts/bus";

const silent = () => {};

describe("supervise", () => {
  test("restarts a loop that crashes; the supervisor (daemon) lives", async () => {
    let starts = 0;
    const loop: SupervisedLoop = {
      name: "crashy",
      start: async () => {
        starts += 1;
        if (starts <= 2) throw new Error("boom");
        // Third start: settle cleanly by resolving (a real loop would run forever).
      },
    };
    const sup = new Supervisor(
      systemClock,
      { baseBackoffMs: 1, maxBackoffMs: 2, maxRestarts: 0, crashWindowMs: 60_000 },
      silent,
    );
    sup.register(loop);
    sup.start();
    // Let the backoff restarts flush.
    await new Promise((r) => setTimeout(r, 60));
    expect(starts).toBeGreaterThanOrEqual(3);
    await sup.stop();
  });

  test("a crash in one loop does not affect a sibling", async () => {
    let siblingRan = false;
    const crashy: SupervisedLoop = {
      name: "crashy",
      start: async () => {
        throw new Error("boom");
      },
    };
    const healthy: SupervisedLoop = {
      name: "healthy",
      start: async () => {
        siblingRan = true;
      },
    };
    const sup = new Supervisor(
      systemClock,
      { baseBackoffMs: 1, maxBackoffMs: 2, maxRestarts: 3, crashWindowMs: 60_000 },
      silent,
    );
    sup.register(crashy);
    sup.register(healthy);
    sup.start();
    await new Promise((r) => setTimeout(r, 40));
    expect(siblingRan).toBe(true);
    await sup.stop();
  });

  test("quarantines a hard-looping crasher after the restart budget", async () => {
    const loop: SupervisedLoop = {
      name: "hardloop",
      start: async () => {
        throw new Error("always");
      },
    };
    const sup = new Supervisor(
      systemClock,
      { baseBackoffMs: 1, maxBackoffMs: 1, maxRestarts: 3, crashWindowMs: 60_000 },
      silent,
    );
    sup.register(loop);
    sup.start();
    await new Promise((r) => setTimeout(r, 60));
    expect(sup.stateOf("hardloop")).toBe("quarantined");
    await sup.stop();
  });

  test("stop() cancels a pending backoff and tears the loop down", async () => {
    let stopped = false;
    const loop: SupervisedLoop = {
      name: "l",
      start: async () => {
        throw new Error("x");
      },
      stop: () => {
        stopped = true;
      },
    };
    const sup = new Supervisor(
      systemClock,
      { baseBackoffMs: 1000, maxBackoffMs: 1000, maxRestarts: 10, crashWindowMs: 60_000 },
      silent,
    );
    sup.register(loop);
    sup.start();
    await new Promise((r) => setTimeout(r, 10));
    await sup.stop();
    expect(stopped).toBe(true);
    expect(sup.stateOf("l")).toBe("stopped");
  });
});
