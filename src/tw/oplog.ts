// tally — the `prev_*` shadow-field derivation (IMPLEMENTATION-PLAN M1.3 `oplog.ts`; CLI-SURFACE
// §2.3 note, §1.1a `prev_*` shadow-fields ruling).
//
// TaskChampion's own operation log computes attribute-level deltas for undo/sync. Under the
// shell-out constraint (never in-process; jul9 ruling) tally re-derives that delta at the ONLY
// legal access altitude: by exporting the row immediately BEFORE a mutation and diffing it against
// the post-mutation row. The computed `prev_state` / `prev_status` are surfaced additive-optionally
// on the wire (`job.*` / `pane.*` events) — consumers read them rather than re-deriving prior
// state, and adding them NEVER bumps `protocol_version` (§2.5).
//
// The seam is "capture before mutate": a caller snapshots the pre-image via `capture()` (one
// `task export <uuid>`), performs the mutation through the veneer, then asks `derive()` for the
// shadow. No mutation happens here — this module only reads and diffs.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { PrevShadow, TaskRow, TaskStatus } from "../contracts/task";
import type { TaskClient } from "./client";

/**
 * The pre-image of a row captured immediately before a mutation. `row` is undefined when the row
 * did not yet exist (a create) — the shadow of a create carries no `prev_*`.
 */
export interface PreImage {
  readonly uuid: string;
  readonly row: TaskRow | undefined;
}

/**
 * Capture the pre-mutation image of a row (one `task export <uuid>`). Call this at the last moment
 * before a mutating `task import`. If the row does not yet exist the pre-image is empty.
 */
export async function capture(client: TaskClient, uuid: string): Promise<PreImage> {
  const row = await client.exportOne(uuid);
  return { uuid, row };
}

/**
 * The tally-canonical "state" of a row for shadow purposes. A row's high-level state is its
 * job-lifecycle-adjacent `labor_class`+`status` projection; but the wire `prev_state` the SPEC
 * names is the row's semantic state string. We derive it as the row's `status` refined by the
 * `labor_class`, so a re-dispatch (fresh→recovered) is visible as a state change and a completion
 * (pending→completed) is visible as one too. This is a stable, testable projection — never a
 * protocol-bearing value.
 */
export function rowState(row: TaskRow | undefined): string | undefined {
  if (row === undefined) return undefined;
  const labor = typeof row.labor_class === "string" ? row.labor_class : "fresh";
  return `${row.status}:${labor}`;
}

/**
 * Derive the `prev_*` shadow from a captured pre-image and the post-mutation row. Returns only the
 * fields that actually CHANGED (additive-optional discipline — an unchanged field is omitted, not
 * emitted as an echo). A create (no pre-image row) yields an empty shadow.
 */
export function derive(pre: PreImage, post: TaskRow): PrevShadow {
  const shadow: PrevShadow = {};
  const prev = pre.row;
  if (prev === undefined) return shadow;

  const prevStatus = prev.status;
  if (prevStatus !== post.status) {
    shadow.prev_status = prevStatus as TaskStatus;
  }

  const prevState = rowState(prev);
  const postState = rowState(post);
  if (prevState !== undefined && prevState !== postState) {
    shadow.prev_state = prevState;
  }

  return shadow;
}

/**
 * Capture-mutate-derive in one call: snapshot the pre-image, run the caller's `mutate` (which
 * performs the veneer write and returns the post-image row), and return the post row plus its
 * `prev_*` shadow. This is the ergonomic path the jobs engine uses so it never forgets to capture
 * before mutating.
 */
export async function withShadow(
  client: TaskClient,
  uuid: string,
  mutate: (pre: PreImage) => Promise<TaskRow>,
): Promise<{ row: TaskRow; shadow: PrevShadow }> {
  const pre = await capture(client, uuid);
  const row = await mutate(pre);
  const shadow = derive(pre, row);
  return { row, shadow };
}

/** True when a shadow carries any `prev_*` field (a real transition was captured). */
export function hasShadow(shadow: PrevShadow): boolean {
  return shadow.prev_state !== undefined || shadow.prev_status !== undefined;
}
