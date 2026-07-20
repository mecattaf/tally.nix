// tally — watcher-ingest tests (IMPLEMENTATION-PLAN M1.6: watcher NDJSON ingestion).
//
// Asserts the `kitty.watcher_event` param validator and the WatcherIngest re-emission onto the bus:
// a well-formed edge (as posted by hooks/kitty/tally-watcher.py over the socket) validates and lands
// on the bus for session-model to join; a malformed edge is rejected with a ValidationError so the
// daemon replies with an invalid_params error frame.

import { describe, expect, test } from "bun:test";
import {
  WatcherIngest,
  validateWatcherEvent,
  WATCHER_EVENT_KINDS,
  KITTY_WATCHER_BUS_EVENT,
  type KittyWatcherEvent,
  type SensorEdgeBus,
} from "../../src/kitty/watcher-ingest.ts";
import { ValidationError } from "../../src/contracts/errors.ts";

/** A tiny in-memory SensorEdgeBus recording emits by name. */
function fakeEdgeBus(): SensorEdgeBus & { emitted: Array<{ event: string; payload: unknown }> } {
  const handlers = new Map<string, Array<(p: unknown) => void>>();
  const emitted: Array<{ event: string; payload: unknown }> = [];
  return {
    emitted,
    emit(event, payload) {
      emitted.push({ event, payload });
      for (const h of handlers.get(event) ?? []) h(payload);
    },
    on(event, handler) {
      let list = handlers.get(event);
      if (!list) {
        list = [];
        handlers.set(event, list);
      }
      list.push(handler);
      return () => {
        const list = handlers.get(event);
        if (list) {
          const i = list.indexOf(handler);
          if (i !== -1) list.splice(i, 1);
        }
      };
    },
  };
}

describe("validateWatcherEvent", () => {
  test("accepts a well-formed edge with only required fields", () => {
    const ev = validateWatcherEvent({ kind: "window_closed", kitty_window_id: 7 });
    expect(ev).toEqual({ kind: "window_closed", kitty_window_id: 7 });
  });

  test("accepts every documented kind", () => {
    for (const kind of WATCHER_EVENT_KINDS) {
      const ev = validateWatcherEvent({ kind, kitty_window_id: 1 });
      expect(ev.kind).toBe(kind);
    }
  });

  test("carries optional facts through (cwd, title, focus, user-var, ts)", () => {
    const raw = {
      kind: "user_var_change",
      kitty_window_id: 3,
      cwd: "/home/tom/work",
      title: "◐ working",
      is_focused: true,
      user_var_key: "tally_pane",
      user_var_value: "term-0707-1530:p2",
      ts: "2026-07-09T12:00:00Z",
    };
    expect(validateWatcherEvent(raw)).toEqual(raw as KittyWatcherEvent);
  });

  test("ignores unknown extra fields (forward-compat)", () => {
    const ev = validateWatcherEvent({ kind: "focus_change", kitty_window_id: 2, future_field: 99 });
    expect(ev).toEqual({ kind: "focus_change", kitty_window_id: 2 });
  });

  test("rejects an unknown kind", () => {
    expect(() => validateWatcherEvent({ kind: "explode", kitty_window_id: 1 })).toThrow(ValidationError);
  });

  test("rejects a missing / non-numeric window id", () => {
    expect(() => validateWatcherEvent({ kind: "window_closed" })).toThrow(ValidationError);
    expect(() => validateWatcherEvent({ kind: "window_closed", kitty_window_id: "7" })).toThrow(ValidationError);
  });

  test("rejects wrong-typed optionals", () => {
    expect(() => validateWatcherEvent({ kind: "title_change", kitty_window_id: 1, title: 5 })).toThrow(ValidationError);
    expect(() => validateWatcherEvent({ kind: "focus_change", kitty_window_id: 1, is_focused: "yes" })).toThrow(
      ValidationError,
    );
  });

  test("rejects a non-object", () => {
    expect(() => validateWatcherEvent(null)).toThrow(ValidationError);
    expect(() => validateWatcherEvent([1, 2])).toThrow(ValidationError);
  });
});

describe("WatcherIngest.handleRpc (the kitty.watcher_event carrier)", () => {
  test("validates and re-emits the edge onto the bus", () => {
    const bus = fakeEdgeBus();
    const ingest = new WatcherIngest(bus);
    const res = ingest.handleRpc({ kind: "window_created", kitty_window_id: 12, cwd: "/x" });
    expect(res).toEqual({ ok: true, kind: "window_created" });
    expect(bus.emitted).toHaveLength(1);
    expect(bus.emitted[0]!.event).toBe(KITTY_WATCHER_BUS_EVENT);
    expect(bus.emitted[0]!.payload).toEqual({ kind: "window_created", kitty_window_id: 12, cwd: "/x" });
  });

  test("a malformed RPC post throws ValidationError (⇒ invalid_params on the wire)", () => {
    const bus = fakeEdgeBus();
    const ingest = new WatcherIngest(bus);
    expect(() => ingest.handleRpc({ nope: true })).toThrow(ValidationError);
    expect(bus.emitted).toHaveLength(0);
  });

  test("onEdge delivers normalized edges to a subscriber (session-model's join point)", () => {
    const bus = fakeEdgeBus();
    const ingest = new WatcherIngest(bus);
    const seen: KittyWatcherEvent[] = [];
    ingest.onEdge((ev) => seen.push(ev));
    ingest.handleRpc({ kind: "focus_change", kitty_window_id: 4, is_focused: false });
    ingest.ingest({ kind: "window_closed", kitty_window_id: 4 });
    expect(seen.map((e) => e.kind)).toEqual(["focus_change", "window_closed"]);
  });

  test("NDJSON round-trip: a wire-shaped post decodes then ingests", () => {
    // Simulate what tally-watcher.py sends: an NDJSON request frame with method + params.
    const bus = fakeEdgeBus();
    const ingest = new WatcherIngest(bus);
    const line =
      JSON.stringify({
        id: "watcher-1-1",
        method: "kitty.watcher_event",
        params: { kind: "cmd_start", kitty_window_id: 5, cwd: "/home/tom", ts: "2026-07-09T00:00:00Z" },
      }) + "\n";
    const frame = JSON.parse(line.trimEnd());
    expect(frame.method).toBe("kitty.watcher_event");
    const res = ingest.handleRpc(frame.params);
    expect(res.kind).toBe("cmd_start");
    expect(bus.emitted[0]!.payload).toMatchObject({ kind: "cmd_start", kitty_window_id: 5 });
  });
});
