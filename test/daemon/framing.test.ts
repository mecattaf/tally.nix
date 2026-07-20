// daemon-core framing round-trips + FRAME_CAP enforcement (CLI-SURFACE §2.1).

import { describe, expect, test } from "bun:test";
import { encodeFrame, fitsFrameCap, LineDecoder } from "../../src/daemon/framing";
import { FRAME_CAP } from "../../src/contracts/constants";
import { FrameTooLarge, ValidationError } from "../../src/contracts/errors";

describe("framing", () => {
  test("encodeFrame emits one LF-terminated JSON line with no raw newlines", () => {
    const line = encodeFrame({ id: 1, method: "session.snapshot", note: "a\nb" });
    expect(line.endsWith("\n")).toBe(true);
    // Exactly one newline (the terminator); the embedded newline is escaped inside the string.
    expect(line.split("\n").length).toBe(2);
    const parsed = JSON.parse(line.trim());
    expect(parsed.note).toBe("a\nb");
  });

  test("round-trips a value through encode → decode", () => {
    const value = { seq: 42, id: "e1", event: "job.completed", job_id: "j1", exit_code: 0 };
    const line = encodeFrame(value);
    const dec = new LineDecoder();
    const frames = [...dec.push(line)];
    expect(frames.length).toBe(1);
    expect(frames[0]!.value).toEqual(value);
  });

  test("decoder splits multiple frames and skips blank lines", () => {
    const dec = new LineDecoder();
    const buf = `{"a":1}\n\n{"b":2}\n`;
    const frames = [...dec.push(buf)];
    expect(frames.map((f) => f.value)).toEqual([{ a: 1 }, { b: 2 }]);
  });

  test("decoder reassembles a frame split across chunks", () => {
    const dec = new LineDecoder();
    expect([...dec.push(`{"a":`)].length).toBe(0);
    const frames = [...dec.push(`1}\n`)];
    expect(frames.map((f) => f.value)).toEqual([{ a: 1 }]);
  });

  test("encodeFrame refuses a frame over FRAME_CAP", () => {
    const big = { blob: "x".repeat(FRAME_CAP) };
    expect(() => encodeFrame(big)).toThrow(FrameTooLarge);
  });

  test("decoder throws FrameTooLarge on an oversized completed line", () => {
    const dec = new LineDecoder();
    const oversized = "x".repeat(FRAME_CAP + 10) + "\n";
    expect(() => [...dec.push(oversized)]).toThrow(FrameTooLarge);
  });

  test("decoder throws FrameTooLarge on an unterminated buffer past the cap", () => {
    const dec = new LineDecoder();
    const oversized = "x".repeat(FRAME_CAP + 10); // no newline
    expect(() => [...dec.push(oversized)]).toThrow(FrameTooLarge);
  });

  test("decoder throws ValidationError on non-JSON", () => {
    const dec = new LineDecoder();
    expect(() => [...dec.push("not json\n")]).toThrow(ValidationError);
  });

  test("fitsFrameCap boundary", () => {
    expect(fitsFrameCap("x".repeat(FRAME_CAP))).toBe(true);
    expect(fitsFrameCap("x".repeat(FRAME_CAP + 1))).toBe(false);
  });
});
