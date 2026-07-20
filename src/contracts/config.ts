// tally — the daemon/CLI config shape (IMPLEMENTATION-PLAN §3 Config, M3.3 module surface;
// SPEC "Module option surface"). Rendered by the nix home-manager module to
// `$XDG_CONFIG_HOME/tally/config.json`, read at boot. This file owns the shape + a hand-rolled
// loader with defaults, so daemon and CLI agree on every key.

import { ValidationError } from "./errors";
import { HEARTBEAT_MS } from "./constants";
import type { Pool } from "./job";

/** The role of this host (SPEC "Module option surface"): the daemon runs only on `conductor`. */
export type Role = "conductor" | "receiver";

/** One pls pool's declaration (IMPLEMENTATION-PLAN M1.5 pools.ts; tally owns pool config, PS#5). */
export interface PoolConfig {
  name: Pool;
  /** Broker address (over TB3/tailnet); never a hardcoded hostname (DECISIONS Q9). */
  broker: string;
  /** Priority ordering hint — lower is served first when both are eligible. */
  priority: number;
  /** Single-lease-per-pool capacity; v0 is always 1 (PS#5 `PLS_CAPACITY=1`). */
  capacity: number;
}

/** gh intake config — wired but OFF by default (IMPLEMENTATION-PLAN M2.4, M3.3; DECISIONS Q8). */
export interface IntakeGhConfig {
  enable: boolean;
  sources: string[];
}

/**
 * Detector cadence overrides (IMPLEMENTATION-PLAN §1 flag 4 — provisional, re-rulable). Milliseconds.
 * `working_poll_ms` while a pane's agent is `working`; `idle_poll_ms` otherwise.
 */
export interface DetectorConfig {
  working_poll_ms: number;
  idle_poll_ms: number;
}

/** The whole tally config (rendered by the nix module; IMPLEMENTATION-PLAN §3 Config). */
export interface TallyConfig {
  role: Role;
  /** Where clients reach the daemon — pure configuration, no hostname frozen (DECISIONS Q9). */
  conductorHost: string;
  /**
   * zmx session-name globs discovery is scoped to (IMPLEMENTATION-PLAN M3.3 `sessions`, risk 11).
   * `[]` = observe ALL enumerated sessions.
   */
  sessions: string[];
  pools: PoolConfig[];
  intake: { gh: IntakeGhConfig };
  detector: DetectorConfig;
  /** Informational binary semver surfaced in the snapshot's `daemon_version`. */
  daemonVersion: string;
  /** Idle-connection heartbeat cadence in ms. */
  heartbeatMs: number;
}

/** The compiled-in defaults — a valid conductor config with the two GPU pools and gh intake OFF. */
export function defaultConfig(): TallyConfig {
  return {
    role: "conductor",
    conductorHost: "localhost",
    sessions: [],
    pools: [
      { name: "worker-gpu", broker: "localhost", priority: 0, capacity: 1 },
      { name: "controller-gpu", broker: "localhost", priority: 1, capacity: 1 },
    ],
    intake: { gh: { enable: false, sources: [] } },
    detector: { working_poll_ms: 2000, idle_poll_ms: 10000 },
    daemonVersion: "0.1.0",
    heartbeatMs: HEARTBEAT_MS,
  };
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function reqString(v: unknown, path: string): string {
  if (typeof v !== "string") throw new ValidationError(`${path} must be a string`, path);
  return v;
}

function reqNumber(v: unknown, path: string): number {
  if (typeof v !== "number" || !Number.isFinite(v)) {
    throw new ValidationError(`${path} must be a finite number`, path);
  }
  return v;
}

function reqBool(v: unknown, path: string): boolean {
  if (typeof v !== "boolean") throw new ValidationError(`${path} must be a boolean`, path);
  return v;
}

function reqStringArray(v: unknown, path: string): string[] {
  if (!Array.isArray(v)) throw new ValidationError(`${path} must be an array`, path);
  return v.map((x, i) => reqString(x, `${path}[${i}]`));
}

/**
 * Hand-rolled config loader (IMPLEMENTATION-PLAN: hand-rolled validation, no zod). Merges a parsed
 * JSON object over the compiled defaults, validating every present key. Unknown keys are ignored
 * (forward-compat). Missing keys fall back to the default — the config file is a partial override.
 */
export function loadConfig(raw: unknown): TallyConfig {
  const cfg = defaultConfig();
  if (raw === undefined || raw === null) return cfg;
  if (!isObject(raw)) throw new ValidationError("config must be a JSON object", "$");

  if (raw.role !== undefined) {
    const role = reqString(raw.role, "role");
    if (role !== "conductor" && role !== "receiver") {
      throw new ValidationError(`role must be "conductor" or "receiver"`, "role");
    }
    cfg.role = role;
  }
  if (raw.conductorHost !== undefined) cfg.conductorHost = reqString(raw.conductorHost, "conductorHost");
  if (raw.sessions !== undefined) cfg.sessions = reqStringArray(raw.sessions, "sessions");
  if (raw.daemonVersion !== undefined) cfg.daemonVersion = reqString(raw.daemonVersion, "daemonVersion");
  if (raw.heartbeatMs !== undefined) cfg.heartbeatMs = reqNumber(raw.heartbeatMs, "heartbeatMs");

  if (raw.pools !== undefined) {
    if (!Array.isArray(raw.pools)) throw new ValidationError("pools must be an array", "pools");
    cfg.pools = raw.pools.map((p, i) => {
      const path = `pools[${i}]`;
      if (!isObject(p)) throw new ValidationError(`${path} must be an object`, path);
      return {
        name: reqString(p.name, `${path}.name`),
        broker: reqString(p.broker, `${path}.broker`),
        priority: reqNumber(p.priority, `${path}.priority`),
        capacity: reqNumber(p.capacity, `${path}.capacity`),
      };
    });
  }

  if (raw.intake !== undefined) {
    if (!isObject(raw.intake)) throw new ValidationError("intake must be an object", "intake");
    if (raw.intake.gh !== undefined) {
      const gh = raw.intake.gh;
      if (!isObject(gh)) throw new ValidationError("intake.gh must be an object", "intake.gh");
      cfg.intake.gh = {
        enable: gh.enable !== undefined ? reqBool(gh.enable, "intake.gh.enable") : cfg.intake.gh.enable,
        sources: gh.sources !== undefined ? reqStringArray(gh.sources, "intake.gh.sources") : cfg.intake.gh.sources,
      };
    }
  }

  if (raw.detector !== undefined) {
    const d = raw.detector;
    if (!isObject(d)) throw new ValidationError("detector must be an object", "detector");
    if (d.working_poll_ms !== undefined) cfg.detector.working_poll_ms = reqNumber(d.working_poll_ms, "detector.working_poll_ms");
    if (d.idle_poll_ms !== undefined) cfg.detector.idle_poll_ms = reqNumber(d.idle_poll_ms, "detector.idle_poll_ms");
  }

  // Cross-field assertion (SPEC "Module option surface"): conductorHost required when enabled.
  if (cfg.role === "conductor" && cfg.conductorHost.trim() === "") {
    throw new ValidationError("conductorHost is required for a conductor role", "conductorHost");
  }

  return cfg;
}
