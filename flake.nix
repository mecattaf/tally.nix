{
  description = "tally: contention and proof for impure labor";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      home-manager,
    }:
    let
      adapterLibrary = import ./nix/lib/adapters.nix { lib = nixpkgs.lib; };
      priorityRanks = import ./nix/lib/priority-ranks.nix;
    in
    {
      lib.adapters = adapterLibrary;
      lib.priorityRanks = priorityRanks;
      lib.tallyWitnessUnitHooks = {
        OnSuccess = [ "tally-witness-emit@success:%n.service" ];
        OnFailure = [ "tally-witness-emit@failure:%n.service" ];
      };
      nixosModules.tally = import ./nix/modules/nixos.nix self;
      homeManagerModules.tally = import ./nix/modules/home-manager.nix self;
    }
    // flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        adapterConfig = pkgs.writeText "tally-adapter-config.json" (
          builtins.toJSON {
            pools = { };
            adapters = adapterLibrary.presets // {
              nix-custom = adapterLibrary.mkAdapter {
                argv = [
                  "custom-agent"
                  "--structured"
                ];
                resume = [
                  "custom-agent"
                  "--resume"
                  "%<sessionRef>%"
                ];
                scrape.sessionRef = adapterLibrary.mkScrapeCapture {
                  mode = "jsonPath";
                  pattern = "$.session";
                };
                env.CUSTOM_AGENT_MODE = "batch";
                extraConfig.origin = "pure-nix-check";
              };
            };
          }
        );
        producerConfig = pkgs.writeText "tally-producer-config.json" (
          builtins.toJSON {
            pools.slot = {
              resource = "build-slot";
              capacity = 1;
            };
            adapters.shell = adapterLibrary.presets.shell;
            producers = {
              daily = {
                kind = "calendar";
                onCalendar = "daily";
                enqueue = {
                  argv = [ "calendar-job" ];
                  pool = "slot";
                  dedupKey = "daily-%Y%m%d";
                };
              };
              drop.kind = "events-dir";
              github = {
                kind = "gh";
                enable = true;
                sources = [
                  {
                    search = {
                      repo = "agency-agency/spec";
                      labels = [ "agency:codex-ready" ];
                      state = "open";
                    };
                  }
                ];
                triggers.commandComments = [ "/tally run" ];
                allowSelfTriggered = true;
                allowedActors = [ "tally-bot" ];
                postEvidence = true;
                closeOnPass = true;
                enqueue = {
                  argv = [ "gh-job" ];
                  pool = "slot";
                };
              };
              effects = {
                kind = "build-effect";
                watch = "jsonl";
                path = "/var/empty/tally-effects.jsonl";
                onKey = {
                  argv = [ "effect-job" ];
                  pool = "slot";
                };
              };
              health = {
                kind = "pool-reachability";
                probePool = "slot";
                hysteresis = 3;
                onLost = {
                  argv = [ "pool-lost" ];
                  pool = "slot";
                };
                onReturn = {
                  argv = [ "pool-return" ];
                  pool = "slot";
                };
                onReturnAttest = {
                  argv = [ "assess-return" ];
                  pool = "slot";
                  noEnqueue = true;
                };
              };
            };
          }
        );
        tally = pkgs.rustPlatform.buildRustPackage {
          pname = "tally";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          doCheck = true;
          nativeCheckInputs = [ pkgs.taskwarrior3 ];
          postInstall = ''
            ln -s tally $out/bin/tallyd
          '';
          meta.mainProgram = "tally";
        };
        tallyWitnessEmit = import ./nix/lib/witness-emitter.nix {
          lib = pkgs.lib;
          inherit pkgs;
          tallyPackage = tally;
        };
        stockHome = home-manager.lib.homeManagerConfiguration {
          inherit pkgs;
          modules = [
            self.homeManagerModules.tally
            {
              home = {
                username = "tally-stock";
                homeDirectory = "/tmp/tally-stock-home";
                stateVersion = "26.11";
              };
              services.tally = {
                enable = true;
                pools = {
                  stock = {
                    resource = "build-slot";
                    enforce = "cooperative";
                  };
                  programmatic = {
                    resource = "budget";
                    predicate.windowed-consumption = {
                      windowSec = 604800;
                      consumptionCap = 18000;
                    };
                    usageMeter.argv = [
                      "${pkgs.coreutils}/bin/sleep"
                      "infinity"
                    ];
                    credentials.METER_TOKEN = "/run/credentials/tally-meter";
                  };
                };
                adapters.project-codex = {
                  argv = [
                    "codex"
                    "exec"
                    "--json"
                    "--"
                  ];
                  resume = [
                    "codex"
                    "resume"
                    "%<sessionRef>%"
                    "--model"
                    "%<model>%"
                    "--"
                  ];
                  scrape.sessionRef = {
                    mode = "jsonPath";
                    pattern = "$..thread_id";
                  };
                  scrape.model = {
                    mode = "jsonPath";
                    pattern = "$..model";
                  };
                  launch = {
                    allowPrePromptArgv = true;
                    cwdArgv = [
                      "-C"
                      "%<cwd>%"
                    ];
                    model = {
                      argv = [
                        "--model"
                        "%<value>%"
                      ];
                      allowedValues = [ "gpt-5-codex" ];
                    };
                    effort = {
                      argv = [
                        "-c"
                        "model_reasoning_effort=%<value>%"
                      ];
                      allowedValues = [ "high" ];
                    };
                  };
                };
                executors.worker = {
                  host = "worker.example";
                  user = "tally-worker";
                  identityFile = "/run/credentials/tally-worker-key";
                  knownHostsFile = "/etc/tally/worker-known-hosts";
                  program = "/run/current-system/sw/bin/tally";
                  stateDir = "/var/lib/tally-remote";
                };
                producers = {
                  daily = {
                    kind = "calendar";
                    onCalendar = "daily";
                    credentials.PRODUCER_TOKEN = "/run/credentials/tally-producer";
                    enqueue = {
                      argv = [ "calendar-job" ];
                      pool = [
                        "programmatic"
                        "stock"
                      ];
                      executor = "worker";
                      consumptionEstimate = 1;
                      credentials.JOB_TOKEN = "/run/credentials/tally-job";
                    };
                  };
                  effects = {
                    kind = "build-effect";
                    watch = "jsonl";
                    path = "/var/empty/tally-effects.jsonl";
                    onKey = {
                      argv = [ "effect-job" ];
                      pool = "stock";
                    };
                  };
                  health = {
                    kind = "pool-reachability";
                    probePool = "stock";
                    onReturnAttest = {
                      argv = [ "assess-return" ];
                      pool = "stock";
                      noEnqueue = true;
                    };
                  };
                  github = {
                    kind = "gh";
                    enable = true;
                    sources = [
                      {
                        search = {
                          repo = "agency-agency/spec";
                          labels = [ "agency:codex-ready" ];
                          state = "open";
                        };
                      }
                    ];
                    triggers.commandComments = [ "/tally run" ];
                    postGateSummary = true;
                    requestReview = true;
                    closeOnAcceptance = true;
                    enqueue = {
                      argv = [ "Review \${gh.url}" ];
                      adapter = "project-codex";
                      cwd = "/worktrees/\${gh.repoName}";
                      workspace = {
                        repo = "agency-agency/spec";
                        baseRev = "origin/main";
                        branch = "tally-intake";
                        worktreePath = "/worktrees/spec";
                      };
                      adapterOptions = {
                        prePromptArgv = [ "--dangerously-bypass-approvals-and-sandbox" ];
                        environment.NO_COLOR = "1";
                        model = "gpt-5-codex";
                        effort = "high";
                      };
                      gateManifest = {
                        path = "/worktrees/spec/.tally/gates.json";
                        requiredGateIds = [
                          "tests"
                          "clippy"
                        ];
                        acceptancePolicy = "execution-and-gates";
                      };
                      pool = "stock";
                    };
                  };
                  drop.kind = "events-dir";
                };
              };
            }
          ];
        };
        disabledHome = home-manager.lib.homeManagerConfiguration {
          inherit pkgs;
          modules = [
            self.homeManagerModules.tally
            {
              home = {
                username = "tally-disabled";
                homeDirectory = "/tmp/tally-disabled-home";
                stateVersion = "26.11";
              };
              services.tally.enable = false;
            }
          ];
        };
        invalidProducerHome = home-manager.lib.homeManagerConfiguration {
          inherit pkgs;
          modules = [
            self.homeManagerModules.tally
            {
              home = {
                username = "tally-invalid-producer";
                homeDirectory = "/tmp/tally-invalid-producer-home";
                stateVersion = "26.11";
              };
              services.tally = {
                enable = true;
                pools.slot.resource = "build-slot";
                producers = {
                  missing = { };
                  misspelled.kind = "event-directory";
                  bad-close = {
                    kind = "gh";
                    postEvidence = false;
                    closeOnPass = true;
                    enqueue = {
                      argv = [ "gh-job" ];
                      pool = "slot";
                    };
                  };
                };
              };
            }
          ];
        };
        moduleCommon = import ./nix/modules/common.nix {
          inherit self pkgs;
          lib = pkgs.lib;
        };
        invalidProducerSchema = pkgs.lib.evalModules {
          modules = [
            {
              options.services.tally = moduleCommon.mkOptions {
                defaultPackage = tally;
                defaultDataDir = "/tmp/tally-data";
                defaultStateDir = "/tmp/tally-state";
              };
              config.services.tally.pools.slot.resource = "build-slot";
              config.services.tally.producers = {
                missing = { };
                misspelled.kind = "event-directory";
                bad-close = {
                  kind = "gh";
                  postEvidence = false;
                  closeOnPass = true;
                  enqueue = {
                    argv = [ "gh-job" ];
                    pool = "slot";
                  };
                };
              };
            }
          ];
        };
        invalidProducerAssertions = builtins.filter (entry: !entry.assertion) (
          moduleCommon.mkAssertions invalidProducerSchema.config.services.tally
        );
        invalidProducerMessages = map (entry: entry.message) invalidProducerAssertions;
        invalidProducerAttempt = builtins.tryEval (
          builtins.deepSeq invalidProducerHome.activationPackage true
        );
        nixosBase = {
          system.stateVersion = "26.11";
          boot.loader.grub.enable = false;
          fileSystems."/" = {
            device = "none";
            fsType = "tmpfs";
          };
        };
        stockNixos = nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.tally
            nixosBase
            {
              services.tally = {
                enable = true;
                pools.stock = {
                  resource = "build-slot";
                  enforce = "cooperative";
                };
              };
            }
          ];
        };
        stockHostTest = pkgs.testers.runNixOSTest {
          name = "tally-stock-host-activation";
          nodes.machine =
            { ... }:
            {
              imports = [
                self.nixosModules.tally
                home-manager.nixosModules.home-manager
              ];

              system.stateVersion = "26.11";
              users.users.tally = {
                isNormalUser = true;
                uid = 1000;
                createHome = true;
                home = "/var/lib/tally-test-user";
              };

              services.tally = {
                enable = true;
                pools.stock = {
                  resource = "build-slot";
                  enforce = "cooperative";
                };
              };

              home-manager = {
                useGlobalPkgs = true;
                useUserPackages = true;
                users.tally = {
                  imports = [ self.homeManagerModules.tally ];
                  home = {
                    username = "tally";
                    homeDirectory = "/var/lib/tally-test-user";
                    stateVersion = "26.11";
                  };
                  services.tally = {
                    enable = true;
                    pools.stock = {
                      resource = "build-slot";
                      enforce = "cooperative";
                    };
                    producers.drop.kind = "events-dir";
                  };
                };
              };
            };
          testScript = ''
            machine.start()
            machine.wait_for_unit("multi-user.target")
            machine.wait_for_unit("tally-daemon.service")
            machine.succeed("systemctl is-active tally-daemon.service")
            machine.succeed("test -d /var/lib/tally")
            machine.succeed("test -d /var/log/tally")
            machine.succeed("grep -F '\"enforce\":\"cooperative\"' /etc/tally/config.json")

            machine.wait_for_unit("home-manager-tally.service")
            machine.succeed("loginctl enable-linger tally")
            machine.succeed("systemctl start user@1000.service")
            machine.wait_until_succeeds(
              "runuser -u tally -- env HOME=/var/lib/tally-test-user XDG_RUNTIME_DIR=/run/user/1000 systemctl --user is-active tally-daemon.service"
            )
            machine.succeed("test -S /run/user/1000/tally/tally.sock")

            user = "runuser -u tally -- env HOME=/var/lib/tally-test-user XDG_RUNTIME_DIR=/run/user/1000"
            machine.wait_until_succeeds(
              user + " systemctl --user show tally-clean-removed-producers.service --property=ExecMainStartTimestampMonotonic --value | grep -E '^[1-9][0-9]*$'"
            )
            machine.wait_until_succeeds(
              user + " systemctl --user show tally-producer-drop.timer --property=LastTriggerUSec --value | grep -Ev '^(|n/a)$'"
            )
            machine.wait_until_succeeds(
              user + " journalctl --user --unit=tally-producer-drop.service --output=cat --no-pager | grep -F '\"barrier\":\"barrier:drain:' | grep -F '\"rejected\":0'"
            )
            machine.wait_until_succeeds(
              user + " systemctl --user show tally-drain.timer --property=LastTriggerUSec --value | grep -Ev '^(|n/a)$'"
            )
            machine.wait_until_succeeds(
              user + " journalctl --user --unit=tally-drain.service --output=cat --no-pager | grep -F '\"barrier\":\"barrier:drain:' | grep -F '\"rejected\":0'"
            )
          '';
        };
        badPoolNixos = nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.tally
            nixosBase
            {
              services.tally = {
                enable = true;
                pools = {
                  bad-vram = {
                    resource = "build-slot";
                    capacity = 2;
                    budgetGb = 8;
                  };
                  bad-mutex = {
                    resource = "mutex";
                    capacity = 2;
                  };
                };
              };
            }
          ];
        };
        badPoolAttempt = builtins.tryEval (builtins.deepSeq badPoolNixos.config.system.build.toplevel true);
        unknownMultiPoolNixos = nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.tally
            nixosBase
            {
              services.tally = {
                enable = true;
                pools.stock.resource = "build-slot";
                producers.daily = {
                  kind = "calendar";
                  onCalendar = "daily";
                  enqueue = {
                    argv = [ "calendar-job" ];
                    pool = [
                      "stock"
                      "missing"
                    ];
                  };
                };
              };
            }
          ];
        };
        unknownMultiPoolAttempt = builtins.tryEval (
          builtins.deepSeq unknownMultiPoolNixos.config.system.build.toplevel true
        );
        unknownMultiPoolMessages = builtins.map (entry: entry.message) (
          builtins.filter (entry: !entry.assertion) unknownMultiPoolNixos.config.assertions
        );
        invalidPoolSetNixos = nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.tally
            nixosBase
            {
              services.tally = {
                enable = true;
                pools.stock.resource = "build-slot";
                producers = {
                  empty = {
                    kind = "calendar";
                    onCalendar = "daily";
                    enqueue = {
                      argv = [ "empty-pool-job" ];
                      pool = [ ];
                    };
                  };
                  duplicate = {
                    kind = "calendar";
                    onCalendar = "daily";
                    enqueue = {
                      argv = [ "duplicate-pool-job" ];
                      pool = [
                        "stock"
                        "stock"
                      ];
                    };
                  };
                };
              };
            }
          ];
        };
        invalidPoolSetAttempt = builtins.tryEval (
          builtins.deepSeq invalidPoolSetNixos.config.system.build.toplevel true
        );
        invalidPoolSetMessages = builtins.map (entry: entry.message) (
          builtins.filter (entry: !entry.assertion) invalidPoolSetNixos.config.assertions
        );
        unknownExecutorNixos = nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.tally
            nixosBase
            {
              services.tally = {
                enable = true;
                pools.stock.resource = "build-slot";
                producers.daily = {
                  kind = "calendar";
                  onCalendar = "daily";
                  enqueue = {
                    argv = [ "calendar-job" ];
                    pool = "stock";
                    executor = "missing-worker";
                  };
                };
              };
            }
          ];
        };
        unknownExecutorAttempt = builtins.tryEval (
          builtins.deepSeq unknownExecutorNixos.config.system.build.toplevel true
        );
        unknownExecutorMessages = builtins.map (entry: entry.message) (
          builtins.filter (entry: !entry.assertion) unknownExecutorNixos.config.assertions
        );
        forbiddenAttempt =
          module:
          builtins.tryEval (
            builtins.deepSeq
              (nixpkgs.lib.nixosSystem {
                inherit system;
                modules = [
                  self.nixosModules.tally
                  nixosBase
                  {
                    services.tally.enable = true;
                  }
                  module
                ];
              }).config.system.build.toplevel
              true
          );
        forbiddenAttempts = [
          (forbiddenAttempt { services.tally.pools.stock.enforce = "dmem"; })
          (forbiddenAttempt { services.tally.pools.stock.enforce = "dmemcg-booster"; })
          (forbiddenAttempt { services.tally.pools.stock.remote.host = "worker"; })
          (forbiddenAttempt { services.tally.pools.stock.servingSlice = "worker.slice"; })
          (forbiddenAttempt { services.tally.patchedSystemd.enable = true; })
          (forbiddenAttempt { services.tally.lease.remoteHeartbeatSec = 15; })
          (forbiddenAttempt { services.tally.lease.remoteReapSec = 45; })
          (forbiddenAttempt { services.tally.conductorHost = "example-host"; })
        ];
        homeServices = stockHome.config.systemd.user.services;
        homeTimers = stockHome.config.systemd.user.timers;
        homeServiceExec =
          name:
          let
            value = homeServices.${name}.Service.ExecStart;
          in
          if builtins.isList value then builtins.head value else value;
        checkedHomeConfig = stockHome.config.xdg.configFile."tally/config.json".source;
        systemDaemon = stockNixos.config.systemd.services.tally-daemon;
        systemWitnessEmitter = stockNixos.config.systemd.services."tally-witness-emit@";
        moduleContract =
          assert homeTimers ? tally-producer-daily;
          assert homeTimers.tally-producer-daily.Timer.OnCalendar == "daily";
          assert homeServices.tally-producer-health.Service.Restart == "always";
          assert homeServices.tally-producer-health.Unit.StartLimitIntervalSec == 0;
          assert homeServices.tally-producer-effects.Service.Restart == "always";
          assert homeServices.tally-producer-effects.Unit.StartLimitIntervalSec == 0;
          assert homeServices.tally-producer-github.Service.Restart == "always";
          assert homeServices.tally-producer-github.Unit.StartLimitIntervalSec == 0;
          assert homeTimers ? tally-producer-drop;
          assert homeTimers.tally-producer-drop.Timer.OnActiveSec == "1s";
          assert homeTimers.tally-producer-drop.Timer.OnUnitActiveSec == "60s";
          assert homeTimers.tally-drain.Timer.OnActiveSec == "1s";
          assert homeTimers.tally-drain.Timer.OnUnitActiveSec == "5s";
          assert homeServices ? tally-meter-programmatic;
          assert homeServices.tally-meter-programmatic.Service.Restart == "always";
          assert homeServices.tally-meter-programmatic.Unit.StartLimitIntervalSec == 0;
          assert builtins.elem "TALLY_METER_BUDGET_CLASS=programmatic"
            homeServices.tally-meter-programmatic.Service.Environment;
          assert builtins.elem "TALLY_METER_POOL=programmatic"
            homeServices.tally-meter-programmatic.Service.Environment;
          assert builtins.elem "TALLY_METER_POLL_INTERVAL_SEC=120"
            homeServices.tally-meter-programmatic.Service.Environment;
          assert builtins.any (pkgs.lib.hasPrefix "TALLY_METER_EVENT_PATH=")
            homeServices.tally-meter-programmatic.Service.Environment;
          assert builtins.elem "METER_TOKEN:/run/credentials/tally-meter"
            homeServices.tally-meter-programmatic.Service.LoadCredential;
          assert builtins.elem "PRODUCER_TOKEN:/run/credentials/tally-producer"
            homeServices.tally-producer-daily.Service.LoadCredential;
          assert homeServices ? tally-clean-removed-producers;
          assert !(homeServices.tally-clean-removed-producers.Unit ? Before);
          assert disabledHome.config.home.activation ? tallyCleanRemovedProducers;
          assert homeServices ? "tally-witness-emit@";
          assert builtins.elem
            "TALLY_ATTESTATION_LEDGER=/tmp/tally-stock-home/.local/share/tally/attestations.jsonl"
            homeServices."tally-witness-emit@".Service.Environment;
          assert builtins.elem tallyWitnessEmit stockHome.config.home.packages;
          assert builtins.elem tallyWitnessEmit stockNixos.config.environment.systemPackages;
          assert builtins.elem "TALLY_ATTESTATION_LEDGER=/var/lib/tally/data/attestations.jsonl"
            systemWitnessEmitter.serviceConfig.Environment;
          assert systemDaemon.serviceConfig.StateDirectory == "tally";
          assert systemDaemon.serviceConfig.LogsDirectory == "tally";
          assert systemDaemon.serviceConfig.RestrictAddressFamilies == [ "AF_UNIX" ];
          assert builtins.elem "AF_UNIX" homeServices.tally-daemon.Service.RestrictAddressFamilies;
          assert builtins.elem "AF_INET" homeServices.tally-daemon.Service.RestrictAddressFamilies;
          assert builtins.elem "AF_INET6" homeServices.tally-daemon.Service.RestrictAddressFamilies;
          assert !(systemDaemon.serviceConfig ? Delegate);
          assert builtins.all (
            service: !(service.Service ? Delegate) && !(service.Service ? DeviceMemoryMax)
          ) (builtins.attrValues homeServices);
          assert
            self.lib.tallyWitnessUnitHooks.OnSuccess == [
              "tally-witness-emit@success:%n.service"
            ];
          assert
            self.lib.tallyWitnessUnitHooks.OnFailure == [
              "tally-witness-emit@failure:%n.service"
            ];
          pkgs.runCommand "tally-module-contract" { nativeBuildInputs = [ pkgs.jq ]; } ''
            grep -F -- '__producer-dispatch' ${homeServiceExec "tally-producer-daily"}
            grep -F -- '"kind":"calendar"' ${homeServiceExec "tally-producer-daily"}
            grep -F -- 'XDG_RUNTIME_DIR' ${homeServiceExec "tally-producer-daily"}
            if grep -F -- '%t/tally/tally.sock' ${homeServiceExec "tally-producer-daily"}; then
              echo 'producer script contains an unexpanded systemd specifier' >&2
              exit 1
            fi
            grep -F -- '__producer-dispatch' ${homeServiceExec "tally-producer-health"}
            grep -F -- 'pool-reachability' ${homeServiceExec "tally-producer-health"}
            grep -F -- 'systemctl --user stop "$unit"' ${homeServiceExec "tally-clean-removed-producers"}
            grep -F -- 'witness append --ledger "$ledger" --payload "$payload"' \
              ${tallyWitnessEmit}/bin/tally-witness-emit
            jq -e '
              .enqueue.depthCap == 3 and
              .enqueue.fanoutCap == 64 and
              .lease.yieldGraceSec == 20 and
              .pools.stock.enforce == "cooperative" and
              .pools.programmatic.usageMeter.budgetClass == "programmatic" and
              .pools.programmatic.credentials.METER_TOKEN == "/run/credentials/tally-meter" and
              .producers.daily.enqueue.pool == ["programmatic", "stock"] and
              .producers.daily.enqueue.executor == "worker" and
              .producers.daily.enqueue.credentials.JOB_TOKEN == "/run/credentials/tally-job" and
              .producers.effects.onKey.pool == "stock" and
              .producers.health.onReturnAttest.noEnqueue == true and
              .producers.github.sources[0].search.repo == "agency-agency/spec" and
              .producers.github.sources[0].search.labels == ["agency:codex-ready"] and
              .producers.github.sources[0].search.state == "open" and
              .producers.github.triggers.commandComments == ["/tally run"] and
              .producers.github.allowSelfTriggered == false and
              .producers.github.allowedActors == [] and
              .producers.github.postReceipt == true and
              .producers.github.postGateSummary == true and
              .producers.github.requestReview == true and
              .producers.github.closeOnAcceptance == true and
              .producers.github.neverMutate == false and
              .producers.github.closeOnPass == false and
              .producers.github.enqueue.argv == ["Review ''${gh.url}"] and
              .producers.github.enqueue.cwd == "/worktrees/''${gh.repoName}" and
              .producers.github.enqueue.workspace.repo == "agency-agency/spec" and
              .producers.github.enqueue.workspace.worktreePath == "/worktrees/spec" and
              .producers.github.enqueue.adapterOptions.prePromptArgv == ["--dangerously-bypass-approvals-and-sandbox"] and
              .producers.github.enqueue.adapterOptions.environment.NO_COLOR == "1" and
              .producers.github.enqueue.gateManifest.requiredGateIds == ["tests", "clippy"] and
              .producers.github.enqueue.gateManifest.acceptancePolicy == "execution-and-gates" and
              .executors.worker.kind == "ssh" and
              .executors.worker.host == "worker.example" and
              .executors.worker.user == "tally-worker" and
              .executors.worker.identityFile == "/run/credentials/tally-worker-key" and
              .executors.worker.knownHostsFile == "/etc/tally/worker-known-hosts" and
              .executors.worker.program == "/run/current-system/sw/bin/tally" and
              .executors.worker.stateDir == "/var/lib/tally-remote" and
              .adapters["project-codex"].argv == ["codex", "exec", "--json", "--"] and
              .adapters["project-codex"].launch.cwdArgv == ["-C", "%<cwd>%"] and
              .adapters["project-codex"].launch.model.allowedValues == ["gpt-5-codex"] and
              ([.. | objects | keys[]] | any(. == "remote" or . == "servingSlice" or . == "patchedSystemd") | not)
            ' ${checkedHomeConfig}
            touch "$out"
          '';
      in
      {
        packages = {
          inherit tally;
          tally-witness-emit = tallyWitnessEmit;
          default = tally;
        };
        apps = {
          default = flake-utils.lib.mkApp { drv = tally; };
          dev = {
            type = "app";
            program = "${pkgs.writeShellScript "tally-dev" ''
              exec ${tally}/bin/tally daemon run --mock
            ''}";
          };
        };
        checks = {
          inherit tally;
          stock-home-activation = stockHome.activationPackage;
          stock-nixos-activation = stockNixos.config.system.build.toplevel;
          stock-host-activation = stockHostTest;
          module-layer = moduleContract;
          bad-pool-rejected =
            assert !badPoolAttempt.success;
            pkgs.runCommand "tally-bad-pool-rejected" { } ''
              touch "$out"
            '';
          unknown-multi-pool-rejected =
            assert builtins.any (
              message: nixpkgs.lib.hasInfix "references unknown pool missing" message
            ) unknownMultiPoolMessages;
            assert !unknownMultiPoolAttempt.success;
            pkgs.runCommand "tally-unknown-multi-pool-rejected" { } ''
              touch "$out"
            '';
          invalid-pool-sets-rejected =
            assert builtins.any (
              message: nixpkgs.lib.hasInfix "requires a non-empty pool set" message
            ) invalidPoolSetMessages;
            assert builtins.any (
              message: nixpkgs.lib.hasInfix "pool set contains duplicates" message
            ) invalidPoolSetMessages;
            assert !invalidPoolSetAttempt.success;
            pkgs.runCommand "tally-invalid-pool-sets-rejected" { } ''
              touch "$out"
            '';
          unknown-executor-rejected =
            assert builtins.any (
              message: nixpkgs.lib.hasInfix "references unknown executor missing-worker" message
            ) unknownExecutorMessages;
            assert !unknownExecutorAttempt.success;
            pkgs.runCommand "tally-unknown-executor-rejected" { } ''
              touch "$out"
            '';
          producer-kind-required =
            assert builtins.elem
              "tally producer missing requires an explicit kind; expected one of calendar, build-effect, pool-reachability, gh, events-dir"
              invalidProducerMessages;
            assert builtins.elem
              ''tally producer misspelled has unknown kind "event-directory"; expected one of calendar, build-effect, pool-reachability, gh, events-dir''
              invalidProducerMessages;
            assert builtins.elem "gh producer bad-close closeOnPass=true requires postEvidence=true"
              invalidProducerMessages;
            assert !invalidProducerAttempt.success;
            pkgs.runCommand "tally-producer-kind-required" { } ''
              touch "$out"
            '';
          forbidden-options-absent =
            assert builtins.all (attempt: !attempt.success) forbiddenAttempts;
            pkgs.runCommand "tally-forbidden-options-absent" { } ''
              touch "$out"
            '';
          malformed-config-rejected = pkgs.runCommand "tally-malformed-config-rejected" { } ''
            printf '{"pools":{"bad":{"capacity":0}}}' > bad.json
            if ${tally}/bin/tally --mode check-config --config bad.json; then
              echo "malformed config was accepted" >&2
              exit 1
            fi
            touch $out
          '';
          adapter-presets = pkgs.runCommand "tally-adapter-presets" { nativeBuildInputs = [ pkgs.jq ]; } ''
            ${tally}/bin/tally --mode check-config --config ${adapterConfig}
            grep -F '"nix-custom"' ${adapterConfig} >/dev/null
            grep -F '"claude-code"' ${adapterConfig} >/dev/null
            grep -F '"codex"' ${adapterConfig} >/dev/null
            grep -F '"pi"' ${adapterConfig} >/dev/null
            grep -F '"shell"' ${adapterConfig} >/dev/null
            test "$(jq -c '.adapters.pi.argv' ${adapterConfig})" = '["pi","--mode","json","--"]'
            test "$(jq -c '.adapters.pi.resume' ${adapterConfig})" = '["pi","--mode","json","--session","%<sessionRef>%","--model","%<model>%","--"]'
            test "$(jq -c '.adapters["claude-code"].argv' ${adapterConfig})" = '["claude","--print","--verbose","--output-format","stream-json","--"]'
            test "$(jq -c '.adapters["claude-code"].resume' ${adapterConfig})" = '["claude","--resume","%<sessionRef>%","--model","%<model>%","--print","--verbose","--output-format","stream-json","--"]'
            test "$(jq -c '.adapters.codex.argv' ${adapterConfig})" = '["codex","exec","--json","--"]'
            test "$(jq -c '.adapters.codex.resume' ${adapterConfig})" = '["codex","-C","%<cwd>%","exec","resume","--json","--model","%<model>%","%<sessionRef>%","--"]'
            test "$(jq -c '.adapters.codex.launch.cwdArgv' ${adapterConfig})" = '["-C","%<cwd>%"]'
            test "$(jq -c '.adapters.codex.launch.sandboxPolicies["dangerously-bypass"]' ${adapterConfig})" = '["--dangerously-bypass-approvals-and-sandbox"]'
            test "$(jq -c '.adapters.shell' ${adapterConfig})" = '{"argv":[],"env":{},"extraConfig":{},"launch":{},"resume":null,"scrape":{},"yieldHook":null}'
            for preset in pi claude-code codex; do
              test "$(jq -c --arg preset "$preset" '.adapters[$preset].yieldHook' ${adapterConfig})" = '["tally","lease","status"]'
              test "$(jq -r --arg preset "$preset" '.adapters[$preset].scrape.sessionRef.mode' ${adapterConfig})" = jsonPath
              test "$(jq -r --arg preset "$preset" '.adapters[$preset].scrape.model.mode' ${adapterConfig})" = jsonPath
              test "$(jq -r --arg preset "$preset" '.adapters[$preset].scrape.usage.mode' ${adapterConfig})" = jsonPath
            done
            test "$(jq -r '.adapters.pi.scrape.sessionRef.pattern' ${adapterConfig})" = '$.id'
            test "$(jq -r '.adapters["claude-code"].scrape.sessionRef.pattern' ${adapterConfig})" = '$..session_id'
            test "$(jq -r '.adapters.codex.scrape.sessionRef.pattern' ${adapterConfig})" = '$..thread_id'
            test "$(jq -r '.adapters.codex.extraConfig.modelFlag' ${adapterConfig})" = '--model'
            test "$(jq -r '.adapters["nix-custom"].env.CUSTOM_AGENT_MODE' ${adapterConfig})" = batch
            launch="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render nix-custom -- 'payload arg' "")"
            test "$(printf '%s' "$launch" | jq -c '.argv')" = '["custom-agent","--structured","payload arg",""]'
            test "$(printf '%s' "$launch" | jq -r '.env.CUSTOM_AGENT_MODE')" = batch
            resume="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render nix-custom --captures '{"sessionRef":"nix-session"}' -- '--option-looking')"
            test "$(printf '%s' "$resume" | jq -c '.argv')" = '["custom-agent","--resume","nix-session","--option-looking"]'
            : > empty.err
            printf '%s\n' \
              '{"type":"session","id":"pi-session","model":"Pi/Exact.Model"}' \
              '{"type":"message","model":"Pi/Exact.Model","usage":{"input_tokens":11}}' > pi.jsonl
            pi_render="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render pi --scrape-stdout "$PWD/pi.jsonl" --scrape-stderr "$PWD/empty.err" -- work)"
            test "$(printf '%s' "$pi_render" | jq -c '.argv')" = '["pi","--mode","json","--session","pi-session","--model","Pi/Exact.Model","--","work"]'
            test "$(printf '%s' "$pi_render" | jq -c '.captures.usage')" = '{"input_tokens":11}'
            printf '%s\n' \
              '{"type":"system","subtype":"init","session_id":"claude-session","model":"Claude/Exact.Model"}' \
              '{"type":"assistant","message":{"model":"Claude/Exact.Model","usage":{"input_tokens":12}}}' > claude.jsonl
            claude_render="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render claude-code --scrape-stdout "$PWD/claude.jsonl" --scrape-stderr "$PWD/empty.err" -- work)"
            test "$(printf '%s' "$claude_render" | jq -c '.argv')" = '["claude","--resume","claude-session","--model","Claude/Exact.Model","--print","--verbose","--output-format","stream-json","--","work"]'
            test "$(printf '%s' "$claude_render" | jq -c '.captures.usage')" = '{"input_tokens":12}'
            printf '%s\n' \
              '{"type":"thread.started","thread_id":"codex-thread","model":"Codex/Exact.Model"}' \
              '{"type":"turn.completed","model":"Codex/Exact.Model","usage":{"input_tokens":13}}' > codex.jsonl
            codex_render="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render codex --cwd "$PWD" --scrape-stdout "$PWD/codex.jsonl" --scrape-stderr "$PWD/empty.err" -- work)"
            expected_codex="$(jq -cn --arg cwd "$PWD" '["codex","-C",$cwd,"exec","resume","--json","--model","Codex/Exact.Model","codex-thread","--","work"]')"
            test "$(printf '%s' "$codex_render" | jq -c '.argv')" = "$expected_codex"
            test "$(printf '%s' "$codex_render" | jq -c '.captures.usage')" = '{"input_tokens":13}'
            touch $out
          '';
          producer-registry =
            pkgs.runCommand "tally-producer-registry" { nativeBuildInputs = [ pkgs.jq ]; }
              ''
                ${tally}/bin/tally --mode check-config --config ${producerConfig}
                test "$(jq -r '.producers | keys | join(",")' ${producerConfig})" = 'daily,drop,effects,github,health'
                test "$(jq -r '[.producers[] | select(has("pool") or has("priority") or has("adapter"))] | length' ${producerConfig})" = 0
                producer_state="$PWD/state"
                daily="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch daily --state-dir "$producer_state" --event '{"kind":"calendar"}')"
                test "$(printf '%s' "$daily" | jq -r 'keys[0]')" = emitted
                own_event='{"kind":"gh","source":"search","repo":"agency-agency/spec","number":21,"htmlUrl":"https://github.com/agency-agency/spec/issues/21","itemType":"issue","nodeId":"I-self","itemAuthor":"tally-bot","triggerActor":"tally-bot","selfActor":"tally-bot","triggerKind":"command-comment","eventId":"comment-42","commentId":"comment-42","triggerTimestamp":"2026-07-20T12:30:00Z","context":{"schemaVersion":2,"title":"Self-authored issue","body":"untrusted $(must-not-run)","state":"open","labels":["agency:codex-ready"],"assignees":["tally-bot"],"triggeringComment":{"id":"comment-42","author":"tally-bot","body":"/tally run"}}}'
                own="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch github --state-dir "$producer_state" --event "$own_event")"
                test "$(printf '%s' "$own" | jq -r 'keys[0]')" = emitted
                rejected_event='{"kind":"gh","source":"search","repo":"agency-agency/spec","number":21,"htmlUrl":"https://github.com/agency-agency/spec/issues/21","itemType":"issue","nodeId":"I-self","itemAuthor":"tally-bot","triggerActor":"untrusted-user","selfActor":"tally-bot","triggerKind":"command-comment","eventId":"comment-43","commentId":"comment-43","triggerTimestamp":"2026-07-20T12:31:00Z","context":{"schemaVersion":2,"title":"Self-authored issue","body":"untrusted","state":"open","labels":["agency:codex-ready"],"assignees":["tally-bot"],"triggeringComment":{"id":"comment-43","author":"untrusted-user","body":"/tally run"}}}'
                rejected="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch github --state-dir "$producer_state" --event "$rejected_event")"
                test "$(printf '%s' "$rejected" | jq -r '.filtered.reason')" = trigger-actor-not-allowed
                effect="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch effects --state-dir "$producer_state" --event '{"kind":"build-effect","storePath":"${pkgs.hello}"}')"
                test "$(printf '%s' "$effect" | jq -r '.[0] | keys[0]')" = emitted
                duplicate="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch effects --state-dir "$producer_state" --event '{"kind":"build-effect","storePath":"${pkgs.hello}"}')"
                test "$(printf '%s' "$duplicate" | jq -r '.[0]')" = duplicate
                for probe in 1 2; do
                  lost="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch health --engine-only --state-dir "$producer_state" --event '{"kind":"pool-reachability","reachable":false}')"
                  test "$(printf '%s' "$lost" | jq -r '.transition')" = null
                done
                lost="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch health --engine-only --state-dir "$producer_state" --event '{"kind":"pool-reachability","reachable":false}')"
                test "$(printf '%s' "$lost" | jq -r '.transition')" = lost
                test "$(printf '%s' "$lost" | jq -r '.emitted | length')" = 1
                for probe in 1 2; do
                  returned="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch health --engine-only --state-dir "$producer_state" --event '{"kind":"pool-reachability","reachable":true}')"
                  test "$(printf '%s' "$returned" | jq -r '.transition')" = null
                done
                returned="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch health --engine-only --state-dir "$producer_state" --event '{"kind":"pool-reachability","reachable":true}')"
                test "$(printf '%s' "$returned" | jq -r '.transition')" = returned
                test "$(printf '%s' "$returned" | jq -r '.emitted | length')" = 2
                find "$producer_state/events" -maxdepth 1 -name '*.producer.json' -print0 \
                  | xargs -0 jq -s 'map(select(.noEnqueue == true)) | length == 1' \
                  | grep -Fx true >/dev/null
                touch $out
              '';
        };
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            clippy
            jq
            rustc
            rustfmt
            sqlite
            taskwarrior3
          ];
        };
        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
