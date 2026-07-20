// tally — the canonical filesystem paths (CLI-SURFACE §2.1; SPEC "Physical append", recover();
// IMPLEMENTATION-PLAN M1.1 epoch.ts, M1.2 ledger, M2.5 events dir). All paths are XDG-based; the
// resolver reads the environment so tests can point them at a tmp dir.

/** The XDG-relevant environment the path resolver reads. Injectable for tests. */
export interface PathEnv {
  XDG_RUNTIME_DIR?: string;
  XDG_DATA_HOME?: string;
  XDG_STATE_HOME?: string;
  XDG_CONFIG_HOME?: string;
  HOME?: string;
}

function homeOr(env: PathEnv, fallback: string): string {
  const home = env.HOME ?? "";
  return home ? `${home}${fallback}` : fallback.replace(/^\//, "");
}

/** `$XDG_RUNTIME_DIR/tally` — the runtime directory (socket lives here). */
export function runtimeDir(env: PathEnv): string {
  const base = env.XDG_RUNTIME_DIR ?? `/run/user/${process.getuid?.() ?? 1000}`;
  return `${base}/tally`;
}

/** `$XDG_DATA_HOME/tally` (default `~/.local/share/tally`) — permanent proof artifacts. */
export function dataDir(env: PathEnv): string {
  const base = env.XDG_DATA_HOME ?? homeOr(env, "/.local/share");
  return `${base}/tally`;
}

/** `$XDG_STATE_HOME/tally` (default `~/.local/state/tally`) — mutable runtime state (epoch, events). */
export function stateDir(env: PathEnv): string {
  const base = env.XDG_STATE_HOME ?? homeOr(env, "/.local/state");
  return `${base}/tally`;
}

/** `$XDG_CONFIG_HOME/tally` (default `~/.config/tally`) — the nix-rendered config. */
export function configDir(env: PathEnv): string {
  const base = env.XDG_CONFIG_HOME ?? homeOr(env, "/.config");
  return `${base}/tally`;
}

/** The Unix-domain socket (CLI-SURFACE §2.1). */
export function socketPath(env: PathEnv): string {
  return `${runtimeDir(env)}/tally.sock`;
}

/** The append-only witness JSONL ledger (SPEC "Physical append"; IMPLEMENTATION-PLAN M1.2). */
export function ledgerPath(env: PathEnv): string {
  return `${dataDir(env)}/witness.jsonl`;
}

/** The lease-epoch counter file (daemon-incremented backstop, issue #9; IMPLEMENTATION-PLAN M1.1 epoch.ts). */
export function epochPath(env: PathEnv): string {
  return `${stateDir(env)}/epoch`;
}

/**
 * The pls shim's grant-generation counter file (the lease-epoch PRIMARY source, PS#21). The shim is
 * its SOLE writer (nix/pls-shim.py `_next_generation`); the daemon only READS it at boot so `epoch`
 * and `pls-generation` converge on the single monotone lease-epoch series (issue #4).
 */
export function plsGenerationPath(env: PathEnv): string {
  return `${stateDir(env)}/pls-generation`;
}

/** The `events/` drop directory (IMPLEMENTATION-PLAN M2.5 triggers). */
export function eventsDir(env: PathEnv): string {
  return `${stateDir(env)}/events`;
}

/** `events/done/` — archived accepted drop files. */
export function eventsDoneDir(env: PathEnv): string {
  return `${eventsDir(env)}/done`;
}

/** `events/rejected/` — quarantined malformed drop files. */
export function eventsRejectedDir(env: PathEnv): string {
  return `${eventsDir(env)}/rejected`;
}

/**
 * The per-unit exit-record directory (`unit-exit/`). A transient unit's ExecStopPost writes its
 * `$EXIT_STATUS` here because `systemd-run --collect` garbage-collects the unit the moment it
 * stops — even with no daemon alive to observe it — so a daemon restarted after the exit finds
 * `LoadState=not-found` and no ExecMainStatus; this record is the exit-code conjunct recovery's
 * reconciliation then gates on (issue #3).
 */
export function unitExitDir(env: PathEnv): string {
  return `${stateDir(env)}/unit-exit`;
}

/** The exit-record file for one transient unit inside an exit dir (`<dir>/<unit>.exit`). */
export function unitExitFileIn(dir: string, unit: string): string {
  return `${dir}/${unit}.exit`;
}

/** The durable exit-status record for one transient unit (`unit-exit/<unit>.exit`). */
export function unitExitPath(env: PathEnv, unit: string): string {
  return unitExitFileIn(unitExitDir(env), unit);
}

/** The nix-rendered config file (IMPLEMENTATION-PLAN §3 Config). */
export function configPath(env: PathEnv): string {
  return `${configDir(env)}/config.json`;
}

/** The full resolved path set — convenience for the daemon's boot and for tests. */
export interface TallyPaths {
  runtimeDir: string;
  dataDir: string;
  stateDir: string;
  configDir: string;
  socket: string;
  ledger: string;
  epoch: string;
  plsGeneration: string;
  events: string;
  eventsDone: string;
  eventsRejected: string;
  unitExit: string;
  config: string;
}

/** Resolve every tally path from one environment. */
export function resolvePaths(env: PathEnv): TallyPaths {
  return {
    runtimeDir: runtimeDir(env),
    dataDir: dataDir(env),
    stateDir: stateDir(env),
    configDir: configDir(env),
    socket: socketPath(env),
    ledger: ledgerPath(env),
    epoch: epochPath(env),
    plsGeneration: plsGenerationPath(env),
    events: eventsDir(env),
    eventsDone: eventsDoneDir(env),
    eventsRejected: eventsRejectedDir(env),
    unitExit: unitExitDir(env),
    config: configPath(env),
  };
}
