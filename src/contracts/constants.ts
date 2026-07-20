// tally — shared numeric/string constants (CLI-SURFACE §2.1, §2.5; IMPLEMENTATION-PLAN §3).
//
// These bounds are tally's OWN frame budget (CLI-SURFACE §2.1 note) — not inherited from any
// vendored source. They are the single source of truth every module imports; the golden tests
// pin them so a drift is a build failure.

/**
 * Wire protocol version. Integer, starts at 1. Additive changes NEVER bump it (new event names,
 * new categories, new *optional* fields). A bump happens only on a breaking change: removing or
 * renaming a field, narrowing an enum (e.g. a 5th AgentStatus), or changing a field's meaning.
 * (CLI-SURFACE §2.5)
 */
export const PROTOCOL_VERSION = 1 as const;

/** Fixed stream identifier carried in the snapshot's `protocol` field (CLI-SURFACE §2.2). */
export const PROTOCOL_ID = "tally.delta" as const;

/**
 * Bounded in-memory replay ring — tally's own memory budget of exactly 4096 events
 * (CLI-SURFACE §2.1). A subscriber requesting a `from_seq` older than the ring's oldest gets
 * `gap:true` and must re-snapshot.
 */
export const REPLAY_RING = 4096 as const;

/**
 * Per-frame cap of 64 KiB (CLI-SURFACE §2.1). A `pane.output_matched` scrape read exceeding this
 * sets `read.truncated=true`.
 */
export const FRAME_CAP = 65536 as const;

/**
 * A subscriber whose unacked backlog exceeds 1024 frames receives a final `stream.overflow` and
 * is disconnected (CLI-SURFACE §2.1).
 */
export const MAX_UNACKED = 1024 as const;

/** Idle-connection heartbeat cadence, ~15s, suppressible via `include_heartbeat=false`. */
export const HEARTBEAT_MS = 15000 as const;

/** Socket file mode — 0600, single operator, local-only (CLI-SURFACE §2.1). */
export const SOCKET_MODE = 0o600 as const;
