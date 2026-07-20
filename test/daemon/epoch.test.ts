// daemon-core epoch: monotone across restarts, pls-generation primacy (PS#9/PS#21).

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { writeFileSync } from "node:fs";
import { bumpEpoch, readEpochCounter, writeEpochCounter } from "../../src/daemon/epoch";
import { makeTmpEnv, type TmpEnv } from "../helpers/tmp";

describe("epoch", () => {
  let tmp: TmpEnv;
  beforeEach(() => {
    tmp = makeTmpEnv();
  });
  afterEach(() => tmp.cleanup());

  test("genesis: absent counter reads 0, first bump yields 1", () => {
    expect(readEpochCounter(tmp.env)).toBe(0);
    const r = bumpEpoch(tmp.env);
    expect(r.epoch).toBe(1);
    expect(r.source).toBe("genesis");
    expect(readEpochCounter(tmp.env)).toBe(1);
  });

  test("bumps monotonically across simulated restarts", () => {
    const a = bumpEpoch(tmp.env);
    const b = bumpEpoch(tmp.env);
    const c = bumpEpoch(tmp.env);
    expect(a.epoch).toBe(1);
    expect(b.epoch).toBe(2);
    expect(c.epoch).toBe(3);
  });

  test("converges STRICTLY past a higher pls generation (issue #4 fence: never merely adopt)", () => {
    writeEpochCounter(tmp.env, 5);
    const r = bumpEpoch(tmp.env, 42);
    // Strictly greater, not equal: an epoch merely EQUAL to the last grant generation would leave
    // a zombie holding generation 42 unfenced by recover()'s strict `<` comparison.
    expect(r.epoch).toBe(43);
    expect(r.source).toBe("pls-generation");
    expect(readEpochCounter(tmp.env)).toBe(43);
    // Next boot without pls still moves forward.
    expect(bumpEpoch(tmp.env).epoch).toBe(44);
  });

  // issue #4: the shim's grant counter ($XDG_STATE_HOME/tally/pls-generation) and the daemon's
  // boot-fence counter ($XDG_STATE_HOME/tally/epoch) must form ONE monotone lease_epoch series.
  // Before the convergence, no boot path ever supplied a plsGeneration, so the two files ran as
  // independent counters (the observed boot fence 4/5/6 interleaved with grants 16…30 under the
  // single wire name).
  test("boot reads the shim's pls-generation file itself — one monotone series (issue #4)", () => {
    writeEpochCounter(tmp.env, 6); // the restart-fence counter (the observed 4/5/6 series)
    writeFileSync(tmp.plsGenerationPath, "30\n"); // the shim's grant counter (the observed 16…30 series)
    const r = bumpEpoch(tmp.env); // NO caller-supplied generation — exactly the production boot path
    expect(r.epoch).toBe(31); // strictly past every issued grant, not counter+1==7
    expect(r.source).toBe("pls-generation");
    expect(readEpochCounter(tmp.env)).toBe(31);
  });

  test("a corrupt pls-generation file is ignored (backstop counter still advances)", () => {
    writeEpochCounter(tmp.env, 6);
    writeFileSync(tmp.plsGenerationPath, "garbage\n");
    const r = bumpEpoch(tmp.env);
    expect(r.epoch).toBe(7);
    expect(r.source).toBe("counter-file");
  });

  test("ignores a pls generation not ahead of the counter (stays monotone)", () => {
    writeEpochCounter(tmp.env, 10);
    const r = bumpEpoch(tmp.env, 3); // stale generation
    expect(r.epoch).toBe(11);
    expect(r.source).toBe("counter-file");
  });

  test("corrupt counter file falls back to genesis", () => {
    writeEpochCounter(tmp.env, 7);
    // Overwrite with garbage.
    writeFileSync(tmp.epochPath, "not-a-number\n");
    expect(readEpochCounter(tmp.env)).toBe(0);
  });

  // issue #9: the daemon is the SOLE increment owner (nix/units.nix no longer runs a
  // separate ExecStartPre incrementer). Across successive boots, the epoch the daemon
  // ANNOUNCES must equal exactly what is PERSISTED to the counter file — never off by
  // the extra +1 a second incrementer would introduce — and each boot's epoch must be
  // strictly greater than the last.
  test("two successive daemon boots: announced epoch == persisted file, each strictly greater", () => {
    const first = bumpEpoch(tmp.env);
    expect(first.epoch).toBe(readEpochCounter(tmp.env));

    const second = bumpEpoch(tmp.env);
    expect(second.epoch).toBe(readEpochCounter(tmp.env));
    expect(second.epoch).toBeGreaterThan(first.epoch);
  });
});
