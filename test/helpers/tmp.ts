// test/helpers/tmp.ts
//
// Tmp-dir + XDG-env scaffolding for tally tests, plus witness-ledger helpers.
// Every module that touches the filesystem (witness ledger, epoch counter,
// events/ drop dir, config.json, taskchampion store) resolves its paths from
// XDG_* vars; this helper stamps an isolated tmp tree with those vars set so a
// test never touches the operator's real state.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { mkdtempSync, rmSync, mkdirSync, writeFileSync, readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHash } from "node:crypto";

/** The XDG base dirs tally reads, all rooted under one tmp tree. */
export interface TmpEnv {
  /** The tmp root; everything lives under here. */
  readonly root: string;
  readonly XDG_RUNTIME_DIR: string;
  readonly XDG_STATE_HOME: string;
  readonly XDG_DATA_HOME: string;
  readonly XDG_CONFIG_HOME: string;
  /** Canonical derived tally paths (mirror src/contracts/paths.ts). */
  readonly socketPath: string;
  readonly ledgerPath: string;
  readonly epochPath: string;
  readonly plsGenerationPath: string;
  readonly eventsDir: string;
  readonly configPath: string;
  /** An env object suitable for spreading into ExecOptions.env / process.env. */
  readonly env: Record<string, string>;
  /** Recursively remove the tmp tree. */
  cleanup(): void;
}

/**
 * Create an isolated tmp tree with all XDG dirs and the tally sub-directories
 * (`tally/`) pre-created, so a module can write its socket/ledger/epoch/events
 * immediately. Call `cleanup()` in an afterEach.
 */
export function makeTmpEnv(): TmpEnv {
  const root = mkdtempSync(join(tmpdir(), "tally-test-"));
  const runtime = join(root, "runtime");
  const state = join(root, "state");
  const data = join(root, "data");
  const config = join(root, "config");
  for (const d of [runtime, state, data, config]) {
    mkdirSync(join(d, "tally"), { recursive: true });
  }
  const eventsDir = join(state, "tally", "events");
  mkdirSync(join(eventsDir, "done"), { recursive: true });
  mkdirSync(join(eventsDir, "rejected"), { recursive: true });

  const env: Record<string, string> = {
    XDG_RUNTIME_DIR: runtime,
    XDG_STATE_HOME: state,
    XDG_DATA_HOME: data,
    XDG_CONFIG_HOME: config,
  };

  return {
    root,
    XDG_RUNTIME_DIR: runtime,
    XDG_STATE_HOME: state,
    XDG_DATA_HOME: data,
    XDG_CONFIG_HOME: config,
    socketPath: join(runtime, "tally", "tally.sock"),
    ledgerPath: join(data, "tally", "witness.jsonl"),
    epochPath: join(state, "tally", "epoch"),
    plsGenerationPath: join(state, "tally", "pls-generation"),
    eventsDir,
    configPath: join(config, "tally", "config.json"),
    env,
    cleanup() {
      rmSync(root, { recursive: true, force: true });
    },
  };
}

/**
 * Apply the tmp env's XDG vars to the current `process.env` and return a
 * restore function. Use when a module reads `process.env` directly rather than
 * accepting an injected env.
 */
export function withEnv(env: Record<string, string>): () => void {
  const prev: Record<string, string | undefined> = {};
  for (const [k, v] of Object.entries(env)) {
    prev[k] = process.env[k];
    process.env[k] = v;
  }
  return () => {
    for (const [k, v] of Object.entries(prev)) {
      if (v === undefined) delete process.env[k];
      else process.env[k] = v;
    }
  };
}

/** sha256 hex of a UTF-8 string. */
export function sha256Hex(input: string): string {
  return createHash("sha256").update(input, "utf8").digest("hex");
}

/**
 * The tally witness hash-input rule (SPEC "Per-line hash chain", jul9): the
 * canonical hash is `"sha256:" + hex(sha256(the line's JSON with the `hash`
 * field cleared))`. "Cleared" = the `hash` key present but set to the empty
 * string, so the field position is stable. This helper reproduces that rule so
 * fixtures and witness tests agree byte-for-byte.
 */
export function witnessLineHash(record: Record<string, unknown>): string {
  const cleared = { ...record, hash: "" };
  return "sha256:" + sha256Hex(JSON.stringify(cleared));
}

/**
 * Serialize a single witness record to its on-disk JSON line, computing `hash`
 * over the cleared form. `prev_hash` and `seq` must already be set on `record`.
 */
export function witnessLine(record: Record<string, unknown>): string {
  const hash = witnessLineHash(record);
  return JSON.stringify({ ...record, hash });
}

/**
 * Write an array of already-serialized JSONL lines to a ledger path (creating
 * parent dirs), each terminated with LF. Returns the path.
 */
export function writeLedger(path: string, lines: readonly string[]): string {
  mkdirSync(join(path, ".."), { recursive: true });
  writeFileSync(path, lines.map((l) => l + "\n").join(""), "utf8");
  return path;
}

/** Read a JSONL ledger back into parsed objects (blank lines skipped). */
export function readLedger(path: string): Record<string, unknown>[] {
  if (!existsSync(path)) return [];
  return readFileSync(path, "utf8")
    .split("\n")
    .filter((l) => l.trim().length > 0)
    .map((l) => JSON.parse(l) as Record<string, unknown>);
}

/**
 * Build a well-formed, correctly-chained witness ledger from a list of partial
 * records (each missing `seq`/`prev_hash`/`hash`). The genesis `prev_hash` is
 * the all-zero digest, matching the witness module's chain root. Returns the
 * serialized lines.
 */
export function buildChain(records: readonly Record<string, unknown>[]): string[] {
  const GENESIS = "sha256:" + "0".repeat(64);
  const lines: string[] = [];
  let prevHash = GENESIS;
  records.forEach((rec, i) => {
    const withChain = { ...rec, seq: i + 1, prev_hash: prevHash };
    const hash = witnessLineHash(withChain);
    lines.push(JSON.stringify({ ...withChain, hash }));
    prevHash = hash;
  });
  return lines;
}
