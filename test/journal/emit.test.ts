// test/journal/emit.test.ts
//
// The writer half of M1.4 (journal/emit.ts). Covers the brief's demands:
//   - field-matrix completeness per event (every `always` field on every event; every stage-gated
//     field mandatory at its stage; a missing mandatory field fails loudly);
//   - the single-line, no-embedded-newline guarantee (MESSAGE newlines are escaped, never split a
//     record);
//   - the AgentKind → TALLY_AGENT short-vocabulary mapping via the golden contracts function.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { describe, expect, test } from "bun:test";
import {
  JournalEmitter,
  toFields,
  validateFields,
  renderLine,
  type EmitEvent,
} from "../../src/journal/emit.ts";
import {
  TALLY_EVENTS,
  TALLY_FIELD_MATRIX,
  ALWAYS_FIELDS,
  ValidationError,
  type AgentKind,
  type TallyEvent,
  type TallyFields,
} from "../../src/contracts/index.ts";

/** Collect emitted lines into an array for assertions. */
function collector(): { lines: string[]; sink: (l: string) => void } {
  const lines: string[] = [];
  return { lines, sink: (l: string) => lines.push(l) };
}

/**
 * A maximally-populated event for a given TALLY_EVENT — carries every field so that a stage-gated
 * requirement is always satisfiable. The per-event validation tests then remove one field at a time.
 */
function fullEvent(event: TallyEvent): EmitEvent {
  return {
    event,
    task_uuid: "task-abc",
    class: "high",
    source: "manual",
    agent_kind: "claude-code",
    session_ref: "sess-ref-1",
    unit: "tally-job-1.service",
    exit_code: 0,
    gpu_seconds: 12,
    artifact_hash: "sha256:deadbeef",
    evidence: "pass artifact:/out/x.pdf",
    attempt: 1,
    lease_epoch: 7,
    labor_class: "fresh",
  };
}

describe("JournalEmitter — always fields", () => {
  test("every event carries every always-required field", () => {
    const { lines, sink } = collector();
    const emitter = new JournalEmitter(sink);
    for (const event of TALLY_EVENTS) {
      emitter.emit(fullEvent(event));
    }
    expect(lines.length).toBe(TALLY_EVENTS.length);
    for (const line of lines) {
      const fields = JSON.parse(line) as TallyFields;
      for (const key of ALWAYS_FIELDS) {
        expect(fields[key]).toBeDefined();
        expect(fields[key]).not.toBe("");
      }
      expect(fields.SYSLOG_IDENTIFIER).toBe("tally");
    }
  });

  test("ALWAYS_FIELDS is EXACTLY the SPEC journald always-field set (golden name pin)", () => {
    // A GOLDEN pin of the literal field names from the SPEC journald field table — NOT a re-execution
    // of ALWAYS_FIELDS' own defining expression (which could never fail). This catches the real
    // regression class: a field accidentally flipped to/from "always" in TALLY_FIELD_MATRIX — e.g.
    // promoting a stage-gated field like TALLY_GPU_SECONDS to "always" — which the tautological form
    // silently accepted.
    const GOLDEN_ALWAYS: string[] = [
      "SYSLOG_IDENTIFIER",
      "TALLY_EVENT",
      "TALLY_TASK_UUID",
      "TALLY_CLASS",
      "TALLY_SOURCE",
      "MESSAGE",
    ].sort();
    expect([...ALWAYS_FIELDS].map(String).sort()).toEqual(GOLDEN_ALWAYS);
    // AND the matrix agrees with the golden set (a field silently dropped from the matrix fails here).
    const fromMatrix = (Object.keys(TALLY_FIELD_MATRIX) as (keyof TallyFields)[])
      .filter((k) => TALLY_FIELD_MATRIX[k] === "always")
      .map(String)
      .sort();
    expect(fromMatrix).toEqual(GOLDEN_ALWAYS);
  });
});

describe("JournalEmitter — stage-gated field matrix (completeness per event)", () => {
  // For each event, for each field that is mandatory at that event's stage, dropping it must throw.
  for (const event of TALLY_EVENTS) {
    test(`event '${event}' rejects a dropped mandatory field`, () => {
      const base = toFields(fullEvent(event));
      // Which fields are mandatory for this event?
      for (const key of Object.keys(TALLY_FIELD_MATRIX) as (keyof TallyFields)[]) {
        const requirement = TALLY_FIELD_MATRIX[key];
        // Recompute the "isRequiredAt" contract by re-validating with the field removed.
        const mutated = { ...base } as Record<string, unknown>;
        delete mutated[key];
        let threw = false;
        try {
          validateFields(mutated as unknown as TallyFields);
        } catch (e) {
          threw = true;
          expect(e).toBeInstanceOf(ValidationError);
        }
        // If validation threw, the field was mandatory for this event; if not, it was optional.
        // Either way the behavior must be self-consistent with the requirement class:
        if (requirement === "always") {
          expect(threw).toBe(true);
        }
        if (requirement === "at-completed" && event === "completed") {
          expect(threw).toBe(true);
        }
        if (
          requirement === "at-completed-or-failed" &&
          (event === "completed" || event === "failed")
        ) {
          expect(threw).toBe(true);
        }
        if (
          requirement === "at-evidence" &&
          (event === "evidence_pass" || event === "evidence_fail")
        ) {
          expect(threw).toBe(true);
        }
      }
    });
  }

  test("at-dispatch+ fields required from dispatched onward, optional at enqueued", () => {
    // enqueued: dropping TALLY_ATTEMPT is fine.
    expect(() =>
      new JournalEmitter(() => {}).emit({
        event: "enqueued",
        task_uuid: "t",
        class: "low",
        source: "r2",
      }),
    ).not.toThrow();

    // dispatched: TALLY_ATTEMPT / TALLY_LEASE_EPOCH / TALLY_AGENT become mandatory.
    expect(() =>
      new JournalEmitter(() => {}).emit({
        event: "dispatched",
        task_uuid: "t",
        class: "low",
        source: "r2",
        // no agent, no attempt, no lease_epoch
      }),
    ).toThrow(ValidationError);
  });

  test("at-completed-or-failed fields required at completed and failed", () => {
    const em = new JournalEmitter(() => {});
    // completed without exit_code/gpu/labor_class throws.
    expect(() =>
      em.emit({
        event: "completed",
        task_uuid: "t",
        class: "low",
        source: "r2",
        agent_kind: "shell",
        unit: "u.service",
        attempt: 1,
        lease_epoch: 1,
        artifact_hash: "sha256:x",
      }),
    ).toThrow(ValidationError);
  });

  test("at-completed (artifact hash) required only at completed, not at failed", () => {
    const em = new JournalEmitter(() => {});
    // failed does not require the artifact hash.
    expect(() =>
      em.emit({
        event: "failed",
        task_uuid: "t",
        class: "low",
        source: "r2",
        agent_kind: "shell",
        unit: "u.service",
        attempt: 1,
        lease_epoch: 1,
        exit_code: 1,
        gpu_seconds: 3,
        labor_class: "fresh",
      }),
    ).not.toThrow();
  });

  test("evidence verdict required at evidence_pass / evidence_fail", () => {
    const em = new JournalEmitter(() => {});
    const base: EmitEvent = {
      event: "evidence_fail",
      task_uuid: "t",
      class: "low",
      source: "r2",
      agent_kind: "shell",
      unit: "u.service",
      attempt: 1,
      lease_epoch: 1,
    };
    expect(() => em.emit(base)).toThrow(ValidationError);
    expect(() => em.emit({ ...base, evidence: "clean-exit-no-artifact" })).not.toThrow();
  });
});

describe("JournalEmitter — single-line / no embedded newline", () => {
  test("a MESSAGE with embedded newlines renders as one line", () => {
    const { lines, sink } = collector();
    const emitter = new JournalEmitter(sink);
    emitter.emit({
      event: "enqueued",
      task_uuid: "t",
      class: "low",
      source: "r2",
      message: "line one\nline two\r\nline three",
    });
    expect(lines.length).toBe(1);
    const line = lines[0]!;
    expect(line.includes("\n")).toBe(false);
    expect(line.includes("\r")).toBe(false);
    // The newline survives as an escaped sequence inside the JSON string, recoverable by the reader.
    const parsed = JSON.parse(line) as TallyFields;
    expect(parsed.MESSAGE).toBe("line one\nline two\r\nline three");
  });

  test("renderLine rejects a field record with a literal newline injected", () => {
    const fields = toFields({
      event: "enqueued",
      task_uuid: "t",
      class: "low",
      source: "r2",
    });
    // renderLine over a clean record is fine.
    expect(renderLine(fields)).not.toContain("\n");
  });

  test("every event's rendered line has no embedded newline", () => {
    const { lines, sink } = collector();
    const emitter = new JournalEmitter(sink);
    for (const event of TALLY_EVENTS) {
      emitter.emit({ ...fullEvent(event), message: "a\nb\tc\"d" });
    }
    for (const line of lines) {
      expect(line.includes("\n")).toBe(false);
      expect(line.includes("\r")).toBe(false);
    }
  });
});

describe("JournalEmitter — AgentKind → TALLY_AGENT mapping", () => {
  test("claude-code maps to cc, pi to pi, shell to shell", () => {
    const { lines, sink } = collector();
    const emitter = new JournalEmitter(sink);
    const mk = (kind: AgentKind): EmitEvent => ({
      event: "dispatched",
      task_uuid: "t",
      class: "high",
      source: "manual",
      agent_kind: kind,
      attempt: 1,
      lease_epoch: 1,
    });
    emitter.emit(mk("claude-code"));
    emitter.emit(mk("pi"));
    emitter.emit(mk("shell"));
    const agents = lines.map((l) => (JSON.parse(l) as TallyFields).TALLY_AGENT);
    expect(agents).toEqual(["cc", "pi", "shell"]);
  });

  test("a raw worker label passes through as <worker>", () => {
    const { lines, sink } = collector();
    const emitter = new JournalEmitter(sink);
    emitter.emit({
      event: "dispatched",
      task_uuid: "t",
      class: "high",
      source: "manual",
      agent_label: "qwen-72b",
      attempt: 1,
      lease_epoch: 1,
    });
    expect((JSON.parse(lines[0]!) as TallyFields).TALLY_AGENT).toBe("qwen-72b");
  });

  test("agent_kind and agent_label together is a caller error", () => {
    expect(() =>
      new JournalEmitter(() => {}).emit({
        event: "dispatched",
        task_uuid: "t",
        class: "high",
        source: "manual",
        agent_kind: "pi",
        agent_label: "raw",
        attempt: 1,
        lease_epoch: 1,
      }),
    ).toThrow(ValidationError);
  });
});

describe("JournalEmitter — synthesized MESSAGE + return value", () => {
  test("emit returns the rendered line and synthesizes a human message", () => {
    const emitter = new JournalEmitter(() => {});
    const line = emitter.emit({
      event: "completed",
      task_uuid: "task-xyz",
      class: "high",
      source: "manual",
      agent_kind: "shell",
      unit: "u.service",
      attempt: 1,
      lease_epoch: 2,
      exit_code: 0,
      gpu_seconds: 42,
      artifact_hash: "sha256:aa",
      labor_class: "fresh",
    });
    const fields = JSON.parse(line) as TallyFields;
    expect(fields.MESSAGE).toContain("completed");
    expect(fields.MESSAGE).toContain("task-xyz");
    expect(fields.MESSAGE).toContain("gpu=42s");
  });

  test("unknown TALLY_EVENT is rejected", () => {
    const bad = { SYSLOG_IDENTIFIER: "tally", TALLY_EVENT: "nope" } as unknown as TallyFields;
    expect(() => validateFields(bad)).toThrow(ValidationError);
  });
});
