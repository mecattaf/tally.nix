// tally daemon-core — the `session.wait` predicate engine (CLI-SURFACE §2.4, byte-for-byte).
//
// `session.wait` is one-shot BLOCKING: internally subscribe + filter + first-match(es) +
// auto-unsubscribe. It is the Seam-A `--wait` barrier. Three predicate subjects:
//   - `job {job_ids[], until:[completed|failed], count:N}` — barrier = enqueue-N-await-N.
//   - `agent {agent_ids[], until_status, count:N}` — over the FULL four-value AgentStatus
//     (IMPLEMENTATION-PLAN §3: required so `agent wait --status idle|working` is servable).
//   - `pane_output {pane_id, regex}` — agent panes only; `is_viewer` REJECTED (anti-loop invariant
//     #4). wait.ts NEVER imports sensors/detector: it satisfies the read through the
//     `WaitScrapeProvider` seam the detector registered, and returns the `pane.output_matched` event
//     the detector emits as the RPC result (it CONSUMES, never emits).
//
// On timeout the result is `{timed_out:true, satisfied, pending}`. Timing is via the injected `Clock`.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Clock } from "../contracts/exec";
import type { Bus, Unsubscribe, WaitScrapeProvider, JobBarrierProvider } from "../contracts/bus";
import type { WaitParams, WaitResult, JobPredicate, AgentPredicate, PaneOutputPredicate } from "../contracts/wire";
import type {
  JobCompletedPayload,
  JobFailedPayload,
  AgentStatusChangedPayload,
  AgentDetectedPayload,
} from "../contracts/events";
import { TallyError, ViewerRejected } from "../contracts/errors";

/** What the wait engine needs from daemon-core: the bus (job/agent events) + the scrape provider. */
export interface WaitHost {
  bus: Bus;
  clock: Clock;
  waitScrape(): WaitScrapeProvider | null;
  /** The jobs engine's barrier (drains already-terminal jobs), or null if the engine has not mounted. */
  jobBarrier(): JobBarrierProvider | null;
}

/**
 * Execute a `session.wait`. Resolves with the `WaitResult` (`satisfied` populated), or — on
 * `timeout_ms` — `{timed_out:true, satisfied, pending}`. A `pane_output` predicate targeting an
 * `is_viewer` pane rejects with `ViewerRejected`; a `pane_output` with no detector mounted rejects
 * with `unsupported`.
 */
export function runWait(host: WaitHost, params: WaitParams): Promise<WaitResult> {
  switch (params.predicate.subject) {
    case "job":
      return waitJob(host, params.predicate, params.timeout_ms);
    case "agent":
      return waitAgent(host, params.predicate, params.timeout_ms);
    case "pane_output":
      return waitPaneOutput(host, params.predicate, params.timeout_ms);
  }
}

/**
 * Barrier over terminal job outcomes. Satisfied when `count` distinct jobs from `job_ids` reach one
 * of the `until` outcomes (`completed`/`failed`). A job that is BOTH listed and reaches a terminal
 * outcome counts once. Subscribes to `job.completed` and `job.failed` on the bus.
 */
async function waitJob(host: WaitHost, pred: JobPredicate, timeoutMs?: number): Promise<WaitResult> {
  // Prefer the jobs engine's BarrierTracker when mounted: it DRAINS ALREADY-RECORDED TERMINALS first,
  // so a `session.wait {job_ids:[…]}` issued AFTER the jobs already finished (the normal sequential
  // enqueue-then-wait flow) resolves immediately instead of hanging on future-only bus events. The
  // bus-subscription path below is the fallback for a daemon with no jobs engine mounted (e.g. tests).
  const barrier = host.jobBarrier();
  if (barrier !== null) {
    const r = await barrier.awaitJobIds(pred.job_ids, pred.count, timeoutMs);
    const satisfied = r.satisfied.map((d) => ({ event: `job.${d.state}`, job_id: d.job_id, task_uuid: d.task_uuid, verdict: d.verdict }));
    if (r.timed_out) return { satisfied, timed_out: true, pending: r.pending };
    return { satisfied };
  }

  const wanted = new Set(pred.job_ids);
  const wantCompleted = pred.until.includes("completed");
  const wantFailed = pred.until.includes("failed");
  const satisfied: unknown[] = [];
  const seen = new Set<string>();

  return new Promise<WaitResult>((resolve) => {
    const unsubs: Unsubscribe[] = [];
    let cancelTimer: (() => void) | null = null;
    const done = (timedOut: boolean) => {
      for (const u of unsubs) u();
      if (cancelTimer) cancelTimer();
      if (timedOut) {
        const pending = pred.job_ids.filter((id) => !seen.has(id));
        resolve({ satisfied, timed_out: true, pending });
      } else {
        resolve({ satisfied });
      }
    };
    const consider = (job_id: string, payload: unknown) => {
      if (!wanted.has(job_id) || seen.has(job_id)) return;
      seen.add(job_id);
      satisfied.push(payload);
      if (satisfied.length >= pred.count) done(false);
    };
    if (wantCompleted) {
      unsubs.push(host.bus.on("job.completed", (p: JobCompletedPayload) => consider(p.job_id, { event: "job.completed", ...p })));
    }
    if (wantFailed) {
      unsubs.push(host.bus.on("job.failed", (p: JobFailedPayload) => consider(p.job_id, { event: "job.failed", ...p })));
      // A gate-fail is a terminal failure outcome for the barrier too (mirrors the verdict, §1.1a).
      unsubs.push(host.bus.on("job.evidence_fail", (p) => consider(p.job_id, { event: "job.evidence_fail", ...p })));
    }
    if (pred.count <= 0) {
      // Degenerate barrier: already satisfied.
      done(false);
      return;
    }
    if (timeoutMs !== undefined) {
      cancelTimer = host.clock.setTimer(timeoutMs, () => done(true));
    }
  });
}

/**
 * Wait until `count` distinct agents from `agent_ids` reach `until_status`. Subscribes to
 * `agent.status_changed` (the spine) AND `agent.detected` (an agent may be first seen already at the
 * target status). Over the full four-value AgentStatus.
 */
function waitAgent(host: WaitHost, pred: AgentPredicate, timeoutMs?: number): Promise<WaitResult> {
  const wanted = new Set(pred.agent_ids);
  const satisfied: unknown[] = [];
  const seen = new Set<string>();

  return new Promise<WaitResult>((resolve) => {
    const unsubs: Unsubscribe[] = [];
    let cancelTimer: (() => void) | null = null;
    const done = (timedOut: boolean) => {
      for (const u of unsubs) u();
      if (cancelTimer) cancelTimer();
      if (timedOut) {
        const pending = pred.agent_ids.filter((id) => !seen.has(id));
        resolve({ satisfied, timed_out: true, pending });
      } else {
        resolve({ satisfied });
      }
    };
    const consider = (agent_id: string, status: string, payload: unknown) => {
      if (!wanted.has(agent_id) || seen.has(agent_id)) return;
      if (status !== pred.until_status) return;
      seen.add(agent_id);
      satisfied.push(payload);
      if (satisfied.length >= pred.count) done(false);
    };
    unsubs.push(
      host.bus.on("agent.status_changed", (p: AgentStatusChangedPayload) =>
        consider(p.agent_id, p.status, { event: "agent.status_changed", ...p }),
      ),
    );
    unsubs.push(
      host.bus.on("agent.detected", (p: AgentDetectedPayload) =>
        consider(p.agent_id, p.status, { event: "agent.detected", ...p }),
      ),
    );
    if (pred.count <= 0) {
      done(false);
      return;
    }
    if (timeoutMs !== undefined) {
      cancelTimer = host.clock.setTimer(timeoutMs, () => done(true));
    }
  });
}

/**
 * Satisfy a `pane_output` predicate through the `WaitScrapeProvider` seam. The detector's provider
 * performs the throttled `kitty @ get-text` read, REJECTS `is_viewer` panes (surfacing
 * `ViewerRejected`), and emits `pane.output_matched`. wait.ts returns that same event as the result —
 * it consumes, never emits, and never imports the detector.
 */
async function waitPaneOutput(host: WaitHost, pred: PaneOutputPredicate, timeoutMs?: number): Promise<WaitResult> {
  const provider = host.waitScrape();
  if (!provider) {
    throw new TallyError(
      "unsupported",
      "session.wait pane_output requires the detector (WaitScrapeProvider) to be mounted",
      { pane_id: pred.pane_id },
    );
  }
  const req = timeoutMs !== undefined
    ? { pane_id: pred.pane_id, regex: pred.regex, timeout_ms: timeoutMs }
    : { pane_id: pred.pane_id, regex: pred.regex };
  try {
    const result = await provider.awaitPaneOutput(req);
    // Return the `pane.output_matched`-shaped event verbatim as the satisfying result.
    return {
      satisfied: [
        {
          event: "pane.output_matched",
          pane_id: result.pane_id,
          session_id: result.session_id,
          matched_line: result.matched_line,
          read: result.read,
        },
      ],
    };
  } catch (err) {
    if (err instanceof ViewerRejected) throw err;
    if (err instanceof TallyError && err.code === "timeout") {
      return { satisfied: [], timed_out: true, pending: [pred.pane_id] };
    }
    throw err;
  }
}
