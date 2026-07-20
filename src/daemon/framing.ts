// tally daemon-core — NDJSON framing (CLI-SURFACE §2.1, byte-for-byte).
//
// The wire is exactly one UTF-8 JSON object per line, LF-terminated, no raw embedded newlines. A
// per-frame cap of 64 KiB (`FRAME_CAP`) bounds every direction. This module owns the two halves:
//   - `encodeFrame` — a value → one LF-terminated JSON line, refusing anything over the cap.
//   - `LineDecoder` — a streaming byte/string sink that yields complete lines, enforcing the cap on
//     an unterminated buffer so a peer cannot exhaust memory by never sending a newline.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { StringDecoder } from "node:string_decoder";
import { FRAME_CAP } from "../contracts/constants";
import { FrameTooLarge, ValidationError } from "../contracts/errors";

/**
 * Encode a single frame value to its on-wire line: `JSON.stringify(value) + "\n"`. Throws
 * `FrameTooLarge` when the encoded UTF-8 byte length exceeds `FRAME_CAP` (the newline is not counted
 * against the cap — the cap bounds the JSON object itself, §2.1). `JSON.stringify` already emits no
 * raw newlines inside string values (they are escaped as `\n`), upholding the "no raw embedded
 * newlines" invariant.
 */
export function encodeFrame(value: unknown): string {
  const json = JSON.stringify(value);
  if (json === undefined) {
    throw new ValidationError("frame value is not JSON-serializable", "$");
  }
  const bytes = Buffer.byteLength(json, "utf8");
  if (bytes > FRAME_CAP) {
    throw new FrameTooLarge(bytes, FRAME_CAP);
  }
  return json + "\n";
}

/**
 * Whether an already-serialized JSON string fits the frame cap. Used by producers that build a line
 * incrementally (e.g. the replay ring truncation path) to decide before committing to a frame.
 */
export function fitsFrameCap(json: string): boolean {
  return Buffer.byteLength(json, "utf8") <= FRAME_CAP;
}

/** A decoded line plus its parsed JSON value. `raw` is retained for diagnostics. */
export interface DecodedFrame {
  raw: string;
  value: unknown;
}

/**
 * A streaming NDJSON line decoder. Feed it chunks (string or bytes) via `push`; it accumulates a
 * buffer and yields every complete LF-terminated line. Blank lines are skipped (a keepalive idiom).
 * The cap is enforced two ways:
 *   - a completed line longer than `FRAME_CAP` throws `FrameTooLarge` (the connection is
 *     protocol-broken and the server closes it);
 *   - an UNTERMINATED buffer that grows past `FRAME_CAP` also throws, so a peer that never sends a
 *     newline cannot pin unbounded memory.
 * Each yielded line is `JSON.parse`d; a parse failure surfaces as a `ValidationError` the caller maps
 * to an `invalid_frame` response.
 */
export class LineDecoder {
  // Buffer raw BYTES, not a string: a multibyte UTF-8 codepoint split across two socket chunks must
  // NOT be decoded per-chunk (that yields U+FFFD replacement chars in both halves — silent payload
  // corruption inside a JSON string value). We accumulate bytes and only decode a complete line
  // (LF-terminated) to UTF-8, so no codepoint is ever decoded across a chunk boundary.
  // A stateful UTF-8 StringDecoder RETAINS a partial multibyte codepoint across pushes: bytes that
  // straddle a chunk boundary are buffered and surfaced only once the whole codepoint has arrived, so a
  // multibyte sequence split across two socket reads is never decoded into U+FFFD replacement chars
  // (which would silently corrupt a JSON string payload). We accumulate the DECODED string and split on
  // LF. No codepoint is ever decoded across a chunk boundary.
  private readonly decoder = new StringDecoder("utf8");
  private buf = "";

  /**
   * Push a chunk and pull every complete frame it (and prior partials) now yield. The generator is
   * eager over the current buffer; the caller iterates it fully before pushing more. Frames yielded
   * before a later bad line in the same chunk ARE emitted (the throw happens only when the bad line
   * is reached), so the caller can iterate incrementally and serve the earlier valid frames.
   */
  *push(chunk: string | Uint8Array | Buffer): Generator<DecodedFrame, void, void> {
    this.buf += typeof chunk === "string" ? chunk : this.decoder.write(Buffer.from(chunk as Uint8Array));
    let nl: number;
    while ((nl = this.buf.indexOf("\n")) !== -1) {
      const line = this.buf.slice(0, nl);
      this.buf = this.buf.slice(nl + 1);
      // Strip a trailing CR for CRLF tolerance without weakening the LF-terminated contract.
      const trimmed = line.endsWith("\r") ? line.slice(0, -1) : line;
      if (trimmed.trim().length === 0) continue;
      const bytes = Buffer.byteLength(trimmed, "utf8");
      if (bytes > FRAME_CAP) {
        throw new FrameTooLarge(bytes, FRAME_CAP);
      }
      let value: unknown;
      try {
        value = JSON.parse(trimmed);
      } catch (err) {
        throw new ValidationError(
          `invalid JSON frame: ${err instanceof Error ? err.message : String(err)}`,
          "$",
        );
      }
      yield { raw: trimmed, value };
    }
    // Guard the unterminated remainder against unbounded growth.
    if (Buffer.byteLength(this.buf, "utf8") > FRAME_CAP) {
      const bytes = Buffer.byteLength(this.buf, "utf8");
      this.buf = "";
      throw new FrameTooLarge(bytes, FRAME_CAP);
    }
  }

  /** Bytes currently buffered but not yet a complete line (for diagnostics/tests). */
  get pending(): number {
    return Buffer.byteLength(this.buf, "utf8");
  }
}
