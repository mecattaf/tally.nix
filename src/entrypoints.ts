// tally — the entrypoint wiring (the composition seam `main.ts` imports).
//
// This is the ONE module that attaches the real daemon + CLI entrypoints onto `main.ts`'s registry,
// keeping `main.ts` itself free of any static import of daemon-core or cli internals (the layered-build
// discipline — the scaffold module owns the registry, this module owns the wiring). `main.ts` imports
// this for its side effects:
//
//   • `import "./cli"` runs `registerCli()` (its module-level `registerEntrypoint("cli", runCli)`), so
//     the compiled binary dispatches every §1 CLI verb to the real surface (not the layer-0 fallback).
//   • `registerEntrypoint("daemon", …)` points `tally daemon [run]` at the fully-composed daemon
//     (`runComposedDaemon`) — the real composition root that mounts SessionModel, JobsEngine,
//     DetectorModule, triggers, kitty watcher-ingest, gh intake, the nine cross-cutting handlers, and
//     the snapshot provider, then runs `recover()` before serving.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { registerEntrypoint } from "./registry";
import { runComposedDaemon } from "./compose";
import { runCli } from "./cli/index";

// Side-effect import: `src/cli/index.ts` self-registers the "cli" entrypoint on load.
import "./cli/index";

// Attach the real daemon entrypoint (supersedes main.ts's layer-0 fallback daemon). `daemon run` (or a
// bare `daemon`) boots the composed daemon; every OTHER `daemon <verb>` — chiefly `daemon drain`, the
// systemd-timer ingress — is a THIN SOCKET CLIENT and must route to the CLI's internal path, not the
// daemon boot (which rejects non-`run` subcommands with exit 2). Without this, `tally daemon drain`
// (the tally-drain.timer ExecStart) fails every tick and the drain ingress never fires.
registerEntrypoint("daemon", (argv: string[]) => {
  const sub = argv[0];
  if (sub === undefined || sub === "run") return runComposedDaemon(argv);
  return runCli(["daemon", ...argv]);
});
