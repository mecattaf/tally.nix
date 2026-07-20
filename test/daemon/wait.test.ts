// daemon-core session.wait predicate engine (CLI-SURFACE §2.4): job/agent barriers, pane_output via
// the WaitScrapeProvider seam incl. viewer rejection, and timeout semantics.

import { describe, expect, test } from "bun:test";
import { runWait, type WaitHost } from "../../src/daemon/wait";
import { DaemonBus } from "../../src/daemon/state";
import { systemClock } from "../../src/contracts/exec";
import { ViewerRejected, TallyError } from "../../src/contracts/errors";
import type { WaitScrapeProvider, PaneOutputWaitRequest, PaneOutputWaitResult } from "../../src/contracts/bus";
import type { JobCompletedPayload, AgentStatusChangedPayload } from "../../src/contracts/events";

function completed(job_id: string): JobCompletedPayload {
  return { job_id, task_uuid: null, exit_code: 0, gpu_seconds: null, artifact_hash: null, labor_class: "fresh" };
}
function statusChanged(agent_id: string, status: AgentStatusChangedPayload["status"]): AgentStatusChangedPayload {
  return { agent_id, pane_id: "s:p", session_id: "s", status, detector: "hook", since: "t" };
}

function hostWith(bus: DaemonBus, provider: WaitScrapeProvider | null = null): WaitHost {
  // No job-barrier provider wired: these tests exercise the bus-subscription fallback path for the job
  // predicate (the BarrierTracker-backed path is covered by the engine + e2e barrier suites).
  return { bus, clock: systemClock, waitScrape: () => provider, jobBarrier: () => null };
}

describe("session.wait", () => {
  test("job barrier satisfied when N distinct jobs complete", async () => {
    const bus = new DaemonBus();
    const p = runWait(hostWith(bus), {
      predicate: { subject: "job", job_ids: ["a", "b", "c"], until: ["completed", "failed"], count: 2 },
    });
    bus.emit("job.completed", completed("a"));
    bus.emit("job.completed", completed("a")); // dup ignored
    bus.emit("job.completed", completed("b"));
    const r = await p;
    expect(r.timed_out).toBeUndefined();
    expect(r.satisfied.length).toBe(2);
  });

  test("job barrier times out with pending list", async () => {
    const bus = new DaemonBus();
    const r = await runWait(hostWith(bus), {
      predicate: { subject: "job", job_ids: ["a", "b"], until: ["completed"], count: 2 },
      timeout_ms: 20,
    });
    expect(r.timed_out).toBe(true);
    expect(r.pending).toEqual(["a", "b"]);
  });

  test("agent barrier accepts the full four-value status (idle/working)", async () => {
    const bus = new DaemonBus();
    const p = runWait(hostWith(bus), {
      predicate: { subject: "agent", agent_ids: ["ag1"], until_status: "working", count: 1 },
    });
    bus.emit("agent.status_changed", statusChanged("ag1", "done")); // wrong status, ignored
    bus.emit("agent.status_changed", statusChanged("ag1", "working"));
    const r = await p;
    expect(r.satisfied.length).toBe(1);
  });

  test("pane_output routes through WaitScrapeProvider and returns pane.output_matched", async () => {
    const bus = new DaemonBus();
    const provider: WaitScrapeProvider = {
      async awaitPaneOutput(req: PaneOutputWaitRequest): Promise<PaneOutputWaitResult> {
        return {
          pane_id: req.pane_id,
          session_id: "s",
          matched_line: "BUILD OK",
          read: { source: "detection", format: "text", text: "BUILD OK", revision: 1, truncated: false },
        };
      },
    };
    const r = await runWait(hostWith(bus, provider), {
      predicate: { subject: "pane_output", pane_id: "s:p1", regex: "BUILD OK" },
    });
    const ev = r.satisfied[0] as Record<string, unknown>;
    expect(ev.event).toBe("pane.output_matched");
    expect(ev.matched_line).toBe("BUILD OK");
  });

  test("pane_output on a viewer pane rejects with ViewerRejected", async () => {
    const bus = new DaemonBus();
    const provider: WaitScrapeProvider = {
      async awaitPaneOutput(req): Promise<PaneOutputWaitResult> {
        throw new ViewerRejected(req.pane_id);
      },
    };
    await expect(
      runWait(hostWith(bus, provider), {
        predicate: { subject: "pane_output", pane_id: "s:viewer", regex: "x" },
      }),
    ).rejects.toBeInstanceOf(ViewerRejected);
  });

  test("pane_output with no detector mounted is unsupported", async () => {
    const bus = new DaemonBus();
    await expect(
      runWait(hostWith(bus, null), {
        predicate: { subject: "pane_output", pane_id: "s:p", regex: "x" },
      }),
    ).rejects.toMatchObject({ code: "unsupported" });
  });

  test("pane_output provider timeout surfaces timed_out", async () => {
    const bus = new DaemonBus();
    const provider: WaitScrapeProvider = {
      async awaitPaneOutput(): Promise<PaneOutputWaitResult> {
        throw new TallyError("timeout", "deadline");
      },
    };
    const r = await runWait(hostWith(bus, provider), {
      predicate: { subject: "pane_output", pane_id: "s:p", regex: "x" },
      timeout_ms: 5,
    });
    expect(r.timed_out).toBe(true);
  });
});
