# test/nix/eval-hm.nix — a standalone evaluator for homeManagerModules.tally (nix/hm-module.nix),
# used by test/nix/hm-module.test.ts. It evaluates the module through lib.evalModules against a
# LIGHTWEIGHT stub that declares only the home-manager options the tally module sets
# (home.packages, home.activation, xdg.configFile, systemd.user.{services,targets,timers}), so the
# module's generated artifacts (units, config.json, assertions) can be inspected WITHOUT pulling
# home-manager itself as a flake input.
#
# The eval is `--impure` on <nixpkgs> and takes the caller's option overrides via `--arg args`.
# On an assertion failure (e.g. conductorHost null when enabled) evalModules throws — the bun test
# asserts on that throw. On success it returns a JSON-safe projection of the interesting artifacts.
#
# No vendor code (clean-room, CLI-SURFACE §4).

{ pkgs ? import <nixpkgs> { }
, module ? ../../nix/hm-module.nix
, args ? { }
}:

let
  lib = pkgs.lib;

  # A minimal `lib.hm.dag` shim so the module's `home.activation` entry evaluates the same way
  # home-manager's real dag would (entryAfter wraps the text with before/after deps). We only need
  # the produced attrset shape (`{ text, after, before }`) — the actual ordering is home-manager's.
  hmLib = lib // {
    hm = {
      dag = {
        entryAfter = after: text: { inherit text after; before = [ ]; data = text; };
        entryBefore = before: text: { inherit text before; after = [ ]; data = text; };
        entryAnywhere = text: { inherit text; after = [ ]; before = [ ]; data = text; };
      };
    };
  };

  # Stub declarations for the home-manager options the tally module writes into. Types are permissive
  # (attrs / listOf anything) so we never fight home-manager's real, richer types — we only need the
  # module's VALUES to land somewhere inspectable.
  hmStub = { lib, ... }: {
    options = {
      # `assertions` is normally declared by home-manager's assertions module; declare it here so
      # the tally module's `config.assertions = [ … ]` lands somewhere inspectable (we enforce them
      # in this evaluator, not evalModules).
      assertions = lib.mkOption {
        type = lib.types.listOf (lib.types.submodule {
          options = {
            assertion = lib.mkOption { type = lib.types.bool; };
            message = lib.mkOption { type = lib.types.str; };
          };
        });
        default = [ ];
      };
      warnings = lib.mkOption { type = lib.types.listOf lib.types.str; default = [ ]; };
      home.packages = lib.mkOption { type = lib.types.listOf lib.types.package; default = [ ]; };
      home.activation = lib.mkOption { type = lib.types.attrsOf lib.types.anything; default = { }; };
      xdg.configFile = lib.mkOption { type = lib.types.attrsOf lib.types.anything; default = { }; };
      systemd.user.services = lib.mkOption { type = lib.types.attrsOf lib.types.anything; default = { }; };
      systemd.user.targets = lib.mkOption { type = lib.types.attrsOf lib.types.anything; default = { }; };
      systemd.user.timers = lib.mkOption { type = lib.types.attrsOf lib.types.anything; default = { }; };
    };
  };

  # A tiny package the tally `package` option can default to in the eval (a real store path with a
  # `version` attr and a `bin/tally`, so the units' ExecStart is a plausible path).
  fakeTally = pkgs.writeShellApplication
    {
      name = "tally";
      text = "echo tally";
    } // { version = "0.1.0-test"; };

  overrideModule = { ... }: {
    config.services.tally = args;
  };

  evaluated = lib.evalModules {
    modules = [
      hmStub
      module
      overrideModule
      # Supply the required `package` + `plsClient` option defaults (the flake normally does this)
      # unless the caller overrode them.
      {
        config.services.tally.package = lib.mkDefault fakeTally;
        config.services.tally.plsClient = lib.mkDefault fakeTally;
      }
      # Thread the shimmed lib (with `hm.dag`) into every module's `lib` arg AND supply `pkgs`, so
      # the tally module's `home.activation` (lib.hm.dag.entryAfter) and `import ./units.nix
      # { inherit lib pkgs cfg; }` both resolve. Overriding `_module.args.lib` replaces the default
      # lib the module system passes to modules.
      { _module.args.lib = hmLib; _module.args.pkgs = pkgs; }
    ];
    specialArgs = { };
  };

  cfg = evaluated.config;
  tallyCfg = cfg.services.tally;

  # Assertions the module declared — evalModules does NOT enforce them (that is home-manager's
  # assertion-checking module), so we surface them and let the test decide.
  assertions = cfg.assertions or [ ];
  failed = builtins.filter (a: !a.assertion) assertions;

in
{
  # Whether any assertion failed + the messages (the test checks conductorHost/role gating here).
  assertionsPassed = failed == [ ];
  failedMessages = map (a: a.message) failed;

  # The systemd user units the module generated (names only — enough to assert daemon presence).
  serviceNames = builtins.attrNames (cfg.systemd.user.services or { });
  targetNames = builtins.attrNames (cfg.systemd.user.targets or { });
  timerNames = builtins.attrNames (cfg.systemd.user.timers or { });

  # The daemon unit's load-bearing fields (SPEC "Emission path": StandardOutput=journal +
  # SyslogIdentifier=tally; Restart=always; ExecStart), if present.
  daemon =
    let s = (cfg.systemd.user.services or { });
    in if s ? tally-daemon then {
      execStart = s.tally-daemon.Service.ExecStart;
      standardOutput = s.tally-daemon.Service.StandardOutput;
      syslogIdentifier = s.tally-daemon.Service.SyslogIdentifier;
      restart = s.tally-daemon.Service.Restart;
      # issue #9: no ExecStartPre epoch-increment (the daemon is the sole increment owner) — `or
      # null` so this stays readable now that the field is absent from the Service attrset.
      execStartPre = s.tally-daemon.Service.ExecStartPre or null;
      wantedBy = s.tally-daemon.Install.WantedBy;
    } else null;

  drain =
    let s = (cfg.systemd.user.services or { });
    in if s ? tally-drain then {
      execStart = s.tally-drain.Service.ExecStart;
      type = s.tally-drain.Service.Type;
    } else null;

  drainTimer =
    let t = (cfg.systemd.user.timers or { });
    in if t ? tally-drain then {
      persistent = t.tally-drain.Timer.Persistent;
      onUnitActiveSec = t.tally-drain.Timer.OnUnitActiveSec;
    } else null;

  # config.json content, re-read from the generated store path so the test can assert the
  # TallyConfig shape (role/conductorHost/sessions/pools/intake/detector).
  configJson =
    let f = (cfg.xdg.configFile or { });
    in if f ? "tally/config.json" then
      builtins.fromJSON (builtins.readFile f."tally/config.json".source)
    else null;

  # Whether the ambient pls-lease-wrap package is on home.packages (name match).
  packageNames = map (p: p.name or (p.pname or "")) (cfg.home.packages or [ ]);

  # The read-only watcherScript export FORCED to a string — this is the read that used to throw
  # "read-only, but it's set multiple times" (DECISIONS Q4). Forcing it proves the export is usable.
  watcherScript = toString tallyCfg.watcherScript;

  # The daemon unit's PATH + PLS_POOL_URLS env (as a list of "KEY=VALUE" strings), for the PATH-hygiene
  # + per-pool-broker-URL assertions.
  daemonEnv =
    let s = (cfg.systemd.user.services or { });
    in if s ? tally-daemon then (s.tally-daemon.Service.Environment or [ ]) else [ ];

  role = tallyCfg.role;
}
