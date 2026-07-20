// tally daemon-core — the idle heartbeat driver (CLI-SURFACE §2.1, §2.3).
//
// Idle connections get a `heartbeat{ts, latest_seq}` every ~15s unless the subscriber set
// `include_heartbeat=false`. The heartbeat is a CONTROL frame: not replayable, no own `seq`, does not
// advance any cursor, never enters the replay ring (§2.3). This module owns the cadence timer only;
// the actual per-subscriber gating (`include_heartbeat`) lives in `Subscription.deliverControl`.
//
// Timing is driven through the injected `Clock` seam so tests advance it deterministically.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { HEARTBEAT_MS } from "../contracts/constants";
import type { Clock } from "../contracts/exec";
import type { HeartbeatPayload } from "../contracts/events";

/** What the heartbeat driver needs from the daemon: the current latest seq and a fan-out sink. */
export interface HeartbeatHost {
  latestSeq(): number;
  nowIso(): string;
  /** Fan a heartbeat control frame out to every subscriber that wants it. */
  emitHeartbeat(payload: HeartbeatPayload): void;
}

/**
 * The heartbeat driver. `start` arms a repeating timer at the configured cadence; each tick builds a
 * `heartbeat{ts, latest_seq}` and hands it to the host's fan-out. `stop` disarms it. Idempotent:
 * calling `start` twice replaces the timer.
 */
export class Heartbeat {
  private cancel: (() => void) | null = null;

  constructor(
    private readonly clock: Clock,
    private readonly host: HeartbeatHost,
    private readonly intervalMs: number = HEARTBEAT_MS,
  ) {}

  /** Whether the cadence timer is currently armed. */
  get running(): boolean {
    return this.cancel !== null;
  }

  start(): void {
    this.stop();
    this.cancel = this.clock.setInterval(this.intervalMs, () => this.tick());
  }

  stop(): void {
    if (this.cancel) {
      this.cancel();
      this.cancel = null;
    }
  }

  /** Fire one heartbeat immediately (the tick body; exposed for tests). */
  tick(): void {
    this.host.emitHeartbeat({ ts: this.host.nowIso(), latest_seq: this.host.latestSeq() });
  }
}
