# nix/units.nix — the pure artifact-rendering library for the tally home-manager module
# (IMPLEMENTATION-PLAN M3.3 nix-module). NET-NEW file: it is the "typed options in → generated
# artifacts out" engine (microvm.nix *shape*, never a dependency; SPEC "Flake outputs").
#
# It takes the resolved `cfg` (the `services.tally` option values), `pkgs`, and `lib`, and returns
# every generated artifact the home-manager module wires:
#
#   • configJson        — the `$XDG_CONFIG_HOME/tally/config.json` the daemon reads at boot,
#                         rendered EXACTLY to the `TallyConfig` shape src/contracts/config.ts loads
#                         (role, conductorHost, sessions, pools[], intake.gh, detector, …).
#   • plsLeaseWrap      — the ambient `pls-lease-wrap` script package installed on PATH (SPEC "The
#                         ambient default"; matches src/pls/wrap.ts `renderWrapScript`: exec
#                         `tally pls-wrap -- "$@"`).
#   • daemonService     — `tally-daemon.service` (conductor only): ExecStart=`tally daemon run`,
#                         StandardOutput=journal + SyslogIdentifier=tally (the ruled emission path,
#                         SPEC "Emission path"), Restart=always, the runtime dir for the socket.
#   • drainService      — `tally-drain.service` oneshot (`tally daemon drain` — a thin socket
#                         client posting queue.drain; M2.5).
#   • drainTimer        — `tally-drain.timer` (Persistent=true; SPEC "Flake outputs").
#   • plsBrokerService  — the pls broker unit(s), one per LOCAL pool (broker == "localhost"), each
#                         carrying the rendered pool config as PLS_* env (tally owns pls config,
#                         PS#5/OUV-CM). Remote-brokered pools (worker over TB3/tailnet) run their
#                         broker on the OTHER box — this module never starts a broker for them.
#
# All units are systemd USER units (linger-compatible, WantedBy=default.target). The daemon unit
# exists ONLY on the conductor role (SPEC "Module option surface": the daemon runs only on
# conductor); a receiver gets the CLI + wrap on PATH and no daemon/drain/broker units.
#
# No zmx/receiver/kitty config is rendered here (the boundary — that substrate is dotfiles-owned,
# SPEC "Flake outputs"). No vendor code (clean-room, CLI-SURFACE §4).

{ lib, pkgs, cfg }:

let
  inherit (lib) nameValuePair filter;

  tallyBin = "${cfg.package}/bin/tally";

  # The daemon/drain unit PATH. Built by JOINING NON-EMPTY parts (so an empty runtimeInputs default
  # does not produce a leading `:` — a zero-length PATH element that POSIX resolves as the current
  # directory, a quiet binary-hijack surface). Includes the PER-USER home-manager profile dirs
  # (/etc/profiles/per-user/%u/bin and %h/.nix-profile/bin) AHEAD of the system profile so the
  # dotfiles/HM-installed AMBIENT binaries (kitty, zmx) the daemon shells out to are actually resolved
  # — the wholesale PATH override otherwise discards the user manager's PATH, ENOENT-ing them.
  unitPath = lib.concatStringsSep ":" (lib.filter (s: s != "") [
    (lib.makeBinPath cfg.runtimeInputs)
    "/etc/profiles/per-user/%u/bin"
    "%h/.nix-profile/bin"
    "/run/current-system/sw/bin"
    "/run/wrappers/bin"
  ]);

  # --- config.json ------------------------------------------------------------------------------
  # Rendered to the exact `TallyConfig` shape src/contracts/config.ts::loadConfig accepts. Every key
  # here is a documented option (or a compiled default the loader also carries); unknown keys are
  # ignored by the loader, but we emit none. `daemonVersion` mirrors the package version so the
  # snapshot's `daemon_version` matches the shipped binary.
  poolsJson = map
    (p: {
      name = p.name;
      broker = p.broker;
      priority = p.priority;
      capacity = p.capacity;
    })
    cfg.pools;

  tallyConfig = {
    role = cfg.role;
    conductorHost = cfg.conductorHost;
    sessions = cfg.sessions;
    pools = poolsJson;
    intake = {
      gh = {
        enable = cfg.intake.gh.enable;
        sources = cfg.intake.gh.sources;
      };
    };
    detector = {
      working_poll_ms = cfg.detector.workingPollMs;
      idle_poll_ms = cfg.detector.idlePollMs;
    };
    daemonVersion = cfg.package.version or "0.1.0";
    heartbeatMs = cfg.heartbeatMs;
  };

  configJson = (pkgs.formats.json { }).generate "tally-config.json" tallyConfig;

  # --- pls-lease-wrap ---------------------------------------------------------------------------
  # The ambient GPU-lease wrapper installed on PATH. Byte-for-byte the shape src/pls/wrap.ts
  # `renderWrapScript(tallyBin)` emits: `exec <tally> pls-wrap -- "$@"`. Rendered here in Nix (build
  # time) rather than shelling the binary, so the store path is pure. Any invocation prefixed with
  # `pls-lease-wrap` is lease-gated WITHOUT the caller importing tally (SPEC "The ambient default").
  plsLeaseWrap = pkgs.writeShellApplication {
    name = "pls-lease-wrap";
    # The wrap acquires DIRECTLY (src/pls/wrap.ts: no daemon), and broker.ts resolves `pls` from PATH,
    # so the wrap must carry its own pls client — a caller's ambient shell has no `pls` otherwise.
    runtimeInputs = [ cfg.plsClient ];
    text = ''
      # pls-lease-wrap — tally's ambient GPU-lease wrapper (SPEC "The ambient default").
      # Every heavy (GPU-touching) invocation prefixed with this runs under a pls lease,
      # so a subagent is tally-compatible without knowing tally exists. Owned by the tally
      # module (tally owns pls's pool config, PS#5). Do not edit by hand — it is generated.
      exec ${tallyBin} pls-wrap -- "$@"
    '';
  };

  # --- lease-epoch backstop ---------------------------------------------------------------------
  # PS#21: `lease_epoch` = the pls lease generation as PRIMARY source, backstopped by a persisted
  # counter file (`$XDG_STATE_HOME/tally/epoch`) so the epoch stays monotone across an unclean
  # reboot. The DAEMON is the sole owner of that file's increment (src/daemon/epoch.ts::bumpEpoch —
  # atomic temp-then-rename write, strictly monotone, adopts a higher pls generation when offered).
  # There is deliberately no separate ExecStartPre incrementer here: one used to run alongside
  # epoch.ts's own boot-time bump, so a single restart consumed TWO epoch values — the file an
  # external reader saw (ExecStartPre's write) never matched the value the daemon announced on the
  # wire (issue #9). epoch.ts already handles "run outside systemd" (dev rig, tests, `tally daemon
  # run` by hand) by itself, so it is also correct under systemd — StateDirectory="tally" (below)
  # is all systemd needs to contribute (the state dir must exist before the daemon's first write;
  # epoch.ts's own `mkdirSync` would otherwise race a bare `tally daemon run` outside systemd, but
  # under the unit StateDirectory= already guarantees it).

  # --- pls broker command -----------------------------------------------------------------------
  # The broker is the pls SERVER (`server/app.py`), NOT the `bin/pls` client the scaffold-owned
  # `packages.pls` wraps. When `cfg.plsBroker.command` is set (the flake wires it to a broker
  # entrypoint / the pls source's server), we use it verbatim; otherwise we derive it from
  # `cfg.plsBroker.source` (the `flake = false` pls source path) as `python3 <source>/server/app.py`.
  # One of the two MUST be provided when a local broker unit is generated — asserted in hm-module.nix.
  brokerCommandFor = _pool:
    if cfg.plsBroker.command != null then
      cfg.plsBroker.command
    else
      [ "${pkgs.python3}/bin/python3" "${toString cfg.plsBroker.source}/server/app.py" ];

  # PLS_* env for a pool's broker (README env table): capacity, budget, port, label, unit. Priority
  # ordering is a client-side hint (tally's own queueing), so it is NOT a broker env — the broker
  # only enforces capacity+budget. One broker process per LOCAL pool, each on its own port.
  brokerEnvFor = p: {
    PLS_HOST = cfg.plsBroker.host;
    PLS_PORT = toString (cfg.plsBroker.basePort + p.priority);
    PLS_CAPACITY = toString p.capacity;
    PLS_BUDGET = toString p.budgetGb;
    PLS_LABEL = p.name;
    PLS_UNIT = "GB";
  };

  # The ONE local-broker predicate, shared by both the unit-generation gate and the client URL map so
  # the two sides can never disagree on what "local" means (previously units.nix counted "127.0.0.1"
  # local while the client did not, leaving a useless PLS_BROKER env for it).
  isLocalBroker = b: b == "localhost" || b == "" || b == "127.0.0.1";

  # Pools whose broker runs on THIS box. A remote-brokered pool (worker over TB3/tailnet) has its
  # broker on the other box — we never start a unit for it here (boundary).
  localPools = filter (p: isLocalBroker p.broker) cfg.pools;

  # The per-pool broker URL the CLIENT targets, resolved END-TO-END so the daemon actually reaches the
  # right broker: a local pool → 127.0.0.1:<basePort + priority> (the same port the pool's broker unit
  # binds), a remote pool → http://<broker-host>:<basePort + priority>. Rendered into the daemon/drain
  # units as PLS_POOL_URLS (a JSON pool→url map the pls client shim reads), so controller-gpu traffic
  # reaches the controller broker, not the worker's, and a remote pool is governed against its own box.
  poolUrlFor = p:
    let
      port = cfg.plsBroker.basePort + p.priority;
      host = if isLocalBroker p.broker then "127.0.0.1" else p.broker;
    in
    "http://${host}:${toString port}";
  poolUrls = builtins.listToAttrs (map (p: nameValuePair p.name (poolUrlFor p)) cfg.pools);
  poolUrlsJson = builtins.toJSON poolUrls;

  # Two pools sharing a priority would collide on one broker port (basePort + priority). Assert against
  # it so the port derivation stays unambiguous.
  localPriorities = map (p: p.priority) localPools;
  hasPortCollision = (lib.length (lib.unique localPriorities)) != (lib.length localPriorities);

  # --- systemd user units -----------------------------------------------------------------------
  daemonService = {
    Unit = {
      Description = "tally — agent-session orchestration daemon (conductor)";
      # Ordered after the broker units so a lease is servable the moment the daemon boots; a soft
      # want, never a hard require — the daemon runs fine (degraded) if a broker is down.
      After = [ "tally-pls-broker.target" ];
      Wants = [ "tally-pls-broker.target" ];
    };
    Service = {
      Type = "simple";
      # No ExecStartPre epoch-increment here (issue #9 — see "lease-epoch backstop" above): the
      # daemon (epoch.ts::bumpEpoch) is the SOLE owner of the counter-file increment, so the file
      # and the announced `lease_epoch` always agree.
      ExecStart = "${tallyBin} daemon run";
      Restart = "always";
      RestartSec = 2;
      # The ruled emission path (SPEC "Emission path", DECISIONS jul9): the daemon writes the
      # structured TALLY_* fields to STDOUT; StandardOutput=journal captures them under
      # SyslogIdentifier=tally. NOT a native journal-socket client.
      StandardOutput = "journal";
      StandardError = "journal";
      SyslogIdentifier = "tally";
      # The socket lives under $XDG_RUNTIME_DIR/tally (mode 0700 dir; the socket itself is 0600,
      # enforced by the daemon). RuntimeDirectory is relative to $XDG_RUNTIME_DIR for user units.
      RuntimeDirectory = "tally";
      RuntimeDirectoryMode = "0700";
      # State (epoch counter, events/ drop dir) and data (witness ledger) dirs, created + owned.
      StateDirectory = "tally";
      # PATH for the daemon's shell-outs (task/pls/gh/git/journalctl/systemd-run/kitty/zmx). zmx and
      # kitty are ambient (dotfiles-owned substrate, resolved via the per-user profile dirs in
      # unitPath); task/pls/gh/git/journalctl are pinned via runtimeInputs. PLS_POOL_URLS carries the
      # per-pool broker URL map so every pls call reaches the correct broker (per-pool port).
      Environment = [
        "PATH=${unitPath}"
        "PLS_POOL_URLS=${poolUrlsJson}"
      ];
    };
    Install = {
      WantedBy = [ "default.target" ];
    };
  };

  drainService = {
    Unit = {
      Description = "tally — events/ + TW re-present drain (thin socket client → queue.drain)";
      # The oneshot is a THIN SOCKET CLIENT (M2.5): it posts queue.drain and exits; the DAEMON does
      # the sweep. If the daemon is down the client fails (non-zero) and the timer retries next tick.
      After = [ "tally-daemon.service" ];
      Requisite = [ "tally-daemon.service" ];
    };
    Service = {
      Type = "oneshot";
      ExecStart = "${tallyBin} daemon drain";
      StandardOutput = "journal";
      StandardError = "journal";
      SyslogIdentifier = "tally";
      Environment = [
        "PATH=${unitPath}"
        "PLS_POOL_URLS=${poolUrlsJson}"
      ];
    };
  };

  drainTimer = {
    Unit = {
      Description = "tally — periodic events/ + TW re-present drain";
    };
    Timer = {
      OnBootSec = cfg.drain.onBootSec;
      OnUnitActiveSec = cfg.drain.interval;
      # Persistent=true (SPEC "Flake outputs"): a tick missed while the machine was off fires on the
      # next boot, so a durable row queued during downtime is re-presented promptly.
      Persistent = true;
      Unit = "tally-drain.service";
    };
    Install = {
      WantedBy = [ "timers.target" ];
    };
  };

  # One broker service per local pool, named `tally-pls-broker-<pool>`. Each PartOf the
  # `tally-pls-broker.target` the daemon orders after, so `systemctl --user start
  # tally-pls-broker.target` brings all local brokers up together.
  plsBrokerServices = lib.listToAttrs (map
    (p: nameValuePair "tally-pls-broker-${p.name}" {
      Unit = {
        Description = "pls broker — pool ${p.name} (capacity=${toString p.capacity}, budget=${toString p.budgetGb}GB)";
        PartOf = [ "tally-pls-broker.target" ];
      };
      Service = {
        Type = "simple";
        ExecStart = lib.escapeShellArgs (brokerCommandFor p);
        Restart = "always";
        RestartSec = 2;
        Environment = lib.mapAttrsToList (k: v: "${k}=${v}") (brokerEnvFor p);
        StandardOutput = "journal";
        StandardError = "journal";
        SyslogIdentifier = "tally-pls";
      };
      Install = {
        WantedBy = [ "tally-pls-broker.target" ];
      };
    })
    localPools);

  # The grouping target the daemon orders after and the broker services attach to.
  plsBrokerTarget = {
    Unit = {
      Description = "tally — all local pls broker units";
    };
    Install = {
      WantedBy = [ "default.target" ];
    };
  };

in
{
  inherit
    configJson
    plsLeaseWrap
    daemonService
    drainService
    drainTimer
    plsBrokerServices
    plsBrokerTarget
    localPools
    hasPortCollision
    poolUrls
    ;

  # The full systemd.user.services attrset the daemon/drain/broker units compose into (conductor
  # only — the hm-module gates this behind role == "conductor").
  services = {
    tally-daemon = daemonService;
    tally-drain = drainService;
  } // plsBrokerServices;

  targets = {
    tally-pls-broker = plsBrokerTarget;
  };

  timers = {
    tally-drain = drainTimer;
  };
}
