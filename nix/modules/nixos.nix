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
  daemonWrapper = pkgs.writeShellScript "tally-daemon" ''
    export XDG_RUNTIME_DIR="/run/user/$(${pkgs.coreutils}/bin/id -u)"
    exec ${lib.escapeShellArgs daemonArgv}
  '';
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
      assertion = cfg.campaigns == { };
      message = "services.tally.campaigns must be empty in the NixOS module; configure campaigns with the Home Manager module (tally.homeManagerModules.tally)";
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
      assertions = unsupportedConfigAssertions;
    }
    (lib.mkIf cfg.enable {
      assertions = common.mkAssertions cfg;

      environment.systemPackages = [
        installedPackage
        witnessEmitter
      ];
      environment.etc."tally/config.json".source = checkedConfig;

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
          WatchdogSec = "30s";
          Restart = "always";
          RestartSec = "2s";
          Environment = [
            "TALLY_CONFIG_GENERATION=${checkedConfig}"
            "TALLY_NIX_PROGRAM=${pkgs.nix}/bin/nix"
            "TALLY_NIX_STORE_PROGRAM=${pkgs.nix}/bin/nix-store"
          ];
          RuntimeDirectory = "tally";
          RuntimeDirectoryMode = "0700";
          StateDirectory = "tally";
          StateDirectoryMode = "0700";
          LogsDirectory = "tally";
          LogsDirectoryMode = "0700";
          ExecStartPre = lib.escapeShellArgs [
            "${pkgs.coreutils}/bin/install"
            "-d"
            "-m"
            "0700"
            (toString cfg.dataDir)
            (toString cfg.stateDir)
          ];
          ExecStart = daemonWrapper;
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
          Environment = [ "TALLY_ATTESTATION_LEDGER=${toString cfg.dataDir}/attestations.jsonl" ];
          UMask = "0077";
          NoNewPrivileges = true;
          ProtectSystem = "strict";
          ReadWritePaths = [ (toString cfg.dataDir) ];
        };
      };

      systemd.services.tally-drain = {
        description = "drain tally producer event files";
        after = [ "tally-daemon.service" ];
        unitConfig.ConditionPathExists = configPath;
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
        description = "prune expired tally GC roots, capture archives, and ingress event files";
        after = [ "tally-daemon.service" ];
        requires = [ "tally-daemon.service" ];
        serviceConfig = {
          Type = "oneshot";
          User = cfg.user;
          Group = cfg.group;
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
  ];
}
