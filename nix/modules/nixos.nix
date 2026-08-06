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
  ghLogin = import ../lib/gh-login.nix;
  checkedConfig = common.mkCheckedConfig cfg;
  installedPackage = common.mkInstalledPackage cfg;
  witnessEmitter = common.mkWitnessEmitter cfg;
  configPath = "/etc/tally/config.json";
  socketPath = "/run/tally/tally.sock";
  eventsDir = "${toString cfg.stateDir}/events";
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
  forge = cfg.campaignForge;
  # The whole campaign execution surface is one switch. A NixOS host that does
  # not want campaigns renders exactly what it rendered before this option
  # existed: no campaign pools, no driver adapter, no continuation entry, and
  # no poll units.
  campaignSurface = forge.enable;
  # `""` is reachable only when the login assertion below has already failed.
  # Keeping these expressions total means an operator who enables the surface
  # without declaring an identity reads that assertion instead of a null
  # coercion error from somewhere inside the identity writer.
  forgeLogin = if forge.login == null then "" else forge.login;
  forgeGitUserName = if forge.gitUserName == null then forgeLogin else forge.gitUserName;
  forgeGitUserEmail =
    if forge.gitUserEmail == null then "${forgeLogin}@users.noreply.github.com" else forge.gitUserEmail;

  # The identity story, in one program. A Home Manager campaign runs as the
  # operator, so `gh` and `git` resolve the operator's own authenticated
  # identity from their own HOME; a system service has no such thing. The
  # daemon launches every campaign job with `systemd-run --user` inside the
  # service account's own user manager, so HOME -- not a unit environment and
  # not a LoadCredential the shipped driver never reads -- is the only place an
  # identity reaches the driver, the agent's commits, and the poll scan alike.
  # Hence: the service account gets a real home, and activation materialises
  # the identity in it.
  #
  # The token arrives on stdin, so it is never a program argument, never in the
  # Nix store, and the file it comes from need only be readable by root.
  #
  # `--remove` is the teardown half: turning the surface off reverts the account
  # record to `/var/empty`, which would otherwise leave a live token at rest in
  # a directory nothing looks at again. It removes only files this writer left,
  # which is what the marker is for -- an operator who pointed `homeDir` at a
  # pre-existing home keeps their own `.gitconfig`.
  forgeIdentityMarker = ".tally-campaign-forge-identity";
  forgeIdentityProgram = pkgs.writeShellApplication {
    name = "tally-campaign-forge-identity";
    runtimeInputs = [ pkgs.coreutils ];
    text = ''
      marker=${lib.escapeShellArg forgeIdentityMarker}
      mode="write"
      home=""
      login=""
      git_name=""
      git_email=""
      while [ "$#" -gt 0 ]; do
        case "$1" in
          --remove) mode="remove"; shift ;;
          --home) home="$2"; shift 2 ;;
          --login) login="$2"; shift 2 ;;
          --git-name) git_name="$2"; shift 2 ;;
          --git-email) git_email="$2"; shift 2 ;;
          *) echo "tally-campaign-forge-identity: unknown argument $1" >&2; exit 2 ;;
        esac
      done
      if [ -z "$home" ]; then
        echo "tally-campaign-forge-identity: --home is required" >&2
        exit 2
      fi

      if [ "$mode" = "remove" ]; then
        # A no-op unless this writer owns the directory's contents.
        if [ -e "$home/$marker" ]; then
          rm -f -- \
            "$home/.config/gh/hosts.yml" \
            "$home/.config/gh/config.yml" \
            "$home/.gitconfig" \
            "$home/$marker"
        fi
        exit 0
      fi

      for required in "$login" "$git_name" "$git_email"; do
        if [ -z "$required" ]; then
          echo "tally-campaign-forge-identity: --login, --git-name, and --git-email are required" >&2
          exit 2
        fi
      done

      token="$(tr -d '\r\n')"
      if [ -z "$token" ]; then
        echo "tally-campaign-forge-identity: services.tally.campaignForge.tokenFile produced an empty token" >&2
        exit 1
      fi

      umask 0077
      install -d -m 0700 -- "$home" "$home/.config" "$home/.config/gh"

      # Both the current per-user shape and the legacy top-level keys, so the
      # first `gh` invocation reads the token instead of rewriting the file it
      # was handed.
      printf '%s:\n    users:\n        %s:\n            oauth_token: %s\n    user: %s\n    oauth_token: %s\n    git_protocol: https\n' \
        github.com "$login" "$token" "$login" "$token" >"$home/.config/gh/hosts.yml.new"
      chmod 0600 -- "$home/.config/gh/hosts.yml.new"
      mv -f -- "$home/.config/gh/hosts.yml.new" "$home/.config/gh/hosts.yml"

      # The file `gh` would otherwise write itself on first use -- and it fails
      # the whole call when it cannot ("failed to write config after
      # migration"). Writing it here is what makes this home read-only-safe for
      # every consumer: the driver adapter, the `shell` adapter that runs the
      # campaign's own self-continuation, and the agent adapters alike.
      printf 'version: "1"\n' >"$home/.config/gh/config.yml.new"
      chmod 0600 -- "$home/.config/gh/config.yml.new"
      mv -f -- "$home/.config/gh/config.yml.new" "$home/.config/gh/config.yml"

      # git pushes over https with the same token, through gh's own credential
      # helper, so the token stays in exactly one file. The helper path is
      # absolute because git runs it without this program's PATH.
      printf '[user]\n\tname = %s\n\temail = %s\n[credential "https://github.com"]\n\thelper = !%s auth git-credential\n' \
        "$git_name" "$git_email" ${lib.escapeShellArg "${pkgs.gh}/bin/gh"} >"$home/.gitconfig.new"
      chmod 0600 -- "$home/.gitconfig.new"
      mv -f -- "$home/.gitconfig.new" "$home/.gitconfig"

      printf 'tally-campaign-forge-identity\n' >"$home/$marker.new"
      chmod 0600 -- "$home/$marker.new"
      mv -f -- "$home/$marker.new" "$home/$marker"
    '';
  };

  campaignPollProgram = pkgs.writeShellApplication {
    name = "tally-campaign-poll";
    runtimeInputs = [
      pkgs.gh
      pkgs.git
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

  # The campaign layer seeds one events-dir registry entry that renders no unit
  # on either module: `tally-drain.timer` already drains that directory. It is
  # the one producer a NixOS host may carry, because carrying it costs no unit
  # and the continuation payload is written against it by name.
  seededProducers = lib.optionals campaignSurface [ "campaign-continuation" ];
  unsupportedConfigAssertions = [
    {
      assertion = builtins.attrNames (builtins.removeAttrs cfg.producers seededProducers) == [ ];
      message = "services.tally.producers must be empty in the NixOS module; configure producers with the Home Manager module (tally.homeManagerModules.tally)";
    }
    {
      assertion = cfg.flows == { };
      message = "services.tally.flows must be empty in the NixOS module; configure flows with the Home Manager module (tally.homeManagerModules.tally)";
    }
    {
      assertion = cfg.campaigns == { };
      message = "services.tally.campaigns must be empty in the NixOS module: a declared campaign is driven by a managed GitHub producer unit and only the Home Manager module renders producer units. Set services.tally.campaignForge.enable = true and arm forge-native campaigns with `tally campaign arm`, or configure declared campaigns with the Home Manager module (tally.homeManagerModules.tally)";
    }
    {
      assertion = lib.all (pool: pool.usageMeter == null) (builtins.attrValues cfg.pools);
      message = "services.tally.pools.<name>.usageMeter must be null in the NixOS module; configure usage meters with the Home Manager module (tally.homeManagerModules.tally)";
    }
    {
      assertion = !campaignSurface || cfg.enable;
      message = "services.tally.campaignForge.enable requires services.tally.enable";
    }
    {
      assertion = !campaignSurface || forge.login != null;
      message = "services.tally.campaignForge.login must name the GitHub account the tally system service acts as; unlike the Home Manager module there is no ambient operator identity to inherit";
    }
    {
      assertion = !campaignSurface || forge.login == null || ghLogin.isValid forge.login;
      message = "services.tally.campaignForge.login must be a GitHub login: alphanumerics and interior hyphens, at most ${toString ghLogin.maxLength} characters";
    }
    {
      assertion = !campaignSurface || forge.tokenFile != null;
      message = "services.tally.campaignForge.tokenFile must be the absolute path of a file holding that account's GitHub token; it is read at activation and never enters the Nix store";
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
      campaignForge = lib.mkOption {
        default = { };
        description = ''
          Campaign execution surface for the system service, and the forge
          identity it acts as. Enabling this renders the generic campaign pools
          (`campaign`, `campaign-agent`, `campaign-control`, `flow`), the
          packaged spec-build driver adapter, and the continuation registry
          entry, then installs `tally-campaign-poll` when
          `services.tally.campaignPoll.enable` is also on, so that a
          forge-native campaign armed with `tally campaign arm` has somewhere to
          dispatch into. Declared `services.tally.campaigns` stay Home Manager
          only either way: those are driven by a managed GitHub producer unit,
          and the NixOS module renders no producer units.

          The identity is the substantive part. A Home Manager campaign runs as
          the operator and inherits the operator's own `gh` and `git`
          authentication; the system service account has none, and every
          campaign job — the driver's pull requests and merges, the agent's
          commits — runs as that account. Activation therefore gives the
          account a real home and writes the identity into it: a `gh` hosts file
          holding the declared token, a `.gitconfig` binding the commit identity
          and a `gh auth git-credential` helper for https pushes, and the `gh`
          config file that `gh` would otherwise insist on writing itself, which
          is what leaves the home read-only-safe for every consumer afterwards.
          Turning this back off removes those files again.
        '';
        type = lib.types.submodule {
          options = {
            enable = lib.mkOption {
              type = lib.types.bool;
              default = false;
              example = true;
              description = ''
                Render the campaign execution surface on this host. Off by
                default: without it the module deploys the daemon only, and an
                armed campaign has no pools, driver adapter, or poll units to
                dispatch into.
              '';
            };
            login = lib.mkOption {
              type = lib.types.nullOr lib.types.str;
              default = null;
              example = "tally-bot";
              description = ''
                GitHub login the system service acts as. Required when the
                surface is enabled; it is written into the service account's
                `gh` hosts file and is the account whose pull requests, merges,
                and comments a campaign produces.
              '';
            };
            tokenFile = lib.mkOption {
              type = lib.types.nullOr lib.types.externalPath;
              default = null;
              example = "/run/secrets/tally-campaign-forge-token";
              description = ''
                Absolute path of a file containing that account's GitHub token,
                with the scopes needed to read the campaign issue graph and to
                push, open, and merge pull requests. Read by root at activation
                and piped to the identity writer on standard input, so the file
                needs no particular ownership, the token never becomes a
                program argument, and no secret enters the Nix store.
              '';
            };
            gitUserName = lib.mkOption {
              type = lib.types.nullOr lib.types.str;
              default = null;
              defaultText = lib.literalExpression "services.tally.campaignForge.login";
              example = "tally";
              description = "Commit author name for campaign commits. Defaults to the login.";
            };
            gitUserEmail = lib.mkOption {
              type = lib.types.nullOr lib.types.str;
              default = null;
              defaultText = lib.literalExpression "\"\${login}@users.noreply.github.com\"";
              example = "tally@example.invalid";
              description = "Commit author email for campaign commits. Defaults to the login's GitHub no-reply address.";
            };
            homeDir = lib.mkOption {
              type = lib.types.externalPath;
              default = "/var/lib/tally/forge";
              description = ''
                Home directory of the service account. It holds the forge
                identity files and is where `gh`, `git`, and the campaign CLI's
                own XDG defaults resolve from; the account's home is
                `/var/empty` while the surface is off.
              '';
            };
          };
        };
      };
    };

  config = lib.mkMerge [
    {
      services.tally.adapters = common.adapterDefaults;
      assertions = unsupportedConfigAssertions;
    }
    # The generic campaign surface, rendered by the same builder the Home
    # Manager module uses. With `campaigns` empty it contributes exactly the
    # generic half: the campaign pools, the flow pool, the driver adapter, the
    # fanout floor, and the continuation registry entry -- which is what
    # `tally campaign arm` validates a host against before it spends agent time.
    (lib.mkIf campaignSurface { services.tally = common.mkCampaignConfig cfg; })
    (lib.mkIf campaignSurface {
      # The driver's forge identity lives in the service account's home, and
      # `gh` rewrites its own configuration file the first time it runs against
      # a config directory it did not write ("failed to write config after
      # migration" when it cannot). Under the compatibility default nothing
      # constrains that write; under `strict` or `production` only the paths
      # named here stay writable, so hardening the driver adapter without this
      # would break every campaign job on this module and no other.
      # `extraWritablePaths` is a plain list definition on both sides, so this
      # extends the campaign layer's entry rather than replacing it.
      services.tally.adapters.spec-build-driver.extraWritablePaths = [ (toString forge.homeDir) ];
    })
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
            eventsDir
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
          ];
          RuntimeDirectory = "tally";
          RuntimeDirectoryMode = "0700";
          StateDirectory = "tally";
          StateDirectoryMode = "0700";
          LogsDirectory = "tally";
          LogsDirectoryMode = "0700";
          # The events directory is created here, not lazily by the first
          # ingress write: a job whose adapter names it in extraWritablePaths
          # cannot start at all while it is missing, and the campaign
          # continuation payload is written into it by name.
          ExecStartPre = lib.escapeShellArgs [
            "${pkgs.coreutils}/bin/install"
            "-d"
            "-m"
            "0700"
            (toString cfg.dataDir)
            (toString cfg.stateDir)
            eventsDir
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
    (lib.mkIf (cfg.enable && campaignSurface) {
      assertions = [
        {
          # Every campaign class continues itself through this one drain, and on
          # this module `tally-drain.timer` is the only drainer there can be:
          # nothing here renders producer units, so a self-draining entry would
          # describe a timer that does not exist.
          assertion =
            builtins.hasAttr "campaign-continuation" cfg.producers
            && cfg.producers.campaign-continuation.kind == "events-dir"
            && !cfg.producers.campaign-continuation.selfDrain;
          message = "tally requires the generic events-dir producer campaign-continuation with selfDrain = false; the NixOS module renders no producer units and tally-drain.timer is its only drainer";
        }
      ];

      # A real home for the service account, because that is where `gh`, `git`,
      # and the campaign CLI's own XDG fallbacks look. Campaign jobs are
      # transient units in this account's own user manager, so they inherit it.
      users.users.${cfg.user} = {
        home = toString forge.homeDir;
        createHome = true;
      };

      system.activationScripts.tallyCampaignForgeIdentity = {
        # `users` for the account record. The secret provisioners are named only
        # when the estate actually runs one, because an activation dependency on
        # a snippet that does not exist is an evaluation error -- and relying on
        # `setupSecrets` and `agenixInstall` sorting before `tally…` is
        # alphabetical luck, not an ordering.
        deps = [
          "users"
        ]
        ++ lib.filter (script: config.system.activationScripts ? ${script}) [
          "agenixInstall"
          "setupSecrets"
        ];
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
            (toString forge.homeDir)
          ]}
          # A missing secret is named, not left to a bare redirection error.
          # The surface is already rendered by this point -- activation cannot
          # unbuild units -- so the poll service additionally refuses to run
          # without the identity file this writes, which keeps an unprovisioned
          # host quiet instead of failing a unit every tick.
          if [ ! -r ${lib.escapeShellArg (toString forge.tokenFile)} ]; then
            echo ${lib.escapeShellArg "tally: services.tally.campaignForge.tokenFile is unreadable (${toString forge.tokenFile}); the campaign surface is enabled but the ${cfg.user} account has no forge identity. Provision that file (sops-nix and agenix both run before this snippet) and activate again."} >&2
            false
          else
            ${
              lib.escapeShellArgs [
                "${pkgs.util-linux}/bin/runuser"
                "-u"
                cfg.user
                "--"
                "${forgeIdentityProgram}/bin/tally-campaign-forge-identity"
                "--home"
                (toString forge.homeDir)
                "--login"
                forgeLogin
                "--git-name"
                forgeGitUserName
                "--git-email"
                forgeGitUserEmail
              ]
            } < ${lib.escapeShellArg (toString forge.tokenFile)}
          fi
        '';
      };

      systemd.services.tally-campaign-poll = lib.mkIf cfg.campaignPoll.enable {
        description = "reconcile armed forge-native tally campaigns";
        after = [
          "network-online.target"
          "tally-daemon.service"
        ];
        # An `After=` on a target nothing pulls into the transaction orders
        # against nothing: the timer arms 15s after boot and the scan's first
        # act is an authenticated forge read, so without this the first poll of
        # every boot could fail before the network was up.
        wants = [ "network-online.target" ];
        requires = [ "tally-daemon.service" ];
        # A scan with no forge identity fails on its first API call, every tick.
        # Conditioning on the identity file makes an unprovisioned host skip the
        # unit -- systemd records a skipped start, not a failed one -- and start
        # polling by itself once activation has written it.
        unitConfig.ConditionPathExists = [
          configPath
          "${toString forge.homeDir}/.config/gh/hosts.yml"
        ];
        serviceConfig = {
          Type = "oneshot";
          User = cfg.user;
          Group = cfg.group;
          # The scan holds the registry lock exclusively across its forge
          # round-trips, so a wedged call would block interactive arm, disarm,
          # and list until this fires.
          TimeoutStartSec = cfg.campaignPoll.timeout;
          ExecStart = "${campaignPollProgram}/bin/tally-campaign-poll";
          Environment = [ "HOME=${toString forge.homeDir}" ];
          UMask = "0077";
          NoNewPrivileges = true;
          PrivateTmp = true;
          ProtectSystem = "strict";
          # The home is writable because `gh` owns its own configuration
          # directory: a read-only one turns a config migration into a failed
          # scan every tick.
          ReadWritePaths = [
            (toString cfg.stateDir)
            (toString forge.homeDir)
          ];
          RestrictAddressFamilies = [
            "AF_UNIX"
            "AF_INET"
            "AF_INET6"
          ];
        };
      };

      systemd.timers.tally-campaign-poll = lib.mkIf cfg.campaignPoll.enable {
        description = "poll armed forge-native tally campaigns";
        wantedBy = [ "timers.target" ];
        timerConfig = {
          OnActiveSec = "15s";
          OnUnitActiveSec = cfg.campaignPoll.interval;
          Persistent = true;
          Unit = "tally-campaign-poll.service";
        };
      };
    })
    # Turning the surface off reverts the account record to `/var/empty`, which
    # on its own would leave a live GitHub token at rest in a directory nothing
    # looks at again. The writer's `--remove` mode deletes exactly the files it
    # wrote, keyed on its own marker, so a host that never enabled the surface
    # and a `homeDir` pointed at somebody's real home are both untouched.
    # Removing the tally module altogether still leaves the directory: there is
    # no activation left to run. The docs say so.
    (lib.mkIf (cfg.enable && !campaignSurface) {
      system.activationScripts.tallyCampaignForgeTeardown = {
        deps = [ "users" ];
        text = lib.escapeShellArgs [
          "${forgeIdentityProgram}/bin/tally-campaign-forge-identity"
          "--remove"
          "--home"
          (toString forge.homeDir)
        ];
      };
    })
  ];
}
