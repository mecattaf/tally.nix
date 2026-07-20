# nix/hm-module.nix — homeManagerModules.tally: the PRIMARY, load-bearing module
# (IMPLEMENTATION-PLAN M3.3 nix-module; SPEC "Flake outputs"). This file OVERWRITES the layer-0
# scaffold placeholder (the empty-body module the scaffold created so the flake evaluated at
# layer 0 — the "scaffold creates, nix-module overwrites" handoff, M0.1/M3.3/plan-risk 12).
#
# "Typed options in → generated artifacts out" (microvm.nix *shape*, never a dependency; FS§5).
# Everything tally owns is USER-lifecycle, so home-manager is primary: systemd user units (the
# daemon on conductor; the drain timer+oneshot; the pls broker units + rendered pool config; the
# lease-epoch backstop), the ambient pls-lease-wrap on PATH, the config.json the daemon reads, the
# cooperative-hook install (home-manager activation), and the CLI on PATH. It ships NO
# zmx/receiver/kitty config — that substrate is dotfiles-owned (SPEC "Flake outputs").
#
# The option surface is MINIMAL (PS#17): enable · role · conductorHost · sessions · package, plus
# the two options concrete need already pulls on — `watcherScript` (read-only export of the kitty
# watcher store path, DECISIONS Q4) and `intake.gh` (wired, shipped OFF) — plus the pool/broker/
# detector/drain rendering options `units.nix` needs to emit the units. Where the docs define no
# type (`sessions`, `plsBroker` port math), this module rules a provisional minimal one, flagged
# for Tom's re-ruling exactly like the detector flags 4/5 (plan §1 / risk 11) — pure daemon-config,
# no protocol bump.
#
# No vendor code (clean-room, CLI-SURFACE §4).

{ config, lib, pkgs, ... }:

let
  cfg = config.services.tally;

  poolType = lib.types.submodule ({ ... }: {
    options = {
      name = lib.mkOption {
        type = lib.types.str;
        description = "Pool name (e.g. worker-gpu, controller-gpu). Matches the src/contracts Pool enum.";
      };
      broker = lib.mkOption {
        type = lib.types.str;
        default = "localhost";
        description = ''
          The pls broker address serving this pool. `localhost` ⇒ this module generates a local
          broker unit; a remote address (worker over TB3/tailnet, DECISIONS Q9) ⇒ the broker runs on
          the other box and NO unit is generated here (boundary). Never a frozen hostname.
        '';
      };
      priority = lib.mkOption {
        type = lib.types.int;
        default = 0;
        description = "Client-side ordering hint — LOWER is served first when both pools are eligible.";
      };
      capacity = lib.mkOption {
        type = lib.types.int;
        default = 1;
        description = "Single-lease-per-pool capacity (PS#5 PLS_CAPACITY=1); v0 is always 1.";
      };
      budgetGb = lib.mkOption {
        type = lib.types.int;
        default = 128;
        description = "VRAM-GB budget the pls `--cost` admission math runs against.";
      };
    };
  });

  # The default two GPU pools tally declares day-1 (SPEC "The pools"; mirrors src/pls/pools.ts):
  # worker-gpu PRIORITIZED (priority 0), controller-gpu (priority 1). Both local by default (dev /
  # single-box); the operator repoints `worker-gpu.broker` at the worker box in real deployment.
  defaultPools = [
    { name = "worker-gpu"; broker = "localhost"; priority = 0; capacity = 1; budgetGb = 128; }
    { name = "controller-gpu"; broker = "localhost"; priority = 1; capacity = 1; budgetGb = 128; }
  ];

  units = import ./units.nix { inherit lib pkgs cfg; };

  # Any pool wanting a LOCAL broker (broker == localhost) needs a broker command or source.
  hasLocalBroker = units.localPools != [ ];

  # Seed for a create-if-missing ~/.taskrc (see home.activation below). taskwarrior 3.x ABORTS
  # before running ANY command when ~/.taskrc is absent, which breaks tally's row-backed enqueues
  # (the daemon shells out to `task`). tally only needs the file to EXIST — it does NOT own the
  # content — so the seed is just a 2-line `#`-comment header (verified: taskrc comment syntax is
  # `#`) identifying the file's origin. A hand-edited taskrc is never overwritten (the activation
  # guards on `[ ! -e ]`; this is deliberately NOT home.file, which would clobber).
  taskrcSeed = pkgs.writeText "tally-taskrc-seed" ''
    # ~/.taskrc — created by the tally home-manager module because taskwarrior 3.x refuses to run without it.
    # tally does NOT own this file: it only ensures existence. Edit freely — rebuilds never overwrite it.
  '';
in
{
  # ------------------------------------------------------------------------------------------------
  # Option surface (SPEC "Module option surface" — minimal, PS#17).
  # ------------------------------------------------------------------------------------------------
  options.services.tally = {
    enable = lib.mkEnableOption "tally agent-session orchestration daemon + CLI";

    role = lib.mkOption {
      type = lib.types.enum [ "conductor" "receiver" ];
      default = "conductor";
      description = ''
        Whether the daemon runs here (`conductor`) or this host is a `receiver` (CLI + wrap only, no
        daemon/drain/broker units). SPEC "Module option surface": the daemon runs only on conductor.
      '';
    };

    conductorHost = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        Where clients reach the daemon. Pure configuration — no hostname is frozen anywhere in tally
        (DECISIONS Q9). REQUIRED when `enable` is set (asserted below).
      '';
    };

    sessions = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = ''
        Provisional (re-rulable, plan §1 flags-4/5 discipline / risk 11): a list of zmx
        session-name globs the daemon scopes discovery to. `[]` = observe ALL enumerated sessions.
        Rendered to config.sessions, read by src/model/discovery.ts. Pure daemon-config; Tom may
        narrow/redefine (e.g. per-workspace maps) without a protocol bump.
      '';
    };

    package = lib.mkOption {
      type = lib.types.package;
      description = "The tally package (one Bun binary: daemon + CLI). Defaulted by the flake to packages.tally.";
    };

    plsClient = lib.mkOption {
      type = lib.types.package;
      description = ''
        The `pls` GPU-lease CLIENT package (the tally-owned shim mapping tally's broker CLI onto the
        upstream broker's HTTP API). Defaulted by the flake to packages.pls. It is added to the
        `pls-lease-wrap` runtimeInputs AND to home.packages on BOTH roles so the AMBIENT wrap
        (`tally pls-wrap`, which resolves `pls` from PATH and acquires DIRECTLY, no daemon) always
        finds its client — regardless of the caller's shell PATH or whether daemon units exist.
      '';
    };

    watcherScript = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      # The store path is the option's DEFAULT (resolved from this module's own tree), NOT a config
      # definition: a readOnly option rejects ANY definition (mkDefault included), so the flake used to
      # DEFINE this and every consumer READ threw "read-only, but it's set multiple times". Declaring
      # the value as the default here — and removing the flake's definition — makes the export usable:
      # `readOnly` still forbids a consumer from OVERRIDING it, but the default is not a definition.
      default = ../hooks/kitty/tally-watcher.py;
      description = ''
        Read-only export of the kitty watcher store path (`hooks/kitty/tally-watcher.py`) so the
        dotfiles-owned kitty.conf `watcher` registration line never rots (DECISIONS Q4). Consumers
        READ `config.services.tally.watcherScript`; they never set it (readOnly). The module ships NO
        kitty.conf line itself (boundary).
      '';
      readOnly = true;
    };

    intake.gh = {
      enable = lib.mkEnableOption "gh intake (wired but OFF by default; PS#21/DECISIONS Q8)";
      sources = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        description = "Opt-in gh intake sources (empty = none). Rendered to config.intake.gh.sources.";
      };
    };

    # --- pool / broker rendering (tally owns pls config, PS#5/OUV-CM) ---------------------------
    pools = lib.mkOption {
      type = lib.types.listOf poolType;
      default = defaultPools;
      description = ''
        The pls pools tally declares (SPEC "The pools"; mirrors src/pls/pools.ts). Rendered to
        config.pools (broker/priority/capacity read by the daemon) and, for LOCAL pools, into a
        systemd user broker unit. Defaults to the two GPU pools (worker-gpu prioritized,
        controller-gpu).
      '';
    };

    plsBroker = {
      command = lib.mkOption {
        type = lib.types.nullOr (lib.types.listOf lib.types.str);
        default = null;
        description = ''
          ExecStart argv for a local pls broker unit (the pls SERVER, `server/app.py` — NOT the
          `bin/pls` client the scaffold's `packages.pls` wraps). When null, derived from `source`.
          The flake wires this (or `source`) so the module never guesses a derivation. One of
          `command`/`source` is required when any pool is local (asserted below).
        '';
      };
      source = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        description = ''
          Path to the `flake = false` pls source tree (the flake wires this to `inputs.pls`). When
          `command` is null and a local broker is needed, the broker ExecStart is
          `python3 <source>/server/app.py`.
        '';
      };
      host = lib.mkOption {
        type = lib.types.str;
        default = "127.0.0.1";
        description = "PLS_HOST bind address for local broker units (loopback by default).";
      };
      basePort = lib.mkOption {
        type = lib.types.port;
        default = 5555;
        description = ''
          Base PLS_PORT for local brokers; each pool binds `basePort + priority` so the two default
          pools land on distinct ports (worker 5555, controller 5556). Provisional (re-rulable).
        '';
      };
    };

    drain = {
      interval = lib.mkOption {
        type = lib.types.str;
        default = "5min";
        description = "OnUnitActiveSec for the drain timer (systemd time span).";
      };
      onBootSec = lib.mkOption {
        type = lib.types.str;
        default = "1min";
        description = "OnBootSec for the drain timer (first drain after boot).";
      };
    };

    detector = {
      workingPollMs = lib.mkOption {
        type = lib.types.int;
        default = 2000;
        description = "Scrape poll cadence (ms) while a pane's agent is `working` (plan §1 flag 4, provisional).";
      };
      idlePollMs = lib.mkOption {
        type = lib.types.int;
        default = 10000;
        description = "Fallback scrape poll cadence (ms) otherwise (plan §1 flag 4, provisional).";
      };
    };

    heartbeatMs = lib.mkOption {
      type = lib.types.int;
      default = 15000;
      description = "Idle-connection heartbeat cadence (ms; CLI-SURFACE §2 ~15s).";
    };

    runtimeInputs = lib.mkOption {
      type = lib.types.listOf lib.types.package;
      default = [ ];
      description = ''
        Extra packages placed on the daemon/drain units' PATH for the daemon's shell-outs — `task`
        (pinned taskwarrior 3.x via pkgs.taskwarrior3), the `pls` client (scaffold `packages.pls`
        wrap), `gh`, `git`, `python3`, plus the tally binary itself. `zmx` and `kitty` are ambient
        (dotfiles-owned substrate, boundary). The flake supplies sensible defaults.
      '';
    };

    installHooks = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Whether home-manager activation runs `tally hooks install` (the module-owned cooperative-hook
        installer — CLI-SURFACE §5 flag 2; SPEC boundary "tally SHIPS"). Idempotent + cooperative
        (merges, never clobbers foreign hooks). Off ⇒ the operator installs hooks manually.
      '';
    };
  };

  # ------------------------------------------------------------------------------------------------
  # Generated artifacts.
  # ------------------------------------------------------------------------------------------------
  config = lib.mkIf cfg.enable {
    assertions = [
      {
        # SPEC "Module option surface" + src/contracts/config.ts cross-field check: a conductor MUST
        # know where clients reach it.
        assertion = cfg.conductorHost != null && cfg.conductorHost != "";
        message = "services.tally.conductorHost is required when services.tally.enable is set (DECISIONS Q9).";
      }
      {
        # The daemon runs only on conductor (SPEC): a receiver enabling gh intake is meaningless
        # (intake is a daemon-hosted supervised loop, M2.4).
        assertion = cfg.role == "conductor" || !cfg.intake.gh.enable;
        message = "services.tally.intake.gh.enable requires role = \"conductor\" (the daemon hosts intake).";
      }
      {
        # A local broker unit needs an ExecStart — the flake must wire plsBroker.command OR .source.
        assertion = cfg.role != "conductor" || !hasLocalBroker || cfg.plsBroker.command != null || cfg.plsBroker.source != null;
        message = ''
          services.tally has local pls pools (broker = "localhost") but neither
          services.tally.plsBroker.command nor .source is set — the module cannot render a broker
          ExecStart. Wire one (the flake sets .source = inputs.pls).
        '';
      }
      {
        # Each local pool's broker binds basePort + priority; two local pools sharing a priority would
        # collide on one port (and the per-pool URL map would be ambiguous). Reject it explicitly.
        assertion = cfg.role != "conductor" || !units.hasPortCollision;
        message = ''
          services.tally has two local pls pools sharing a priority — their broker units would collide
          on the same port (plsBroker.basePort + priority). Give each local pool a distinct priority.
        '';
      }
    ];

    # The tally binary + the ambient pls-lease-wrap + the pls CLIENT on PATH, on BOTH roles (a
    # receiver still runs heavy tenants lease-wrapped and issues CLI verbs to the remote conductor;
    # the pls client must be present so the ambient wrap's DIRECT acquire resolves it).
    home.packages = [ cfg.package units.plsLeaseWrap cfg.plsClient ];

    # The nix-rendered config the daemon/CLI read at $XDG_CONFIG_HOME/tally/config.json (both roles;
    # a receiver's CLI reads conductorHost from it). `.source` links the generated store path.
    xdg.configFile."tally/config.json".source = units.configJson;

    # Systemd USER units (linger-compatible, WantedBy=default.target) — conductor ONLY (SPEC: the
    # daemon runs only on conductor). A receiver gets none.
    systemd.user.services = lib.mkIf (cfg.role == "conductor") units.services;
    systemd.user.targets = lib.mkIf (cfg.role == "conductor") units.targets;
    systemd.user.timers = lib.mkIf (cfg.role == "conductor") units.timers;

    home.activation = lib.mkMerge [
      # Ensure ~/.taskrc EXISTS (taskwarrior 3.x refuses to run without it — SPEC gpu-cooldown
      # smoke). Enable-gated only (the whole config block is lib.mkIf cfg.enable), on BOTH roles,
      # independent of installHooks: the daemon's `task` shell-outs need it regardless. Create-if-
      # missing (never clobber a hand-edited taskrc); the seed carries a `#`-comment origin header.
      {
        tallyProvisionTaskrc = lib.hm.dag.entryAfter [ "writeBoundary" ] ''
          if [ ! -e "$HOME/.taskrc" ]; then
            run install -m0644 ${taskrcSeed} "$HOME/.taskrc"
          fi
        '';
      }
      # Cooperative-hook install at home-manager activation (M3.2; idempotent, merges not clobbers).
      # Runs on both roles — a receiver may still host agent panes whose harness posts hook events to
      # the (remote) conductor's daemon. Guarded so activation never hard-fails if the binary is
      # momentarily absent.
      (lib.mkIf cfg.installHooks {
        tallyHooksInstall = lib.hm.dag.entryAfter [ "writeBoundary" ] ''
          if [ -x "${cfg.package}/bin/tally" ]; then
            run ${cfg.package}/bin/tally hooks install || true
          fi
        '';
      })
    ];
  };
}
