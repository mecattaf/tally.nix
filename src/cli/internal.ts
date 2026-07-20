// tally CLI — the internal verbs (IMPLEMENTATION-PLAN M3.1; CLI-SURFACE §1.5 internal / §0):
//
//   tally daemon run                — boot the real daemon (systemd `ExecStart`). Delegates to
//                                     daemon-core's boot entrypoint (the daemon composition root).
//   tally daemon drain              — a THIN SOCKET CLIENT posting the internal-additive `queue.drain`
//                                     RPC (M2.5): the DAEMON sweeps `events/` + re-presents pending TW
//                                     rows. Fails (non-zero) when the socket is absent — the systemd
//                                     timer retries at the next tick.
//   tally pls-wrap -- <cmd>         — run <cmd> under a pls GPU lease (the ambient default; M1.5).
//   tally hooks install [...]       — install the cooperative pi/CC detector hooks (M3.2).
//
// `daemon run`/`pls-wrap`/`hooks install` are served by the modules that own their mechanism
// (daemon-core, pls M1.5, hooks M3.2). The CLI dispatches to them so the frozen verb surface is
// complete and genuinely works end-to-end; the composition root passes the real substrate paths in
// production (pls broker config from `config.json`; hook store paths from the nix module).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { runDaemonEntrypoint, loadConfigFromDisk } from "../daemon/index";
import { PoolRegistry } from "../pls/pools";
import { parseWrapArgs, runWrap } from "../pls/wrap";
import { installHooks, INSTALL_KINDS, type InstallKind, type InstallResult } from "../hooks/installer";
import { connectClient, DaemonUnreachable } from "./client";
import { bunExec } from "./query";
import { printError, printJson, printLine } from "./output";
import { hasFlag, flag, wantsJson, type CliContext } from "./index";
import { clientOpts } from "./queue";

/** Route the internal verbs (`daemon`/`pls-wrap`/`hooks`). */
export async function runInternal(ctx: CliContext): Promise<number> {
  switch (ctx.noun) {
    case "daemon":
      return runDaemonVerb(ctx);
    case "pls-wrap":
      return runPlsWrap(ctx);
    case "hooks":
      return runHooks(ctx);
    default:
      printError(ctx.writer, `unknown internal command '${ctx.noun}'`);
      return 127;
  }
}

// ---------------------------------------------------------------------------------------------
// daemon run | drain.
// ---------------------------------------------------------------------------------------------

async function runDaemonVerb(ctx: CliContext): Promise<number> {
  const verb = ctx.verb;
  if (verb === "drain") {
    return runDrain(ctx);
  }
  if (verb === undefined || verb === "run") {
    // Boot the real daemon via daemon-core's entrypoint (the composition root supersedes this with the
    // fully-mounted daemon in production; this bare boot mounts daemon-core itself).
    return runDaemonEntrypoint(["run"], ctx.env as Record<string, string | undefined>);
  }
  printError(ctx.writer, `unknown daemon subcommand '${verb}' (expected run|drain)`);
  return 2;
}

/**
 * `tally daemon drain`: connect to the socket and issue `queue.drain`. The daemon does the sweep +
 * re-present (M2.5); this is a thin client owning NO jobs engine, NO queue, NO lease. Fails non-zero
 * when the socket is absent so the systemd `tally-drain.timer` retries at the next tick.
 */
async function runDrain(ctx: CliContext): Promise<number> {
  let client;
  try {
    client = await connectClient(clientOpts(ctx));
  } catch (err) {
    if (err instanceof DaemonUnreachable) {
      printError(ctx.writer, "daemon drain: socket absent — the daemon is not running (systemd retries at the next timer tick)");
      return 1;
    }
    throw err;
  }
  try {
    const result = await client.call<{ swept?: number; re_presented?: number; [k: string]: unknown }>("queue.drain", {});
    if (wantsJson(ctx.args)) {
      printJson(ctx.writer, result);
    } else {
      const swept = typeof result.swept === "number" ? result.swept : 0;
      const rep = typeof result.re_presented === "number" ? result.re_presented : 0;
      printLine(ctx.writer, `drained: swept ${swept} event file(s), re-presented ${rep} row(s)`);
    }
    return 0;
  } finally {
    client.close();
  }
}

// ---------------------------------------------------------------------------------------------
// pls-wrap — run a heavy command under a pls GPU lease (the ambient default; M1.5).
// ---------------------------------------------------------------------------------------------

async function runPlsWrap(ctx: CliContext): Promise<number> {
  // pls-wrap has its own `--`-terminated grammar; reconstruct the raw argv the module parser expects
  // (the CLI tokenizer already split off the `--` passthrough into `ctx.args.passthrough`).
  const raw: string[] = [];
  // Re-emit the pre-`--` flags in the module's grammar (--pool/--cost/--priority/--tenant).
  for (const name of ["--pool", "--cost", "--priority", "--tenant"] as const) {
    const v = flag(ctx.args, name);
    if (v !== undefined) raw.push(name, v);
  }
  raw.push("--");
  if (ctx.args.passthrough !== undefined) {
    raw.push(...ctx.args.passthrough);
  } else {
    // No `--` given: treat the positionals as the command (lenient — a bare `pls-wrap echo hi`).
    raw.push(...ctx.args.positionals);
  }

  let opts;
  try {
    opts = parseWrapArgs(raw);
  } catch (err) {
    printError(ctx.writer, err instanceof Error ? err.message : String(err));
    return 2;
  }

  const config = loadConfigFromDisk(ctx.env);
  const registry = PoolRegistry.fromConfig(config.pools);
  const code = await runWrap(bunExec(), registry, opts);
  return code;
}

// ---------------------------------------------------------------------------------------------
// hooks install — the cooperative detector-hook installer (M3.2).
// ---------------------------------------------------------------------------------------------

function runHooks(ctx: CliContext): number {
  if (ctx.verb !== "install") {
    printError(ctx.writer, `unknown hooks verb '${ctx.verb ?? "(none)"}' (expected install)`);
    return 2;
  }
  const opts: { kind?: InstallKind; dryRun?: boolean; env?: NodeJS.ProcessEnv } = {
    env: ctx.env as NodeJS.ProcessEnv,
  };
  const kind = flag(ctx.args, "--kind");
  if (kind !== undefined) {
    if (!(INSTALL_KINDS as readonly string[]).includes(kind)) {
      printError(ctx.writer, `--kind must be one of ${INSTALL_KINDS.join("|")}`);
      return 2;
    }
    opts.kind = kind as InstallKind;
  }
  if (hasFlag(ctx.args, "--dry-run")) opts.dryRun = true;

  let result: InstallResult;
  try {
    result = installHooks(opts);
  } catch (err) {
    printError(ctx.writer, err instanceof Error ? err.message : String(err));
    return 1;
  }

  if (wantsJson(ctx.args)) {
    printJson(ctx.writer, result);
  } else {
    printLine(ctx.writer, `hooks ${result.dryRun ? "(dry-run) " : ""}install:`);
    for (const a of result.actions) {
      printLine(ctx.writer, `  ${a.action}  ${a.kind}  ${a.path}  — ${a.detail}`);
    }
  }
  return 0;
}
