// tally — error taxonomy and the wire error shape (CLI-SURFACE §2 Response `{id, error}`; §2.5).
//
// Errors ride the NDJSON wire as the `error` member of a Response frame. `unsupported_protocol`
// is the one named protocol-negotiation error (CLI-SURFACE §2.5: `{code:"unsupported_protocol",
// supported:[…]}`). All other codes are internal-additive per §2.5 (adding one never bumps the
// protocol version); consumers MUST tolerate unknown codes.

/** The set of error codes tally emits on the wire. Additive — consumers tolerate unknown codes. */
export type WireErrorCode =
  | "unsupported_protocol"
  | "invalid_params"
  | "invalid_frame"
  | "frame_too_large"
  | "unknown_method"
  | "not_found"
  | "unsupported"
  | "internal"
  | "timeout"
  | "viewer_rejected"
  | "unknown_subscription"
  | "epoch_changed";

/**
 * The wire error object carried in `Response.error`. `code` is machine-readable; `message` is
 * human-readable; `data` carries code-specific detail (e.g. `unsupported_protocol` carries
 * `{supported:number[]}`).
 */
export interface WireError {
  code: WireErrorCode;
  message: string;
  data?: Record<string, unknown>;
}

/** The distinguished protocol-negotiation error (CLI-SURFACE §2.5). */
export interface UnsupportedProtocolError extends WireError {
  code: "unsupported_protocol";
  data: { supported: number[] };
}

/**
 * A `TallyError` is a domain error that can be projected onto the wire. Every module throws this
 * (or a subclass) rather than a bare `Error`, so daemon-core can serialize it into a Response
 * `error` object without guessing a code.
 */
export class TallyError extends Error {
  readonly code: WireErrorCode;
  readonly data?: Record<string, unknown>;

  constructor(code: WireErrorCode, message: string, data?: Record<string, unknown>) {
    super(message);
    this.name = "TallyError";
    this.code = code;
    if (data !== undefined) this.data = data;
    // Restore prototype chain across the TS/ES class-extends-Error boundary.
    Object.setPrototypeOf(this, new.target.prototype);
  }

  /** Project to the wire `error` object of a Response frame. */
  toWire(): WireError {
    const w: WireError = { code: this.code, message: this.message };
    if (this.data !== undefined) w.data = this.data;
    return w;
  }
}

/** Params failed a hand-rolled validator (see `wire.ts` narrowers). Carries the offending path. */
export class ValidationError extends TallyError {
  constructor(message: string, path?: string) {
    super("invalid_params", message, path ? { path } : undefined);
    this.name = "ValidationError";
    Object.setPrototypeOf(this, ValidationError.prototype);
  }
}

/** The daemon cannot serve the requested `min_protocol`/`max_protocol` range (CLI-SURFACE §2.5). */
export class UnsupportedProtocol extends TallyError {
  constructor(supported: number[]) {
    super(
      "unsupported_protocol",
      `daemon cannot serve requested protocol range; supported: ${supported.join(", ")}`,
      { supported },
    );
    this.name = "UnsupportedProtocol";
    Object.setPrototypeOf(this, UnsupportedProtocol.prototype);
  }
}

/**
 * A `session.wait pane_output` (or `pane capture --source detection`) targeted an `is_viewer`
 * pane — refused, upholding anti-loop invariant #4 (CLI-SURFACE §2.4, §3.3).
 */
export class ViewerRejected extends TallyError {
  constructor(paneId: string) {
    super("viewer_rejected", `pane ${paneId} is a viewer (is_viewer=true); refused`, {
      pane_id: paneId,
    });
    this.name = "ViewerRejected";
    Object.setPrototypeOf(this, ViewerRejected.prototype);
  }
}

/** A frame exceeded `FRAME_CAP` (64 KiB) — the connection is protocol-broken (CLI-SURFACE §2.1). */
export class FrameTooLarge extends TallyError {
  constructor(size: number, cap: number) {
    super("frame_too_large", `frame of ${size} bytes exceeds cap ${cap}`, { size, cap });
    this.name = "FrameTooLarge";
    Object.setPrototypeOf(this, FrameTooLarge.prototype);
  }
}
