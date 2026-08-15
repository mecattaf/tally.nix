self:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.tally;
  common = import ./common.nix { inherit self lib pkgs; };
  checkedConfig = common.mkCheckedConfig cfg;
  installedPackage = common.mkInstalledPackage cfg;
  witnessEmitter = common.mkWitnessEmitter cfg;
  configPath = "/etc/tally/config.json";
  socketPath = "/run/tally/tally.sock";
  eventsDir = "${toString cfg.stateDir}/events";
  captureArchiveDir = "${toString cfg.stateDir}/capture/archive";
  daemonArgv = [
    "${cfg.package}/bin/tally"
    "--config"
    configPath
    "--socket"
    socketPath
    "daemon"
    "run"
    "--state-dir"
    (toString cfg.stateDir)
    "--data-dir"
    (toString cfg.dataDir)
    "--yield-grace-sec"
    (toString cfg.lease.yieldGraceSec)
  ];
  daemonWrapper = pkgs.writeShellScript "tally-daemon" ''
    export XDG_RUNTIME_DIR="/run/user/$(${pkgs.coreutils}/bin/id -u)"
    exec ${lib.escapeShellArgs daemonArgv}
  '';
  campaignPollProgram = pkgs.writeShellApplication {
    name = "tally-campaign-poll";
    runtimeInputs = [
      pkgs.git
      pkgs.nix
    ];
    text = ''
      exec ${
        lib.escapeShellArgs [
          "${cfg.package}/bin/tally"
          "--config"
          configPath
          "--socket"
          socketPath
          "campaign"
          "poll"
          "--once"
          "--state-dir"
          (toString cfg.stateDir)
        ]
      }
    '';
  };

  unsupportedConfigAssertions = [
    {
      assertion = cfg.producers == { };
      message = "services.tally.producers must be empty in the NixOS module; configure producers with the Home Manager module (tally.homeManagerModules.tally)";
    }
    {
      assertion = cfg.flows == { };
      message = "services.tally.flows must be empty in the NixOS module; configure flows with the Home Manager module (tally.homeManagerModules.tally)";
    }
    {
      assertion = lib.all (pool: pool.usageMeter == null) (builtins.attrValues cfg.pools);
      message = "services.tally.pools.<name>.usageMeter must be null in the NixOS module; configure usage meters with the Home Manager module (tally.homeManagerModules.tally)";
    }
  ];
in
{
  options.services.tally =
    common.mkOptions {
      defaultPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.tally;
      defaultDataDir = "/var/lib/tally/data";
      defaultStateDir = "/var/lib/tally/state";
    }
    // {
      user = lib.mkOption {
        type = lib.types.str;
        default = "tally";
        description = "Dedicated unprivileged account that runs the tally system service and jobs.";
      };
      group = lib.mkOption {
        type = lib.types.str;
        default = "tally";
        description = "Primary group for the dedicated tally system service account.";
      };
    };

  config = lib.mkMerge [
    {
      services.tally.adapters = common.adapterDefaults;
      assertions = unsupportedConfigAssertions ++ [
        # A job carries no limit nobody declared (vestige-sweep V-1): the
        # daemon argv renders no job-limit flags at all. This evaluation-time
        # assertion pins the flag set the argv may carry, so no limit flag
        # can ride back into the unit rendering; the module-layer flake
        # check forces this module's config, so it grades under
        # checks.<system>.module-layer.
        {
          assertion =
            builtins.filter (token: lib.hasPrefix "--" token) daemonArgv == [
              "--config"
              "--socket"
              "--state-dir"
              "--data-dir"
              "--yield-grace-sec"
            ];
          message = "services.tally: the daemon argv must carry no flag beyond the sanctioned set — job limits are declared per job or not at all (vestige-sweep V-1)";
        }
      ];
    }
    # The generic runtime contract shared with Home Manager: resource pools,
    # driver adapter, and fanout floor. `tally campaign arm` validates this host
    # surface before it spends agent time; campaign policy comes from the
    # committed worklist rather than from module declarations.
    { services.tally = common.mkCampaignRuntimeConfig cfg; }
    (lib.mkIf cfg.enable {
      assertions = common.mkAssertions cfg;

      environment.systemPackages = [
        installedPackage
        witnessEmitter
      ];
      environment.etc."tally/config.json".source = checkedConfig;

      # The deployment's data directory in the login environment as well as in
      # the units below (#416). The units are given `--data-dir` explicitly and
      # never needed it; the operator's own shell is where an omitted
      # `--data-dir` used to resolve to `~/.local/share/tally` and turn
      # `reader-state archive` into a silent no-op with a success message.
      # This store is mode 0700 and owned by `cfg.user`, so an operator who is
      # not that user now gets a refusal naming the path instead — which is the
      # point: the failure becomes visible. `mkDefault`, so a host that wants a
      # different value keeps it.
      environment.variables.TALLY_DATA_DIR = lib.mkDefault (toString cfg.dataDir);

      users.groups.${cfg.group} = { };
      users.users.${cfg.user} = {
        isSystemUser = true;
        group = cfg.group;
        linger = true;
      };

      system.activationScripts.tallyRuntimeDirectories = {
        deps = [ "users" ];
        text = ''
          ${lib.escapeShellArgs [
            "${pkgs.coreutils}/bin/install"
            "-d"
            "-m"
            "0700"
            "-o"
            cfg.user
            "-g"
            cfg.group
            "/var/lib/tally"
            (toString cfg.dataDir)
            (toString cfg.stateDir)
            eventsDir
            captureArchiveDir
          ]}
          ${lib.escapeShellArgs [
            "${pkgs.coreutils}/bin/chown"
            "--recursive"
            "--no-dereference"
            "${cfg.user}:${cfg.group}"
            (toString cfg.dataDir)
            (toString cfg.stateDir)
          ]}
        '';
      };

      systemd.services.tally-daemon = {
        description = "tally contention and proof system daemon";
        after = [ "network.target" ];
        wantedBy = [ "multi-user.target" ];
        restartTriggers = [ checkedConfig ];
        unitConfig.ConditionPathExists = configPath;
        serviceConfig = {
          Type = "notify";
          User = cfg.user;
          Group = cfg.group;
          NotifyAccess = "main";
          # The daemon derives its liveness budgets from this period: it pings
          # every WatchdogSec/4, reports a dispatch loop that has not come back
          # around for 2x WatchdogSec, and stops pinging at 10x, after which
          # systemd takes one more period to restart. Moving this number moves
          # all four; daemon::notify pins them at 30s.
          WatchdogSec = "30s";
          # Startup is charged here and never to WatchdogSec, because the
          # service watchdog is not armed until READY=1. The daemon now sends
          # EXTEND_TIMEOUT_USEC= at every startup phase boundary, so this is
          # the budget for one phase rather than for the whole of Daemon::open;
          # it matches daemon::startup::STARTUP_PHASE_BUDGET. Declared rather
          # than inherited so the limit is a choice this module made, not
          # whichever DefaultTimeoutStartSec the manager happens to carry.
          TimeoutStartSec = "90s";
          Restart = "always";
          RestartSec = "2s";
          Environment = [
            "TALLY_CONFIG_GENERATION=${checkedConfig}"
            "TALLY_NIX_PROGRAM=${pkgs.nix}/bin/nix"
            "TALLY_NIX_STORE_PROGRAM=${pkgs.nix}/bin/nix-store"
            # The deployment's data directory, exported wherever this module
            # configures one (#416): a direct-file verb that resolves its
            # default through TALLY_DATA_DIR aims at this store instead of
            # creating a fresh one wherever its XDG fallback lands.
            "TALLY_DATA_DIR=${toString cfg.dataDir}"
          ];
          RuntimeDirectory = "tally";
          RuntimeDirectoryMode = "0700";
          StateDirectory = "tally";
          StateDirectoryMode = "0700";
          LogsDirectory = "tally";
          LogsDirectoryMode = "0700";
          # Adapter write grants must exist before a hardened transient job
          # starts, so create both campaign-owned state paths up front.
          ExecStartPre = lib.escapeShellArgs [
            "${pkgs.coreutils}/bin/install"
            "-d"
            "-m"
            "0700"
            (toString cfg.dataDir)
            (toString cfg.stateDir)
            eventsDir
            captureArchiveDir
          ];
          ExecStart = daemonWrapper;
          # Ruled backstop (vestige-sweep V-12), not a job cap: the daemon is a
          # small process, and if it ever reaches this limit the recovery stays
          # legible — Restart=always brings it back and the 30s watchdog bounds
          # the wedged interval. The CPUWeight line was deleted as a restatement
          # of systemd's own default weight; job units carry no limit nobody
          # declared (V-1) and receive no limit flags in the argv above.
          MemoryMax = "8G";
          UMask = "0077";
          NoNewPrivileges = true;
          PrivateTmp = true;
          ProtectSystem = "strict";
          ReadWritePaths = [
            (toString cfg.dataDir)
            (toString cfg.stateDir)
          ];
          RestrictAddressFamilies = [
            "AF_UNIX"
          ]
          ++ lib.optionals (cfg.executors != { }) [
            "AF_INET"
            "AF_INET6"
          ];
          SystemCallFilter = [ "@system-service" ];
        };
      };

      systemd.services."tally-witness-emit@" = {
        description = "append an advisory tally attestation for %i";
        serviceConfig = {
          Type = "oneshot";
          User = cfg.user;
          Group = cfg.group;
          ExecStart = lib.escapeShellArgs [
            "${witnessEmitter}/bin/tally-witness-emit"
            "%i"
          ];
          Environment = [
            "TALLY_ATTESTATION_LEDGER=${toString cfg.dataDir}/attestations.jsonl"
            "TALLY_DATA_DIR=${toString cfg.dataDir}"
          ];
          UMask = "0077";
          NoNewPrivileges = true;
          ProtectSystem = "strict";
          ReadWritePaths = [ (toString cfg.dataDir) ];
        };
      };

      systemd.services.tally-drain = {
        description = "drain tally producer event files";
        after = [ "tally-daemon.service" ];
        # `after` orders startup but says nothing about the daemon being gone.
        # The timer fires every five seconds and a daemon restart takes longer
        # than that, so an activation that restarts tally-daemon reliably
        # catches a drain mid-flight and turns a benign deploy into a unit
        # failure (#411). Conditioning on the socket the command actually
        # connects to makes that invocation a recorded *skip* instead. systemd
        # ANDs repeated conditions, so the config guard is kept, not replaced.
        unitConfig.ConditionPathExists = [
          configPath
          socketPath
        ];
        serviceConfig = {
          Type = "oneshot";
          User = cfg.user;
          Group = cfg.group;
          ExecStart = lib.escapeShellArgs [
            "${cfg.package}/bin/tally"
            "--socket"
            socketPath
            "daemon"
            "drain"
          ];
          UMask = "0077";
        };
      };

      systemd.timers.tally-drain = {
        description = "periodically drain tally producer event files";
        wantedBy = [ "timers.target" ];
        timerConfig = {
          OnActiveSec = "1s";
          OnUnitActiveSec = "5s";
          Unit = "tally-drain.service";
        };
      };

      systemd.services.tally-retention = lib.mkIf cfg.retention.enable {
        description = "prune expired tally GC roots, briefs, capture archives, and ingress event files";
        after = [ "tally-daemon.service" ];
        requires = [ "tally-daemon.service" ];
        serviceConfig = {
          Type = "oneshot";
          User = cfg.user;
          Group = cfg.group;
          TimeoutStartSec = "infinity";
          ExecStart = lib.escapeShellArgs (common.mkRetentionArgv cfg);
          Environment = [
            "PATH=${lib.makeBinPath [ pkgs.nix ]}"
            "TALLY_DATA_DIR=${toString cfg.dataDir}"
          ];
          UMask = "0077";
          NoNewPrivileges = true;
          PrivateTmp = true;
          ProtectSystem = "strict";
          ReadWritePaths = [
            (toString cfg.dataDir)
            (toString cfg.stateDir)
          ];
        };
      };

      systemd.timers.tally-retention = lib.mkIf cfg.retention.enable {
        description = "schedule tally store-evidence retention";
        wantedBy = [ "timers.target" ];
        timerConfig = {
          OnCalendar = cfg.retention.onCalendar;
          Persistent = true;
          Unit = "tally-retention.service";
        };
      };
    })
    (lib.mkIf cfg.enable {
      systemd.services.tally-campaign-poll = lib.mkIf cfg.campaignPoll.enable {
        description = "reconcile locally armed tally campaigns";
        after = [
          "network-online.target"
          "tally-daemon.service"
        ];
        # Polling reads the durable base and campaign refs from the configured
        # Git remote. It is local-forge orchestration, but it is not offline.
        wants = [ "network-online.target" ];
        requires = [ "tally-daemon.service" ];
        unitConfig.ConditionPathExists = [ configPath ];
        serviceConfig = {
          Type = "oneshot";
          User = cfg.user;
          Group = cfg.group;
          # The scan holds the registry lock while reconciling durable Git
          # state, so a wedged call would block interactive arm, disarm, and
          # list until this fires.
          TimeoutStartSec = cfg.campaignPoll.timeout;
          ExecStart = "${campaignPollProgram}/bin/tally-campaign-poll";
          UMask = "0077";
          NoNewPrivileges = true;
          PrivateTmp = true;
          ProtectSystem = "strict";
          ReadWritePaths = [ (toString cfg.stateDir) ];
          RestrictAddressFamilies = [
            "AF_UNIX"
            "AF_INET"
            "AF_INET6"
          ];
        };
      };

      systemd.timers.tally-campaign-poll = lib.mkIf cfg.campaignPoll.enable {
        description = "poll locally armed tally campaigns";
        wantedBy = [ "timers.target" ];
        timerConfig = {
          OnActiveSec = "15s";
          OnUnitActiveSec = cfg.campaignPoll.interval;
          Persistent = true;
          Unit = "tally-campaign-poll.service";
        };
      };
    })
  ];
}
