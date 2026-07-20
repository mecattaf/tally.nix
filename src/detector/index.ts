// tally — the detector module barrel + daemon mount (IMPLEMENTATION-PLAN M2.3).
//
// The detector is an in-daemon SUPERVISED thread (restart isolation, PS#15a) with two
// precedence-ordered strategies (hook AUTHORITATIVE, scrape UNIVERSAL FALLBACK) classifying exactly
// `blocked|working|done|idle`. This module wires the `DetectorLoop` onto the daemon at boot via the
// composition root (`main.ts`): it registers the loop as a supervised loop, the `agent.hook_event`
// RPC carrier, the `agent.explain` RPC read, the `WaitScrapeProvider` (for `session.wait
// pane_output`), and the `SnapshotSectionProvider<"agents">` (the single store reads the `agents[]`
// leg — the detector never owns a second store, risk 9).

import type { DaemonMount, WaitScrapeProvider } from "../contracts/bus";
import type { SnapshotSectionProvider } from "../contracts/snapshot";
import type { AgentKind } from "../contracts/agent";
import { ValidationError } from "../contracts/errors";
import { DetectorLoop, type DetectorLoopOptions, type ManifestSet } from "./loop";
import { parseManifest, type Manifest } from "./manifest";
import { validateHookEventParams } from "./hooks";

export { DetectorLoop } from "./loop";
export type { DetectorLoopOptions, ManifestSet } from "./loop";
export * from "./manifest";
export * from "./regions";
export * from "./osc";
export * from "./classify";
export * from "./records";
export * from "./hooks";

/**
 * The daemon mount surface the detector needs. It extends the base `DaemonMount` with the two
 * provider-registration hooks daemon-core's `Daemon` exposes (`registerWaitScrapeProvider`,
 * `registerSnapshotSection`) — the composition root passes the concrete `Daemon`, which implements
 * all of them. Typed structurally so the detector never imports daemon-core.
 */
export interface DetectorDaemonMount extends DaemonMount {
  registerWaitScrapeProvider(provider: WaitScrapeProvider): void;
  registerSnapshotSection(provider: SnapshotSectionProvider): void;
}

/** Options to construct + mount the detector module (the composition root supplies these). */
export interface DetectorModuleOptions extends DetectorLoopOptions {}

/**
 * Load a `ManifestSet` from raw TOML sources keyed by kind. The composition root reads the
 * `manifests/*.toml` files (bundled) and passes their text here; this parses + validates each and
 * asserts the manifest's declared `kind` matches the key it was filed under.
 */
export function loadManifests(sources: Partial<Record<AgentKind, string>>): ManifestSet {
  const set: ManifestSet = {};
  for (const [kind, text] of Object.entries(sources) as Array<[AgentKind, string | undefined]>) {
    if (text === undefined) continue;
    const manifest: Manifest = parseManifest(text);
    if (manifest.kind !== kind) {
      throw new ValidationError(
        `manifest filed under "${kind}" declares kind "${manifest.kind}"`,
        "kind",
      );
    }
    set[kind] = manifest;
  }
  return set;
}

/**
 * The detector daemon module. Construct with the loop options (exec/bus/clock/manifests/cadence),
 * then `mount(daemon)` at boot. `mount` registers everything and is the single wiring point the
 * composition root calls.
 */
export class DetectorModule {
  readonly loop: DetectorLoop;

  constructor(opts: DetectorModuleOptions) {
    this.loop = new DetectorLoop(opts);
  }

  /** Wire the detector onto the daemon (called by `main.ts` at boot, before `daemon.start()`). */
  mount(daemon: DetectorDaemonMount): void {
    // The supervised loop (restart isolation): the scrape poll + bus-fed pane registry.
    daemon.registerSupervised(this.loop);

    // Strategy-1 sensor edge: the cooperative hooks post `agent.hook_event`.
    daemon.registerRpc("agent.hook_event", (params: unknown) => {
      const validated = validateHookEventParams(params);
      this.loop.applyHookEvent(validated);
      return { ok: true };
    });

    // The `agent.explain` read (CLI-SURFACE §1.4): why a pane is working/blocked/done.
    daemon.registerRpc("agent.explain", (params: unknown) => {
      const agentId = readAgentId(params);
      const explain = this.loop.explainAgent(agentId);
      if (explain === null) {
        throw new ValidationError(`no explain-data for agent "${agentId}"`, "agent_id");
      }
      return explain;
    });

    // The `WaitScrapeProvider`: daemon-core/wait.ts calls this for a `session.wait pane_output` read.
    daemon.registerWaitScrapeProvider(this.loop);

    // The `SnapshotSectionProvider<"agents">`: the single store reads the `agents[]` leg at assembly.
    daemon.registerSnapshotSection(this.loop);
  }
}

function readAgentId(params: unknown): string {
  if (typeof params === "object" && params !== null && !Array.isArray(params)) {
    const v = (params as Record<string, unknown>).agent_id;
    if (typeof v === "string" && v.length > 0) return v;
  }
  throw new ValidationError("agent.explain params must carry a non-empty agent_id", "agent_id");
}
