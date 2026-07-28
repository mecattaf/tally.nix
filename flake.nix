{
  description = "tally: contention and proof for impure labor";

  inputs = {
    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
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
      advisory-db,
      nixpkgs,
      flake-utils,
      home-manager,
    }:
    let
      supportedSystems = [ "x86_64-linux" ];
      adapterLibrary = import ./nix/lib/adapters.nix { lib = nixpkgs.lib; };
      catalogLibrary = import ./nix/lib/catalog.nix {
        lib = nixpkgs.lib;
        inherit self;
      };
      priorityRanks = import ./nix/lib/priority-ranks.nix;
    in
    {
      lib.adapters = adapterLibrary;
      lib.priorityRanks = priorityRanks;
      lib.tally.mkCatalog = catalogLibrary.mkCatalog;
      lib.tallyWitnessUnitHooks = {
        OnSuccess = [ "tally-witness-emit@success:%n.service" ];
        OnFailure = [ "tally-witness-emit@failure:%n.service" ];
      };
      nixosModules.tally = import ./nix/modules/nixos.nix self;
      homeManagerModules.tally = import ./nix/modules/home-manager.nix self;
    }
    // flake-utils.lib.eachSystem supportedSystems (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        advisoryDbRepository = pkgs.runCommand "tally-advisory-db" { nativeBuildInputs = [ pkgs.git ]; } ''
          mkdir -p "$out"
          cp -R ${advisory-db}/. "$out/"
          chmod -R u+w "$out"
          git init --quiet "$out"
          git -C "$out" add --all
          GIT_AUTHOR_DATE="@${toString advisory-db.lastModified}" \
            GIT_COMMITTER_DATE="@${toString advisory-db.lastModified}" \
            git -C "$out" \
              -c user.name=tally-release-gate \
              -c user.email=tally-release-gate@invalid \
              commit --quiet --message="Pinned RustSec advisory database ${advisory-db.rev}"
        '';
        isLinux = pkgs.stdenv.hostPlatform.isLinux;
        tallySource = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./Cargo.lock
            ./Cargo.toml
            ./crates
            ./doc/src/reference/rpc-protocol.md
            ./examples/flows/academic-ocr.js
            ./examples/flows/monthly-review.js
            ./test/fixtures/flows
            ./test/fixtures/git-ai
            ./test/fixtures/ledger
            ./test/fixtures/shell-command-provider
            ./test/fixtures/traces
          ];
        };
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
                skillBundle = "review protocol α\n";
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
                  noEnqueue = true;
                };
              };
              github-flow = {
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
                triggers.commandComments = [ "/pooled-review" ];
                allowSelfTriggered = true;
                allowedActors = [ "tally-bot" ];
                enqueue = {
                  argv = [
                    "tally"
                    "flow"
                    "run"
                    "${./examples/flows/pooled-review.js}"
                    "--args"
                    "{\"subject\":\"\${gh.url}\",\"minimumValid\":2}"
                    "--max-nodes"
                    "1000"
                    "--catalog"
                    "${catalogFixture}"
                  ];
                  pool = "slot";
                  noEnqueue = false;
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
          src = tallySource;
          cargoLock.lockFile = ./Cargo.lock;
          doCheck = true;
          preCheck = ''
            export TALLY_NIX_CATALOG_FIXTURE=${catalogFixtureUnchecked}
          '';
          nativeCheckInputs = [
            pkgs.git
            pkgs.taskwarrior3
          ];
          postInstall = ''
            ln -s tally $out/bin/tallyd
          '';
          meta.mainProgram = "tally";
        };
        mkCargoCheck =
          {
            pname,
            tool,
            command,
          }:
          tally.overrideAttrs (previous: {
            inherit pname;
            nativeBuildInputs = (previous.nativeBuildInputs or [ ]) ++ [ tool ];
            doCheck = false;
            buildPhase = ''
              runHook preBuild
              ${command}
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              touch "$out"
              runHook postInstall
            '';
            postInstall = "";
          });
        rustfmtCheck = mkCargoCheck {
          pname = "tally-rustfmt-check";
          tool = pkgs.rustfmt;
          command = "cargo fmt --all --check";
        };
        clippyCheck = mkCargoCheck {
          pname = "tally-clippy-check";
          tool = pkgs.clippy;
          command = "cargo clippy --workspace --all-targets --all-features -- -D warnings";
        };
        nixfmtCheck =
          pkgs.runCommand "tally-nixfmt-check"
            {
              src = self;
              nativeBuildInputs = [
                pkgs.findutils
                pkgs.nixfmt-rfc-style
              ];
            }
            ''
              find "$src" -type f -name '*.nix' -print0 \
                | sort -z \
                | xargs -0 nixfmt --check
              touch "$out"
            '';
        optionsJson = optionsDoc: "${optionsDoc.optionsJSON}/share/doc/nixos/options.json";
        transformTallyOption =
          option:
          let
            root = toString ./.;
            transformed = option // {
              declarations = map (
                declaration:
                let
                  declarationString = toString declaration;
                  relative = pkgs.lib.removePrefix root declarationString;
                in
                if pkgs.lib.hasPrefix root declarationString then
                  {
                    name = "tally.nix${relative}";
                    url = "https://github.com/mecattaf/tally.nix/blob/main${relative}";
                  }
                else
                  declaration
              ) option.declarations;
            };
          in
          if pkgs.lib.hasPrefix "_module." option.name then
            transformed // { internal = true; }
          else if option.name == "services.tally.producers.<name>.kind" then
            builtins.removeAttrs (
              transformed
              // {
                type = "one of \"calendar\", \"events-dir\", \"gh\", \"build-effect\", or \"pool-reachability\"";
                example = pkgs.lib.literalExpression ''"calendar"'';
                description = ''
                  Required discriminator selecting this producer's field set.
                  There is no producer-level default: every registry entry must
                  name one of the five supported kinds explicitly.
                '';
              }
            ) [ "default" ]
          else if option.name == "services.tally.producers.<name>.pollIntervalSec" then
            transformed
            // {
              description = ''
                Polling interval in seconds. This controls GitHub intake for a
                "gh" producer and the event-directory timer for an
                "events-dir" producer.
              '';
            }
          else if option.name == "services.tally.producers.<name>.enqueue" then
            transformed
            // {
              description = ''
                Job payload emitted by a "calendar" producer at each firing or
                by a "gh" producer for each accepted trigger.
              '';
            }
          else
            transformed;
        producerOptionsDoc = builtins.foldl' (
          accumulated: producerType:
          let
            evaluated = pkgs.lib.evalModules {
              modules = [
                {
                  options.services.tally.producers = pkgs.lib.mkOption {
                    type = pkgs.lib.types.attrsOf producerType;
                    default = { };
                    description = "Producer registry.";
                  };
                }
              ];
            };
          in
          pkgs.nixosOptionsDoc {
            inherit (evaluated) options;
            transformOptions = transformTallyOption;
            baseOptionsJSON = if accumulated == null then null else optionsJson accumulated;
            variablelistId = "tally-producer-options";
          }
        ) null (builtins.attrValues moduleCommon.producerTypesForDocumentation);
        tallyCoreOptions = pkgs.lib.evalModules {
          modules = [
            {
              options.services.tally = moduleCommon.mkOptions {
                defaultPackage = tally;
                defaultDataDir = "/var/lib/tally/data";
                defaultStateDir = "/var/lib/tally/state";
              };
            }
          ];
        };
        onlyTallyOptions = options: {
          services.tally = options.services.tally;
        };
        mkTallyOptionsDoc =
          {
            options,
            variablelistId,
          }:
          pkgs.nixosOptionsDoc {
            options = onlyTallyOptions options;
            transformOptions = transformTallyOption;
            baseOptionsJSON = optionsJson producerOptionsDoc;
            inherit variablelistId;
          };
        coreOptionsDoc = mkTallyOptionsDoc {
          options = tallyCoreOptions.options;
          variablelistId = "tally-core-options";
        };
        homeManagerOptionsEvaluation = home-manager.lib.homeManagerConfiguration {
          inherit pkgs;
          modules = [
            self.homeManagerModules.tally
            {
              home = {
                username = "tally-doc";
                homeDirectory = "/var/empty/tally-doc";
                stateVersion = "26.11";
              };
            }
          ];
        };
        nixosOptionsEvaluation = pkgs.lib.evalModules {
          specialArgs = { inherit pkgs; };
          modules = [
            self.nixosModules.tally
            (
              { lib, ... }:
              {
                config._module.check = false;
                options._module.args = lib.mkOption {
                  internal = true;
                };
              }
            )
          ];
        };
        homeManagerOptionsDoc = mkTallyOptionsDoc {
          options = homeManagerOptionsEvaluation.options;
          variablelistId = "tally-home-manager-options";
        };
        nixosOptionsDoc = mkTallyOptionsDoc {
          options = nixosOptionsEvaluation.options;
          variablelistId = "tally-nixos-options";
        };
        documentation = pkgs.stdenvNoCC.mkDerivation {
          pname = "tally-doc";
          version = "0.1.0";
          src = ./doc;
          nativeBuildInputs = [
            pkgs.jq
            pkgs.mdbook
            pkgs.mdbook-linkcheck2
          ];
          SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          strictDeps = true;
          dontConfigure = true;
          buildPhase = ''
            runHook preBuild

            generated_pages=(
              src/configuration/core-options.md
              src/configuration/home-manager-options.md
              src/configuration/nixos-options.md
            )
            for page in "''${generated_pages[@]}"; do
              if [ -e "$page" ]; then
                echo "generated options page must not be checked in: $page" >&2
                exit 1
              fi
            done

            chmod u+w src/configuration
            {
              cat generated/core-options-intro.md
              printf '\n'
              cat ${coreOptionsDoc.optionsCommonMark}
            } > src/configuration/core-options.md
            {
              cat generated/home-manager-options-intro.md
              printf '\n'
              cat ${homeManagerOptionsDoc.optionsCommonMark}
            } > src/configuration/home-manager-options.md
            {
              cat generated/nixos-options-intro.md
              printf '\n'
              cat ${nixosOptionsDoc.optionsCommonMark}
            } > src/configuration/nixos-options.md

            core_json=${optionsJson coreOptionsDoc}
            home_json=${optionsJson homeManagerOptionsDoc}
            nixos_json=${optionsJson nixosOptionsDoc}
            jq -S 'keys' "$core_json" > core-option-keys.json
            jq -S 'keys' "$home_json" > home-option-keys.json
            jq -S 'keys' "$nixos_json" > nixos-option-keys.json
            cmp core-option-keys.json home-option-keys.json
            jq -S '
              keys - [
                "services.tally.group",
                "services.tally.user"
              ]
            ' "$nixos_json" > nixos-common-option-keys.json
            cmp core-option-keys.json nixos-common-option-keys.json
            jq -e '
              has("services.tally.group")
              and has("services.tally.user")
            ' "$nixos_json" >/dev/null
            jq -e '
              all(to_entries[];
                (.value.type | type == "string" and length > 0)
                and (.value.description | type == "string" and length > 0)
              )
              and has("services.tally.producers.<name>.enqueue.gateManifest.requiredGateIds")
              and has("services.tally.adapters.<name>.hardening")
              and has("services.tally.transport.maxFrameBytes")
              and has("services.tally.scheduling.agingThresholdSec")
              and (
                [
                  keys[]
                  | select(startswith("services.tally.flows.<name>."))
                ]
                | length == 13
              )
            ' "$core_json" >/dev/null

            ${pkgs.bash}/bin/bash ./check-summary.sh src
            mdbook build --dest-dir book
            runHook postBuild
          '';
          installPhase = ''
            runHook preInstall
            mkdir -p "$out"
            cp -R book/html/. "$out/"
            runHook postInstall
          '';
          meta = {
            description = "The checked tally documentation book";
            license = pkgs.lib.licenses.mit;
          };
        };
        documentationPublisher = pkgs.writeShellApplication {
          name = "tally-publish-docs";
          runtimeInputs = [
            pkgs.coreutils
            pkgs.git
            pkgs.rsync
          ];
          text = ''
            exec ${pkgs.bash}/bin/bash ${./doc/publish.sh} ${documentation} "$@"
          '';
        };
        catalogFixtureInput = import ./test/fixtures/catalog/valid.nix;
        catalogFixtureUnchecked = catalogLibrary.renderCatalog (
          catalogFixtureInput
          // {
            inherit pkgs;
          }
        );
        catalogFixture = catalogLibrary.mkCatalog (
          catalogFixtureInput
          // {
            inherit pkgs;
            package = tally;
          }
        );
        mkCatalogRejectionCheck =
          {
            name,
            fixture,
            expectedMessage,
          }:
          let
            input = import fixture;
            evaluated = catalogLibrary.evalCatalog input;
            failureMessages = map (entry: entry.message) (
              builtins.filter (entry: !entry.assertion) (catalogLibrary.mkCatalogAssertions evaluated)
            );
            attempt = builtins.tryEval (
              toString (
                catalogLibrary.mkCatalog (
                  input
                  // {
                    inherit pkgs;
                    package = tally;
                  }
                )
              )
            );
          in
          assert failureMessages != [ ];
          assert builtins.head failureMessages == expectedMessage;
          assert !attempt.success;
          pkgs.runCommand name { } ''
            printf '%s\n' ${pkgs.lib.escapeShellArg expectedMessage} >"$out"
          '';
        tallyWitnessEmit = import ./nix/lib/witness-emitter.nix {
          lib = pkgs.lib;
          inherit pkgs;
          tallyPackage = tally;
        };
        # Public test-only key for the isolated Attic VM; never use it as a secret.
        atticServerEnvironment = pkgs.runCommand "tally-attic-server-environment" { } ''
          printf 'ATTIC_SERVER_TOKEN_RS256_SECRET_BASE64=' >"$out"
          ${pkgs.coreutils}/bin/base64 -w0 \
            ${./test/fixtures/attic/throwaway-token-rs256.pem} >>"$out"
          printf '\n' >>"$out"
        '';
        flowWorkerHandoff = pkgs.writeShellApplication {
          name = "tally-fs7-worker-handoff";
          runtimeInputs = [
            pkgs.attic-client
            pkgs.coreutils
            pkgs.git
            pkgs.nix
          ];
          text = ''
            work="$(mktemp -d)"
            trap 'rm -rf "$work"' EXIT
            git clone /srv/tally-fs7-handoff.git "$work/repository"
            git -C "$work/repository" config user.name "tally FS-7 worker"
            git -C "$work/repository" config user.email "tally-fs7-worker@example.invalid"
            printf '%s\n' 'artifact-created-on-worker' >"$work/repository/artifact.txt"
            printf '%s\n' 'artifact-created-on-worker-through-attic' >"$work/attic-artifact"
            store_path="$(
              nix --extra-experimental-features nix-command \
                store add --name tally-attic-handoff "$work/attic-artifact"
            )"
            attic push tally:tally-handoff "$store_path"
            printf '%s\n' "$store_path" >"$work/repository/attic-store-path.txt"
            git -C "$work/repository" add artifact.txt attic-store-path.txt
            git -C "$work/repository" commit -m 'FS-7 worker artifact'
            touch /tmp/tally-fs7-worker-started
            sleep 15
            git -C "$work/repository" push origin HEAD:refs/heads/artifact
          '';
        };
        flowCoordinatorHandoff = pkgs.writeShellApplication {
          name = "tally-fs7-coordinator-handoff";
          runtimeInputs = [
            pkgs.coreutils
            pkgs.git
            pkgs.nix
          ];
          text = ''
            work="$(mktemp -d)"
            trap 'rm -rf "$work"' EXIT
            git clone --branch artifact git://worker/tally-fs7-handoff.git "$work/repository"
            test "$(cat "$work/repository/artifact.txt")" = artifact-created-on-worker
            store_path="$(cat "$work/repository/attic-store-path.txt")"
            case "$store_path" in
              /nix/store/*-tally-attic-handoff) ;;
              *)
                echo "worker returned an invalid Attic store path: $store_path" >&2
                exit 1
                ;;
            esac
            if nix-store --query --hash "$store_path" >/dev/null 2>&1; then
              echo "Attic handoff path was present before substitution: $store_path" >&2
              exit 1
            fi
            nix-store --realise "$store_path"
            test "$(cat "$store_path")" = artifact-created-on-worker-through-attic
            {
              printf '%s\n' 'artifact-created-on-worker'
              git -C "$work/repository" rev-parse HEAD
            } >/tmp/tally-fs7-coordinator-consumed
            {
              printf '%s\n' "$store_path"
              cat "$store_path"
            } >/tmp/tally-attic-coordinator-consumed
          '';
        };
        multiHostFlowArgs = {
          workerProgram = "${flowWorkerHandoff}/bin/tally-fs7-worker-handoff";
          coordinatorProgram = "${flowCoordinatorHandoff}/bin/tally-fs7-coordinator-handoff";
        };
        multiHostFlowArgsJson = builtins.toJSON multiHostFlowArgs;
        gitAiRemoteTally = pkgs.writeShellScript "tally-d12-remote-helper" ''
          export PATH="/var/lib/tally-worker/fleet-bin:${pkgs.git}/bin:${pkgs.coreutils}/bin:${pkgs.python3}/bin:$PATH"
          exec ${tally}/bin/tally "$@"
        '';
        gitAiRemoteJob = pkgs.writeShellApplication {
          name = "tally-d12-remote-job";
          runtimeInputs = [
            pkgs.coreutils
            pkgs.git
          ];
          text = ''
            printf '%s\n' 'authored on the remote worktree host' > remote-authored.txt
            git add remote-authored.txt
            git commit -qm 'D12 remote authorship fixture'
            revision="$(git rev-parse HEAD)"
            printf \
              '{"schemaVersion":1,"artifact":{"resultRevision":"%s"},"gates":[{"id":"d12","status":"pass"}]}\n' \
              "$revision" > gates.json
            touch remote-job-running
            sleep 15
          '';
        };
        gitAiRemoteConfig = pkgs.writeText "tally-d12-remote-config.json" (
          builtins.toJSON {
            retention.enable = false;
            attestations.exec.enable = false;
            gitAi = {
              enable = true;
              mode = "required";
              awaitTimeoutSec = 30;
              globalAwaitOk = false;
            };
            pools.worker-slot = {
              resource = "build-slot";
              capacity = 1;
              enforce = "cooperative";
            };
            adapters.shell.hardening = "workspace";
            executors.worker = {
              kind = "ssh";
              host = "worker";
              user = "tally-worker";
              sshProgram = "${pkgs.openssh}/bin/ssh";
              identityFile = "/etc/tally-fs7/id_ed25519";
              knownHostsFile = "/etc/tally-fs7/worker-known-hosts";
              program = "${gitAiRemoteTally}";
              stateDir = "/var/lib/tally-remote";
              connectTimeoutSec = 5;
              serverAliveIntervalSec = 1;
              serverAliveCountMax = 2;
              retryIntervalMs = 100;
            };
          }
        );
        flowReplayProgram = pkgs.writeShellApplication {
          name = "tally-fs7-replay";
          text = ''
            if [ "$#" -ne 2 ]; then
              echo "usage: tally-fs7-replay FLOW_RUN_ID RUNNER_JOB_ID" >&2
              exit 2
            fi
            export TALLY_TASK_UUID="$1"
            export TALLY_JOB_ID="$2"
            exec ${tally}/bin/tally \
              --config /var/lib/tally-coordinator/.config/tally/config.json \
              --socket /run/user/1000/tally/tally.sock \
              flow run ${./test/fixtures/flows/multi-host.js} \
              --args ${pkgs.lib.escapeShellArg multiHostFlowArgsJson} \
              --max-nodes 2 \
              --flow-run-id "$1"
          '';
        };
        execAttestationMutator = pkgs.writeShellApplication {
          name = "tally-exec-attestation-mutator";
          runtimeInputs = [
            pkgs.coreutils
            pkgs.jq
          ];
          text = ''
            if [ "$#" -ne 3 ]; then
              echo "usage: tally-exec-attestation-mutator MODE INPUT OUTPUT" >&2
              exit 2
            fi

            mode="$1"
            input="$2"
            output="$3"
            case "$mode" in
              stale-next | rewrite) ;;
              *)
                echo "unknown mutation mode: $mode" >&2
                exit 2
                ;;
            esac

            previous="sha256:0000000000000000000000000000000000000000000000000000000000000000"
            first=1
            : >"$output"
            while IFS= read -r record; do
              if [ "$first" -eq 1 ]; then
                record="$(jq -c '.payload.exitCode = 17' <<<"$record")"
              fi

              if [ "$mode" = rewrite ] || [ "$first" -eq 1 ]; then
                if [ "$mode" = rewrite ]; then
                  cleared="$(jq -c --arg previous "$previous" \
                    '.prev_hash = $previous | .hash = ""' <<<"$record")"
                else
                  cleared="$(jq -c '.hash = ""' <<<"$record")"
                fi
                digest="$(printf '%s' "$cleared" | sha256sum | cut -d' ' -f1)"
                record="$(jq -c --arg hash "sha256:$digest" \
                  '.hash = $hash' <<<"$cleared")"
              fi

              printf '%s\n' "$record" >>"$output"
              previous="$(jq -r '.hash' <<<"$record")"
              first=0
            done <"$input"

            if [ "$first" -eq 1 ]; then
              echo "input ledger is empty" >&2
              exit 2
            fi
          '';
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
                transport.maxFrameBytes = 33554432;
                scheduling.agingThresholdSec = 900;
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
                  flow-run-mutex = {
                    resource = "mutex";
                    capacity = 1;
                    predicate.co-residency = { };
                  };
                  build.resource = "build-slot";
                  worker-gpu.resource = "vram";
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
                  hardening = "strict";
                  skillRevision = "project-codex-v3";
                };
                adapters.shell.hardening = "workspace";
                adapters.explicit-none = {
                  argv = [ "true" ];
                  hardening = "none";
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
                flows = {
                  fixture = {
                    script = ./test/fixtures/flows/valid.js;
                    onCalendar = "daily";
                    args.task = "ship";
                    catalog = catalogFixture;
                    budgetPool = "programmatic";
                    workloadMutex = "flow-run-mutex";
                    extraEnv.FLOW_MODE = "fixture";
                    credentials.FLOW_TOKEN = "/run/credentials/tally-flow";
                  };
                  manual = {
                    script = ./test/fixtures/flows/valid.js;
                    args.task = "manual";
                    catalog = catalogFixture;
                  };
                  monthly-dedup = {
                    script = ./test/fixtures/flows/valid.js;
                    onCalendar = "monthly";
                    dedupKey = "monthly-local-ai-review-%Y-%m";
                    evidence = [
                      "exit:0"
                      "artifact:/tmp/monthly-review-receipt.json"
                      "hash:sha256"
                    ];
                    args.task = "monthly";
                    catalog = catalogFixture;
                  };
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
        invalidFlowSchema = pkgs.lib.evalModules {
          modules = [
            {
              options.services.tally = moduleCommon.mkOptions {
                defaultPackage = tally;
                defaultDataDir = "/tmp/tally-data";
                defaultStateDir = "/tmp/tally-state";
              };
              config.services.tally = {
                pools = {
                  build.resource = "build-slot";
                  flow.resource = "cpu-slot";
                  worker-gpu.resource = "vram";
                  wide-mutex = {
                    resource = "vram";
                    capacity = 2;
                    predicate.co-residency = { };
                  };
                  programmatic = {
                    resource = "budget";
                    predicate.windowed-consumption = {
                      windowSec = 18000;
                      consumptionCap = 100;
                    };
                  };
                };
                flows.bad-budget = {
                  script = ./test/fixtures/flows/valid.js;
                  args.task = "ship";
                  catalog = catalogFixture;
                  budgetPool = "missing-budget";
                };
                flows.missing-mutex = {
                  script = ./test/fixtures/flows/valid.js;
                  args.task = "ship";
                  catalog = catalogFixture;
                  workloadMutex = "absent";
                };
                flows.reserved-mutex = {
                  script = ./test/fixtures/flows/valid.js;
                  args.task = "ship";
                  catalog = catalogFixture;
                  workloadMutex = "flow";
                };
                flows.wrong-mutex = {
                  script = ./test/fixtures/flows/valid.js;
                  args.task = "ship";
                  catalog = catalogFixture;
                  workloadMutex = "worker-gpu";
                };
                flows.windowed-mutex = {
                  script = ./test/fixtures/flows/valid.js;
                  args.task = "ship";
                  catalog = catalogFixture;
                  workloadMutex = "programmatic";
                };
                flows.wide-mutex = {
                  script = ./test/fixtures/flows/valid.js;
                  args.task = "ship";
                  catalog = catalogFixture;
                  workloadMutex = "wide-mutex";
                };
              };
            }
          ];
        };
        invalidFlowMessages = map (entry: entry.message) (
          builtins.filter (entry: !entry.assertion) (
            moduleCommon.mkAssertions invalidFlowSchema.config.services.tally
          )
        );
        mkFlowConfig =
          {
            script,
            args ? { },
            catalog ? null,
            maxNodes ? 1000,
            flowOptions ? { },
            pools ? {
              build.resource = "build-slot";
              worker-gpu.resource = "vram";
            },
          }:
          (pkgs.lib.evalModules {
            modules = [
              {
                options.services.tally = moduleCommon.mkOptions {
                  defaultPackage = tally;
                  defaultDataDir = "/tmp/tally-data";
                  defaultStateDir = "/tmp/tally-state";
                };
                config.services.tally = {
                  pools = pools // {
                    flow = {
                      resource = "cpu-slot";
                      capacity = 8;
                      enforce = "cooperative";
                      hardPreempt = false;
                    };
                  };
                  flows.fixture = {
                    inherit
                      script
                      args
                      catalog
                      maxNodes
                      ;
                  }
                  // flowOptions;
                };
              }
              (
                { config, ... }:
                {
                  config.services.tally.adapters = moduleCommon.adapterDefaults;
                  config.services.tally.producers = moduleCommon.mkFlowProducers config.services.tally.flows;
                }
              )
            ];
          }).config.services.tally;
        flowValidCheckedConfig = moduleCommon.mkCheckedConfig (mkFlowConfig {
          script = ./test/fixtures/flows/valid.js;
          args.task = "ship";
          catalog = catalogFixture;
        });
        mkScheduledFlowConfig =
          flowOptions:
          mkFlowConfig {
            script = ./test/fixtures/flows/valid.js;
            args.task = "ship";
            catalog = catalogFixture;
            inherit flowOptions;
          };
        flowDedupTemplateFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-dedup-template-failure";
          drv = moduleCommon.mkCheckedConfig (mkScheduledFlowConfig {
            onCalendar = "monthly";
            dedupKey = "monthly-review-%Q";
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            "dedupKey is not a valid strftime template"
          ];
        };
        flowArtifactEvidenceFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-artifact-evidence-failure";
          drv = moduleCommon.mkCheckedConfig (mkScheduledFlowConfig {
            onCalendar = "monthly";
            evidence = [
              "exit:0"
              "artifact:relative/monthly-review-receipt.json"
              "hash:sha256"
            ];
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            "artifact evidence requires an absolute path"
          ];
        };
        flowNonliteralFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-nonliteral-meta-failure";
          drv = moduleCommon.mkCheckedConfig (mkFlowConfig {
            script = ./test/fixtures/flows/nonliteral-meta.js;
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            ''tally: {"name":"FlowMetaError","code":"meta-nonliteral","message":"meta must contain only JSON-compatible literals","location":{"line":5,"column":25}}''
          ];
        };
        flowBannedGlobalFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-banned-global-failure";
          drv = moduleCommon.mkCheckedConfig (mkFlowConfig {
            script = ./test/fixtures/flows/banned-global.js;
          });
          expectedBuilderExitCode = 10;
          expectedBuilderLogEntries = [
            ''tally: {"name":"FlowDeterminismError","code":"determinism-violation","message":"banned global Math.random is unavailable in flow scripts","location":{"line":8,"column":1},"details":{"global":"Math.random"}}''
          ];
        };
        flowUndeclaredPoolFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-undeclared-pool-failure";
          drv = moduleCommon.mkCheckedConfig (mkFlowConfig {
            script = ./test/fixtures/flows/undeclared-pool.js;
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            ''tally: {"name":"FlowPoolError","code":"undeclared-pool","message":"pool \"worker-gpu\" is used by the script but absent from meta.pools","location":{"line":8,"column":31},"details":{"pool":"worker-gpu"}}''
          ];
        };
        flowBadArgsSchemaFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-bad-args-schema-failure";
          drv = moduleCommon.mkCheckedConfig (mkFlowConfig {
            script = ./test/fixtures/flows/bad-args-schema.js;
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            ''tally: {"name":"FlowMetaError","code":"args-schema-invalid","message":"meta.argsSchema is not a valid JSON Schema: \"definitely-not-a-json-schema-type\" is not valid under any of the schemas listed in the 'anyOf' keyword","location":{"line":1,"column":21}}''
          ];
        };
        flowArgsMismatchFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-args-mismatch-failure";
          drv = moduleCommon.mkCheckedConfig (mkFlowConfig {
            script = ./test/fixtures/flows/valid.js;
            args.task = 7;
            catalog = catalogFixture;
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            ''tally: {"name":"FlowArgsError","code":"args-schema-mismatch","message":"args do not match meta.argsSchema: /task: 7 is not of type \"string\"","location":{"line":1,"column":21},"details":{"errors":["/task: 7 is not of type \"string\""]}}''
          ];
        };
        flowPoolClosureFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-pool-closure-failure";
          drv = moduleCommon.mkCheckedConfig (mkFlowConfig {
            script = ./test/fixtures/flows/valid.js;
            args.task = "ship";
            catalog = catalogFixture;
            pools.build.resource = "build-slot";
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            "tally flow fixture references unknown pool worker-gpu"
          ];
        };
        flowWindowedConsumptionFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-windowed-consumption-failure";
          drv = moduleCommon.mkCheckedConfig (mkFlowConfig {
            script = ./test/fixtures/flows/valid.js;
            args.task = "ship";
            catalog = catalogFixture;
            pools = {
              build.resource = "build-slot";
              worker-gpu = {
                resource = "budget";
                predicate.windowed-consumption = {
                  windowSec = 18000;
                  consumptionCap = 100;
                };
              };
            };
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            ''tally: {"name":"FlowPoolError","code":"windowed-consumption-excluded","message":"flows are excluded from windowed-consumption admission by design; use priorities to control contention between workloads (pool \"worker-gpu\")","details":{"pool":"worker-gpu","control":"priorities"}}''
          ];
        };
        flowReservedPoolFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-reserved-pool-failure";
          drv = moduleCommon.mkCheckedConfig (mkFlowConfig {
            script = ./test/fixtures/flows/reserved-flow-pool.js;
            pools = { };
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            "tally flow fixture script meta.pools must not include flow"
          ];
        };
        flowReservedBuildPoolFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-reserved-build-pool-failure";
          drv = moduleCommon.mkCheckedConfig (mkFlowConfig {
            script = ./test/fixtures/flows/reserved-build-pool.js;
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            "tally flow fixture script meta.pools must not include build"
          ];
        };
        flowMaxNodesFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-max-nodes-failure";
          drv = moduleCommon.mkCheckedConfig (mkFlowConfig {
            script = ./test/fixtures/flows/valid.js;
            args.task = "ship";
            catalog = catalogFixture;
            maxNodes = 4;
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            "tally flow fixture maxNodes 4 is less than script meta.maxNodes 5"
          ];
        };
        flowCatalogFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-catalog-schema-failure";
          drv = moduleCommon.mkCheckedConfig (mkFlowConfig {
            script = ./test/fixtures/flows/valid.js;
            args.task = "ship";
            catalog = ./test/fixtures/flows/invalid-catalog.json;
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            ''tally: {"name":"FlowCatalogError","code":"catalog-schema-mismatch","message":"catalog does not match schema: /members/0: \"family\" is a required property; /members/0: \"maker\" is a required property; /members/0: \"classes\" is a required property; /members/0: \"adapter\" is a required property; /members/0: \"pools\" is a required property; /members/0: \"launch\" is a required property","location":{"line":1,"column":1},"details":{"errors":["/members/0: \"family\" is a required property","/members/0: \"maker\" is a required property","/members/0: \"classes\" is a required property","/members/0: \"adapter\" is a required property","/members/0: \"pools\" is a required property","/members/0: \"launch\" is a required property"]}}''
          ];
        };
        flowCatalogRequiredFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-catalog-required-failure";
          drv = moduleCommon.mkCheckedConfig (mkFlowConfig {
            script = ./test/fixtures/flows/valid.js;
            args.task = "ship";
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            "tally flow fixture declares selectors but has no catalog"
          ];
        };
        flowCatalogSelectorFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-catalog-selector-failure";
          drv = moduleCommon.mkCheckedConfig (mkFlowConfig {
            script = ./examples/flows/pooled-review.js;
            args = {
              subject = "audit";
              minimumValid = 2;
            };
            catalog = ./test/fixtures/flows/catalog.json;
            pools.worker-gpu.resource = "vram";
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            ''tally: {"name":"FlowSelectorError","code":"selector-empty","message":"selector class \"pooled-strongest\" resolves to no catalog members","location":{"line":1,"column":21},"details":{"selector":"pooled-strongest"}}''
          ];
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
        unsupportedSystemNixos = nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.tally
            nixosBase
            {
              services.tally = {
                producers.daily = {
                  kind = "calendar";
                  onCalendar = "daily";
                  enqueue = {
                    argv = [ "daily-job" ];
                    pool = "metered";
                  };
                };
                flows.fixture.script = ./test/fixtures/flows/valid.js;
                pools.metered = {
                  resource = "budget";
                  predicate.windowed-consumption = {
                    windowSec = 3600;
                    consumptionCap = 1000;
                  };
                  usageMeter = {
                    argv = [ "usage-meter" ];
                    pollIntervalSec = 60;
                    budgetClass = "metered";
                  };
                };
              };
            }
          ];
        };
        unsupportedSystemMessages = builtins.map (entry: entry.message) (
          builtins.filter (entry: !entry.assertion) unsupportedSystemNixos.config.assertions
        );
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
              users.users.tallyhome = {
                isNormalUser = true;
                uid = 1000;
                createHome = true;
                home = "/var/lib/tally-test-user";
              };

              services.tally = {
                enable = true;
                retention.onCalendar = "2099-01-01 00:00:00";
                pools.stock = {
                  resource = "build-slot";
                  enforce = "cooperative";
                };
              };

              home-manager = {
                useGlobalPkgs = true;
                useUserPackages = true;
                users.tallyhome = {
                  imports = [ self.homeManagerModules.tally ];
                  home = {
                    username = "tallyhome";
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
            machine.succeed("systemctl start linger-users.service")
            machine.succeed("test -e /var/lib/systemd/linger/tally")
            machine.wait_for_unit("tally-daemon.service")
            machine.wait_for_unit("tally-drain.timer")
            machine.wait_for_unit("tally-retention.timer")
            machine.succeed("systemctl is-active tally-daemon.service")
            machine.succeed("systemctl is-active tally-drain.timer")
            machine.succeed("systemctl is-active tally-retention.timer")
            machine.succeed("test \"$(systemctl show tally-daemon.service --property=User --value)\" = tally")
            machine.succeed("test \"$(stat -c '%U:%G:%a' /run/tally)\" = tally:tally:700")
            machine.succeed("test -d /var/lib/tally")
            machine.succeed("test -d /var/log/tally")
            machine.succeed("grep -F '\"enforce\":\"cooperative\"' /etc/tally/config.json")

            machine.succeed("systemctl stop tally-daemon.service")
            machine.succeed("install -o root -g root -m 0600 /dev/null /var/lib/tally/data/attestations.jsonl")
            machine.succeed("chown -R root:root /var/lib/tally")
            machine.succeed("/run/current-system/activate")
            machine.succeed("test \"$(stat -c '%U:%G:%a' /var/lib/tally/data/attestations.jsonl)\" = tally:tally:600")
            machine.succeed("systemctl start tally-daemon.service")
            machine.wait_for_unit("tally-daemon.service")
            machine.succeed("systemctl start 'tally-witness-emit@success:upgrade-check.service'")
            machine.succeed("test \"$(stat -c '%U:%G:%a' /var/lib/tally/data/attestations.jsonl)\" = tally:tally:600")
            machine.succeed("${pkgs.jq}/bin/jq -e 'select(.payload.outcome == \"success\" and .payload.unit == \"upgrade-check\")' /var/lib/tally/data/attestations.jsonl")

            machine.wait_for_unit("home-manager-tallyhome.service")
            machine.succeed("loginctl enable-linger tallyhome")
            machine.succeed("systemctl start user@1000.service")
            machine.wait_until_succeeds(
              "runuser -u tallyhome -- env HOME=/var/lib/tally-test-user XDG_RUNTIME_DIR=/run/user/1000 systemctl --user is-active tally-daemon.service"
            )
            machine.succeed("test -S /run/user/1000/tally/tally.sock")

            user = "runuser -u tallyhome -- env HOME=/var/lib/tally-test-user XDG_RUNTIME_DIR=/run/user/1000"
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
        systemSocketExecutionTest = pkgs.testers.runNixOSTest {
          name = "tally-system-socket-execution";
          nodes.machine =
            { ... }:
            {
              imports = [ self.nixosModules.tally ];
              system.stateVersion = "26.11";

              services.tally = {
                enable = true;
                dataDir = "/srv/tally/data";
                stateDir = "/srv/tally/state";
                retention.enable = false;
                pools.stock = {
                  resource = "build-slot";
                  capacity = 1;
                  enforce = "cooperative";
                };
              };
            };
          testScript = ''
            import json

            machine.start()
            machine.wait_for_unit("multi-user.target")
            machine.succeed("systemctl start linger-users.service")
            machine.succeed("test -e /var/lib/systemd/linger/tally")
            machine.wait_for_unit("tally-daemon.service")
            uid = machine.succeed("id -u tally").strip()
            machine.wait_for_unit(f"user@{uid}.service")

            machine.succeed("test -S /run/tally/tally.sock")
            machine.succeed("test \"$(stat -c '%U:%G:%a' /run/tally)\" = tally:tally:700")
            machine.succeed("test \"$(stat -c '%U:%G:%a' /srv/tally/data)\" = tally:tally:700")
            machine.succeed("test \"$(stat -c '%U:%G:%a' /srv/tally/state)\" = tally:tally:700")

            result = json.loads(machine.succeed(
              "${tally}/bin/tally --socket /run/tally/tally.sock enqueue --pool stock --wait -- ${pkgs.coreutils}/bin/true"
            ))
            assert result["verdict"] == "pass", result
            assert result["exit_code"] == 0, result
            task = result["task_uuid"]

            queried = json.loads(machine.succeed(
              "${tally}/bin/tally --socket /run/tally/tally.sock query job " + task
            ))
            assert queried["job"]["terminalVerdict"] == "pass", queried
            exit_record = f"/srv/tally/state/unit-exit/{task}.json"
            machine.succeed(f"test \"$(stat -c '%U:%G' {exit_record})\" = tally:tally")
            machine.succeed(f"test \"$(stat -c '%u' {exit_record})\" != 0")
          '';
        };
        retentionTest = pkgs.testers.runNixOSTest {
          name = "tally-retention-liveness-floor";
          nodes.machine =
            { ... }:
            {
              imports = [ home-manager.nixosModules.home-manager ];

              system.stateVersion = "26.11";
              virtualisation.memorySize = 1536;
              nix.settings.experimental-features = [ "nix-command" ];
              users.users.tally = {
                isNormalUser = true;
                uid = 1000;
                createHome = true;
                home = "/var/lib/tally-retention";
                linger = true;
              };

              home-manager = {
                useGlobalPkgs = true;
                useUserPackages = true;
                users.tally = {
                  imports = [ self.homeManagerModules.tally ];
                  home = {
                    username = "tally";
                    homeDirectory = "/var/lib/tally-retention";
                    stateVersion = "26.11";
                  };
                  services.tally = {
                    enable = true;
                    retention.enable = false;
                    pools.stock = {
                      resource = "build-slot";
                      enforce = "cooperative";
                    };
                  };
                };
              };
            };
          testScript = ''
            machine.start()
            machine.wait_for_unit("multi-user.target")
            machine.wait_for_unit("home-manager-tally.service")
            machine.wait_for_unit("user@1000.service")

            user = "runuser -u tally -- env HOME=/var/lib/tally-retention XDG_RUNTIME_DIR=/run/user/1000"
            socket = "/run/user/1000/tally/tally.sock"
            data = "/var/lib/tally-retention/.local/share/tally"
            cli = user + " ${tally}/bin/tally --socket " + socket
            nix_store = user + " ${pkgs.nix}/bin/nix-store"
            machine.wait_until_succeeds("test -S " + socket)

            def enqueue(command):
              status, output = machine.execute(command)
              if status != 0:
                daemon_log = machine.succeed(
                  user + " journalctl --user --unit=tally-daemon.service --output=cat --no-pager"
                )
                raise Exception(output + "\n--- tally daemon journal ---\n" + daemon_log)
              return output

            machine.succeed("printf old-only > /tmp/tally-old-only")
            machine.succeed("printf shared > /tmp/tally-shared")
            old_only = machine.succeed(nix_store + " --add /tmp/tally-old-only").strip()
            shared = machine.succeed(nix_store + " --add /tmp/tally-shared").strip()

            enqueue(
              cli + " enqueue --pool stock" +
              " --evidence store:" + old_only +
              " --evidence store:" + shared +
              " --wait -- ${pkgs.coreutils}/bin/sleep 1"
            )
            machine.succeed(nix_store + " --check-validity " + old_only)
            machine.succeed(nix_store + " --check-validity " + shared)

            machine.succeed("${pkgs.coreutils}/bin/sleep 4")
            enqueue(
              cli + " enqueue --pool stock" +
              " --evidence store:" + shared +
              " --wait -- ${pkgs.coreutils}/bin/sleep 1"
            )
            ledger_hash = machine.succeed(
              "${pkgs.coreutils}/bin/sha256sum " + data + "/witness.jsonl"
            ).split()[0]

            report = machine.succeed(
              user + " ${tally}/bin/tally gc --horizon 3s --collect --data-dir " + data
            )
            assert '"rootsPruned":1' in report, report
            assert ledger_hash == machine.succeed(
              "${pkgs.coreutils}/bin/sha256sum " + data + "/witness.jsonl"
            ).split()[0]
            machine.fail(nix_store + " --check-validity " + old_only)
            machine.succeed(nix_store + " --check-validity " + shared)
          '';
        };
        flowMultiHostTest = pkgs.testers.runNixOSTest {
          name = "tally-flow-multi-host";
          nodes = {
            coordinator =
              { ... }:
              {
                imports = [ home-manager.nixosModules.home-manager ];

                system.stateVersion = "26.11";
                virtualisation.memorySize = 1536;
                networking.firewall.allowedTCPPorts = [ 8080 ];
                nix.settings.trusted-users = [
                  "root"
                  "tally"
                ];
                users.users.tally = {
                  isNormalUser = true;
                  uid = 1000;
                  createHome = true;
                  home = "/var/lib/tally-coordinator";
                  linger = true;
                };
                environment.systemPackages = [
                  tally
                  pkgs.attic-client
                  pkgs.git
                  pkgs.jq
                ];
                services.atticd = {
                  enable = true;
                  environmentFile = atticServerEnvironment;
                };
                environment.etc = {
                  "tally-fs7/id_ed25519" = {
                    source = ./test/fixtures/ssh/fs7_coordinator_ed25519;
                    mode = "0400";
                    user = "tally";
                    group = "users";
                  };
                  "tally-fs7/worker-known-hosts" = {
                    source = ./test/fixtures/ssh/fs7_worker_known_hosts;
                    mode = "0444";
                  };
                };

                home-manager = {
                  useGlobalPkgs = true;
                  useUserPackages = true;
                  users.tally = {
                    imports = [ self.homeManagerModules.tally ];
                    home = {
                      username = "tally";
                      homeDirectory = "/var/lib/tally-coordinator";
                      stateVersion = "26.11";
                    };
                    services.tally = {
                      enable = true;
                      pools = {
                        coordinator-slot = {
                          resource = "build-slot";
                          capacity = 1;
                        };
                        worker-slot = {
                          resource = "build-slot";
                          capacity = 1;
                        };
                      };
                      executors.worker = {
                        host = "worker";
                        user = "tally-worker";
                        identityFile = "/etc/tally-fs7/id_ed25519";
                        knownHostsFile = "/etc/tally-fs7/worker-known-hosts";
                        program = "${tally}/bin/tally";
                        stateDir = "/var/lib/tally-remote";
                        connectTimeoutSec = 5;
                        serverAliveIntervalSec = 1;
                        serverAliveCountMax = 2;
                        retryIntervalMs = 100;
                      };
                      flows.multi-host = {
                        script = ./test/fixtures/flows/multi-host.js;
                        onCalendar = "2099-01-01 00:00:00";
                        args = multiHostFlowArgs;
                        maxNodes = 2;
                        runtimeMaxSec = 120;
                      };
                    };
                  };
                };
              };

            worker =
              { lib, ... }:
              {
                system.stateVersion = "26.11";
                virtualisation.memorySize = 1024;
                networking.firewall.allowedTCPPorts = [
                  22
                  9418
                ];
                environment.systemPackages = [
                  tally
                  pkgs.git
                  pkgs.jq
                ];
                environment.etc."ssh/ssh_host_ed25519_key" = {
                  source = ./test/fixtures/ssh/fs7_worker_host_ed25519;
                  mode = "0600";
                };
                environment.etc."ssh/ssh_host_ed25519_key.pub" = {
                  source = ./test/fixtures/ssh/fs7_worker_host_ed25519.pub;
                  mode = "0644";
                };
                users.users.tally-worker = {
                  isNormalUser = true;
                  uid = 1001;
                  createHome = true;
                  home = "/var/lib/tally-worker";
                  linger = true;
                  openssh.authorizedKeys.keys = [
                    (builtins.readFile ./test/fixtures/ssh/fs7_coordinator_ed25519.pub)
                  ];
                };
                services.openssh = {
                  enable = true;
                  hostKeys = [
                    {
                      path = "/etc/ssh/ssh_host_ed25519_key";
                      type = "ed25519";
                    }
                  ];
                  settings = {
                    PasswordAuthentication = false;
                    KbdInteractiveAuthentication = false;
                    PermitRootLogin = "no";
                    AllowUsers = [ "tally-worker" ];
                  };
                };
                systemd.tmpfiles.rules = [
                  "d /var/lib/tally-remote 0700 tally-worker users -"
                  "d /srv/tally-fs7-handoff.git 0750 tally-worker users -"
                ];
                systemd.services.tally-fs7-repository = {
                  description = "initialize the FS-7 sanctioned Git handoff repository";
                  wantedBy = [ "multi-user.target" ];
                  before = [ "tally-fs7-git.service" ];
                  path = [
                    pkgs.coreutils
                    pkgs.git
                  ];
                  script = ''
                    if [ ! -d /srv/tally-fs7-handoff.git/objects ]; then
                      git init --bare /srv/tally-fs7-handoff.git
                    fi
                    touch /srv/tally-fs7-handoff.git/git-daemon-export-ok
                  '';
                  serviceConfig = {
                    Type = "oneshot";
                    RemainAfterExit = true;
                    User = "tally-worker";
                    Group = "users";
                  };
                };
                systemd.services.tally-fs7-git = {
                  description = "serve the FS-7 Git handoff repository";
                  wantedBy = [ "multi-user.target" ];
                  after = [
                    "network.target"
                    "tally-fs7-repository.service"
                  ];
                  requires = [ "tally-fs7-repository.service" ];
                  serviceConfig = {
                    Type = "simple";
                    User = "tally-worker";
                    Group = "users";
                    ExecStart = lib.escapeShellArgs [
                      "${pkgs.git}/bin/git"
                      "daemon"
                      "--reuseaddr"
                      "--base-path=/srv"
                      "--export-all"
                      "--verbose"
                      "/srv/tally-fs7-handoff.git"
                    ];
                    Restart = "on-failure";
                  };
                };
              };
          };
          testScript = ''
            start_all()

            coordinator.wait_for_unit("atticd.service")
            coordinator.wait_for_open_port(8080)
            attic_token = coordinator.succeed(
              "atticd-atticadm make-token --sub tally-multi-host --validity 1h "
              "--create-cache '*' --pull '*' --push '*' --delete '*' "
              "--configure-cache '*' --configure-cache-retention '*'"
            ).strip()
            coordinator.succeed(
              "runuser -u tally -- env HOME=/var/lib/tally-coordinator "
              "${pkgs.attic-client}/bin/attic login --set-default tally "
              "http://coordinator:8080 " + attic_token
            )
            coordinator.succeed(
              "runuser -u tally -- env HOME=/var/lib/tally-coordinator "
              "${pkgs.attic-client}/bin/attic cache create --public tally:tally-handoff"
            )
            coordinator.succeed(
              "runuser -u tally -- env HOME=/var/lib/tally-coordinator "
              "${pkgs.attic-client}/bin/attic use tally:tally-handoff"
            )
            worker.succeed(
              "runuser -u tally-worker -- env HOME=/var/lib/tally-worker "
              "${pkgs.attic-client}/bin/attic login tally "
              "http://coordinator:8080 " + attic_token
            )

            worker.wait_for_unit("sshd.service")
            worker.wait_for_unit("tally-fs7-git.service")
            worker.wait_until_succeeds(
              "runuser -u tally-worker -- env HOME=/var/lib/tally-worker XDG_RUNTIME_DIR=/run/user/1001 systemctl --user is-active default.target"
            )

            coordinator.wait_for_unit("home-manager-tally.service")
            coordinator.wait_until_succeeds(
              "runuser -u tally -- env HOME=/var/lib/tally-coordinator XDG_RUNTIME_DIR=/run/user/1000 systemctl --user is-active tally-daemon.service"
            )
            worker_ssh = (
              "runuser -u tally -- ${pkgs.openssh}/bin/ssh -F /dev/null "
              "-o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes "
              "-o UserKnownHostsFile=/etc/tally-fs7/worker-known-hosts "
              "-i /etc/tally-fs7/id_ed25519 tally-worker@worker"
            )
            coordinator.succeed(worker_ssh + " true")

            user = (
              "runuser -u tally -- env HOME=/var/lib/tally-coordinator "
              "XDG_RUNTIME_DIR=/run/user/1000"
            )
            worker_user = (
              "runuser -u tally-worker -- env HOME=/var/lib/tally-worker "
              "XDG_RUNTIME_DIR=/run/user/1001"
            )
            cli = (
              user
              + " ${tally}/bin/tally"
              + " --config /var/lib/tally-coordinator/.config/tally/config.json"
              + " --socket /run/user/1000/tally/tally.sock"
            )

            coordinator.succeed(
              "jq -e '.pools.flow.capacity == 8 and "
              ".producers[\"flow-multi-host\"].enqueue.executor == null and "
              ".producers[\"flow-multi-host\"].enqueue.noEnqueue == false' "
              "/var/lib/tally-coordinator/.config/tally/config.json"
            )
            coordinator.succeed(
              user + " systemctl --user start tally-producer-flow-multi-host.service"
            )
            coordinator.wait_until_succeeds(
              cli + " query jobs --source calendar | jq -e '.items | length == 1'"
            )
            coordinator.succeed(
              cli
              + " query jobs --source calendar | jq -r '.items[0].anchor' "
              + "> /tmp/tally-fs7-parent"
            )
            coordinator.wait_until_succeeds(
              cli
              + " query job \"$(cat /tmp/tally-fs7-parent)\" "
              + "| jq -e '.job.liveJobId != null'"
            )
            coordinator.succeed(
              cli
              + " query job \"$(cat /tmp/tally-fs7-parent)\" "
              + "| jq -r '.job.liveJobId' > /tmp/tally-fs7-parent-job"
            )

            worker.wait_until_succeeds("test -f /tmp/tally-fs7-worker-started")
            worker.succeed(
              "runuser -u tally-worker -- env HOME=/var/lib/tally-worker "
              "XDG_RUNTIME_DIR=/run/user/1001 systemctl --user list-units "
              "'tally-job-*.service' --state=running --no-legend | grep -F tally-job-"
            )
            coordinator.succeed(
              cli
              + " query jobs --flow-run \"$(cat /tmp/tally-fs7-parent)\" "
              + "| jq -e '.items | length == 1 and .[0].executor == \"worker\"'"
            )
            coordinator.succeed(
              cli
              + " query jobs --flow-run \"$(cat /tmp/tally-fs7-parent)\" "
              + "| jq -er '.items[0].anchor' > /tmp/tally-fs7-child"
            )
            coordinator.succeed(
              "epoch=/var/lib/tally-coordinator/.local/state/tally/lease_epoch; "
              "grep -E '^[1-9][0-9]*$' \"$epoch\"; "
              "cp \"$epoch\" /tmp/tally-fs7-old-epoch"
            )
            worker.succeed(
              "unit=$("
              + worker_user
              + " systemctl --user list-units 'tally-job-*.service' "
              + "--state=running --no-legend --plain "
              + "| ${pkgs.gawk}/bin/awk 'NR == 1 { print $1 }'); "
              + "test -n \"$unit\"; "
              + "printf '%s\\n' \"$unit\" > /tmp/tally-fs7-worker-unit; "
              + worker_user
              + " systemctl --user show \"$unit\" --property=InvocationID --value "
              + "> /tmp/tally-fs7-worker-invocation; "
              + worker_user
              + " systemctl --user show \"$unit\" --property=MainPID --value "
              + "> /tmp/tally-fs7-worker-pid; "
              + "grep -E '^[0-9a-f]{32}$' /tmp/tally-fs7-worker-invocation; "
              + "grep -E '^[1-9][0-9]*$' /tmp/tally-fs7-worker-pid"
            )

            coordinator.succeed(
              user
              + " systemctl --user show tally-daemon.service --property=MainPID --value "
              + "> /tmp/tally-fs7-old-daemon-pid"
            )
            coordinator.succeed("kill -KILL \"$(cat /tmp/tally-fs7-old-daemon-pid)\"")
            coordinator.wait_until_succeeds(
              user
              + " systemctl --user show tally-daemon.service --property=MainPID --value "
              + "| grep -E '^[1-9][0-9]*$'"
            )
            coordinator.wait_until_succeeds(
              "test \"$("
              + user
              + " systemctl --user show tally-daemon.service --property=MainPID --value"
              + ")\" != \"$(cat /tmp/tally-fs7-old-daemon-pid)\""
            )
            coordinator.wait_until_succeeds("test -S /run/user/1000/tally/tally.sock")
            coordinator.succeed(
              "old=$(cat /tmp/tally-fs7-old-epoch); "
              "current=$(cat /var/lib/tally-coordinator/.local/state/tally/lease_epoch); "
              "test \"$current\" -eq \"$((old + 1))\""
            )
            coordinator.wait_until_succeeds(
              cli
              + " query jobs --flow-run \"$(cat /tmp/tally-fs7-parent)\" "
              + "| jq -e --arg anchor \"$(cat /tmp/tally-fs7-child)\" "
              + "--argjson epoch \"$(cat /tmp/tally-fs7-old-epoch)\" "
              + "'.items | length == 1 and .[0].anchor == $anchor and "
              + ".[0].executor == \"worker\" and .[0].liveState == \"running\" and "
              + ".[0].currentAttempt == 1 and .[0].leaseEpoch == $epoch'"
            )
            coordinator.succeed(
              "job=$(cat /tmp/tally-fs7-child); "
              "epoch=$(cat /var/lib/tally-coordinator/.local/state/tally/lease_epoch); "
              "jq -e --arg job \"$job\" --argjson epoch \"$epoch\" "
              + "'select(.epoch == $epoch and .event.kind == \"granted\" and "
              + ".event.grant.epoch == $epoch and .event.grant.jobId == $job)' "
              + "/var/lib/tally-coordinator/.local/state/tally/lease-events.jsonl"
            )
            worker.succeed(
              "unit=$(cat /tmp/tally-fs7-worker-unit); "
              + worker_user
              + " systemctl --user is-active --quiet \"$unit\"; "
              + "test \"$("
              + worker_user
              + " systemctl --user show \"$unit\" --property=InvocationID --value"
              + ")\" = \"$(cat /tmp/tally-fs7-worker-invocation)\"; "
              + "test \"$("
              + worker_user
              + " systemctl --user show \"$unit\" --property=MainPID --value"
              + ")\" = \"$(cat /tmp/tally-fs7-worker-pid)\"; "
              + "test \"$("
              + worker_user
              + " systemctl --user list-units 'tally-job-*.service' "
              + "--state=running --no-legend --plain "
              + "| ${pkgs.gnugrep}/bin/grep -c '^tally-job-'"
              + ")\" -eq 1"
            )

            coordinator.succeed(
              user
              + " systemd-run --user --unit=tally-fs7-replay --collect "
              + "--property=Type=exec "
              + "--property=StandardOutput=append:/tmp/tally-fs7-replay.out "
              + "--property=StandardError=append:/tmp/tally-fs7-replay.err -- "
              + "${flowReplayProgram}/bin/tally-fs7-replay "
              + "\"$(cat /tmp/tally-fs7-parent)\" "
              + "\"$(cat /tmp/tally-fs7-parent-job)\""
            )
            coordinator.wait_until_succeeds(
              "grep -F '\"type\":\"flow-report\"' /tmp/tally-fs7-replay.out"
            )
            coordinator.succeed(
              "grep -F '\"disposition\":\"attached\"' /tmp/tally-fs7-replay.out"
            )
            coordinator.wait_until_succeeds(
              cli
              + " query job \"$(cat /tmp/tally-fs7-parent)\" "
              + "| jq -e '.job.terminalVerdict == \"pass\"'"
            )
            coordinator.succeed(
              cli
              + " query jobs --flow-run \"$(cat /tmp/tally-fs7-parent)\" "
              + "| jq -e --arg parent \"$(cat /tmp/tally-fs7-parent)\" "
              + "'.items | length == 2 and "
              + "all(.[]; .source == \"orchestrator\" and "
              + ".parentTaskUuid == $parent and .terminalVerdict == \"pass\") and "
              + "any(.[]; .executor == \"worker\") and "
              + "any(.[]; .executor == null)'"
            )

            coordinator.succeed(
              "grep -Fx artifact-created-on-worker /tmp/tally-fs7-coordinator-consumed"
            )
            coordinator.succeed(
              "${pkgs.git}/bin/git ls-remote git://worker/tally-fs7-handoff.git "
              "refs/heads/artifact | grep -F refs/heads/artifact"
            )
            coordinator.succeed(
              "grep -Fx artifact-created-on-worker-through-attic "
              "/tmp/tally-attic-coordinator-consumed"
            )
            coordinator.succeed(
              "store_path=$(head -n1 /tmp/tally-attic-coordinator-consumed); "
              "store_hash=$(basename \"$store_path\" | cut -d- -f1); "
              "${pkgs.curl}/bin/curl --fail --silent "
              "\"http://coordinator:8080/tally-handoff/$store_hash.narinfo\" "
              "| grep -F \"StorePath: $store_path\""
            )
            coordinator.succeed(
              "${tally}/bin/tally witness verify --ledger "
              "/var/lib/tally-coordinator/.local/share/tally/witness.jsonl"
            )

            worker.succeed(
              "runuser -u tally-worker -- ${pkgs.bash}/bin/bash -euc '"
              "ledger=/var/lib/tally-remote/exec-attestations.jsonl; "
              "task=$(${pkgs.jq}/bin/jq -er .payload.taskUuid \"$ledger\" | head -n1); "
              "attempt=$(${pkgs.jq}/bin/jq -er .payload.attempt \"$ledger\" | head -n1); "
              "lease=$(${pkgs.jq}/bin/jq -er .payload.leaseEpoch \"$ledger\" | head -n1); "
              "payload=$(${pkgs.jq}/bin/jq -er .payload.payloadHash \"$ledger\" | head -n1); "
              "${tally}/bin/tally attest exec "
              "--task-uuid \"$task\" --attempt \"$attempt\" --lease-epoch \"$lease\" "
              "--adapter shell --executor worker --payload-hash \"$payload\" "
              "--evidence exit:0 --ledger \"$ledger\" -- ${pkgs.coreutils}/bin/true'"
            )
            coordinator.succeed(
              worker_ssh
              + " cat /var/lib/tally-remote/exec-attestations.jsonl "
              + "> /tmp/tally-u6-exec.jsonl"
            )
            coordinator.succeed(
              "test \"$(wc -l < /tmp/tally-u6-exec.jsonl)\" -eq 2"
            )

            canon = "/var/lib/tally-coordinator/.local/share/tally/witness.jsonl"
            compare = (
              "${tally}/bin/tally witness compare --canon "
              + canon
              + " --attestations /tmp/tally-u6-exec.jsonl --format json"
            )
            coordinator.succeed(compare + " > /tmp/tally-u6-unanimous.json")
            coordinator.succeed(
              "jq -e '.summary.unanimous >= 1 and .summary.diverged == 0 and "
              "any(.executions[]; .canon.hostId == \"worker\" and "
              "(.attestations | length) >= 1 and "
              "all(.attestations[]; .hostId == \"worker\"))' "
              "/tmp/tally-u6-unanimous.json"
            )

            coordinator.succeed(
              "sed '1s/\"exitCode\":0/\"exitCode\":1/' "
              "/tmp/tally-u6-exec.jsonl > /tmp/tally-u6-flipped.jsonl"
            )
            status, output = coordinator.execute(
              compare.replace(
                "/tmp/tally-u6-exec.jsonl",
                "/tmp/tally-u6-flipped.jsonl",
              )
            )
            assert status == 2, output

            coordinator.succeed(
              "${execAttestationMutator}/bin/tally-exec-attestation-mutator "
              "stale-next /tmp/tally-u6-exec.jsonl /tmp/tally-u6-stale.jsonl"
            )
            status, output = coordinator.execute(
              compare.replace(
                "/tmp/tally-u6-exec.jsonl",
                "/tmp/tally-u6-stale.jsonl",
              )
            )
            assert status == 2, output

            coordinator.succeed(
              "${execAttestationMutator}/bin/tally-exec-attestation-mutator "
              "rewrite /tmp/tally-u6-exec.jsonl /tmp/tally-u6-diverged.jsonl"
            )
            status, output = coordinator.execute(
              compare.replace(
                "/tmp/tally-u6-exec.jsonl",
                "/tmp/tally-u6-diverged.jsonl",
              )
              + " > /tmp/tally-u6-diverged.json"
            )
            assert status == 1, output
            coordinator.succeed(
              "jq -e '.summary.diverged >= 1' /tmp/tally-u6-diverged.json"
            )

            # D12: this test-only protocol peer is copied into a fleet-owned
            # runtime directory. No product derivation or module provisions
            # Git AI; the worker helper merely inherits the fleet PATH.
            worker.succeed(
              "install -d -m 0700 -o tally-worker -g users "
              "/var/lib/tally-worker/fleet-bin "
              "/var/lib/tally-worker/git-ai-worktree"
            )
            worker.succeed(
              "install -m 0755 -o tally-worker -g users "
              "${./test/fixtures/git-ai/fleet-provider.py} "
              "/var/lib/tally-worker/fleet-bin/git-ai"
            )
            worker.succeed(
              "runuser -u tally-worker -- env HOME=/var/lib/tally-worker "
              "${pkgs.git}/bin/git -C /var/lib/tally-worker/git-ai-worktree init -q"
            )
            worker.succeed(
              "runuser -u tally-worker -- env HOME=/var/lib/tally-worker "
              "${pkgs.git}/bin/git -C /var/lib/tally-worker/git-ai-worktree "
              "config user.name 'Tally D12 worker'"
            )
            worker.succeed(
              "runuser -u tally-worker -- env HOME=/var/lib/tally-worker "
              "${pkgs.git}/bin/git -C /var/lib/tally-worker/git-ai-worktree "
              "config user.email tally-d12@example.invalid"
            )
            coordinator.succeed(
              "${tally}/bin/tally --mode check-config --config ${gitAiRemoteConfig}"
            )

            d12_socket = "/run/user/1000/tally/d12-git-ai.sock"
            d12_tally = (
              "${tally}/bin/tally"
              + " --config ${gitAiRemoteConfig}"
              + " --socket "
              + d12_socket
            )
            d12_cli = user + " " + d12_tally
            coordinator.succeed(
              user
              + " systemd-run --user --unit=tally-d12-git-ai-daemon --collect --quiet "
              + "--property=Type=exec -- "
              + "${tally}/bin/tally --config ${gitAiRemoteConfig} --socket "
              + d12_socket
              + " daemon run "
              + "--cpu-weight 100 "
              + "--memory-max-bytes 8589934592 "
              + "--state-dir /var/lib/tally-coordinator/d12-state "
              + "--data-dir /var/lib/tally-coordinator/d12-data"
            )
            coordinator.wait_until_succeeds("test -S " + d12_socket)
            coordinator.succeed(
              user
              + " systemd-run --user --unit=tally-d12-git-ai-enqueue --collect --quiet "
              + "--property=Type=exec "
              + "--property=StandardOutput=append:/tmp/tally-d12-git-ai.out "
              + "--property=StandardError=append:/tmp/tally-d12-git-ai.err -- "
              + d12_tally
              + " enqueue --pool worker-slot --executor worker "
              + "--workspace-repo mecattaf/tally.nix "
              + "--workspace-base-rev unborn "
              + "--workspace-branch d12-vm "
              + "--workspace-worktree /var/lib/tally-worker/git-ai-worktree "
              + "--gate-manifest /var/lib/tally-worker/git-ai-worktree/gates.json "
              + "--required-gate d12 "
              + "--acceptance-policy execution-and-gates "
              + "--evidence exit:0 --wait -- "
              + "${gitAiRemoteJob}/bin/tally-d12-remote-job"
            )
            worker.wait_until_succeeds(
              "test -f /var/lib/tally-worker/git-ai-worktree/remote-job-running"
            )
            worker.succeed(
              "runuser -u tally-worker -- env HOME=/var/lib/tally-worker "
              "XDG_RUNTIME_DIR=/run/user/1001 ${pkgs.bash}/bin/bash -euc '"
              "unit=$(systemctl --user list-units --type=service --state=running "
              "\"tally-git-ai-*.service\" --no-legend "
              "| ${pkgs.gawk}/bin/awk \"NR == 1 { print \\$1 }\"); "
              "test -n \"$unit\"; "
              "systemctl --user show \"$unit\" "
              "--property=ProtectSystem --property=ProtectHome "
              "--property=PrivateTmp --property=NoNewPrivileges "
              "--property=RestrictAddressFamilies --property=ReadWritePaths "
              "> /var/lib/tally-worker/d12-git-ai-hardening'"
            )
            worker.succeed(
              "grep -Fx ProtectSystem=strict "
              "/var/lib/tally-worker/d12-git-ai-hardening"
            )
            worker.succeed(
              "grep -Fx ProtectHome=read-only "
              "/var/lib/tally-worker/d12-git-ai-hardening"
            )
            worker.succeed(
              "grep -Fx PrivateTmp=yes "
              "/var/lib/tally-worker/d12-git-ai-hardening"
            )
            worker.succeed(
              "grep -Fx NoNewPrivileges=yes "
              "/var/lib/tally-worker/d12-git-ai-hardening"
            )
            worker.succeed(
              "grep -Fx RestrictAddressFamilies=AF_UNIX "
              "/var/lib/tally-worker/d12-git-ai-hardening"
            )
            worker.succeed(
              "${pkgs.bash}/bin/bash -euc '"
              "paths=$(sed -n \"s/^ReadWritePaths=//p\" "
              "/var/lib/tally-worker/d12-git-ai-hardening); "
              "test -n \"$paths\"; "
              "for path in $paths; do "
              "case \"$path\" in "
              "/var/lib/tally-remote/git-ai/*|"
              "/var/lib/tally-worker/git-ai-worktree*) ;; "
              "*) echo \"unexpected writable path: $path\" >&2; exit 1 ;; "
              "esac; "
              "done'"
            )
            coordinator.fail(
              user
              + " systemctl --user list-units --type=service --state=running "
              + "\"tally-git-ai-*.service\" --no-legend | grep -F tally-git-ai-"
            )

            coordinator.wait_until_succeeds(
              "grep -F '\"verdict\":\"pass\"' /tmp/tally-d12-git-ai.out"
            )
            coordinator.succeed(
              "jq -er '.task_uuid // .taskUuid' /tmp/tally-d12-git-ai.out "
              "> /tmp/tally-d12-git-ai-task"
            )
            coordinator.succeed(
              d12_cli
              + " query job \"$(cat /tmp/tally-d12-git-ai-task)\" "
              + "> /tmp/tally-d12-git-ai-job.json"
            )
            coordinator.succeed(
              "jq -e '.job.terminalVerdict == \"pass\" and "
              ".job.executor == \"worker\" and "
              ".job.authorship.status == \"bound\" and "
              ".job.authorship.provider == \"git-ai\" and "
              ".job.authorship.providerVersion == \"1.6.17\" and "
              ".job.authorship.identityMismatch == false and "
              ".job.authorship.workspace.value.worktreePath == "
              "\"/var/lib/tally-worker/git-ai-worktree\" and "
              ".job.authorship.gitAiSessions[0].value.tool == "
              "\"remote-fixture\"' /tmp/tally-d12-git-ai-job.json"
            )
            coordinator.succeed(
              d12_cli
              + " query proof --task \"$(cat /tmp/tally-d12-git-ai-task)\" "
              + "| jq -e '.status == \"verified\" and "
              + ".witnessRecord.hostId == \"worker\" and "
              + ".authorship.status == \"bound\"'"
            )
            worker.succeed(
              "observation=$(find /var/lib/tally-remote/git-ai "
              "-name fixture-observation.json -print -quit); "
              "test -n \"$observation\"; "
              "jq -e '.host == \"worker\" and "
              ".attributes.adapter == \"shell\" and "
              ".attributes.taskUuid != null' \"$observation\""
            )
            worker.succeed(
              "find /var/lib/tally-remote/git-ai -name trace-observed "
              "-print -quit | grep -F trace-observed"
            )
            worker.succeed(
              "runuser -u tally-worker -- ${pkgs.git}/bin/git "
              "-C /var/lib/tally-worker/git-ai-worktree "
              "notes --ref refs/notes/ai show HEAD | grep -F "
              "'\"schema_version\":\"authorship/3.0.0\"'"
            )

            worker_scp = (
              "runuser -u tally -- ${pkgs.openssh}/bin/scp -F /dev/null "
              "-o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes "
              "-o UserKnownHostsFile=/etc/tally-fs7/worker-known-hosts "
              "-i /etc/tally-fs7/id_ed25519"
            )
            coordinator.succeed(
              worker_scp
              + " /var/lib/tally-coordinator/d12-data/witness.jsonl "
              + "tally-worker@worker:/var/lib/tally-worker/d12-witness.jsonl"
            )
            coordinator.succeed(
              worker_scp
              + " /tmp/tally-d12-git-ai-task "
              + "tally-worker@worker:/var/lib/tally-worker/d12-task"
            )
            worker.succeed(
              "runuser -u tally-worker -- env HOME=/var/lib/tally-worker "
              "${tally}/bin/tally witness verify-authorship "
              "--ledger /var/lib/tally-worker/d12-witness.jsonl "
              "--repository /var/lib/tally-worker/git-ai-worktree "
              "--task \"$(cat /var/lib/tally-worker/d12-task)\" --format json "
              "| ${pkgs.jq}/bin/jq -e '.ok and .status == \"match\"'"
            )
            coordinator.succeed(
              user + " systemctl --user stop tally-d12-git-ai-daemon.service"
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
        systemServices = stockNixos.config.systemd.services;
        systemTimers = stockNixos.config.systemd.timers;
        systemServiceExec = name: systemServices.${name}.serviceConfig.ExecStart;
        systemDaemon = systemServices.tally-daemon;
        systemWitnessEmitter = systemServices."tally-witness-emit@";
        moduleContract =
          assert
            stockHome.config.services.tally.retention == {
              enable = true;
              horizon = "30d";
              onCalendar = "daily";
            };
          assert stockHome.config.services.tally.attestations.exec.enable;
          assert
            stockHome.config.services.tally.gitAi == {
              enable = false;
              mode = "advisory";
              awaitTimeoutSec = 60;
              globalAwaitOk = false;
            };
          assert homeServices ? tally-retention;
          assert homeTimers ? tally-retention;
          assert homeTimers.tally-retention.Timer.OnCalendar == "daily";
          assert pkgs.lib.hasInfix "gc --horizon 30d --collect" (homeServiceExec "tally-retention");
          assert systemServices ? tally-drain;
          assert systemTimers ? tally-drain;
          assert systemTimers.tally-drain.timerConfig.OnActiveSec == "1s";
          assert systemTimers.tally-drain.timerConfig.OnUnitActiveSec == "5s";
          assert pkgs.lib.hasInfix "--socket /run/tally/tally.sock daemon drain" (
            systemServiceExec "tally-drain"
          );
          assert systemServices.tally-drain.serviceConfig.User == "tally";
          assert systemServices ? tally-retention;
          assert systemTimers ? tally-retention;
          assert systemTimers.tally-retention.timerConfig.OnCalendar == "daily";
          assert pkgs.lib.hasInfix "gc --horizon 30d --collect --data-dir /var/lib/tally/data" (
            systemServiceExec "tally-retention"
          );
          assert systemServices.tally-retention.serviceConfig.User == "tally";
          assert builtins.elem
            "services.tally.producers must be empty in the NixOS module; configure producers with the Home Manager module (tally.homeManagerModules.tally)"
            unsupportedSystemMessages;
          assert builtins.elem
            "services.tally.flows must be empty in the NixOS module; configure flows with the Home Manager module (tally.homeManagerModules.tally)"
            unsupportedSystemMessages;
          assert builtins.elem
            "services.tally.pools.<name>.usageMeter must be null in the NixOS module; configure usage meters with the Home Manager module (tally.homeManagerModules.tally)"
            unsupportedSystemMessages;
          assert homeTimers ? tally-producer-daily;
          assert homeTimers.tally-producer-daily.Timer.OnCalendar == "daily";
          assert homeTimers ? tally-producer-flow-fixture;
          assert homeTimers.tally-producer-flow-fixture.Timer.OnCalendar == "daily";
          assert homeServices ? tally-producer-flow-fixture;
          assert homeTimers ? tally-producer-flow-monthly-dedup;
          assert homeTimers.tally-producer-flow-monthly-dedup.Timer.OnCalendar == "monthly";
          assert homeServices ? tally-producer-flow-monthly-dedup;
          assert !(homeTimers ? tally-producer-flow-manual);
          assert !(homeServices ? tally-producer-flow-manual);
          assert builtins.elem "tally flow bad-budget references unknown budgetPool missing-budget"
            invalidFlowMessages;
          assert builtins.elem "tally flow missing-mutex references unknown workloadMutex absent"
            invalidFlowMessages;
          assert builtins.elem "tally flow reserved-mutex workloadMutex must not be flow or build"
            invalidFlowMessages;
          assert builtins.elem "tally flow wrong-mutex workloadMutex must reference a resource = mutex pool"
            invalidFlowMessages;
          assert builtins.elem
            "tally flow windowed-mutex workloadMutex must not reference a windowed-consumption pool"
            invalidFlowMessages;
          assert builtins.elem "tally flow windowed-mutex workloadMutex must reference a co-residency pool"
            invalidFlowMessages;
          assert builtins.elem "tally flow wide-mutex workloadMutex must reference a capacity-1 pool"
            invalidFlowMessages;
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
          assert stockNixos.config.services.tally.user == "tally";
          assert stockNixos.config.services.tally.group == "tally";
          assert stockNixos.config.users.users.tally.isSystemUser;
          assert stockNixos.config.users.users.tally.linger;
          assert systemDaemon.serviceConfig.User == "tally";
          assert systemDaemon.serviceConfig.Group == "tally";
          assert
            systemDaemon.serviceConfig.ReadWritePaths == [
              "/var/lib/tally/data"
              "/var/lib/tally/state"
            ];
          assert systemWitnessEmitter.serviceConfig.User == "tally";
          assert systemWitnessEmitter.serviceConfig.Group == "tally";
          assert systemWitnessEmitter.serviceConfig.NoNewPrivileges;
          assert systemWitnessEmitter.serviceConfig.ProtectSystem == "strict";
          assert systemWitnessEmitter.serviceConfig.ReadWritePaths == [ "/var/lib/tally/data" ];
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
              .maxFrameBytes == 33554432 and
              .agingThresholdSec == 900 and
              .enqueue.depthCap == 3 and
              .enqueue.fanoutCap == 64 and
              .lease.yieldGraceSec == 20 and
              .retention == {"enable":true,"horizon":"30d","onCalendar":"daily"} and
              .attestations == {"exec":{"enable":true}} and
              .gitAi == {"enable":false,"mode":"advisory","awaitTimeoutSec":60,"globalAwaitOk":false} and
              .pools.build.resource == "build-slot" and
              .pools.build.capacity == 2 and
              .pools.build.enforce == "cooperative" and
              .pools.build.hardPreempt == false and
              .pools.flow.resource == "cpu-slot" and
              .pools.flow.capacity == 8 and
              .pools.flow.enforce == "cooperative" and
              .pools.flow.hardPreempt == false and
              .pools.stock.enforce == "cooperative" and
              .pools.programmatic.usageMeter.budgetClass == "programmatic" and
              .pools.programmatic.credentials.METER_TOKEN == "/run/credentials/tally-meter" and
              .pools["flow-run-mutex"].resource == "mutex" and
              .pools["flow-run-mutex"].capacity == 1 and
              .producers["flow-fixture"].kind == "calendar" and
              .producers["flow-fixture"].onCalendar == "daily" and
              .producers["flow-fixture"].enqueue.argv[0:3] == ["tally", "flow", "run"] and
              .producers["flow-fixture"].enqueue.argv[4] == "--args" and
              (.producers["flow-fixture"].enqueue.argv[5] | fromjson) == {"task":"ship"} and
              .producers["flow-fixture"].enqueue.argv[6:8] == ["--max-nodes", "1000"] and
              .producers["flow-fixture"].enqueue.argv[8] == "--catalog" and
              .producers["flow-fixture"].enqueue.adapter == "shell" and
              .producers["flow-fixture"].enqueue.pool == ["flow","flow-run-mutex"] and
              .producers["flow-fixture"].enqueue.priority == "low" and
              .producers["flow-fixture"].enqueue.dedupKey == "flow-fixture-%Y-%m-%d" and
              .producers["flow-fixture"].enqueue.runtimeMaxSec == 43200 and
              .producers["flow-fixture"].enqueue.evidence == ["exit:0"] and
              .producers["flow-fixture"].enqueue.noEnqueue == false and
              .producers["flow-fixture"].enqueue.adapterOptions.environment.FLOW_MODE == "fixture" and
              .producers["flow-fixture"].enqueue.credentials.FLOW_TOKEN == "/run/credentials/tally-flow" and
              .producers["flow-monthly-dedup"].enqueue.dedupKey == "monthly-local-ai-review-%Y-%m" and
              .producers["flow-monthly-dedup"].enqueue.evidence == ["exit:0","artifact:/tmp/monthly-review-receipt.json","hash:sha256"] and
              (.producers | has("flow-manual") | not) and
              .flows.fixture.workloadMutex == "flow-run-mutex" and
              (.flows.fixture.script | startswith("/nix/store/")) and
              .flows.manual.workloadMutex == null and
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
              .adapters["project-codex"].hardening == "strict" and
              .adapters["project-codex"].skillRevision == "project-codex-v3" and
              .adapters.shell.hardening == "workspace" and
              .adapters["explicit-none"].hardening == "none" and
              (.adapters.pi | has("hardening") | not) and
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
          doc = documentation;
          tally-witness-emit = tallyWitnessEmit;
          default = tally;
        };
        apps = {
          default = flake-utils.lib.mkApp { drv = tally; };
          publish-docs = {
            type = "app";
            program = "${documentationPublisher}/bin/tally-publish-docs";
          };
          dev = {
            type = "app";
            program = "${pkgs.writeShellScript "tally-dev" ''
              exec ${tally}/bin/tally daemon run --mock
            ''}";
          };
        };
        checks = {
          inherit tally;
          rustfmt = rustfmtCheck;
          clippy = clippyCheck;
          nixfmt-check = nixfmtCheck;
          doc = documentation;
          stock-home-activation = stockHome.activationPackage;
          module-layer = moduleContract;
          flow-dialect-accept =
            pkgs.runCommand "tally-flow-dialect-accept"
              {
                checkedConfig = flowValidCheckedConfig;
                nativeBuildInputs = [ pkgs.jq ];
              }
              ''
                ${tally}/bin/tally --mode check-config --config "$checkedConfig"
                meta="$(${tally}/bin/tally flow check ${./test/fixtures/flows/valid.js} \
                  --args '{"task":"ship"}' \
                  --catalog ${catalogFixture})"
                test "$(printf '%s' "$meta" | jq -r '.name')" = fixture-valid
                test "$(printf '%s' "$meta" | jq -c '.pools')" = '["worker-gpu"]'
                drv_meta="$(${tally}/bin/tally flow check ${./test/fixtures/flows/valid-drv.js})"
                test "$(printf '%s' "$drv_meta" | jq -r '.name')" = fixture-valid-drv
                test "$(printf '%s' "$drv_meta" | jq -c '.pools')" = '[]'
                for example in \
                  ${./examples/flows/academic-ocr.js} \
                  ${./examples/flows/agency-nightly.js} \
                  ${./examples/flows/fleet-deploy.js} \
                  ${./examples/flows/monthly-review.js} \
                  ${./examples/flows/pooled-review.js}; do
                  ${tally}/bin/tally flow check "$example" >/dev/null
                done
                touch "$out"
              '';
          flow-dialect-reject-nonliteral-meta = flowNonliteralFailure;
          flow-dialect-reject-banned-global = flowBannedGlobalFailure;
          flow-scheduled-producer-validation =
            pkgs.runCommand "tally-flow-scheduled-producer-validation"
              {
                dedupFailure = flowDedupTemplateFailure;
                evidenceFailure = flowArtifactEvidenceFailure;
              }
              ''
                test -e "$dedupFailure"
                test -e "$evidenceFailure"
                touch "$out"
              '';
          flow-dialect-reject-bad-args-schema =
            pkgs.runCommand "tally-flow-dialect-reject-bad-args-schema"
              {
                schemaFailure = flowBadArgsSchemaFailure;
                argsFailure = flowArgsMismatchFailure;
              }
              ''
                test -e "$schemaFailure"
                test -e "$argsFailure"
                touch "$out"
              '';
          flow-pool-closure =
            pkgs.runCommand "tally-flow-pool-closure"
              {
                lintFailure = flowUndeclaredPoolFailure;
                closureFailure = flowPoolClosureFailure;
                windowedFailure = flowWindowedConsumptionFailure;
                reservedFailure = flowReservedPoolFailure;
                reservedBuildFailure = flowReservedBuildPoolFailure;
                maxNodesFailure = flowMaxNodesFailure;
              }
              ''
                test -e "$lintFailure"
                test -e "$closureFailure"
                test -e "$windowedFailure"
                test -e "$reservedFailure"
                test -e "$reservedBuildFailure"
                test -e "$maxNodesFailure"
                touch "$out"
              '';
          flow-catalog-renderer =
            pkgs.runCommand "tally-flow-catalog-renderer"
              {
                nativeBuildInputs = [ pkgs.jq ];
              }
              ''
                jq -S . ${catalogFixture} > rendered.json
                jq -S . ${./test/fixtures/flows/catalog-resolution.json} > golden.json
                cmp rendered.json golden.json
                jq -e '
                  .version == 1 and
                  [.members[].id] == ["qwen-a", "qwen-b", "llama-a", "mistral-a", "llama-b"] and
                  all(.members[]; .classes | index("pooled-strongest") != null)
                ' ${catalogFixture} >/dev/null
                ${tally}/bin/tally flow check ${./examples/flows/pooled-review.js} \
                  --catalog ${catalogFixture} >/dev/null
                touch "$out"
              '';
          flow-catalog-reject-unknown-class = mkCatalogRejectionCheck {
            name = "tally-flow-catalog-reject-unknown-class";
            fixture = ./test/fixtures/catalog/unknown-class.nix;
            expectedMessage = "tally catalog member qwen-a references unknown class not-declared";
          };
          flow-catalog-reject-empty-class = mkCatalogRejectionCheck {
            name = "tally-flow-catalog-reject-empty-class";
            fixture = ./test/fixtures/catalog/empty-class.nix;
            expectedMessage = "tally catalog class dormant has no members after filtering";
          };
          flow-catalog-reject-unknown-pool = mkCatalogRejectionCheck {
            name = "tally-flow-catalog-reject-unknown-pool";
            fixture = ./test/fixtures/catalog/unknown-pool.nix;
            expectedMessage = "tally catalog member qwen-a references undeclared pool not-declared";
          };
          flow-catalog-reject-missing-diversity = mkCatalogRejectionCheck {
            name = "tally-flow-catalog-reject-missing-diversity";
            fixture = ./test/fixtures/catalog/missing-diversity.nix;
            expectedMessage = "tally catalog class review requires diversity key maker, but member local does not define it";
          };
          flow-catalog-schema =
            pkgs.runCommand "tally-flow-catalog-schema"
              {
                schemaFailure = flowCatalogFailure;
                requiredFailure = flowCatalogRequiredFailure;
                selectorFailure = flowCatalogSelectorFailure;
                nativeBuildInputs = [ pkgs.jq ];
              }
              ''
                test -e "$schemaFailure"
                test -e "$requiredFailure"
                test -e "$selectorFailure"
                jq -e '
                  ."$id" == "https://tally.nix/schemas/flow-catalog-v1.json" and
                  .properties.version.const == 1 and
                  (.properties.members.items."$ref" == "#/$defs/member")
                ' ${./crates/tally-flow/schema/catalog.schema.json} >/dev/null
                ${tally}/bin/tally flow check ${./test/fixtures/flows/valid.js} \
                  --args '{"task":"catalog-golden"}' \
                  --catalog ${catalogFixture} >/dev/null
                test "$(jq -r '.pool.capacity' ${./test/fixtures/flows/catalog-resolution.golden.json})" = 1
                test "$(jq -r '.cases[1].expectedIds | join(",")' \
                  ${./test/fixtures/flows/catalog-resolution.golden.json})" = \
                  'qwen-a,llama-a,mistral-a,qwen-b,llama-b'
                test "$(jq -r '.cases[2].expectedIds | join(",")' \
                  ${./test/fixtures/flows/catalog-resolution.golden.json})" = \
                  'qwen-a,qwen-b,llama-a,mistral-a,llama-b'
                touch "$out"
              '';
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
            ${tally}/bin/tally --mode check-config --config ${checkedHomeConfig}
            test "$(jq -r '.adapters["project-codex"].hardening' ${checkedHomeConfig})" = strict
            test "$(jq -r '.adapters["project-codex"].skillRevision' ${checkedHomeConfig})" = project-codex-v3
            test "$(jq -r '.adapters.shell.hardening' ${checkedHomeConfig})" = workspace
            test "$(jq -r '.adapters["explicit-none"].hardening' ${checkedHomeConfig})" = none
            test "$(jq -r '.adapters.pi.hardening // "absent"' ${checkedHomeConfig})" = absent
            strict_launch="$(${tally}/bin/tally --config ${checkedHomeConfig} __adapter-render project-codex -- payload)"
            test "$(printf '%s' "$strict_launch" | jq -r '.hardening')" = strict
            workspace_launch="$(${tally}/bin/tally --config ${checkedHomeConfig} __adapter-render shell -- /bin/true)"
            test "$(printf '%s' "$workspace_launch" | jq -r '.hardening')" = workspace
            none_launch="$(${tally}/bin/tally --config ${checkedHomeConfig} __adapter-render explicit-none -- payload)"
            test "$(printf '%s' "$none_launch" | jq -r '.hardening')" = none
            grep -F '"nix-custom"' ${adapterConfig} >/dev/null
            grep -F '"claude-code"' ${adapterConfig} >/dev/null
            grep -F '"codex"' ${adapterConfig} >/dev/null
            grep -F '"pi"' ${adapterConfig} >/dev/null
            grep -F '"shell"' ${adapterConfig} >/dev/null
            test "$(jq -c '.adapters.pi.argv' ${adapterConfig})" = '["pi","--mode","json","--"]'
            test "$(jq -c '.adapters.pi.resume' ${adapterConfig})" = '["pi","--mode","json","--session","%<sessionRef>%","--model","%<model>%","--"]'
            test "$(jq -c '.adapters["claude-code"].argv' ${adapterConfig})" = '["claude","--print","--verbose","--output-format","stream-json","--"]'
            test "$(jq -c '.adapters["claude-code"].resume' ${adapterConfig})" = '["claude","--resume","%<sessionRef>%","--model","%<model>%","--print","--verbose","--output-format","stream-json","--"]'
            test "$(jq -c '.adapters["claude-code"].trace' ${adapterConfig})" = '{"framing":"json-lines","stream":"stdout"}'
            test "$(jq -c '.adapters.codex.trace' ${adapterConfig})" = '{"framing":"json-lines","stream":"stdout"}'
            test "$(jq -c '.adapters.shell.trace' ${adapterConfig})" = 'null'
            test "$(jq -c '.adapters.pi.trace' ${adapterConfig})" = 'null'
            test "$(jq -c '.adapters.codex.argv' ${adapterConfig})" = '["codex","exec","--json","--"]'
            test "$(jq -c '.adapters.codex.resume' ${adapterConfig})" = '["codex","-C","%<cwd>%","exec","resume","--json","--model","%<model>%","%<sessionRef>%","--"]'
            test "$(jq -c '.adapters.codex.launch.cwdArgv' ${adapterConfig})" = '["-C","%<cwd>%"]'
            test "$(jq -c '.adapters.codex.launch.sandboxPolicies["dangerously-bypass"]' ${adapterConfig})" = '["--dangerously-bypass-approvals-and-sandbox"]'
            test "$(jq -c '.adapters.shell' ${adapterConfig})" = '{"argv":[],"env":{},"extraConfig":{},"launch":{},"resume":null,"scrape":{},"trace":null,"yieldHook":null}'
            for preset in pi claude-code codex; do
              test "$(jq -c --arg preset "$preset" '.adapters[$preset].yieldHook' ${adapterConfig})" = '["tally","lease","status"]'
              test "$(jq -r --arg preset "$preset" '.adapters[$preset].scrape.sessionRef.mode' ${adapterConfig})" = jsonPath
              test "$(jq -r --arg preset "$preset" '.adapters[$preset].scrape.model.mode' ${adapterConfig})" = jsonPath
              test "$(jq -r --arg preset "$preset" '.adapters[$preset].scrape.usage.mode' ${adapterConfig})" = jsonPath
              test "$(jq -r --arg preset "$preset" '.adapters[$preset].scrape.finalMessage.mode' ${adapterConfig})" = jsonPathLast
            done
            test "$(jq -r '.adapters.shell.scrape.finalMessage // "absent"' ${adapterConfig})" = absent
            test "$(jq -r '.adapters.pi.scrape.sessionRef.pattern' ${adapterConfig})" = '$.id'
            test "$(jq -r '.adapters["claude-code"].scrape.sessionRef.pattern' ${adapterConfig})" = '$..session_id'
            test "$(jq -r '.adapters.codex.scrape.sessionRef.pattern' ${adapterConfig})" = '$..thread_id'
            test "$(jq -r '.adapters.codex.extraConfig.modelFlag' ${adapterConfig})" = '--model'
            jq -e '.adapters["nix-custom"].skillBundle == "review protocol α\n"' ${adapterConfig} >/dev/null
            test "$(jq -r '.adapters["nix-custom"].env.CUSTOM_AGENT_MODE' ${adapterConfig})" = batch
            launch="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render nix-custom -- 'payload arg' "")"
            test "$(printf '%s' "$launch" | jq -c '.argv')" = '["custom-agent","--structured","payload arg",""]'
            test "$(printf '%s' "$launch" | jq -r '.env.CUSTOM_AGENT_MODE')" = batch
            resume="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render nix-custom --captures '{"sessionRef":"nix-session"}' -- '--option-looking')"
            test "$(printf '%s' "$resume" | jq -c '.argv')" = '["custom-agent","--resume","nix-session","--option-looking"]'
            : > empty.err
            printf '%s\n' \
              '{"type":"session","id":"pi-session","model":"Pi/Exact.Model"}' \
              '{"type":"message_end","message":{"role":"assistant","model":"Pi/Exact.Model","content":[{"type":"text","text":"pi first"}],"usage":{"input_tokens":5}}}' \
              '{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"ignore user"}]}}' \
              '{"type":"message_end","message":{"role":"assistant","model":"Pi/Exact.Model","content":[{"type":"text","text":"pi final"}],"usage":{"input_tokens":11}}}' > pi.jsonl
            pi_render="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render pi --scrape-stdout "$PWD/pi.jsonl" --scrape-stderr "$PWD/empty.err" -- work)"
            test "$(printf '%s' "$pi_render" | jq -c '.argv')" = '["pi","--mode","json","--session","pi-session","--model","Pi/Exact.Model","--","work"]'
            test "$(printf '%s' "$pi_render" | jq -c '.captures.usage')" = '{"input_tokens":11}'
            test "$(printf '%s' "$pi_render" | jq -r '.captures.finalMessage')" = 'pi final'
            test "$(printf '%s' "$pi_render" | jq -r '.defaultGateManifest')" = false
            printf '%s\n' \
              '{"type":"system","subtype":"init","session_id":"claude-session","model":"Claude/Exact.Model"}' \
              '{"type":"result","result":"claude first"}' \
              '{"type":"assistant","message":{"model":"Claude/Exact.Model","usage":{"input_tokens":12}}}' \
              '{"type":"result","result":"claude final"}' > claude.jsonl
            claude_render="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render claude-code --scrape-stdout "$PWD/claude.jsonl" --scrape-stderr "$PWD/empty.err" -- work)"
            test "$(printf '%s' "$claude_render" | jq -c '.argv')" = '["claude","--resume","claude-session","--model","Claude/Exact.Model","--print","--verbose","--output-format","stream-json","--","work"]'
            test "$(printf '%s' "$claude_render" | jq -c '.captures.usage')" = '{"input_tokens":12}'
            test "$(printf '%s' "$claude_render" | jq -r '.captures.finalMessage')" = 'claude final'
            test "$(printf '%s' "$claude_render" | jq -r '.defaultGateManifest')" = true
            printf '%s\n' \
              '{"type":"thread.started","thread_id":"codex-thread","model":"Codex/Exact.Model"}' \
              '{"type":"item.completed","item":{"type":"agent_message","text":"codex first"}}' \
              '{"type":"item.completed","item":{"type":"command_execution","text":"ignore command"}}' \
              '{"type":"turn.completed","model":"Codex/Exact.Model","usage":{"input_tokens":13}}' \
              '{"type":"item.completed","item":{"type":"agent_message","text":"codex final"}}' > codex.jsonl
            codex_render="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render codex --cwd "$PWD" --scrape-stdout "$PWD/codex.jsonl" --scrape-stderr "$PWD/empty.err" -- work)"
            expected_codex="$(jq -cn --arg cwd "$PWD" '["codex","-C",$cwd,"exec","resume","--json","--model","Codex/Exact.Model","codex-thread","--","work"]')"
            test "$(printf '%s' "$codex_render" | jq -c '.argv')" = "$expected_codex"
            test "$(printf '%s' "$codex_render" | jq -c '.captures.usage')" = '{"input_tokens":13}'
            test "$(printf '%s' "$codex_render" | jq -r '.captures.finalMessage')" = 'codex final'
            test "$(printf '%s' "$codex_render" | jq -r '.defaultGateManifest')" = true
            shell_render="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render shell -- /bin/true)"
            test "$(printf '%s' "$shell_render" | jq -r '.defaultGateManifest')" = false
            touch $out
          '';
          producer-registry =
            pkgs.runCommand "tally-producer-registry" { nativeBuildInputs = [ pkgs.jq ]; }
              ''
                ${tally}/bin/tally --mode check-config --config ${producerConfig}
                test "$(jq -r '.producers | keys | join(",")' ${producerConfig})" = 'daily,drop,effects,github,github-flow,health'
                test "$(jq -r '[.producers[] | select(has("pool") or has("priority") or has("adapter"))] | length' ${producerConfig})" = 0
                producer_state="$PWD/state"
                daily="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch daily --state-dir "$producer_state" --event '{"kind":"calendar"}')"
                test "$(printf '%s' "$daily" | jq -r 'keys[0]')" = emitted
                own_event='{"kind":"gh","source":"search","repo":"agency-agency/spec","number":21,"htmlUrl":"https://github.com/agency-agency/spec/issues/21","itemType":"issue","nodeId":"I-self","itemAuthor":"tally-bot","triggerActor":"tally-bot","selfActor":"tally-bot","triggerKind":"command-comment","eventId":"comment-42","commentId":"comment-42","triggerTimestamp":"2026-07-20T12:30:00Z","context":{"schemaVersion":2,"title":"Self-authored issue","body":"untrusted $(must-not-run)","state":"open","labels":["agency:codex-ready"],"assignees":["tally-bot"],"triggeringComment":{"id":"comment-42","author":"tally-bot","body":"/tally run"}}}'
                own="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch github --state-dir "$producer_state" --event "$own_event")"
                test "$(printf '%s' "$own" | jq -r 'keys[0]')" = emitted
                own_path="$(printf '%s' "$own" | jq -r '.emitted')"
                jq -e '.argv == ["gh-job"] and .noEnqueue == true' "$own_path" >/dev/null
                flow_event='{"kind":"gh","source":"search","repo":"agency-agency/spec","number":61,"htmlUrl":"https://github.com/agency-agency/spec/issues/61","itemType":"issue","nodeId":"I-flow-61","itemAuthor":"flow-author","triggerActor":"tally-bot","selfActor":"tally-bot","triggerKind":"command-comment","eventId":"notification-61","commentId":"comment-61","triggerTimestamp":"2026-07-26T12:30:00Z","context":{"schemaVersion":2,"title":"Pooled review","body":"untrusted $(must-not-run)","state":"open","labels":["agency:codex-ready"],"assignees":["tally-bot"],"triggeringComment":{"id":"comment-61","author":"tally-bot","body":"/pooled-review"}}}'
                flow="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch github-flow --state-dir "$producer_state" --event "$flow_event")"
                test "$(printf '%s' "$flow" | jq -r 'keys[0]')" = emitted
                flow_path="$(printf '%s' "$flow" | jq -r '.emitted')"
                jq -e \
                  --arg script ${./examples/flows/pooled-review.js} \
                  --arg catalog ${catalogFixture} '
                    .source == "gh" and
                    .noEnqueue == false and
                    .argv[0:3] == ["tally", "flow", "run"] and
                    .argv[3] == $script and
                    .argv[4] == "--args" and
                    (.argv[5] | fromjson) == {
                      "subject": "https://github.com/agency-agency/spec/issues/61",
                      "minimumValid": 2
                    } and
                    .argv[6:9] == ["--max-nodes", "1000", "--catalog"] and
                    .argv[9] == $catalog and
                    ([.argv[] | contains("must-not-run")] | any | not)
                  ' "$flow_path" >/dev/null
                flow_duplicate="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch github-flow --state-dir "$producer_state" --event "$flow_event")"
                test "$(printf '%s' "$flow_duplicate" | jq -r '.')" = duplicate
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
                  | xargs -0 jq -s 'map(select(.noEnqueue == true)) | length == 2' \
                  | grep -Fx true >/dev/null
                touch $out
              '';
        }
        // pkgs.lib.optionalAttrs isLinux {
          stock-nixos-activation = stockNixos.config.system.build.toplevel;
          stock-host-activation = stockHostTest;
          system-socket-execution = systemSocketExecutionTest;
          retention-liveness-floor = retentionTest;
          flow-multi-host = flowMultiHostTest;
        };
        devShells.default = pkgs.mkShell {
          TALLY_ADVISORY_DB_REPOSITORY = advisoryDbRepository;
          TALLY_ADVISORY_DB_REVISION = advisory-db.rev;
          packages = with pkgs; [
            cargo
            cargo-deny
            clippy
            git
            jq
            mdbook
            mdbook-linkcheck2
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
