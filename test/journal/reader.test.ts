// test/journal/reader.test.ts
//
// The read half of M1.4 (journal/reader.ts). Covers the brief's demand: reader round-trip via the
// fake journalctl — every TALLY_* field a writer emits is re-hydrated intact, numeric fields coerced
// back to numbers, the `--task/--session/--event/--since` filters applied, and torn lines skipped.
//
// The round-trip is proven end-to-end: JournalEmitter renders the exact stdout-capture line, we seed
// the FakeJournalctl with the same TALLY_* record, and JournalReader must re-hydrate an identical
// field set.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { describe, expect, test } from "bun:test";
import { FakeExec, ok } from "../helpers/exec-fakes.ts";
import { FakeJournalctl } from "../helpers/fake-journalctl.ts";
import { JournalReader, parseLine, buildArgv, buildFollowArgv } from "../../src/journal/reader.ts";
import { JournalEmitter, toFields } from "../../src/journal/emit.ts";

/** A fake journalctl wired into a fresh FakeExec. */
function wire(): { exec: FakeExec; journal: FakeJournalctl; reader: JournalReader } {
  const exec = new FakeExec();
  const journal = new FakeJournalctl();
  journal.install(exec);
  const reader = new JournalReader(exec);
  return { exec, journal, reader };
}

describe("JournalReader — argv construction", () => {
  test("buildArgv is -t tally -o json with --since passthrough", () => {
    expect(buildArgv({})).toEqual(["journalctl", "-t", "tally", "-o", "json"]);
    expect(buildArgv({ since: "2026-07-09" })).toEqual([
      "journalctl",
      "-t",
      "tally",
      "-o",
      "json",
      "--since",
      "2026-07-09",
    ]);
  });

  test("buildFollowArgv appends -f", () => {
    expect(buildFollowArgv({})).toEqual(["journalctl", "-t", "tally", "-o", "json", "-f"]);
  });
});

describe("JournalReader — round-trip via fake journalctl", () => {
  test("a full emitted record re-hydrates field-for-field", async () => {
    const { journal, reader } = wire();
    // The exact fields the writer would produce for a completed event.
    const emitted = toFields({
      event: "completed",
      task_uuid: "task-abc",
      class: "high",
      source: "manual",
      agent_kind: "claude-code",
      session_ref: "sess-1",
      unit: "tally-job-1.service",
      exit_code: 0,
      gpu_seconds: 42,
      artifact_hash: "sha256:deadbeef",
      evidence: "pass artifact:/out/x.pdf",
      attempt: 2,
      lease_epoch: 7,
      labor_class: "fresh",
    });
    // The writer emits it (proving the emit path), and we seed the same record into journald.
    const writtenLines: string[] = [];
    new JournalEmitter((l) => writtenLines.push(l)).emitFields(emitted);
    expect(writtenLines.length).toBe(1);
    journal.emit(emitted as unknown as never);

    const entries = await reader.read();
    expect(entries.length).toBe(1);
    const f = entries[0]!.fields;

    expect(f.TALLY_EVENT).toBe("completed");
    expect(f.TALLY_TASK_UUID).toBe("task-abc");
    expect(f.TALLY_CLASS).toBe("high");
    expect(f.TALLY_SOURCE).toBe("manual");
    expect(f.TALLY_AGENT).toBe("cc");
    expect(f.TALLY_SESSION_REF).toBe("sess-1");
    expect(f.TALLY_UNIT).toBe("tally-job-1.service");
    // Numeric fields coerced back to numbers.
    expect(f.TALLY_EXIT_CODE).toBe(0);
    expect(f.TALLY_GPU_SECONDS).toBe(42);
    expect(f.TALLY_ATTEMPT).toBe(2);
    expect(f.TALLY_LEASE_EPOCH).toBe(7);
    expect(typeof f.TALLY_EXIT_CODE).toBe("number");
    expect(typeof f.TALLY_GPU_SECONDS).toBe("number");
    expect(f.TALLY_ARTIFACT_HASH).toBe("sha256:deadbeef");
    expect(f.TALLY_EVIDENCE).toBe("pass artifact:/out/x.pdf");
    expect(f.TALLY_LABOR_CLASS).toBe("fresh");
    expect(entries[0]!.realtimeUs).toBeGreaterThan(0);
  });

  test("parseLine directly round-trips the stdout-capture line the writer produced", () => {
    const emitted = toFields({
      event: "enqueued",
      task_uuid: "t-1",
      class: "low",
      source: "r2",
    });
    const line = new JournalEmitter(() => {}).emitFields(emitted);
    // Wrap the writer's stdout line as journald would (MESSAGE = the JSON payload).
    const journaldLine = JSON.stringify({
      __REALTIME_TIMESTAMP: "1720526400000000",
      SYSLOG_IDENTIFIER: "tally",
      MESSAGE: line,
    });
    const entry = parseLine(journaldLine);
    expect(entry).not.toBeNull();
    expect(entry!.fields.TALLY_EVENT).toBe("enqueued");
    expect(entry!.fields.TALLY_TASK_UUID).toBe("t-1");
    expect(entry!.realtimeUs).toBe(1720526400000000);
  });

  test("falls back to top-level TALLY_* keys when MESSAGE is not a JSON payload", () => {
    const journaldLine = JSON.stringify({
      __REALTIME_TIMESTAMP: "1720526400000000",
      SYSLOG_IDENTIFIER: "tally",
      MESSAGE: "human readable only",
      TALLY_EVENT: "started",
      TALLY_TASK_UUID: "t-2",
      TALLY_CLASS: "medium",
      TALLY_SOURCE: "gh",
      TALLY_GPU_SECONDS: "9",
    });
    const entry = parseLine(journaldLine);
    expect(entry).not.toBeNull();
    expect(entry!.fields.TALLY_EVENT).toBe("started");
    expect(entry!.fields.TALLY_TASK_UUID).toBe("t-2");
    expect(entry!.fields.TALLY_GPU_SECONDS).toBe(9);
    expect(entry!.fields.MESSAGE).toBe("human readable only");
  });
});

describe("JournalReader — filters", () => {
  async function seed(): Promise<ReturnType<typeof wire>> {
    const w = wire();
    w.journal
      .emit({ TALLY_EVENT: "enqueued", TALLY_TASK_UUID: "A", TALLY_CLASS: "high", TALLY_SOURCE: "manual" })
      .emit({
        TALLY_EVENT: "dispatched",
        TALLY_TASK_UUID: "A",
        TALLY_CLASS: "high",
        TALLY_SOURCE: "manual",
        TALLY_SESSION_REF: "sref-A",
      })
      .emit({ TALLY_EVENT: "enqueued", TALLY_TASK_UUID: "B", TALLY_CLASS: "low", TALLY_SOURCE: "gh" })
      .emit({
        TALLY_EVENT: "completed",
        TALLY_TASK_UUID: "B",
        TALLY_CLASS: "low",
        TALLY_SOURCE: "gh",
        TALLY_SESSION_REF: "sref-B",
        TALLY_EXIT_CODE: "0",
        TALLY_GPU_SECONDS: "5",
        TALLY_LABOR_CLASS: "fresh",
        TALLY_UNIT: "u.service",
        TALLY_AGENT: "cc",
        TALLY_ATTEMPT: "1",
        TALLY_LEASE_EPOCH: "3",
        TALLY_ARTIFACT_HASH: "sha256:z",
      });
    return w;
  }

  test("--task filters to one uuid", async () => {
    const { reader } = await seed();
    const entries = await reader.read({ task: "A" });
    expect(entries.length).toBe(2);
    expect(entries.every((e) => e.fields.TALLY_TASK_UUID === "A")).toBe(true);
  });

  test("--event filters to one event", async () => {
    const { reader } = await seed();
    const entries = await reader.read({ event: "enqueued" });
    expect(entries.map((e) => e.fields.TALLY_TASK_UUID).sort()).toEqual(["A", "B"]);
  });

  test("--session filters to one session_ref", async () => {
    const { reader } = await seed();
    const entries = await reader.read({ session: "sref-B" });
    expect(entries.length).toBe(1);
    expect(entries[0]!.fields.TALLY_TASK_UUID).toBe("B");
  });

  test("--task + --event compose", async () => {
    const { reader } = await seed();
    const entries = await reader.read({ task: "B", event: "completed" });
    expect(entries.length).toBe(1);
    expect(entries[0]!.fields.TALLY_GPU_SECONDS).toBe(5);
  });

  test("--since is pushed down to journalctl argv", async () => {
    const { exec, reader } = await seed();
    await reader.read({ since: "2026-07-09T12:00:03Z" });
    const call = exec.lastCall("journalctl");
    expect(call).toBeDefined();
    expect(call!.argv).toContain("--since");
    expect(call!.argv).toContain("2026-07-09T12:00:03Z");
  });
});

describe("JournalReader — robustness", () => {
  test("empty journal yields no entries", async () => {
    const { reader } = wire();
    const entries = await reader.read();
    expect(entries).toEqual([]);
  });

  test("torn / non-JSON lines are skipped", async () => {
    const exec = new FakeExec();
    exec.register("journalctl", () =>
      ok(
        [
          "not json at all",
          JSON.stringify({
            __REALTIME_TIMESTAMP: "1720526400000000",
            SYSLOG_IDENTIFIER: "tally",
            MESSAGE: JSON.stringify({ TALLY_EVENT: "started", TALLY_TASK_UUID: "ok" }),
          }),
          "{ truncated",
        ].join("\n") + "\n",
      ),
    );
    const reader = new JournalReader(exec);
    const entries = await reader.read();
    expect(entries.length).toBe(1);
    expect(entries[0]!.fields.TALLY_TASK_UUID).toBe("ok");
  });

  test("a line with no recognizable TALLY_EVENT is skipped", () => {
    const line = JSON.stringify({ SYSLOG_IDENTIFIER: "tally", MESSAGE: "startup" });
    expect(parseLine(line)).toBeNull();
  });

  test("a non-zero journalctl exit with stderr surfaces as an error", async () => {
    const exec = new FakeExec();
    exec.register("journalctl", () => ({ code: 1, stdout: "", stderr: "boom" }));
    const reader = new JournalReader(exec);
    await expect(reader.read()).rejects.toThrow(/journalctl read failed/);
  });
});

describe("JournalReader — follow", () => {
  test("follow yields the buffered entries then stops", async () => {
    const { journal, reader } = wire();
    journal
      .emit({ TALLY_EVENT: "started", TALLY_TASK_UUID: "F1", TALLY_CLASS: "high", TALLY_SOURCE: "manual" })
      .emit({ TALLY_EVENT: "completed", TALLY_TASK_UUID: "F1", TALLY_CLASS: "high", TALLY_SOURCE: "manual", TALLY_EXIT_CODE: "0", TALLY_GPU_SECONDS: "1", TALLY_LABOR_CLASS: "fresh", TALLY_UNIT: "u.service", TALLY_AGENT: "shell", TALLY_ATTEMPT: "1", TALLY_LEASE_EPOCH: "1", TALLY_ARTIFACT_HASH: "sha256:a" });
    const seen: string[] = [];
    for await (const entry of reader.follow()) {
      seen.push(entry.fields.TALLY_EVENT);
    }
    expect(seen).toEqual(["started", "completed"]);
  });

  test("follow applies filters", async () => {
    const { journal, reader } = wire();
    journal
      .emit({ TALLY_EVENT: "started", TALLY_TASK_UUID: "X", TALLY_CLASS: "high", TALLY_SOURCE: "manual" })
      .emit({ TALLY_EVENT: "started", TALLY_TASK_UUID: "Y", TALLY_CLASS: "high", TALLY_SOURCE: "manual" });
    const seen: string[] = [];
    for await (const entry of reader.follow({ task: "Y" })) {
      seen.push(entry.fields.TALLY_TASK_UUID);
    }
    expect(seen).toEqual(["Y"]);
  });
});
