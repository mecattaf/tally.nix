// tally — kitty watcher-event ingestion (IMPLEMENTATION-PLAN M1.6 `watcher-ingest.ts`; DECISIONS Q4).
//
// `hooks/kitty/tally-watcher.py` is the kitty watcher payload — a stdlib-only script kitty runs on
// window lifecycle edges (on_close / on_focus_change / on_cmd_startstop / on_title_change /
// on_set_user_var). It connects to the tally socket and posts the internal-additive RPC
// `kitty.watcher_event` (CLI-SURFACE §3.1, IMPLEMENTATION-PLAN §3 sensor edges). That RPC is the
// EVENT EDGE that replaces existence-polling: instead of the daemon polling `kitty @ ls` to notice a
// window opened/closed/focus-changed, kitty tells it.
//
// This module owns the `kitty.watcher_event` PARAM CONTRACT (an internal-additive carrier — adding
// one is never a protocol bump, §2.5) and the ingestion that validates a posted edge and re-emits it
// onto the in-daemon `Bus`. It performs hand-rolled validation (no zod, per the plan) so a malformed
// post from the watcher is rejected with a `ValidationError` rather than corrupting the model.
//
// SEAM NOTE: this module translates a raw kitty edge into the OBSERVATIONAL delta vocabulary, but the
// authoritative session/pane records (pane ids, session grouping, is_viewer, workspace) live in the
// layer-2 session-model's single store. To avoid importing a layer-2 sibling (illegal per the layer
// rules) OR guessing pane-composite-ids sensors cannot know, watcher-ingest emits a NORMALIZED,
// typed `KittyWatcherEvent` onto the Bus under the internal event name it owns, plus enough raw kitty
// facts (window id, title, cwd, focus, close) for session-model/discovery.ts to join it against
// `kitty @ ls` × `zmx list` and produce the `pane.created/closed/focused` frames. Sensors observe;
// it does not own the tree tiers.

import type { Bus } from "../contracts/bus";
import { ValidationError } from "../contracts/errors";

/** The kitty watcher edge kinds `tally-watcher.py` reports (the kitty watcher callback set). */
export type WatcherEventKind =
  | "window_created" // a new kitty window appeared (first observation)
  | "window_closed" // on_close
  | "focus_change" // on_focus_change
  | "cmd_start" // on_cmd_startstop (a foreground command began)
  | "cmd_stop" // on_cmd_startstop (a foreground command ended)
  | "title_change" // on_title_change (OSC title — a detector fast-path hint)
  | "user_var_change"; // on_set_user_var (the opaque identity back-reference changed)

/** All watcher edge kinds, canonical order — golden-tested for completeness. */
export const WATCHER_EVENT_KINDS = [
  "window_created",
  "window_closed",
  "focus_change",
  "cmd_start",
  "cmd_stop",
  "title_change",
  "user_var_change",
] as const satisfies readonly WatcherEventKind[];

const WATCHER_EVENT_KIND_SET: ReadonlySet<string> = new Set(WATCHER_EVENT_KINDS);

/**
 * The validated `kitty.watcher_event` RPC payload — the param contract this module OWNS. Every field
 * a kitty watcher callback can furnish is here; all but `kind` and `kitty_window_id` are optional
 * because a given callback only knows a subset (e.g. `on_close` knows the id but not a fresh cwd).
 */
export interface KittyWatcherEvent {
  kind: WatcherEventKind;
  /** The kitty window id the edge concerns — the pane binding key (never conflated with the others). */
  kitty_window_id: number;
  /** The window's cwd at the time of the edge, when the callback provides it. */
  cwd?: string;
  /** The OSC/window title at the edge (title_change; a Strategy-2 OSC fast-path hint). */
  title?: string;
  /** Whether the window is focused after the edge (focus_change). */
  is_focused?: boolean;
  /** A user-var key/value pair (user_var_change) — the opaque identity back-reference. */
  user_var_key?: string;
  user_var_value?: string;
  /** ISO-8601 timestamp the watcher stamped, when present. */
  ts?: string;
}

/**
 * The internal-additive bus event this module emits: the NORMALIZED kitty edge, for
 * session-model/discovery.ts to join. It rides the Bus under a name outside the frozen wire event
 * set — sensors never fabricate `pane.*` frames itself (those need session grouping it cannot know);
 * it hands the raw edge to the model, which owns the tiers. The name is namespaced so consumers that
 * only care about wire events ignore it (forward-compat: consumers MUST ignore unknown names).
 */
export const KITTY_WATCHER_BUS_EVENT = "kitty.watcher_event" as const;

/**
 * The Bus interface exposed to internal producers that emit sensor edges. The frozen typed `Bus`
 * (contracts/bus.ts) is keyed to the WIRE `EventName` union; the kitty watcher edge is an INTERNAL
 * pre-wire signal the session-model consumes, not a wire event, so sensors publishes it through this
 * narrow structural port. session-model subscribes to the same name. This keeps sensors free of any
 * layer-2 import while still using the one in-daemon bus instance.
 */
export interface SensorEdgeBus {
  emit(event: string, payload: unknown): void;
  on(event: string, handler: (payload: unknown) => void): () => void;
}

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * Hand-rolled validator for a posted `kitty.watcher_event` (the daemon's ingress guard for the sensor
 * edge — no zod, per the plan). Rejects unknown/missing `kind`, a non-numeric window id, and
 * wrong-typed optionals; ignores unknown extra fields (forward-compat).
 */
export function validateWatcherEvent(v: unknown): KittyWatcherEvent {
  if (!isObj(v)) throw new ValidationError("watcher_event params must be an object", "params");
  const kindRaw = v.kind;
  if (typeof kindRaw !== "string" || !WATCHER_EVENT_KIND_SET.has(kindRaw)) {
    throw new ValidationError(`kind must be one of ${WATCHER_EVENT_KINDS.join("|")}`, "kind");
  }
  const kind = kindRaw as WatcherEventKind;
  if (typeof v.kitty_window_id !== "number" || !Number.isFinite(v.kitty_window_id)) {
    throw new ValidationError("kitty_window_id must be a finite number", "kitty_window_id");
  }
  const out: KittyWatcherEvent = { kind, kitty_window_id: v.kitty_window_id };
  if (v.cwd !== undefined) {
    if (typeof v.cwd !== "string") throw new ValidationError("cwd must be a string", "cwd");
    out.cwd = v.cwd;
  }
  if (v.title !== undefined) {
    if (typeof v.title !== "string") throw new ValidationError("title must be a string", "title");
    out.title = v.title;
  }
  if (v.is_focused !== undefined) {
    if (typeof v.is_focused !== "boolean") throw new ValidationError("is_focused must be a boolean", "is_focused");
    out.is_focused = v.is_focused;
  }
  if (v.user_var_key !== undefined) {
    if (typeof v.user_var_key !== "string") throw new ValidationError("user_var_key must be a string", "user_var_key");
    out.user_var_key = v.user_var_key;
  }
  if (v.user_var_value !== undefined) {
    if (typeof v.user_var_value !== "string") {
      throw new ValidationError("user_var_value must be a string", "user_var_value");
    }
    out.user_var_value = v.user_var_value;
  }
  if (v.ts !== undefined) {
    if (typeof v.ts !== "string") throw new ValidationError("ts must be a string", "ts");
    out.ts = v.ts;
  }
  return out;
}

/**
 * The watcher-ingest surface. It (a) exposes the `kitty.watcher_event` RPC handler the composition
 * root registers via `DaemonMount.registerRpc` (validating + re-emitting the edge), and (b) can be
 * bridged to the typed wire `Bus` when the daemon wants the raw edge on the internal bus for the
 * session-model to consume.
 *
 * It is deliberately thin: it validates and re-emits. It NEVER decides pane ids, session grouping, or
 * is_viewer (those are session-model's) — it forwards the raw kitty facts so the model joins them.
 */
export class WatcherIngest {
  constructor(private readonly bus: SensorEdgeBus) {}

  /**
   * The `kitty.watcher_event` RPC handler (register via `DaemonMount.registerRpc("kitty.watcher_event",
   * ingest.handleRpc)`). Validates the posted edge, emits it onto the bus for the model to join, and
   * returns a small ack the watcher script can ignore. Throws `ValidationError` on a malformed post so
   * the daemon replies with an `invalid_params` error frame.
   */
  handleRpc = (params: unknown): { ok: true; kind: WatcherEventKind } => {
    const ev = validateWatcherEvent(params);
    this.bus.emit(KITTY_WATCHER_BUS_EVENT, ev);
    return { ok: true, kind: ev.kind };
  };

  /**
   * Directly ingest an already-parsed edge (used by tests and by any in-daemon producer that has a
   * structured edge rather than a raw JSON post). Emits it onto the bus.
   */
  ingest(ev: KittyWatcherEvent): void {
    this.bus.emit(KITTY_WATCHER_BUS_EVENT, ev);
  }

  /** Subscribe to the normalized kitty edges (session-model/discovery.ts is the consumer). */
  onEdge(handler: (ev: KittyWatcherEvent) => void): () => void {
    return this.bus.on(KITTY_WATCHER_BUS_EVENT, (p) => handler(p as KittyWatcherEvent));
  }
}

/**
 * Adapt the typed wire `Bus` to the narrow `SensorEdgeBus` port. The daemon holds ONE `Bus` instance;
 * this lets sensors publish/subscribe its internal edge name over that same instance without the
 * typed `Bus` needing the (non-wire) `kitty.watcher_event` name in its `EventPayloadMap`. The cast is
 * confined to this one adapter so the escape hatch is auditable.
 */
export function sensorEdgeBus(bus: Bus): SensorEdgeBus {
  const anyBus = bus as unknown as {
    emit(event: string, payload: unknown): void;
    on(event: string, handler: (payload: unknown) => void): () => void;
  };
  return {
    emit: (event, payload) => anyBus.emit(event, payload),
    on: (event, handler) => anyBus.on(event, handler),
  };
}
