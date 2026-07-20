// test/witness/witness.test.ts
//
// The witness module M1.2 (ledger-as-truth). Covers the brief's demands:
//   - chain across a simulated daemon restart (one unbroken ledger-wide chain);
//   - torn-line discard (a partial trailing write is dropped by the JSON-parse-failure rule);
//   - tamper / truncate / reorder detection with the EXACT breaking seq;
//   - the model-id normalization table;
//   - fsync-per-line append semantics (each append is a complete, immediately-durable JSON line);
//   - the 5-field projection shape + canonical-GPU-seconds aggregation exclusion rule.
//
// Runs against the layer-0 testkit (test/helpers/tmp.ts) and the ledger fixtures
// (test/fixtures/ledger/{valid,tampered}.jsonl). Authored fresh for tally; no vendor/ code.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import {
  WitnessLedger,
  scanChainHead,
  buildRecord,
  computeHash,
  GENESIS_HEAD,
  normalizeModelId,
  parseRecord,
  canonicalGpuSeconds,
  toProjection,
  verifyLedgerFile,
  verifyRecords,
  type WitnessBody,
} from "../../src/witness/index.ts";
import {
  GENESIS_PREV_HASH,
  countsTowardCanonicalGpuSeconds,
  type WitnessRecord,
} from "../../src/contracts/index.ts";
import { makeTmpEnv, readLedger, type TmpEnv } from "../helpers/tmp.ts";

const FIXTURE_DIR = join(import.meta.dir, "..", "fixtures", "ledger");

/** A minimal well-formed heavy-unit body for the given dedup key. */
function body(overrides: Partial<WitnessBody> = {}): WitnessBody {
  return {
    task_uuid: "aaaa0000-0000-4000-8000-000000000001",
    transition_timestamp: "2026-07-09T12:00:00.000Z",
    verdict: "pass",
    exit_code: 0,
    artifact_content_hash: "sha256:" + "cc".repeat(32),
    gpu_seconds: 10,
    wall_clock: 11,
    attempt: 1,
    lease_epoch: 1,
    dedup_key: "ocr:paper-x",
    labor_class: "fresh",
    pool: "worker-gpu",
    charge: { unit: "gpu-seconds", amount: 10, class: "verifiable" },
    model: "vllm/qwen2-vl-ocr",
    ...overrides,
  };
}

describe("chain / hash", () => {
  test("buildRecord stamps seq/prev_hash/hash and verifies against contracts canonicalization", () => {
    const rec = buildRecord(body(), GENESIS_HEAD);
    expect(rec.seq).toBe(1);
    expect(rec.prev_hash).toBe(GENESIS_PREV_HASH);
    expect(rec.hash.startsWith("sha256:")).toBe(true);
    // The stored hash must equal the recompute over the cleared form.
    expect(computeHash(rec)).toBe(rec.hash);
  });

  test("chain links: prev_hash of line N equals hash of line N-1", () => {
    const first = buildRecord(body({ dedup_key: "a" }), GENESIS_HEAD);
    const second = buildRecord(body({ dedup_key: "b" }), { seq: first.seq, hash: first.hash });
    expect(second.seq).toBe(2);
    expect(second.prev_hash).toBe(first.hash);
    expect(computeHash(second)).toBe(second.hash);
  });

  test("trace_ref position preserved when present (still hash-stable)", () => {
    const rec = buildRecord(body({ trace_ref: "pi://trace/1" }), GENESIS_HEAD);
    expect(rec.trace_ref).toBe("pi://trace/1");
    expect(computeHash(rec)).toBe(rec.hash);
  });
});

describe("ledger append (fsync-per-line semantics)", () => {
  let tmp: TmpEnv;
  beforeEach(() => {
    tmp = makeTmpEnv();
  });
  afterEach(() => tmp.cleanup());

  test("open() resolves the XDG ledger path and starts at genesis", () => {
    const ledger = WitnessLedger.open(tmp.env);
    expect(ledger.filePath).toBe(tmp.ledgerPath);
    expect(ledger.chainHead).toEqual(GENESIS_HEAD);
    expect(ledger.nextSeq).toBe(1);
    ledger.close();
  });

  test("each append writes exactly one complete JSON line, immediately durable and single-line", () => {
    const ledger = WitnessLedger.open(tmp.env);
    const r1 = ledger.append(body({ dedup_key: "p1" }));
    const r2 = ledger.append(body({ dedup_key: "p2" }));
    expect(r1.seq).toBe(1);
    expect(r2.seq).toBe(2);
    // Durable immediately (fsync per line) — read the file back WITHOUT closing.
    const raw = readFileSync(tmp.ledgerPath, "utf8");
    const lines = raw.split("\n").filter((l) => l.length > 0);
    expect(lines.length).toBe(2);
    // No line carries an embedded newline (single-line JSON object guarantee).
    for (const l of lines) expect(l.includes("\n")).toBe(false);
    // Each line round-trips to the appended record.
    expect(JSON.parse(lines[0]!)).toEqual(r1 as unknown as Record<string, unknown>);
    expect(JSON.parse(lines[1]!)).toEqual(r2 as unknown as Record<string, unknown>);
    ledger.close();
  });

  test("appended lines form a valid chain end-to-end", () => {
    const ledger = WitnessLedger.open(tmp.env);
    for (let i = 0; i < 5; i++) ledger.append(body({ dedup_key: `p${i}` }));
    ledger.close();
    const report = verifyLedgerFile(tmp.ledgerPath);
    expect(report.ok).toBe(true);
    expect(report.records).toBe(5);
    expect(report.firstSeq).toBe(1);
    expect(report.lastSeq).toBe(5);
  });

  test("append after close throws", () => {
    const ledger = WitnessLedger.open(tmp.env);
    ledger.close();
    expect(() => ledger.append(body())).toThrow();
  });
});

describe("restart-surviving chain (recover chain head across daemon restart)", () => {
  let tmp: TmpEnv;
  beforeEach(() => {
    tmp = makeTmpEnv();
  });
  afterEach(() => tmp.cleanup());

  test("a second ledger opened on the same file continues the ONE chain", () => {
    // First daemon lifetime: append 3.
    const l1 = WitnessLedger.open(tmp.env);
    l1.append(body({ dedup_key: "a" }));
    l1.append(body({ dedup_key: "b" }));
    const last1 = l1.append(body({ dedup_key: "c" }));
    l1.close();

    // Simulated restart: reopen, head recovered from disk.
    const l2 = WitnessLedger.open(tmp.env);
    expect(l2.chainHead.seq).toBe(3);
    expect(l2.chainHead.hash).toBe(last1.hash);
    expect(l2.nextSeq).toBe(4);
    const next = l2.append(body({ dedup_key: "d" }));
    expect(next.seq).toBe(4);
    expect(next.prev_hash).toBe(last1.hash);
    l2.close();

    // The full ledger is one unbroken chain.
    const report = verifyLedgerFile(tmp.ledgerPath);
    expect(report.ok).toBe(true);
    expect(report.records).toBe(4);
    expect(report.lastSeq).toBe(4);
  });

  test("scanChainHead on an absent ledger returns genesis", () => {
    const scan = scanChainHead(join(tmp.root, "nope.jsonl"));
    expect(scan.head).toEqual(GENESIS_HEAD);
    expect(scan.records).toBe(0);
    expect(scan.tornTrailingDiscarded).toBe(false);
  });
});

describe("torn-line discard (partial trailing write)", () => {
  let tmp: TmpEnv;
  beforeEach(() => {
    tmp = makeTmpEnv();
  });
  afterEach(() => tmp.cleanup());

  test("a torn trailing line is discarded on open, chain continues from the last intact record", () => {
    const l1 = WitnessLedger.open(tmp.env);
    const good1 = l1.append(body({ dedup_key: "a" }));
    const good2 = l1.append(body({ dedup_key: "b" }));
    l1.close();

    // Simulate a crash mid-write: append a partial (unterminated, non-JSON) fragment.
    const path = tmp.ledgerPath;
    const torn = readFileSync(path, "utf8") + '{"task_uuid":"partial","seq":3,"prev_';
    writeFileSync(path, torn, "utf8");

    const scan = scanChainHead(path);
    expect(scan.tornTrailingDiscarded).toBe(true);
    expect(scan.records).toBe(2);
    expect(scan.head.seq).toBe(2);
    expect(scan.head.hash).toBe(good2.hash);

    // Reopen and append: the new line is seq 3 chained to the last INTACT record (good2).
    const l2 = WitnessLedger.open(tmp.env);
    expect(l2.nextSeq).toBe(3);
    const recovered = l2.append(body({ dedup_key: "c" }));
    expect(recovered.seq).toBe(3);
    expect(recovered.prev_hash).toBe(good2.hash);
    l2.close();

    // Chain is intact (the torn fragment was overwritten by the good seq-3 line).
    const report = verifyLedgerFile(path);
    expect(report.ok).toBe(true);
    expect(report.records).toBe(3);
    // Sanity: good1 still the genesis line.
    expect(readLedger(path)[0]!.hash).toBe(good1.hash);
  });
});

describe("verify — tamper / truncate / reorder detection with exact breaking seq", () => {
  test("the valid fixture verifies clean", () => {
    const report = verifyLedgerFile(join(FIXTURE_DIR, "valid.jsonl"));
    expect(report.ok).toBe(true);
    expect(report.records).toBe(4);
    expect(report.problems).toEqual([]);
  });

  test("the tampered fixture is caught at the exact breaking seq (2) as a hash mismatch", () => {
    const report = verifyLedgerFile(join(FIXTURE_DIR, "tampered.jsonl"));
    expect(report.ok).toBe(false);
    const hashProblem = report.problems.find((p) => p.kind === "hash-mismatch");
    expect(hashProblem).toBeDefined();
    expect(hashProblem!.seq).toBe(2);
  });

  test("truncation (a dropped middle line) is caught as a prev-hash-mismatch and a seq-gap", () => {
    const all = readLedger(join(FIXTURE_DIR, "valid.jsonl"));
    // Drop the second line (seq 2): now line seq 3's prev_hash points at a hash not present.
    const truncated = [all[0]!, all[2]!, all[3]!];
    const report = verifyRecords(truncated);
    expect(report.ok).toBe(false);
    const prevMismatch = report.problems.find((p) => p.kind === "prev-hash-mismatch");
    expect(prevMismatch).toBeDefined();
    expect(prevMismatch!.seq).toBe(3);
    // Completeness pass also flags the missing seq.
    const gap = report.problems.find((p) => p.kind === "seq-gap");
    expect(gap).toBeDefined();
    expect(gap!.seq).toBe(3);
  });

  test("reorder (two lines swapped) is caught by strict seq ordering", () => {
    const all = readLedger(join(FIXTURE_DIR, "valid.jsonl"));
    const reordered = [all[1]!, all[0]!, all[2]!, all[3]!]; // seq 2,1,3,4
    const report = verifyRecords(reordered);
    expect(report.ok).toBe(false);
    const orderProblem = report.problems.find((p) => p.kind === "seq-order");
    expect(orderProblem).toBeDefined();
    // seq 1 appears after seq 2 → the seq-order break is reported at seq 1.
    expect(orderProblem!.seq).toBe(1);
  });

  test("a byte-flipped single field breaks only that line's hash, at its seq", () => {
    const all = readLedger(join(FIXTURE_DIR, "valid.jsonl")) as unknown as WitnessRecord[];
    const flipped = all.map((r) => ({ ...r }));
    // Flip exit_code on seq 3 without recomputing hash.
    flipped[2]!.exit_code = 137;
    const report = verifyRecords(flipped);
    expect(report.ok).toBe(false);
    const hashProblem = report.problems.find((p) => p.kind === "hash-mismatch");
    expect(hashProblem!.seq).toBe(3);
  });

  test("an unparseable line is reported as a parse-error at its physical line number", () => {
    const report = verifyRecords([], new Map([[7, "{ not json"]]));
    expect(report.ok).toBe(false);
    expect(report.problems[0]!.kind).toBe("parse-error");
    expect(report.problems[0]!.line).toBe(7);
  });

  test("verifyLedgerFile on an absent path is a clean empty ledger", () => {
    const report = verifyLedgerFile("/nonexistent/tally/witness.jsonl");
    expect(report.ok).toBe(true);
    expect(report.records).toBe(0);
  });
});

describe("model-id normalization table", () => {
  const cases: Array<[string | null | undefined, string | null]> = [
    // fully-qualified pass-through
    ["anthropic/claude-opus-4", "anthropic/claude-opus-4"],
    ["vllm/qwen2-vl-ocr", "vllm/qwen2-vl-ocr"],
    ["openai/gpt-4o", "openai/gpt-4o"],
    // bare harness families → provider prefix
    ["claude-opus-4", "anthropic/claude-opus-4"],
    ["claude-3-5-sonnet", "anthropic/claude-3-5-sonnet"],
    ["gpt-4o", "openai/gpt-4o"],
    ["o3-mini", "openai/o3-mini"],
    ["gemini-2.0-flash", "google/gemini-2.0-flash"],
    // case-insensitive family match, original casing preserved
    ["Claude-Opus-4", "anthropic/Claude-Opus-4"],
    // shell runs / empty → null
    [null, null],
    [undefined, null],
    ["", null],
    ["   ", null],
    // unknown bare family preserved verbatim (never fabricate a provider)
    ["mystery-model", "mystery-model"],
    // whitespace trimmed
    ["  claude-haiku  ", "anthropic/claude-haiku"],
  ];
  for (const [input, expected] of cases) {
    test(`normalizeModelId(${JSON.stringify(input)}) === ${JSON.stringify(expected)}`, () => {
      expect(normalizeModelId(input)).toBe(expected);
    });
  }
});

describe("projection shape + canonical-GPU-seconds exclusion", () => {
  test("toProjection yields exactly the 5-field form", () => {
    const rec = buildRecord(body({ gpu_seconds: 42.5, artifact_content_hash: "sha256:" + "ab".repeat(32) }), GENESIS_HEAD);
    const proj = toProjection(rec, ["artifact-exists", "hash-ok", "exit-0"]);
    expect(Object.keys(proj).sort()).toEqual(
      ["artifact_hash", "evidence_checks", "exit_code", "gpu_seconds", "task_uuid"].sort(),
    );
    expect(proj.task_uuid).toBe(rec.task_uuid);
    expect(proj.gpu_seconds).toBe(42.5);
    expect(proj.artifact_hash).toBe(rec.artifact_content_hash);
    expect(proj.exit_code).toBe(0);
    expect(proj.evidence_checks).toEqual(["artifact-exists", "hash-ok", "exit-0"]);
  });

  test("canonical GPU-seconds excludes reused, recovered, and clean-exit-no-artifact lines", () => {
    const fresh = buildRecord(body({ labor_class: "fresh", gpu_seconds: 40, verdict: "pass" }), GENESIS_HEAD);
    const reused = buildRecord(body({ labor_class: "reused", gpu_seconds: 0 }), { seq: 1, hash: fresh.hash });
    const recovered = buildRecord(body({ labor_class: "recovered", gpu_seconds: 30 }), { seq: 2, hash: reused.hash });
    const gateFail = buildRecord(
      body({ labor_class: "fresh", gpu_seconds: 5, verdict: "clean-exit-no-artifact", artifact_content_hash: null }),
      { seq: 3, hash: recovered.hash },
    );
    // Only `fresh` returns true; only the fresh pass line contributes 40.
    expect(countsTowardCanonicalGpuSeconds(fresh)).toBe(true);
    expect(countsTowardCanonicalGpuSeconds(reused)).toBe(false);
    expect(countsTowardCanonicalGpuSeconds(recovered)).toBe(false);
    expect(countsTowardCanonicalGpuSeconds(gateFail)).toBe(false);
    expect(canonicalGpuSeconds([fresh, reused, recovered, gateFail])).toBe(40);
  });

  test("a null-gpu (cloud) fresh line contributes nothing to canonical GPU-seconds", () => {
    const cloud = buildRecord(body({ labor_class: "fresh", gpu_seconds: null, pool: null }), GENESIS_HEAD);
    expect(canonicalGpuSeconds([cloud])).toBe(0);
  });
});

describe("parseRecord validation", () => {
  test("accepts a well-formed fixture record", () => {
    const rec = readLedger(join(FIXTURE_DIR, "valid.jsonl"))[0]!;
    const res = parseRecord(rec);
    expect(res.ok).toBe(true);
  });

  test("rejects a record missing seq", () => {
    const rec = readLedger(join(FIXTURE_DIR, "valid.jsonl"))[0]!;
    const { seq, ...noSeq } = rec as Record<string, unknown>;
    void seq;
    const res = parseRecord(noSeq);
    expect(res.ok).toBe(false);
    if (!res.ok) expect(res.reason).toContain("seq");
  });

  test("rejects a bad labor_class", () => {
    const rec = { ...(readLedger(join(FIXTURE_DIR, "valid.jsonl"))[0]!), labor_class: "bogus" };
    const res = parseRecord(rec);
    expect(res.ok).toBe(false);
  });

  test("rejects a non-object line", () => {
    expect(parseRecord("nope").ok).toBe(false);
    expect(parseRecord(null).ok).toBe(false);
    expect(parseRecord([1, 2]).ok).toBe(false);
  });
});
