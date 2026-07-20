// test/helpers/fake-systemd.ts
//
// A fake of `systemd-run --user --unit tally-job-<id> ... -- <cmd>`, the
// transient-unit execution path the jobs dispatcher uses (M2.2 dispatch.ts).
// In production a heavy unit runs as a transient systemd user unit; under test
// (and the dev rig) there is no user manager, so this fake records the unit
// metadata (`--unit`, env `--setenv`, cwd `--working-directory`) and then FALLS
// THROUGH to running the leaf command directly via the injected FakeExec — the
// same "direct spawn fallback when systemd is absent" the module implements.
//
// The recorded units let dispatch/recover tests assert the TALLY_UNIT naming,
// the env conventions (TALLY_TASK_UUID / TALLY_SESSION_REF / TALLY_YIELD_FD),
// and lease-epoch fencing, while the fall-through lets the job actually complete
// so the evidence gate + witness line are exercised end-to-end.
//
// Also fakes `systemctl --user show <unit>` over a programmable per-unit state
// table (`setUnitStates`), so recovery's pre-represent reconciliation of a
// SURVIVING/exited transient unit (issue #3) is testable: a test scripts
// active → active → exited across the adoption polls, or leaves a unit unseeded
// (LoadState=not-found, the collected/unknown-unit answer).
//
// FIDELITY NOTE (issue #3): real systemd COLLECTS an exited `systemd-run
// --collect` transient unit within moments of it stopping — daemon alive or
// not — so `LoadState=loaded` + an ExecMainStatus is only the narrow
// pre-collection race, and the REALISTIC restart-window shape for an exited
// unit is: unseeded here (not-found) PLUS the durable ExecStopPost exit record
// under `unit-exit/<unit>.exit` (write it with `writeFileSync` in the test,
// standing in for what the unit persisted before it was unloaded).
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { type FakeExec, type ExecResult, type ExecOptions, ok, fail, parseArgs } from "./exec-fakes.ts";

export interface RecordedUnit {
  unit: string;
  scope: "user" | "system";
  cwd?: string;
  env: Record<string, string>;
  /** The leaf argv after the `--` separator. */
  argv: string[];
  /** The result of running the leaf command (via fall-through). */
  result: ExecResult;
}

/**
 * A programmable systemd-run fake. `install(exec)` registers the handler; every
 * transient unit is recorded on `.units` and the leaf command is executed by
 * re-entering the SAME FakeExec (so the leaf binary must also have a fake
 * registered — e.g. a mock OCR worker, or `sh`/`true`).
 *
 * If you want systemd to appear ABSENT (to test the module's direct-spawn
 * fallback branch), do not install this fake at all and let the module fall
 * through to its own Bun.spawn path; the module detects absence by the missing
 * binary. `setAbsent()` makes the fake return the "command not found" shape so a
 * test can force the fallback deterministically.
 */
export class FakeSystemdRun {
  readonly units: RecordedUnit[] = [];
  private absent = false;
  /** Per-unit `systemctl show` state sequences (each show consumes one; the last repeats). */
  private readonly unitStates = new Map<string, FakeUnitState[]>();

  /** Make systemd-run report as absent (exit 127) so the module falls back. */
  setAbsent(): this {
    this.absent = true;
    return this;
  }

  /**
   * Seed the `systemctl --user show <unit>` state sequence for a unit. Each show consumes the next
   * state (the last one repeats), so a test can script a transient unit that SURVIVED a daemon
   * restart across recovery's adoption polls: active → active → exited. An unseeded unit reports
   * `LoadState=not-found` with exit 0 — the answer real systemctl gives for an unknown unit.
   */
  setUnitStates(unit: string, states: FakeUnitState[]): this {
    this.unitStates.set(unit, [...states]);
    return this;
  }

  /** Units recorded so far. */
  recorded(): RecordedUnit[] {
    return this.units;
  }

  /** The most recently launched unit. */
  last(): RecordedUnit | undefined {
    return this.units[this.units.length - 1];
  }

  install(exec: FakeExec): this {
    exec.register("systemd-run", async (args): Promise<ExecResult> => {
      if (this.absent) {
        return fail(127, "systemd-run: command not found");
      }
      // Split at the `--` separator: flags before, leaf argv after.
      const sepIdx = args.indexOf("--");
      const flagArgs = sepIdx === -1 ? args : args.slice(0, sepIdx);
      const leafArgv = sepIdx === -1 ? [] : args.slice(sepIdx + 1);
      const parsed = parseArgs(flagArgs);

      const scope: "user" | "system" = parsed.has("user") ? "user" : "system";
      const unit = parsed.value("unit") ?? "(transient)";
      const cwd = parsed.value("working-directory") ?? parsed.value("W");

      // Collect --setenv KEY=VALUE pairs (repeatable).
      const env: Record<string, string> = {};
      for (const kv of parsed.values("setenv")) {
        const eq = kv.indexOf("=");
        if (eq !== -1) env[kv.slice(0, eq)] = kv.slice(eq + 1);
      }

      if (leafArgv.length === 0) {
        return fail(2, "fake-systemd-run: no leaf command after '--'");
      }

      // Fall through: run the leaf command via the SAME FakeExec, merging the
      // unit env so the leaf sees TALLY_* conventions.
      const runOpts: ExecOptions = cwd !== undefined ? { cwd, env } : { env };
      const result = await exec.run(leafArgv, runOpts);
      this.units.push({
        unit,
        scope,
        env,
        argv: [...leafArgv],
        result,
        ...(cwd !== undefined ? { cwd } : {}),
      });
      return ok(result.stdout, result.stderr);
    });

    exec.register("systemctl", (rawArgs): ExecResult => {
      if (this.absent) {
        return fail(127, "systemctl: command not found");
      }
      const args = rawArgs.filter((a) => a !== "--user");
      const verb = args[0];
      if (verb === "stop") {
        return ok(); // unit stopped (best-effort; the caller never reads the body)
      }
      if (verb === "show") {
        const unit = args[1];
        const seq = unit !== undefined ? this.unitStates.get(unit) : undefined;
        const st = seq === undefined || seq.length === 0 ? undefined : seq.length > 1 ? seq.shift() : seq[0];
        if (st === undefined) {
          // Real systemctl reports an UNKNOWN unit as LoadState=not-found with exit 0.
          return ok("LoadState=not-found\nActiveState=inactive\nResult=success\nExecMainStatus=0\n");
        }
        return ok(
          [
            `LoadState=${st.loadState ?? "loaded"}`,
            `ActiveState=${st.activeState ?? "inactive"}`,
            `Result=${st.result ?? "success"}`,
            `ExecMainStatus=${st.execMainStatus ?? 0}`,
          ].join("\n") + "\n",
        );
      }
      return fail(2, `fake-systemctl: unsupported verb '${verb ?? ""}'`);
    });
    return this;
  }
}

/** A programmable `systemctl --user show` state for one transient unit (defaults: exited clean). */
export interface FakeUnitState {
  loadState?: string;
  activeState?: string;
  result?: string;
  execMainStatus?: number;
}
