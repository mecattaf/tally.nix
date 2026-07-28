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
    { services.tally.adapters = common.adapterDefaults; }
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
    })
  ];
}
