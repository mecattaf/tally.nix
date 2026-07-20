// tally — tier-1 Workspace population (IMPLEMENTATION-PLAN M2.1 `workspace.ts`; CLI-SURFACE §2.2
// TREE TIER 1). tally OBSERVES niri panels; niri owns layout. Best-effort from `niri msg -j
// workspaces` when the compositor is present, else ONE default workspace named by the
// `conductorHost` config so a headless / non-niri host still produces a well-formed §2.2 frame.
//
// niri is read-only and off the critical path — a missing or erroring `niri` never fails discovery;
// it just falls back to the single default workspace. The read goes through the injected `Exec`
// seam (the only way any module shells out), so it is testable against a registered `niri` fake.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Exec, ExecOptions } from "../contracts/exec";
import type { WorkspaceRecord } from "../contracts/snapshot";

/** The niri binary basename. */
export const NIRI_BIN = "niri" as const;

/**
 * One niri workspace as `niri msg -j workspaces` reports it. Parsed defensively (every field
 * optional at ingress); only the fields tally's tier-1 leg consumes are surfaced.
 */
interface RawNiriWorkspace {
  id?: unknown;
  idx?: unknown;
  name?: unknown;
  is_focused?: unknown;
  is_active?: unknown;
  output?: unknown;
}

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * Derive a stable workspace id/label from a raw niri workspace node. niri workspaces may be named
 * (`name`) or bare-indexed; the label prefers the name, falling back to `idx`/`id`. The id is the
 * label (tally keys tier 1 on the human-facing panel identity, since niri owns the real layout).
 */
function workspaceIdentity(raw: RawNiriWorkspace): { id: string; label: string } {
  if (typeof raw.name === "string" && raw.name.length > 0) {
    return { id: raw.name, label: raw.name };
  }
  const idx = typeof raw.idx === "number" ? raw.idx : typeof raw.id === "number" ? raw.id : null;
  const label = idx !== null ? `ws-${idx}` : "ws";
  return { id: label, label };
}

/**
 * Parse `niri msg -j workspaces` JSON into tally's tier-1 records. Returns `null` (not an empty
 * array) when the output is not the expected array shape, so the caller can distinguish "niri says
 * there are no workspaces" (empty array ⇒ still fall back to a default) from "niri unusable" — both
 * paths fall back, but the distinction is explicit. `focused_session` is always null here; the
 * session→workspace binding is stamped by `discovery.ts` when it observes a pane in a session.
 */
export function parseNiriWorkspaces(stdout: string): WorkspaceRecord[] | null {
  return parseNiriWorkspacesWithFocus(stdout)?.records ?? null;
}

/**
 * Parse `niri msg -j workspaces` into the tier-1 records PLUS the niri-focused workspace id (carried
 * OUT-OF-BAND, not on the frozen §2.2 `WorkspaceRecord` wire shape). Discovery uses `focusedId` to
 * derive `snapshot.focus.workspace` and fire `workspace.focused` on real niri focus changes — the
 * `is_focused` flag was previously dropped here, permanently pinning focus to `workspaces[0]`.
 */
export function parseNiriWorkspacesWithFocus(stdout: string): { records: WorkspaceRecord[]; focusedId: string | null } | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(stdout);
  } catch {
    return null;
  }
  if (!Array.isArray(parsed)) return null;
  const out: WorkspaceRecord[] = [];
  const seen = new Set<string>();
  let focusedId: string | null = null;
  for (const node of parsed) {
    if (!isObj(node)) continue;
    const raw = node as RawNiriWorkspace;
    const { id, label } = workspaceIdentity(raw);
    if (seen.has(id)) continue;
    seen.add(id);
    if (raw.is_focused === true && focusedId === null) focusedId = id;
    out.push({ id, label, focused_session: null });
  }
  return { records: out, focusedId };
}

/** The default single workspace named by `conductorHost` (the non-niri / headless fallback). */
export function defaultWorkspace(conductorHost: string): WorkspaceRecord {
  const id = conductorHost && conductorHost.trim().length > 0 ? conductorHost : "default";
  return { id, label: id, focused_session: null };
}

/**
 * The tier-1 workspace source. Reads niri best-effort; on any failure (binary absent, non-zero exit,
 * unparseable output, empty list) it returns the single default workspace. Never throws — niri is
 * observational and off the critical path.
 */
export class WorkspaceSource {
  constructor(
    private readonly exec: Exec,
    private readonly conductorHost: string,
    private readonly bin: string = NIRI_BIN,
  ) {}

  /**
   * Enumerate the current workspaces. Best-effort niri read, falling back to `[defaultWorkspace]`.
   * An empty niri list also falls back to the default so tier 1 is never empty (a well-formed §2.2
   * frame always carries at least one workspace to group sessions under).
   */
  async list(opts?: ExecOptions): Promise<WorkspaceRecord[]> {
    return (await this.listWithFocus(opts)).records;
  }

  /**
   * Enumerate the current workspaces AND the niri-focused workspace id (out-of-band, off the frozen
   * §2.2 wire shape). The fallback default workspace reports itself focused (a single-workspace host
   * has exactly one focus). Discovery uses `focusedId` to derive `snapshot.focus.workspace`.
   */
  async listWithFocus(opts?: ExecOptions): Promise<{ records: WorkspaceRecord[]; focusedId: string | null }> {
    const fallbackWs = defaultWorkspace(this.conductorHost);
    const fallback = { records: [fallbackWs], focusedId: fallbackWs.id };
    let res: { code: number; stdout: string };
    try {
      res = await this.exec.run([this.bin, "msg", "-j", "workspaces"], opts);
    } catch {
      return fallback;
    }
    if (res.code !== 0) return fallback;
    const parsed = parseNiriWorkspacesWithFocus(res.stdout);
    if (parsed === null || parsed.records.length === 0) return fallback;
    return parsed;
  }
}
