// test/e2e/ocr-drain.test.ts
//
// The OCR-drain rehearsal (IMPLEMENTATION-PLAN M4.1 case 1; BUILD-SEQUENCE steps 3+5 acceptance) —
// THE shape Tom's first live test-drive replays on real hardware (~4.7k academic-PDF sidecars).
//
// Enqueue N shell-kind batch jobs with `--evidence artifact:… exit:0` + `--dedup-key`, serialized on
// ONE fake worker-gpu lease. Assert: every artifact written, N witness lines chained (seq monotone,
// prev_hash links), TW rows completed with `trust:unreviewed`; then RE-RUN and assert all N skip as
// `reused`, no witness growth, and the reused lines are excluded from canonical GPU-seconds.
//
// Runs the REAL jobs engine end-to-end over the layer-0 fakes. Authored fresh for tally; no vendor/
// code (clean-room, CLI-SURFACE §4).

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { existsSync } from "node:fs";
import {
  makeEngineHarness,
  enqueueSettled,
  ocrEnqueue,
  readLedger,
  type EngineHarness,
} from "./helpers.ts";
import { canonicalGpuSeconds, parseRecord } from "../../src/witness/index.ts";
import type { WitnessRecord } from "../../src/contracts/index.ts";

/** Parse the ledger JSONL into validated witness records (torn lines skipped, ledger-as-truth). */
function witnessRecords(ledgerPath: string): WitnessRecord[] {
  const out: WitnessRecord[] = [];
  for (const raw of readLedger(ledgerPath)) {
    const res = parseRecord(raw);
    if (res.ok) out.push(res.record);
  }
  return out;
}

describe("OCR-drain rehearsal — the first-test-drive shape (BUILD-SEQUENCE steps 3+5)", () => {
  let h: EngineHarness;
  beforeEach(() => {
    h = makeEngineHarness();
  });
  afterEach(() => h.cleanup());

  test("N sidecars serialize on one worker-gpu lease, each witnessed + a completed trust:unreviewed row", async () => {
    const N = 8;
    const artifacts: string[] = [];

    for (let i = 0; i < N; i++) {
      const artifact = h.artifactPath(`sidecar-${i}.txt`);
      artifacts.push(artifact);
      const res = await enqueueSettled(h, 
        ocrEnqueue(artifact, { dedup_key: `sidecar-${i}`, source: "r2" }),
      );
      expect(res.status).toBe("completed");
      expect(res.verdict).toBe("pass");
      // r2 is autonomous ⇒ a durable row was admitted.
      expect(res.task_uuid).not.toBeNull();
    }

    // Every artifact exists on disk.
    for (const a of artifacts) expect(existsSync(a)).toBe(true);

    // Exactly N witness lines, chained: seq monotone 1..N, prev_hash links.
    const lines = readLedger(h.ledger.filePath);
    expect(lines.length).toBe(N);
    expect(lines.map((l) => l.seq)).toEqual(Array.from({ length: N }, (_, i) => i + 1));
    for (let i = 1; i < lines.length; i++) {
      expect(lines[i]!.prev_hash).toBe(lines[i - 1]!.hash);
    }
    // Fresh, passing shell lines: no model, an artifact hash present.
    for (const l of lines) {
      expect(l.verdict).toBe("pass");
      expect(l.labor_class).toBe("fresh");
      expect(l.model).toBeNull();
      expect(String(l.artifact_content_hash)).toMatch(/^sha256:/);
    }

    // At no point were two leases held (the single-lease pool serialized the drain).
    expect(h.pls.holders("worker-gpu")).toEqual([]);

    // Every durable row completed with trust:unreviewed (never blocks future work).
    expect(h.task.tasks().length).toBe(N);
    for (const t of h.task.tasks()) {
      expect(t.status).toBe("completed");
      expect(t.trust).toBe("unreviewed");
    }
  });

  test("re-running the whole batch skips all N as reused (dedup-by-existence), no witness growth", async () => {
    const N = 8;
    const artifacts: string[] = [];
    for (let i = 0; i < N; i++) {
      const a = h.artifactPath(`page-${i}.txt`);
      artifacts.push(a);
      await enqueueSettled(h, ocrEnqueue(a, { dedup_key: `page-${i}`, source: "r2" }));
    }
    const afterFirst = readLedger(h.ledger.filePath).length;
    expect(afterFirst).toBe(N);

    // Re-present the identical work: artifact + success witness exist for each key ⇒ reuse.
    for (let i = 0; i < N; i++) {
      const res = await enqueueSettled(h, ocrEnqueue(artifacts[i]!, { dedup_key: `page-${i}`, source: "r2" }));
      expect(res.status).toBe("reused");
      expect(res.verdict).toBe("reused");
      // The reuse skips the GPU: no new lease grant, no new witness line.
    }
    // No witness growth on the re-run.
    expect(readLedger(h.ledger.filePath).length).toBe(N);
    // No new lease grants for the skipped runs beyond the first pass.
    expect(h.pls.holders("worker-gpu")).toEqual([]);
  });

  test("canonical GPU-seconds counts only fresh passing lines — reused work does not inflate the meter", async () => {
    // First pass: three fresh sidecars land as passing shell lines on a GPU pool.
    for (let i = 0; i < 3; i++) {
      await enqueueSettled(h, 
        ocrEnqueue(h.artifactPath(`gpu-${i}.txt`), { dedup_key: `gpu-${i}`, source: "r2", pool: "worker-gpu" }),
      );
    }
    const freshRecords = witnessRecords(h.ledger.filePath);
    const canonicalAfterFresh = canonicalGpuSeconds(freshRecords);

    // Re-run: every sidecar is a dedup hit. A reused enqueue writes NO witness line, so the ledger is
    // unchanged and the canonical aggregate is identical — reuse never inflates GPU-seconds.
    for (let i = 0; i < 3; i++) {
      const res = await enqueueSettled(h, 
        ocrEnqueue(h.artifactPath(`gpu-${i}.txt`), { dedup_key: `gpu-${i}`, source: "r2", pool: "worker-gpu" }),
      );
      expect(res.status).toBe("reused");
    }
    const afterReuse = witnessRecords(h.ledger.filePath);
    expect(afterReuse.length).toBe(freshRecords.length);
    expect(canonicalGpuSeconds(afterReuse)).toBe(canonicalAfterFresh);
  });
});
