// test/nix/hm-module.test.ts
//
// Module-eval tests for the tally nix modules (M3.3 nix-module): homeManagerModules.tally
// (nix/hm-module.nix + nix/units.nix) and nixosModules.tally (nix/nixos-module.nix). The brief's
// required cases: `nix flake check` module eval with BOTH roles + the two assertions —
//   • receiver ⇒ NO daemon unit,
//   • conductorHost required when enabled.
//
// The eval is driven through test/nix/eval-hm.nix, a standalone lib.evalModules evaluator that
// inspects the module's generated artifacts (systemd user units, config.json, assertions) WITHOUT
// pulling home-manager as a flake input. Each case runs `nix eval --impure --json` and asserts on
// the JSON projection. On top of the two required cases we also assert the load-bearing emission
// fields (StandardOutput=journal + SyslogIdentifier=tally — SPEC "Emission path"), the config.json
// shape (matches src/contracts/config.ts TallyConfig), the drain timer's Persistent=true, and the
// nixos stub's assert-on-enable behaviour.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { join } from "node:path";

const REPO = join(import.meta.dir, "..", "..");
const HM_HARNESS = join(REPO, "test", "nix", "eval-hm.nix");
const NIXOS_HARNESS = join(REPO, "test", "nix", "eval-nixos.nix");

/** Is a working `nix` on PATH? (Gate the suite so it is meaningful where nix exists, skipped where not.) */
function haveNix(): boolean {
  const r = spawnSync("nix", ["--version"], { encoding: "utf8" });
  return r.status === 0;
}

const NIX = haveNix();

/**
 * Evaluate `attr` of a harness file with the given `services.tally` args, returning the parsed JSON.
 * Throws with the nix stderr when the eval fails (so an assertion-throw case can be asserted).
 */
function nixEval(harness: string, attr: string, args: string): unknown {
  const r = spawnSync(
    "nix",
    ["eval", "--impure", "--json", "--arg", "args", args, "-f", harness, attr],
    { encoding: "utf8", maxBuffer: 32 * 1024 * 1024 },
  );
  if (r.status !== 0) {
    throw new Error(`nix eval failed (attr=${attr}, args=${args}):\n${r.stderr}`);
  }
  return JSON.parse(r.stdout);
}

describe.if(NIX)("homeManagerModules.tally — module eval", () => {
  test("conductor role: generates the daemon, drain, and local pls broker units", () => {
    const names = nixEval(HM_HARNESS, "serviceNames", `{ enable = true; conductorHost = "box1"; }`) as string[];
    expect(names).toContain("tally-daemon");
    expect(names).toContain("tally-drain");
    // One broker unit per LOCAL (localhost) pool — both default GPU pools are local by default.
    expect(names).toContain("tally-pls-broker-worker-gpu");
    expect(names).toContain("tally-pls-broker-controller-gpu");
  });

  test("conductor daemon unit: the ruled emission path + Restart=always, no epoch ExecStartPre", () => {
    const daemon = nixEval(HM_HARNESS, "daemon", `{ enable = true; conductorHost = "box1"; }`) as {
      execStart: string;
      standardOutput: string;
      syslogIdentifier: string;
      restart: string;
      execStartPre: string | null;
      wantedBy: string[];
    };
    expect(daemon).not.toBeNull();
    // SPEC "Emission path": StandardOutput=journal captures TALLY_* stdout under SyslogIdentifier=tally.
    expect(daemon.standardOutput).toBe("journal");
    expect(daemon.syslogIdentifier).toBe("tally");
    expect(daemon.restart).toBe("always");
    expect(daemon.execStart).toContain("daemon run");
    // Issue #9: the PS#21 lease-epoch backstop is NOT an ExecStartPre — a systemd increment racing
    // the daemon's own bumpEpoch() double-incremented every restart, so the file ExecStartPre wrote
    // never matched the announced `lease_epoch`. The daemon (epoch.ts) is the sole increment owner.
    expect(daemon.execStartPre).toBeNull();
    // Linger-compatible user unit.
    expect(daemon.wantedBy).toContain("default.target");
  });

  test("drain: oneshot service + Persistent=true timer (SPEC 'Flake outputs')", () => {
    const drain = nixEval(HM_HARNESS, "drain", `{ enable = true; conductorHost = "box1"; }`) as {
      execStart: string;
      type: string;
    };
    expect(drain.type).toBe("oneshot");
    expect(drain.execStart).toContain("daemon drain");

    const timer = nixEval(HM_HARNESS, "drainTimer", `{ enable = true; conductorHost = "box1"; }`) as {
      persistent: boolean;
      onUnitActiveSec: string;
    };
    expect(timer.persistent).toBe(true);
  });

  test("receiver role: NO daemon unit (the daemon runs only on conductor)", () => {
    const names = nixEval(
      HM_HARNESS,
      "serviceNames",
      `{ enable = true; role = "receiver"; conductorHost = "box1"; }`,
    ) as string[];
    expect(names).toEqual([]);
    // A receiver still gets the CLI + pls-lease-wrap on PATH.
    const pkgs = nixEval(
      HM_HARNESS,
      "packageNames",
      `{ enable = true; role = "receiver"; conductorHost = "box1"; }`,
    ) as string[];
    expect(pkgs).toContain("pls-lease-wrap");
  });

  test("watcherScript (the DECISIONS Q4 read-only export) is READABLE — forcing it does not throw", () => {
    // This is the read that used to throw "read-only, but it's set multiple times"; nixEval would fail
    // (non-zero exit) if it threw. A successful string proves the export is usable.
    const ws = nixEval(HM_HARNESS, "watcherScript", `{ enable = true; conductorHost = "box1"; }`) as string;
    expect(ws).toContain("tally-watcher.py");
  });

  test("daemon PATH has NO leading empty element (no cwd on PATH) even with empty runtimeInputs", () => {
    const env = nixEval(
      HM_HARNESS,
      "daemonEnv",
      `{ enable = true; conductorHost = "box1"; runtimeInputs = [ ]; }`,
    ) as string[];
    const pathEntry = env.find((e) => e.startsWith("PATH="));
    expect(pathEntry).toBeDefined();
    const value = pathEntry!.slice("PATH=".length);
    // No zero-length element (POSIX resolves "" as the current directory): no leading ":", no "::".
    expect(value.startsWith(":")).toBe(false);
    expect(value.includes("::")).toBe(false);
    // The per-user profile dirs are present so the ambient kitty/zmx resolve.
    expect(value).toContain("/etc/profiles/per-user/%u/bin");
    expect(value).toContain(".nix-profile/bin");
  });

  test("daemon carries a per-pool PLS_POOL_URLS map so each pool reaches its own broker", () => {
    const env = nixEval(HM_HARNESS, "daemonEnv", `{ enable = true; conductorHost = "box1"; }`) as string[];
    const entry = env.find((e) => e.startsWith("PLS_POOL_URLS="));
    expect(entry).toBeDefined();
    const map = JSON.parse(entry!.slice("PLS_POOL_URLS=".length)) as Record<string, string>;
    // Default pools land on distinct ports (worker prio 0 → 5555, controller prio 1 → 5556).
    expect(map["worker-gpu"]).toBe("http://127.0.0.1:5555");
    expect(map["controller-gpu"]).toBe("http://127.0.0.1:5556");
  });

  test("two local pools sharing a priority: the port-collision assertion fails", () => {
    const passed = nixEval(
      HM_HARNESS,
      "assertionsPassed",
      `{ enable = true; conductorHost = "box1"; plsBroker.command = [ "/bin/true" ]; pools = [ { name = "a"; broker = "localhost"; priority = 0; capacity = 1; } { name = "b"; broker = "localhost"; priority = 0; capacity = 1; } ]; }`,
    ) as boolean;
    expect(passed).toBe(false);
  });

  test("conductorHost required when enabled: assertion fails when it is null", () => {
    const passed = nixEval(HM_HARNESS, "assertionsPassed", `{ enable = true; }`) as boolean;
    expect(passed).toBe(false);
    const msgs = nixEval(HM_HARNESS, "failedMessages", `{ enable = true; }`) as string[];
    expect(msgs.some((m) => m.includes("conductorHost is required"))).toBe(true);
  });

  test("conductorHost set + broker wired: all assertions pass", () => {
    // A conductor with LOCAL pools needs a broker ExecStart wired (the flake sets
    // plsBroker.command / .source = inputs.pls). Supply a command so the broker assertion passes.
    const passed = nixEval(
      HM_HARNESS,
      "assertionsPassed",
      `{ enable = true; conductorHost = "box1"; plsBroker.command = [ "/bin/true" ]; }`,
    ) as boolean;
    expect(passed).toBe(true);
  });

  test("conductor with LOCAL pools but no broker wiring: assertion fails (must wire command/source)", () => {
    const passed = nixEval(HM_HARNESS, "assertionsPassed", `{ enable = true; conductorHost = "box1"; }`) as boolean;
    expect(passed).toBe(false);
    const msgs = nixEval(HM_HARNESS, "failedMessages", `{ enable = true; conductorHost = "box1"; }`) as string[];
    expect(msgs.some((m) => m.includes("plsBroker"))).toBe(true);
  });

  test("all-remote pools: no local broker needed, assertions pass without broker wiring", () => {
    // When every pool's broker is a remote address (worker over TB3/tailnet), no local broker unit
    // is generated, so the broker-wiring assertion is vacuous.
    const passed = nixEval(
      HM_HARNESS,
      "assertionsPassed",
      `{ enable = true; conductorHost = "box1"; pools = [ { name = "worker-gpu"; broker = "worker.tailnet"; priority = 0; } ]; }`,
    ) as boolean;
    expect(passed).toBe(true);
    const names = nixEval(
      HM_HARNESS,
      "serviceNames",
      `{ enable = true; conductorHost = "box1"; pools = [ { name = "worker-gpu"; broker = "worker.tailnet"; priority = 0; } ]; }`,
    ) as string[];
    // daemon + drain, but NO broker unit (the broker runs on the worker box).
    expect(names).toContain("tally-daemon");
    expect(names.some((n) => n.startsWith("tally-pls-broker-"))).toBe(false);
  });

  test("intake.gh on a receiver is rejected (the daemon hosts intake)", () => {
    const passed = nixEval(
      HM_HARNESS,
      "assertionsPassed",
      `{ enable = true; role = "receiver"; conductorHost = "box1"; intake.gh.enable = true; }`,
    ) as boolean;
    expect(passed).toBe(false);
    const msgs = nixEval(
      HM_HARNESS,
      "failedMessages",
      `{ enable = true; role = "receiver"; conductorHost = "box1"; intake.gh.enable = true; }`,
    ) as string[];
    expect(msgs.some((m) => m.includes("role = \"conductor\""))).toBe(true);
  });

  test("config.json renders to the TallyConfig shape src/contracts/config.ts loads", () => {
    const cfg = nixEval(
      HM_HARNESS,
      "configJson",
      `{ enable = true; conductorHost = "box1"; sessions = [ "work-*" ]; intake.gh = { enable = true; sources = [ "mecattaf/tally" ]; }; }`,
    ) as {
      role: string;
      conductorHost: string;
      sessions: string[];
      pools: Array<{ name: string; broker: string; priority: number; capacity: number }>;
      intake: { gh: { enable: boolean; sources: string[] } };
      detector: { working_poll_ms: number; idle_poll_ms: number };
      heartbeatMs: number;
      daemonVersion: string;
    };
    expect(cfg.role).toBe("conductor");
    expect(cfg.conductorHost).toBe("box1");
    expect(cfg.sessions).toEqual(["work-*"]);
    expect(cfg.pools.map((p) => p.name).sort()).toEqual(["controller-gpu", "worker-gpu"]);
    // Every default pool is single-lease (PS#5).
    expect(cfg.pools.every((p) => p.capacity === 1)).toBe(true);
    expect(cfg.intake.gh.enable).toBe(true);
    expect(cfg.intake.gh.sources).toEqual(["mecattaf/tally"]);
    // Detector cadence defaults (plan §1 flag 4).
    expect(cfg.detector.working_poll_ms).toBe(2000);
    expect(cfg.detector.idle_poll_ms).toBe(10000);
    expect(cfg.heartbeatMs).toBe(15000);
  });

  test("disabled module: no units, no assertions failures (mkIf enable gate)", () => {
    const names = nixEval(HM_HARNESS, "serviceNames", `{ enable = false; }`) as string[];
    expect(names).toEqual([]);
    const passed = nixEval(HM_HARNESS, "assertionsPassed", `{ enable = false; }`) as boolean;
    expect(passed).toBe(true);
  });

  test("sessions default []: observe-all scoping (plan risk 11)", () => {
    const cfg = nixEval(HM_HARNESS, "configJson", `{ enable = true; conductorHost = "box1"; }`) as {
      sessions: string[];
    };
    expect(cfg.sessions).toEqual([]);
  });
});

describe.if(NIX)("nixosModules.tally — the ruled unbuilt thin stub", () => {
  test("disabled: evaluates clean, no assertion failure", () => {
    const passed = nixEval(NIXOS_HARNESS, "assertionsPassed", `{ enable = false; }`) as boolean;
    expect(passed).toBe(true);
  });

  test("enabled: asserts (unbuilt stub — use homeManagerModules.tally)", () => {
    const passed = nixEval(NIXOS_HARNESS, "assertionsPassed", `{ enable = true; }`) as boolean;
    expect(passed).toBe(false);
    const msgs = nixEval(NIXOS_HARNESS, "failedMessages", `{ enable = true; }`) as string[];
    expect(msgs.some((m) => m.toLowerCase().includes("unbuilt"))).toBe(true);
  });
});

// The inverted "nix unavailable" marker used to be a describe.if(!NIX) whose sole test
// `expect(NIX).toBe(false)` PASSED exactly when all the real module-eval coverage silently vanished —
// a green run reporting total coverage loss (the failure mode) — and a permanent 1-skip noise floor in
// every sanctioned (nix-having) run. Replaced with a LOUD guard: in an environment that declares nix
// mandatory (the flake devshell/check exports TALLY_REQUIRE_NIX=1), a missing nix is a hard FAILURE,
// never a silent skip; ad-hoc local runs without nix simply skip the module-eval describe above with no
// extra noise-floor test.
if (!NIX && process.env.TALLY_REQUIRE_NIX === "1") {
  test("nix is required for the module-eval suite (TALLY_REQUIRE_NIX=1) but is not on PATH", () => {
    throw new Error("nix required for the module-eval tests but not found on PATH (TALLY_REQUIRE_NIX=1)");
  });
}
