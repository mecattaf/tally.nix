// test/tw/tw.test.ts
//
// The TaskChampion veneer module (M1.3) against the fake `task` binary. Covers the brief's demands:
//   - UDA bootstrap idempotence (a second bootstrap issues ZERO `task config` writes);
//   - the durable-row admission-test matrix (orchestrator ⇒ no row; every other source ⇒ row);
//   - import/export round-trip (a built row survives import→export byte-faithfully, foreign
//     attributes passed through untouched — merge-not-clobber);
//   - prev_* shadow derivation (capture-before-mutate yields the correct prev_status/prev_state,
//     omitting unchanged fields);
//   - veneer discipline (a machine-state-shaped field can NEVER reach a TW row).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { describe, expect, test } from "bun:test";
import { FakeExec } from "../helpers/exec-fakes.ts";
import { FakeTask } from "../helpers/fake-task.ts";
import type { Clock } from "../../src/contracts/exec.ts";
import {
  TALLY_UDAS,
  TALLY_UDA_NAMES,
  admitsDurableRow,
  type Source,
  type TaskRow,
} from "../../src/contracts/index.ts";

import {
  TaskChampion,
  TaskClient,
  RC_OVERRIDES,
  bootstrapUdas,
  allUdaConfigPairs,
  isBootstrapped,
  admits,
  admissionForEnqueue,
  buildRow,
  completeRow,
  cancelRow,
  setTrust,
  overlayManaged,
  assertVeneerClean,
  FORBIDDEN_ROW_FIELDS,
  capture,
  derive,
  hasShadow,
  rowState,
} from "../../src/tw/index.ts";

/** A deterministic clock so timestamps in tests are stable. */
function fixedClock(iso = "2026-07-09T12:00:00.000Z"): Clock {
  let t = Date.parse(iso);
  return {
    now: () => t,
    nowIso: () => new Date(t).toISOString(),
    async sleep() {},
    setTimer: () => () => {},
    setInterval: () => () => {},
  };
}

/** Wire a FakeExec + FakeTask and return the veneer facade + the store + the exec recorder. */
function harness(seed?: (t: FakeTask) => void) {
  const exec = new FakeExec();
  const task = new FakeTask();
  if (seed) seed(task);
  task.install(exec);
  const tw = new TaskChampion({ exec, clock: fixedClock() });
  return { exec, task, tw };
}

describe("client — shell-out surface", () => {
  test("every invocation carries the rc overrides", async () => {
    const { exec, tw } = harness();
    await tw.client.export();
    const call = exec.lastCall("task")!;
    for (const rc of RC_OVERRIDES) {
      expect(call.argv).toContain(rc);
    }
    expect(call.argv).toContain("export");
  });

  test("export parses the JSON array; import upserts by uuid; round-trips", async () => {
    const { task, tw } = harness();
    const client = tw.client;
    const row: TaskRow = {
      uuid: "11111111-1111-1111-1111-111111111111",
      description: "OCR paper 42",
      status: "pending",
      priority: "M",
      source: "r2",
      agent: "shell",
      labor_class: "fresh",
      dedup_key: "paper-42",
    };
    await client.importOne(row);
    const back = await client.exportOne(row.uuid);
    expect(back?.uuid).toBe(row.uuid);
    expect(back?.description).toBe("OCR paper 42");
    expect(back?.dedup_key).toBe("paper-42");
    expect(back?.source).toBe("r2");
    // The store actually holds it.
    expect(task.get(row.uuid)?.description).toBe("OCR paper 42");
  });

  test("a non-zero task exit becomes a TaskShellError", async () => {
    const exec = new FakeExec();
    exec.register("task", () => ({ code: 3, stdout: "", stderr: "boom" }));
    const client = new TaskClient({ exec });
    await expect(client.export()).rejects.toThrow(/exited 3/);
  });

  test("empty export stdout yields an empty row list", async () => {
    const exec = new FakeExec();
    exec.register("task", () => ({ code: 0, stdout: "\n", stderr: "" }));
    const client = new TaskClient({ exec });
    expect(await client.export()).toEqual([]);
  });
});

describe("udas — bootstrap idempotence", () => {
  test("first bootstrap writes every UDA config key; second writes none", async () => {
    const { exec, task, tw } = harness();

    const first = await tw.bootstrap();
    // Every declared key was written.
    const expectedKeys = allUdaConfigPairs().map(([k]) => k);
    expect(first.written.sort()).toEqual([...expectedKeys].sort());
    expect(first.skipped).toEqual([]);

    // Every tally UDA is now registered in the fake store.
    for (const name of TALLY_UDA_NAMES) {
      expect(task.hasUda(name)).toBe(true);
    }

    // Count the config writes issued by the first bootstrap.
    const configWritesAfterFirst = exec
      .callsFor("task")
      .filter((c) => c.argv.includes("config") && c.argv.length > RC_OVERRIDES.length + 2).length;
    expect(configWritesAfterFirst).toBe(expectedKeys.length);

    exec.clearCalls();

    // Second bootstrap: all skipped, ZERO config writes.
    const second = await tw.bootstrap();
    expect(second.written).toEqual([]);
    expect(second.skipped.sort()).toEqual([...expectedKeys].sort());
    const configWrites = exec
      .callsFor("task")
      .filter((c) => {
        const idx = c.argv.indexOf("config");
        return idx !== -1 && c.argv.length > idx + 1; // `config <name> <value>` — a write, not a dump
      }).length;
    expect(configWrites).toBe(0);
  });

  test("enumerated UDAs render their .values; typed UDAs render their .type", async () => {
    const pairs = allUdaConfigPairs();
    const map = new Map(pairs);
    // `trust` is enumerated.
    expect(map.get("uda.trust.type")).toBe("string");
    expect(map.get("uda.trust.values")).toBe("unreviewed,reviewed,recalled");
    // `lease_epoch` is numeric with no values.
    expect(map.get("uda.lease_epoch.type")).toBe("numeric");
    expect(map.has("uda.lease_epoch.values")).toBe(false);
    // `labor_class` enumerated.
    expect(map.get("uda.labor_class.values")).toBe("fresh,recovered,reused");
  });

  test("isBootstrapped reflects a fully-provisioned config dump", async () => {
    const empty: Record<string, string> = {};
    expect(isBootstrapped(empty)).toBe(false);
    const full = Object.fromEntries(allUdaConfigPairs());
    expect(isBootstrapped(full)).toBe(true);
  });

  test("bootstrapUdas re-writes a key whose value drifted", async () => {
    const { exec, tw } = harness((t) => {
      // Pre-seed trust.type with a WRONG value so bootstrap must correct it.
      t.config["uda.trust.type"] = "numeric";
    });
    const res = await bootstrapUdas(tw.client);
    expect(res.written).toContain("uda.trust.type");
    expect(exec.callsFor("task").some((c) => c.argv.includes("uda.trust.type"))).toBe(true);
  });
});

describe("rows — durable-row admission matrix", () => {
  const nonOrchestrator: Source[] = ["r2", "gh", "calendar", "manual"];

  test("orchestrator source ⇒ no row (task_uuid null)", () => {
    const input = admissionForEnqueue({ source: "orchestrator" });
    expect(input.liveOrchestratorSpawned).toBe(true);
    expect(admits(input)).toBe(false);
    expect(admitsDurableRow(input)).toBe(false);
  });

  test.each(nonOrchestrator)("%s source ⇒ durable row", (source) => {
    const input = admissionForEnqueue({ source });
    expect(input.liveOrchestratorSpawned).toBe(false);
    expect(admits(input)).toBe(true);
  });

  test("an orchestrator unit can be forced durable via overrides (standing drain row)", () => {
    const input = admissionForEnqueue(
      { source: "orchestrator" },
      { liveOrchestratorSpawned: false, crashSurvivable: true },
    );
    expect(admits(input)).toBe(true);
  });

  test("the facade admits() mirrors the predicate", () => {
    const { tw } = harness();
    expect(tw.admits({ source: "orchestrator" })).toBe(false);
    expect(tw.admits({ source: "r2" })).toBe(true);
  });
});

describe("rows — build / complete / cancel / trust", () => {
  const now = () => "2026-07-09T12:00:00.000Z";

  test("buildRow maps priority to native H|M|L, mirrors priority_class, starts pending, no trust", () => {
    const row = buildRow(
      {
        uuid: "aaaa",
        description: "OCR paper 7",
        priority: "high",
        source: "r2",
        kind: "shell",
        dedup_key: "paper-7",
        pool: "worker-gpu",
      },
      now,
    );
    expect(row.priority).toBe("H");
    expect(row.priority_class).toBe("high");
    expect(row.status).toBe("pending");
    expect(row.labor_class).toBe("fresh");
    expect(row.agent).toBe("shell");
    // trust is UNSET at enqueue — written only at completion.
    expect(row.trust).toBeUndefined();
  });

  test("buildRow rejects cwd AND worktree together", () => {
    expect(() =>
      buildRow(
        { uuid: "x", description: "d", priority: "low", source: "manual", kind: "pi", cwd: "/a", worktree: "b" },
        now,
      ),
    ).toThrow(/cwd XOR worktree/);
  });

  test("completeRow flips to completed and writes trust:unreviewed (never blocks future work)", () => {
    const row = buildRow(
      { uuid: "bbbb", description: "d", priority: "medium", source: "gh", kind: "claude-code" },
      now,
    );
    const done = completeRow(row, {}, now);
    expect(done.status).toBe("completed");
    expect(done.trust).toBe("unreviewed");
    expect(done.end).toBeDefined();
    // The input row is NOT mutated.
    expect(row.status).toBe("pending");
    expect(row.trust).toBeUndefined();
  });

  test("setTrust flips only the trust field (review/recall)", () => {
    const row = completeRow(
      buildRow({ uuid: "cccc", description: "d", priority: "low", source: "manual", kind: "shell" }, now),
      {},
      now,
    );
    const reviewed = setTrust(row, "reviewed", now);
    expect(reviewed.trust).toBe("reviewed");
    expect(reviewed.status).toBe("completed");
    const recalled = setTrust(reviewed, "recalled", now);
    expect(recalled.trust).toBe("recalled");
  });

  test("cancelRow marks the row deleted", () => {
    const row = buildRow({ uuid: "dddd", description: "d", priority: "low", source: "manual", kind: "shell" }, now);
    const cancelled = cancelRow(row, now);
    expect(cancelled.status).toBe("deleted");
    expect(cancelled.end).toBeDefined();
  });
});

describe("rows — veneer discipline (no machine-state writes)", () => {
  test("assertVeneerClean throws on each forbidden machine-state field", () => {
    for (const field of FORBIDDEN_ROW_FIELDS) {
      const row: TaskRow = {
        uuid: "e",
        description: "d",
        status: "pending",
        [field]: field === "gpu_seconds" ? 42 : "leaked",
      };
      expect(() => assertVeneerClean(row)).toThrow(/veneer violation/);
    }
  });

  test("a clean row passes", () => {
    const row: TaskRow = { uuid: "e", description: "d", status: "pending", trust: "unreviewed" };
    expect(() => assertVeneerClean(row)).not.toThrow();
  });

  test("overlayManaged merges managed fields WITHOUT clobbering foreign attributes", () => {
    const base: TaskRow = {
      uuid: "f",
      description: "d",
      status: "pending",
      // a foreign attribute the veneer does not own — must survive:
      annotations: [{ entry: "x", description: "note" }],
      priority: "L",
    };
    const merged = overlayManaged(base, { priority: "H", labor_class: "recovered", status: "pending" });
    expect(merged.priority).toBe("H");
    expect(merged.labor_class).toBe("recovered");
    // Foreign attribute preserved.
    expect(merged.annotations).toEqual([{ entry: "x", description: "note" }]);
  });

  test("overlayManaged refuses to carry a machine-state field into a row", () => {
    const base: TaskRow = { uuid: "g", description: "d", status: "pending" };
    // gpu_seconds is not in MANAGED_ROW_FIELDS so it is dropped, never merged — the row stays clean.
    const merged = overlayManaged(base, { gpu_seconds: 99 } as Partial<TaskRow>);
    expect((merged as Record<string, unknown>).gpu_seconds).toBeUndefined();
    expect(() => assertVeneerClean(merged)).not.toThrow();
  });

  test("createRow never lets a heartbeat-shaped field reach the import payload", async () => {
    const { exec, tw } = harness();
    await tw.createRow({ uuid: "h", description: "d", priority: "low", source: "r2", kind: "shell" });
    // Assert no import payload carried any forbidden field.
    const imports = exec.callsFor("task").filter((c) => c.argv.includes("import"));
    expect(imports.length).toBeGreaterThan(0);
    for (const c of imports) {
      const stdin = String(c.opts.stdin ?? "");
      const rows = JSON.parse(stdin) as Record<string, unknown>[];
      for (const r of rows) {
        for (const forbidden of FORBIDDEN_ROW_FIELDS) {
          expect(r[forbidden]).toBeUndefined();
        }
      }
    }
  });
});

describe("oplog — prev_* shadow derivation", () => {
  const now = () => "2026-07-09T12:00:00.000Z";

  test("rowState projects status:labor_class", () => {
    const row = buildRow({ uuid: "s", description: "d", priority: "low", source: "r2", kind: "shell" }, now);
    expect(rowState(row)).toBe("pending:fresh");
    expect(rowState(undefined)).toBeUndefined();
  });

  test("derive returns prev_status + prev_state on a real transition", () => {
    const before = buildRow({ uuid: "p", description: "d", priority: "low", source: "r2", kind: "shell" }, now);
    const after = completeRow(before, {}, now);
    const shadow = derive({ uuid: "p", row: before }, after);
    expect(shadow.prev_status).toBe("pending");
    expect(shadow.prev_state).toBe("pending:fresh");
    expect(hasShadow(shadow)).toBe(true);
  });

  test("derive omits unchanged fields (additive-optional, no echo)", () => {
    const row = buildRow({ uuid: "q", description: "d", priority: "low", source: "r2", kind: "shell" }, now);
    // A no-op mutation (same status/state) yields an empty shadow.
    const shadow = derive({ uuid: "q", row }, { ...row });
    expect(shadow.prev_status).toBeUndefined();
    expect(shadow.prev_state).toBeUndefined();
    expect(hasShadow(shadow)).toBe(false);
  });

  test("a create (no pre-image) yields an empty shadow", () => {
    const row = buildRow({ uuid: "r", description: "d", priority: "low", source: "r2", kind: "shell" }, now);
    const shadow = derive({ uuid: "r", row: undefined }, row);
    expect(hasShadow(shadow)).toBe(false);
  });

  test("capture reads the pre-image from the store via task export", async () => {
    const { tw } = harness();
    await tw.createRow({ uuid: "cap", description: "d", priority: "low", source: "r2", kind: "shell" });
    const pre = await capture(tw.client, "cap");
    expect(pre.row?.uuid).toBe("cap");
    expect(pre.row?.status).toBe("pending");
  });
});

describe("facade — end-to-end mutation with shadow (OCR-shaped lifecycle)", () => {
  test("create → complete carries the prev_* shadow and writes trust:unreviewed", async () => {
    const { task, tw } = harness();
    await tw.createRow({
      uuid: "ocr-1",
      description: "OCR sidecar 1",
      priority: "medium",
      source: "r2",
      kind: "shell",
      dedup_key: "sidecar-1",
    });

    const { row, shadow } = await tw.complete("ocr-1", { laborClass: "fresh" });
    expect(row.status).toBe("completed");
    expect(row.trust).toBe("unreviewed");
    expect(shadow.prev_status).toBe("pending");
    expect(shadow.prev_state).toBe("pending:fresh");

    // The store reflects completion.
    expect(task.get("ocr-1")?.status).toBe("completed");
    expect(task.get("ocr-1")?.trust).toBe("unreviewed");
  });

  test("querying trust:unreviewed finds the completed row", async () => {
    const { tw } = harness();
    await tw.createRow({ uuid: "u1", description: "d", priority: "low", source: "r2", kind: "shell" });
    await tw.complete("u1");
    const rows = await tw.query(["trust:unreviewed"]);
    expect(rows.map((r) => r.uuid)).toContain("u1");
  });

  test("review flips trust; patchManaged re-dispatches (fresh→recovered) preserving foreign attrs", async () => {
    const { task, tw } = harness((t) =>
      t.seed({
        uuid: "rd",
        description: "d",
        status: "pending",
        labor_class: "fresh",
        // foreign attribute the veneer must preserve across a patch:
        project: "corpus",
      }),
    );
    const { row, shadow } = await tw.patchManaged("rd", { labor_class: "recovered", attempt: 2 });
    expect(row.labor_class).toBe("recovered");
    expect(row.attempt).toBe(2);
    expect(shadow.prev_state).toBe("pending:fresh");
    // Foreign attribute survived the round-trip.
    expect(task.get("rd")?.project).toBe("corpus");
  });

  test("completing an unknown row fails loudly", async () => {
    const { tw } = harness();
    await expect(tw.complete("nope")).rejects.toThrow(/unknown row nope/);
  });
});

describe("contracts alignment — UDA vocabulary is the single source of truth", () => {
  test("bootstrap covers exactly the TALLY_UDAS table", () => {
    const declared = new Set(TALLY_UDAS.map((u) => u.name));
    const bootstrapped = new Set(allUdaConfigPairs().map(([k]) => k.split(".")[1]));
    expect([...bootstrapped].sort()).toEqual([...declared].sort());
  });
});

// ---------------------------------------------------------------------------------------------
// REAL-BINARY integration — the durable store is the one substrate a fake cannot certify alone.
// Gated on a `task` binary being present (the flake devshell provides taskwarrior3). Runs the veneer
// against the REAL `task`, asserting a create→complete→export round-trip survives with the right
// status/trust/end AND that entry/end come back in the compact taskwarrior datetime (never a
// fractional-second ISO that a non-UTC host would mis-parse).
// ---------------------------------------------------------------------------------------------

const TASK_BIN = Bun.which("task");
const haveTask = TASK_BIN !== null;

describe.if(haveTask)("TaskChampion — real `task` binary round-trip (integration)", () => {
  test("create → complete → export survives with compact UTC datetimes on a non-UTC TZ", async () => {
    const { mkdtempSync, rmSync } = await import("node:fs");
    const { tmpdir } = await import("node:os");
    const { join } = await import("node:path");
    const { bunExec } = await import("../../src/cli/query.ts");
    const { randomUUID } = await import("node:crypto");

    const dataDir = mkdtempSync(join(tmpdir(), "tally-task-"));
    const rcFile = join(dataDir, "taskrc");
    // Write the UDA vocabulary into the taskrc directly (so `trust` is a known field and round-trips)
    // — real `task config` bootstrap is interactive and orthogonal to this datetime round-trip check.
    const udaLines = allUdaConfigPairs().map(([k, v]) => `${k}=${v}`).join("\n");
    await Bun.write(rcFile, `data.location=${dataDir}\n${udaLines}\n`);
    try {
      // Wrap the real exec to isolate taskwarrior into the temp data dir AND force a NON-UTC TZ (the
      // host condition under which a fractional-second ISO datetime is silently corrupted by hours).
      const base = bunExec();
      const isoEnv = { TASKDATA: dataDir, TASKRC: rcFile, TZ: "America/New_York" };
      const exec: import("../../src/contracts/exec.ts").Exec = {
        run: (argv, opts = {}) => base.run(argv, { ...opts, env: { ...isoEnv, ...(opts.env ?? {}) } }),
        spawn: (argv, opts = {}) => base.spawn(argv, { ...opts, env: { ...isoEnv, ...(opts.env ?? {}) } }),
      };

      const tw = new TaskChampion({ exec });
      const uuid = randomUUID();
      await tw.createRow({ uuid, description: "integration row", source: "r2" as Source, kind: "shell", pool: "worker-gpu", priority: "low" });
      await tw.complete(uuid, { trust: "unreviewed", laborClass: "fresh" });

      const row = await tw.getRow(uuid);
      expect(row).toBeDefined();
      expect(row!.status).toBe("completed");
      expect(row!.trust).toBe("unreviewed");
      // entry/end come back in the COMPACT taskwarrior form (YYYYMMDDTHHMMSSZ), never fractional ISO.
      expect(String(row!.entry)).toMatch(/^\d{8}T\d{6}Z$/);
      if (row!.end !== undefined) expect(String(row!.end)).toMatch(/^\d{8}T\d{6}Z$/);
    } finally {
      rmSync(dataDir, { recursive: true, force: true });
    }
  });
});
