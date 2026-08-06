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
  configPath = "${config.xdg.configHome}/tally/config.json";
  socketPath = "%t/tally/tally.sock";

  daemonArgv = [
    "${cfg.package}/bin/tally"
    "--config"
    configPath
    "--socket"
    socketPath
    "daemon"
    "run"
    "--cpu-weight"
    "100"
    "--memory-max-bytes"
    "8589934592"
    "--state-dir"
    (toString cfg.stateDir)
    "--data-dir"
    (toString cfg.dataDir)
    "--yield-grace-sec"
    (toString cfg.lease.yieldGraceSec)
  ];

  dispatchPrefix = [
    "${cfg.package}/bin/tally"
    "--config"
    configPath
  ];

  dispatchSuffix = producer: [
    "__producer-dispatch"
    producer
    "--state-dir"
    (toString cfg.stateDir)
    "--data-dir"
    (toString cfg.dataDir)
  ];

  mkDispatchProgram =
    producer: event:
    pkgs.writeShellApplication {
      name = "tally-producer-${producer}-dispatch";
      text = ''
        socket="''${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR is required}/tally/tally.sock"
        exec ${lib.escapeShellArgs dispatchPrefix} --socket "$socket" \
          ${lib.escapeShellArgs (dispatchSuffix producer)} \
          --event ${lib.escapeShellArg (builtins.toJSON event)}
      '';
    };

  mkReachabilityProgram =
    producer: pool:
    pkgs.writeShellApplication {
      name = "tally-producer-${producer}-probe";
      runtimeInputs = [ pkgs.jq ];
      text = ''
        socket="''${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR is required}/tally/tally.sock"
        reachable=false
        if pools="$(${
          lib.escapeShellArgs [
            "${cfg.package}/bin/tally"
          ]
        } --socket "$socket" query pools)"; then
          if jq -e --arg pool ${lib.escapeShellArg pool} 'any(.pools[]; .pool == $pool)' \
            <<<"$pools" >/dev/null; then
            reachable=true
          fi
        fi
        event="$(jq -cn --argjson reachable "$reachable" '{kind: "pool-reachability", reachable: $reachable}')"
        exec ${lib.escapeShellArgs dispatchPrefix} --socket "$socket" ${lib.escapeShellArgs (dispatchSuffix producer)} --event "$event"
      '';
    };

  credentialServiceConfig =
    credentials:
    lib.optionalAttrs (credentials != { }) {
      LoadCredential = lib.mapAttrsToList (name: source: "${name}:${toString source}") credentials;
      Environment = [
        "TALLY_CREDENTIALS=${builtins.toJSON (builtins.attrNames credentials)}"
      ];
    };

  producerUnits =
    lib.foldl'
      (
        acc: name:
        let
          producer = cfg.producers.${name};
          unit = "tally-producer-${name}";
          commonUnit = program: {
            Unit = {
              Description = "tally ${producer.kind} producer ${name}";
              After = [ "tally-daemon.service" ];
              ConditionPathExists = "${program}/bin/${program.meta.mainProgram}";
              StartLimitIntervalSec = 0;
            };
          };
          commonService =
            program:
            {
              ExecStart = "${program}/bin/${program.meta.mainProgram}";
              UMask = "0077";
            }
            // credentialServiceConfig producer.credentials;
        in
        if producer.kind == "calendar" then
          let
            program = mkDispatchProgram name { kind = "calendar"; };
          in
          {
            services = acc.services // {
              ${unit} = commonUnit program // {
                Service = commonService program // {
                  Type = "oneshot";
                };
              };
            };
            timers = acc.timers // {
              ${unit} = {
                Unit.Description = "calendar for tally producer ${name}";
                Timer = {
                  OnCalendar = producer.onCalendar;
                  Persistent = true;
                  Unit = "${unit}.service";
                };
                Install.WantedBy = [ "timers.target" ];
              };
            };
            desired = acc.desired ++ [
              "${unit}.service"
              "${unit}.timer"
            ];
          }
        else if producer.kind == "pool-reachability" then
          let
            program = mkReachabilityProgram name producer.probePool;
          in
          {
            services = acc.services // {
              ${unit} = commonUnit program // {
                Service = commonService program // {
                  Type = "simple";
                  Restart = "always";
                  RestartSec = "${toString producer.intervalSec}s";
                };
                Install.WantedBy = [ "default.target" ];
              };
            };
            timers = acc.timers;
            desired = acc.desired ++ [ "${unit}.service" ];
          }
        else if producer.kind == "build-effect" then
          let
            program = mkDispatchProgram name { kind = "build-effect"; };
          in
          {
            services = acc.services // {
              ${unit} = commonUnit program // {
                Service = commonService program // {
                  Type = "simple";
                  Restart = "always";
                  RestartSec = "5s";
                };
                Install.WantedBy = [ "default.target" ];
              };
            };
            timers = acc.timers;
            desired = acc.desired ++ [ "${unit}.service" ];
          }
        else if producer.kind == "gh" && producer.enable then
          let
            program = mkDispatchProgram name { kind = "gh"; };
          in
          {
            services = acc.services // {
              ${unit} = commonUnit program // {
                Service = commonService program // {
                  Type = "simple";
                  Restart = "always";
                  RestartSec = "${toString producer.pollIntervalSec}s";
                };
                Install.WantedBy = [ "default.target" ];
              };
            };
            timers = acc.timers;
            desired = acc.desired ++ [ "${unit}.service" ];
          }
        else if producer.kind == "events-dir" && !producer.selfDrain then
          # A registry entry that declares the contract without adding a second
          # drainer beside tally-drain.timer. Rendering nothing here is what
          # keeps the durable admission origin of an event dropped in this
          # directory from depending on which timer claimed the file first.
          acc
        else if producer.kind == "events-dir" then
          let
            program = mkDispatchProgram name { kind = "events-dir"; };
          in
          {
            services = acc.services // {
              ${unit} = commonUnit program // {
                Service = commonService program // {
                  Type = "oneshot";
                };
              };
            };
            timers = acc.timers // {
              ${unit} = {
                Unit.Description = "events-directory timer for tally producer ${name}";
                Timer = {
                  OnActiveSec = "1s";
                  OnUnitActiveSec = "${toString producer.pollIntervalSec}s";
                  Unit = "${unit}.service";
                };
                Install.WantedBy = [ "timers.target" ];
              };
            };
            desired = acc.desired ++ [
              "${unit}.service"
              "${unit}.timer"
            ];
          }
        else
          acc
      )
      {
        services = { };
        timers = { };
        desired = [ ];
      }
      (builtins.attrNames cfg.producers);

  meterUnits =
    lib.foldl'
      (
        acc: pool:
        let
          meter = cfg.pools.${pool}.usageMeter;
          unit = "tally-meter-${pool}";
        in
        if meter == null then
          acc
        else
          let
            marker = pkgs.writeText "${unit}.json" (
              builtins.toJSON {
                inherit pool;
                inherit (meter) pollIntervalSec budgetClass;
              }
            );
            eventPath = common.meterEventPath cfg.stateDir pool;
            credentials = cfg.pools.${pool}.credentials;
          in
          {
            services = acc.services // {
              ${unit} = {
                Unit = {
                  Description = "tally external usage meter for ${pool}";
                  After = [ "tally-daemon.service" ];
                  ConditionPathExists = marker;
                  StartLimitIntervalSec = 0;
                };
                Service = {
                  Type = "simple";
                  ExecStartPre = lib.escapeShellArgs [
                    "${pkgs.coreutils}/bin/install"
                    "-d"
                    "-m"
                    "0700"
                    (builtins.dirOf eventPath)
                  ];
                  ExecStart = lib.escapeShellArgs meter.argv;
                  Restart = "always";
                  RestartSec = "${toString meter.pollIntervalSec}s";
                  Environment = [
                    "TALLY_METER_POOL=${pool}"
                    "TALLY_METER_EVENT_PATH=${eventPath}"
                    "TALLY_METER_POLL_INTERVAL_SEC=${toString meter.pollIntervalSec}"
                    "TALLY_METER_BUDGET_CLASS=${meter.budgetClass}"
                  ]
                  ++ lib.optional (
                    credentials != { }
                  ) "TALLY_CREDENTIALS=${builtins.toJSON (builtins.attrNames credentials)}";
                  UMask = "0077";
                }
                // lib.optionalAttrs (credentials != { }) {
                  LoadCredential = lib.mapAttrsToList (name: source: "${name}:${toString source}") credentials;
                };
                Install.WantedBy = [ "default.target" ];
              };
            };
            desired = acc.desired ++ [ "${unit}.service" ];
          }
      )
      {
        services = { };
        desired = [ ];
      }
      (builtins.attrNames cfg.pools);

  desiredManagedUnits =
    if cfg.enable then
      lib.sort builtins.lessThan (producerUnits.desired ++ meterUnits.desired)
    else
      [ ];
  desiredManagedUnitsFile = pkgs.writeText "tally-managed-units" (
    lib.concatStringsSep "\n" desiredManagedUnits + "\n"
  );
  cleanupProgram = pkgs.writeShellApplication {
    name = "tally-clean-removed-producers";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.systemd
    ];
    text = ''
      if ! systemctl --user show-environment >/dev/null 2>&1; then
        exit 0
      fi
      while read -r unit _; do
        case "$unit" in
          tally-producer-*|tally-meter-*)
            if ! grep -Fxq -- "$unit" ${desiredManagedUnitsFile}; then
              systemctl --user stop "$unit"
              systemctl --user reset-failed "$unit" 2>/dev/null || true
            fi
            ;;
        esac
      done < <(systemctl --user list-units --all --plain --no-legend \
        'tally-producer-*' 'tally-meter-*')
    '';
  };
  campaignPollProgram = pkgs.writeShellApplication {
    name = "tally-campaign-poll";
    runtimeInputs = [
      pkgs.gh
      pkgs.git
    ];
    text = ''
      socket="''${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR is required}/tally/tally.sock"
      exec ${
        lib.escapeShellArgs [
          "${cfg.package}/bin/tally"
          "--config"
          configPath
          "--socket"
        ]
      } "$socket" ${
        lib.escapeShellArgs [
          "campaign"
          "poll"
          "--once"
          "--state-dir"
          (toString cfg.stateDir)
        ]
      }
    '';
  };
in
{
  options.services.tally = common.mkOptions {
    defaultPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.tally;
    defaultDataDir = "${config.xdg.dataHome}/tally";
    defaultStateDir = "${config.xdg.stateHome}/tally";
    defaultDataDirText = lib.literalExpression ''"''${config.xdg.dataHome}/tally"'';
    defaultStateDirText = lib.literalExpression ''"''${config.xdg.stateHome}/tally"'';
  };

  config = lib.mkMerge [
    {
      services.tally.adapters = common.adapterDefaults;
      services.tally.producers = common.mkFlowProducers cfg cfg.flows;
      home.activation.tallyCleanRemovedProducers = lib.hm.dag.entryAfter [ "writeBoundary" ] ''
        ${cleanupProgram}/bin/tally-clean-removed-producers
      '';
    }
    { services.tally = common.mkCampaignConfig cfg; }
    (lib.mkIf (cfg.flows != { }) {
      services.tally.pools.build = common.buildPoolDefaults;
      services.tally.pools.flow = common.flowPoolDefaults;
    })
    (lib.mkIf cfg.enable {
      assertions = common.mkAssertions cfg ++ [
        {
          # Every campaign class continues itself through this one drain, so
          # the campaign layer installs it unconditionally rather than once per
          # declared campaign. The NixOS layer renders no campaign surface at
          # all, so this belongs here and not in the shared assertions.
          assertion =
            builtins.hasAttr "campaign-continuation" cfg.producers
            && cfg.producers.campaign-continuation.kind == "events-dir";
          message = "tally requires the generic events-dir producer campaign-continuation";
        }
      ];

      home.packages = [
        installedPackage
        witnessEmitter
      ];

      home.activation.tallyRuntimeDirectories =
        lib.hm.dag.entryBetween [ "reloadSystemd" ] [ "writeBoundary" ]
          ''
            ${pkgs.coreutils}/bin/install -d -m 0700 -- \
              ${lib.escapeShellArg (toString cfg.dataDir)} \
              ${lib.escapeShellArg (toString cfg.stateDir)} \
              ${lib.escapeShellArg "${toString cfg.stateDir}/events"}
          '';

      xdg.configFile."tally/config.json" = {
        source = checkedConfig;
      };

      systemd.user.services = {
        tally-daemon = {
          Unit = {
            Description = "tally contention and proof daemon";
            After = [ "network.target" ];
            ConditionPathExists = configPath;
          };
          Service = {
            Type = "notify";
            NotifyAccess = "main";
            # The daemon derives its liveness budgets from this period: it pings
            # every WatchdogSec/4, reports a dispatch loop that has not come
            # back around for 2x WatchdogSec, and stops pinging at 10x, after
            # which systemd takes one more period to restart. Moving this number
            # moves all four; daemon::notify pins them at 30s.
            WatchdogSec = "30s";
            # Startup is charged here and never to WatchdogSec, because the
            # service watchdog is not armed until READY=1. The daemon now sends
            # EXTEND_TIMEOUT_USEC= at every startup phase boundary, so this is
            # the budget for one phase rather than for the whole of
            # Daemon::open; it matches daemon::startup::STARTUP_PHASE_BUDGET.
            # Declared rather than inherited so the limit is a choice this
            # module made, not whichever DefaultTimeoutStartSec the user
            # manager happens to carry.
            TimeoutStartSec = "90s";
            Restart = "always";
            RestartSec = "2s";
            Environment = [
              "TALLY_CONFIG_GENERATION=${checkedConfig}"
              "TALLY_NIX_PROGRAM=${pkgs.nix}/bin/nix"
              "TALLY_NIX_STORE_PROGRAM=${pkgs.nix}/bin/nix-store"
            ];
            RuntimeDirectory = "tally";
            RuntimeDirectoryMode = "0700";
            # The events directory is created here, not lazily by the first
            # ingress write: a job whose adapter names it in
            # extraWritablePaths cannot start at all while it is missing.
            ExecStartPre = lib.escapeShellArgs [
              "${pkgs.coreutils}/bin/install"
              "-d"
              "-m"
              "0700"
              (toString cfg.dataDir)
              (toString cfg.stateDir)
              "${toString cfg.stateDir}/events"
            ];
            ExecStart = lib.escapeShellArgs daemonArgv;
            CPUWeight = 100;
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
          Install.WantedBy = [ "default.target" ];
        };

        tally-drain = {
          Unit = {
            Description = "drain tally producer event files";
            After = [ "tally-daemon.service" ];
            # `After` orders startup but says nothing about the daemon being
            # gone. The timer fires every five seconds and a daemon restart
            # takes longer than that, so an activation that restarts
            # tally-daemon reliably catches a drain mid-flight and turns a
            # benign deploy into a per-user unit failure (#411). Conditioning
            # on the socket the command actually connects to makes that
            # invocation a recorded *skip* instead. systemd ANDs repeated
            # conditions, so the config guard is kept, not replaced.
            ConditionPathExists = [
              configPath
              socketPath
            ];
          };
          Service = {
            Type = "oneshot";
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

        "tally-witness-emit@" = {
          Unit.Description = "append an advisory tally attestation for %i";
          Service = {
            Type = "oneshot";
            ExecStart = lib.escapeShellArgs [
              "${witnessEmitter}/bin/tally-witness-emit"
              "%i"
            ];
            Environment = [ "TALLY_ATTESTATION_LEDGER=${toString cfg.dataDir}/attestations.jsonl" ];
            UMask = "0077";
          };
        };

        tally-clean-removed-producers = {
          Unit.Description = "stop tally producer and meter units removed from configuration";
          Service = {
            Type = "oneshot";
            ExecStart = "${cleanupProgram}/bin/tally-clean-removed-producers";
          };
          Install.WantedBy = [ "default.target" ];
        };

        tally-retention = lib.mkIf cfg.retention.enable {
          Unit = {
            Description = "prune expired tally GC roots, briefs, capture archives, and ingress event files";
            After = [ "tally-daemon.service" ];
            Requires = [ "tally-daemon.service" ];
          };
          Service = {
            Type = "oneshot";
            TimeoutStartSec = "infinity";
            ExecStart = lib.escapeShellArgs (common.mkRetentionArgv cfg);
            Environment = [ "PATH=${lib.makeBinPath [ pkgs.nix ]}" ];
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

        tally-campaign-poll = lib.mkIf cfg.campaignPoll.enable {
          Unit = {
            Description = "reconcile armed forge-native tally campaigns";
            After = [
              "network-online.target"
              "tally-daemon.service"
            ];
            Requires = [ "tally-daemon.service" ];
            ConditionPathExists = configPath;
          };
          Service = {
            Type = "oneshot";
            # The scan holds the registry lock exclusively across its forge
            # round-trips, so a wedged call would block interactive arm,
            # disarm, and list until this fires.
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
      }
      // producerUnits.services
      // meterUnits.services;

      systemd.user.timers = {
        tally-drain = {
          Unit.Description = "periodically drain tally producer event files";
          Timer = {
            OnActiveSec = "1s";
            OnUnitActiveSec = "5s";
            Unit = "tally-drain.service";
          };
          Install.WantedBy = [ "timers.target" ];
        };
        tally-retention = lib.mkIf cfg.retention.enable {
          Unit.Description = "schedule tally store-evidence retention";
          Timer = {
            OnCalendar = cfg.retention.onCalendar;
            Persistent = true;
            Unit = "tally-retention.service";
          };
          Install.WantedBy = [ "timers.target" ];
        };
        tally-campaign-poll = lib.mkIf cfg.campaignPoll.enable {
          Unit.Description = "poll armed forge-native tally campaigns";
          Timer = {
            OnActiveSec = "15s";
            OnUnitActiveSec = cfg.campaignPoll.interval;
            Persistent = true;
            Unit = "tally-campaign-poll.service";
          };
          Install.WantedBy = [ "timers.target" ];
        };
      }
      // producerUnits.timers;
    })
  ];
}
