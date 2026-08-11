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
      ghLoginLibrary = import ./nix/lib/gh-login.nix;
      # The corpus `crates/tally-core/src/producers/validate.rs` runs from the
      # same path. Evaluating it here is what keeps the module assertion and the
      # daemon's config-load check from drifting apart: a login one side accepts
      # and the other rejects is a green deploy with a dead daemon.
      ghLoginVectors = builtins.fromJSON (builtins.readFile ./test/fixtures/gh-login/vectors.json);
      ghLoginVectorFailures = builtins.filter (
        case: ghLoginLibrary.isValid case.login != case.valid
      ) ghLoginVectors.cases;
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
            # The clippy check runs from this source tree, so the disallowed
            # macro list has to travel with it or the lint silently weakens.
            ./clippy.toml
            ./crates
            # Pages a test `include_str!`s to hold the prose to the constant it
            # describes; a doc page a Rust test reads is a fixture, and the
            # packaged build cannot see one that is not named here.
            ./doc/src/flows/submission-and-replay.md
            ./doc/src/reference/errors.md
            ./doc/src/reference/rpc-protocol.md
            ./examples/flows/academic-ocr.js
            ./examples/flows/agency-nightly.js
            ./examples/flows/monthly-review.js
            ./drivers/campaign_worktrees.py
            ./examples/flows/spec-build.js
            ./drivers/spec_build_driver.py
            ./test/fixtures/flows
            ./test/fixtures/gh-login
            ./test/fixtures/git-ai
            ./test/fixtures/ledger
            ./test/fixtures/pools
            ./test/fixtures/redaction
            ./test/fixtures/shell-command-provider
            ./test/fixtures/spec-build
            ./test/fixtures/traces
            ./test/fixtures/usage
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
                hardening = "production";
                extraWritablePaths = [ "/var/lib/custom-agent" ];
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
                    "--args-from-brief"
                    "--max-nodes"
                    "1000"
                    "--catalog"
                    "${catalogFixture}"
                  ];
                  brief = {
                    subject = "\${gh.url}";
                    minimumValid = 2;
                  };
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
          cargoLock = {
            lockFile = ./Cargo.lock;
            # All Boa workspace crates share this one fixed-output git source.
            outputHashes."boa_ast-1.0.0-dev" = "sha256-xdB+SCFjaV+/hJu9n+3Il3vN0TZQXq0V95XmsJ/ihwo=";
          };
          doCheck = true;
          preCheck = ''
            export TALLY_NIX_CATALOG_FIXTURE=${catalogFixtureUnchecked}
          '';
          nativeCheckInputs = [
            pkgs.git
            pkgs.python3
          ];
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postInstall = ''
            mkdir -p "$out/share/tally/flows" "$out/libexec/tally"
            cp ${./examples/flows/spec-build.js} "$out/share/tally/flows/spec-build.js"
            ln -s ${specBuildDriver}/bin/spec-build-driver "$out/libexec/tally/spec-build-driver"
            wrapProgram "$out/bin/tally" \
              --prefix PATH : ${
                pkgs.lib.makeBinPath [
                  pkgs.gh
                  pkgs.git
                ]
              }
            ln -s tally $out/bin/tallyd
          '';
          meta.mainProgram = "tally";
        };
        finalConformanceBar = pkgs.stdenvNoCC.mkDerivation {
          pname = "tally-final-conformance-bar";
          version = "1";
          dontUnpack = true;
          nativeBuildInputs = [ pkgs.makeWrapper ];
          installPhase = ''
            runHook preInstall
            mkdir -p "$out/share/tally-final-conformance-bar" "$out/bin"
            cp -R ${./test/final-bar}/. "$out/share/tally-final-conformance-bar/"
            makeWrapper ${pkgs.python3}/bin/python3 "$out/bin/tally-final-conformance-bar" \
              --add-flags "$out/share/tally-final-conformance-bar/run.py" \
              --prefix PATH : ${
                pkgs.lib.makeBinPath [
                  pkgs.bash
                  pkgs.coreutils
                  pkgs.git
                  pkgs.nix
                  pkgs.python3
                  pkgs.systemd
                ]
              }
            runHook postInstall
          '';
          meta.mainProgram = "tally-final-conformance-bar";
        };
        hardeningProbe = pkgs.writeShellApplication {
          name = "tally-hardening-probe";
          runtimeInputs = [ pkgs.coreutils ];
          text = ''
            set -euo pipefail

            printf 'allowed\n' > /srv/tally/production-agent/allowed
            if printf 'overwritten\n' > /srv/tally/state/forbidden; then
              echo "production job wrote an undeclared state-root file" >&2
              exit 21
            fi
            if printf 'overwritten\n' > /srv/tally/state/capture/foreign.out; then
              echo "production job wrote another execution's capture" >&2
              exit 22
            fi

            socket="$TALLY_SOCKET"
            test -S "$socket"
            ${tally}/bin/tally --socket "$socket" query pools >/dev/null
            printf 'reachable\n' > /srv/tally/production-agent/socket
            printf 'ready\n' > /srv/tally/production-agent/ready
            for ((attempt = 0; attempt < 240; attempt++)); do
              if test -e /srv/tally/production-agent/release; then
                exit 0
              fi
              sleep 0.25
            done
            echo "timed out waiting for the VM property inspection" >&2
            exit 23
          '';
        };
        campaignHostTerminalCloseoutSource = pkgs.writeText "tally-campaign-host-closeout.py" ''
          import importlib.util
          from pathlib import Path
          import subprocess

          source = Path("${campaignDrivers}/spec_build_driver.py")
          spec = importlib.util.spec_from_file_location("spec_build_driver", source)
          if spec is None or spec.loader is None:
              raise RuntimeError("cannot load the packaged spec-build driver")
          driver = importlib.util.module_from_spec(spec)
          spec.loader.exec_module(driver)

          root = Path("/srv/tally/campaign-host")
          repository = "acme/sentinel"
          campaign = "sentinel-config"
          issue_number = "1"
          task_id = "sentinel-checkpoint"
          task = {
              "id": task_id,
              "title": "Sentinel checkpoint",
              "brief": {
                  "issue": {
                      "number": "2",
                      "url": "https://github.com/acme/sentinel/issues/2",
                  }
              },
          }
          body = (root / "master-body").read_text(encoding="utf-8")
          driver.sync_issue_checkboxes(
              repository, issue_number, body, [task], {task_id}
          )

          base_revision = subprocess.run(
              ["git", "-C", str(root / "checkout"), "rev-parse", "origin/main"],
              check=True,
              text=True,
              stdout=subprocess.PIPE,
          ).stdout.strip()
          reconciliation = {
              "campaign": campaign,
              "repository": repository,
              "source": {
                  "sha256": "sha256:" + "a" * 64,
                  "revision": base_revision,
              },
              "baseRevision": base_revision,
              "tasks": [{"id": task_id, "title": task["title"]}],
              "merged": [],
              "checkpoints": [{"taskId": task_id, "revision": base_revision}],
              "remaining": [],
              "diagnoses": [],
              "retries": [],
              "deferrals": [],
              "blocked": [],
              "anomalies": [],
              "warnings": [],
          }
          driver.publish_closing_summary(
              repository,
              # Match --allow-test-local-forge: code settles through local
              # Git state, while the forge-native campaign thread is GitHub.
              {"forge": "local"},
              campaign,
              issue_number,
              driver.campaign_digest(reconciliation, "complete"),
              issue_forge="github",
          )
          driver.close_completed_issue_campaign(
              repository, issue_number, [task]
          )
        '';
        campaignHostTerminalCloseout = pkgs.writeShellApplication {
          name = "tally-campaign-host-closeout";
          runtimeInputs = [
            pkgs.git
            pkgs.python3
          ];
          text = ''
            exec python3 ${campaignHostTerminalCloseoutSource}
          '';
        };
        # Producer-through-consumer fixture for #442. The flow is deliberately
        # tiny, but it is executed by the packaged tally binary as a real
        # daemon-launched transient. Its first node puts a brief just below the
        # 16 MiB brief ceiling inside queue.enqueue; the surrounding RPC frame
        # is therefore over the default frame limit and succeeds only when the
        # child read the system host's non-default maxFrameBytes.
        campaignHostProbe = pkgs.writeShellApplication {
          name = "tally-campaign-host-probe";
          runtimeInputs = [
            pkgs.coreutils
            pkgs.git
          ];
          text = ''
            set -euo pipefail
            printf '%s\n' "$TALLY_TASK_UUID" >> /srv/tally/campaign-host/frame-probes
            passes="$(${pkgs.coreutils}/bin/wc -l < /srv/tally/campaign-host/frame-probes)"
            if test "$passes" -eq 1; then
              # Model the first real driver's local merge without touching the
              # fake forge: only the remote base moves between public polls.
              printf 'merged\n' >> /srv/tally/campaign-host/checkout/README.md
              git -C /srv/tally/campaign-host/checkout add README.md
              git -C /srv/tally/campaign-host/checkout commit --quiet -m 'campaign local merge'
              git -C /srv/tally/campaign-host/checkout push --quiet origin main
            elif test "$passes" -eq 2 && test ! -e /srv/tally/campaign-host/master-closed; then
              # Exercise the packaged driver's terminal mutations in their
              # production order: checkbox edit, final digest, task close,
              # master close. The fake forge below accepts only real paths for
              # --body-file, matching the recorded public grammar.
              ${campaignHostTerminalCloseout}/bin/tally-campaign-host-closeout
            fi
          '';
        };
        campaignHostContinuationEvent = pkgs.writeShellApplication {
          name = "tally-campaign-host-continuation-event";
          runtimeInputs = [
            pkgs.coreutils
            pkgs.jq
          ];
          text = ''
            set -euo pipefail
            # A real terminal reconcile emits no successor. Keep this tiny
            # mechanism fixture faithful: the first pass advances local state
            # and requests one continuation; the second pass is terminal.
            test "$(${pkgs.coreutils}/bin/wc -l < /srv/tally/campaign-host/frame-probes)" -eq 1 \
              || exit 0
            events_dir="$(${pkgs.jq}/bin/jq -er '.continuation.eventsDir' "$TALLY_BRIEF")"
            name="campaign-host-continuation-$TALLY_TASK_UUID.json"
            temporary="$(${pkgs.coreutils}/bin/mktemp "$events_dir/.campaign-host.XXXXXX")"
            ${pkgs.jq}/bin/jq -c \
              --arg dedup "campaign-host-continuation:$TALLY_TASK_UUID" \
              '{
                argv: .continuation.argv,
                adapter: "shell",
                pool: .continuation.pool,
                priority: .continuation.priority,
                source: "events-dir",
                dedupKey: $dedup,
                submission: {mode: "full"},
                evidence: ["exit:0"],
                noEnqueue: false,
                runtimeMaxSec: .continuation.runtimeMaxSec
              }' "$TALLY_BRIEF" > "$temporary"
            ${pkgs.coreutils}/bin/mv "$temporary" "$events_dir/$name"
          '';
        };
        campaignHostProbeFlow = pkgs.writeText "tally-campaign-host-probe.js" ''
          export const meta = {
            name: "campaign-host-probe",
            description: "exercise the inherited campaign config and continuation argv",
            pools: ["campaign-control"],
            argsSchema: {
              type: "object",
              required: ["continuation"],
              properties: {
                continuation: {
                  type: "object",
                  required: ["argv", "pool", "priority"],
                  properties: {
                    argv: { type: "array", minItems: 1, items: { type: "string", minLength: 1 } },
                    pool: { type: "array", minItems: 1, items: { type: "string", minLength: 1 } },
                    priority: { enum: ["interrupt", "high", "medium", "low"] },
                    runtimeMaxSec: { type: ["integer", "null"], minimum: 1 }
                  },
                  additionalProperties: true
                }
              },
              additionalProperties: true
            },
            maxNodes: 2,
            selectors: []
          };

          (async () => {
            // Build the near-limit value with a fixed number of deterministic
            // concatenations. A single 16 MiB String.repeat would spend the
            // engine's loop budget before the queue RPC can exercise its
            // configured frame bound.
            let padding = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
            padding += padding;
            padding += padding;
            padding += padding;
            padding += padding;
            padding += padding;
            padding += padding;
            padding += padding;
            padding += padding;
            padding += padding;
            padding += padding;
            padding += padding;
            padding += padding;
            padding += padding;
            padding += padding;
            padding += padding;
            padding += padding;
            padding += padding;
            padding += padding;
            padding = padding.slice(0, 16776700);
            // {"padding":"..."} is 16,776,714 canonical bytes: admitted by
            // the 16 MiB brief ceiling, while its queue.enqueue RPC envelope
            // is necessarily larger than the 16,777,216-byte default frame.
            await job({
              argv: ["${campaignHostProbe}/bin/tally-campaign-host-probe"],
              adapter: "shell",
              pools: ["campaign-control"],
              brief: { padding },
              priority: "low",
              runtimeMaxSec: 60,
              evidence: ["exit:0"],
              key: "configured-frame",
              label: "configured-frame"
            });
            return job({
              argv: ["${campaignHostContinuationEvent}/bin/tally-campaign-host-continuation-event"],
              adapter: "shell",
              pools: ["campaign-control"],
              brief: { continuation: args.continuation },
              priority: "low",
              runtimeMaxSec: 60,
              evidence: ["exit:0"],
              key: "continuation-event",
              label: "continuation-event"
            });
          })();
        '';
        campaignHostManifest = {
          schemaVersion = 1;
          name = "sentinel-config";
          repository = {
            checkout = "/srv/tally/campaign-host/checkout";
            forge = "local";
          };
          maxTasks = 1;
          maxParallel = 1;
          pool = "sentinel-campaign";
          agent = {
            adapter = "shell";
            argv = [ "${pkgs.coreutils}/bin/true" ];
            approvalPolicy = null;
            sandboxPolicy = null;
            diagnosisSandboxPolicy = null;
          };
          gates = [
            {
              kind = "forbidPaths";
              id = "sentinel-gate";
              forbidPaths = [ "*.sentinel-forbidden" ];
              runtimeMaxSec = 30;
            }
          ];
          tasks = [
            {
              id = "sentinel-checkpoint";
              kind = "checkpoint";
              issue = 2;
              dependencies = [ ];
              argv = [ "${pkgs.coreutils}/bin/true" ];
              runtimeMaxSec = 30;
            }
          ];
        };
        campaignHostBody = ''
          <!-- tally:campaign:v1 -->
          ```json
          ${builtins.toJSON campaignHostManifest}
          ```
          <!-- tally:campaign:v1:end -->
          <!-- tally:campaign-worklist:v1 -->
          - [ ] <!-- tally:campaign-task:v1 id=sentinel-checkpoint --> #2 — Sentinel checkpoint
          <!-- tally:campaign-worklist:v1:end -->
        '';
        campaignHostMaster = pkgs.writeText "tally-campaign-host-master.json" (
          builtins.toJSON {
            number = 1;
            title = "Sentinel config campaign";
            body = campaignHostBody;
            state = "open";
            html_url = "https://github.com/acme/sentinel/issues/1";
            updated_at = "2026-08-08T08:00:00Z";
            user.login = "operator";
          }
        );
        campaignHostSubIssues = pkgs.writeText "tally-campaign-host-sub-issues.json" (
          builtins.toJSON [
            {
              number = 2;
              title = "Sentinel checkpoint";
              body = "Run the sentinel checkpoint.";
              state = "open";
              html_url = "https://github.com/acme/sentinel/issues/2";
              updated_at = "2026-08-08T08:00:00Z";
              user.login = "operator";
            }
          ]
        );
        campaignHostFakeGh = pkgs.writeTextFile {
          name = "tally-campaign-host-fake-gh";
          destination = "/bin/gh";
          executable = true;
          text = ''
            #!${pkgs.python3}/bin/python3
            import json
            import os
            from pathlib import Path
            import sys

            root = Path("/srv/tally/campaign-host")
            arguments = sys.argv[1:]
            caller = Path(f"/proc/{os.getppid()}/cmdline").read_bytes().replace(
                b"\0", b" "
            ).decode("utf-8", errors="replace")
            with (root / "gh-callers").open("a", encoding="utf-8") as handle:
                handle.write(caller + "\t" + json.dumps(arguments) + "\n")

            def emit(value):
                print(json.dumps(value, separators=(",", ":")))

            def comments():
                path = root / "final-digests"
                if not path.exists():
                    return []
                return [
                    json.loads(line)
                    for line in path.read_text(encoding="utf-8").splitlines()
                    if line
                ]

            def issue(number):
                if number == 1:
                    value = json.loads(Path("${campaignHostMaster}").read_text(encoding="utf-8"))
                    value["body"] = (root / "master-body").read_text(encoding="utf-8")
                    if (root / "master-closed").exists():
                        value["state"] = "closed"
                    return value
                if number == 2:
                    value = json.loads(
                        Path("${campaignHostSubIssues}").read_text(encoding="utf-8")
                    )[0]
                    if (root / "subissue-closed").exists():
                        value["state"] = "closed"
                    return value
                raise SystemExit(f"unexpected fake gh issue: {number}")

            def body_file():
                if "--body-file" not in arguments:
                    raise SystemExit("fake gh requires --body-file")
                argument = arguments[arguments.index("--body-file") + 1]
                path = Path(argument)
                if argument == "-" or not path.is_file():
                    raise SystemExit(
                        f"fake gh requires a real --body-file path, got {argument!r}"
                    )
                body = path.read_text(encoding="utf-8")
                with (root / "body-file-records").open("a", encoding="utf-8") as handle:
                    handle.write(json.dumps({"argument": argument, "body": body}) + "\n")
                return body

            if arguments[:2] == ["api", "user"]:
                if arguments not in (
                    ["api", "user"],
                    ["api", "user", "--jq", ".login"],
                ):
                    raise SystemExit(f"unexpected fake gh actor lookup: {arguments!r}")
                if "--jq" in arguments:
                    print("operator")
                else:
                    emit({"login": "operator"})
            elif arguments[:2] == ["api", "graphql"]:
                emit(
                    {
                        "errors": [
                            {
                                "type": "UNDEFINED_FIELD",
                                "message": "fixture has no subIssues field",
                            }
                        ]
                    }
                )
                raise SystemExit(1)
            elif arguments and arguments[0] == "api":
                endpoint = next(
                    (item for item in arguments[1:] if item.startswith("repos/")), ""
                )
                if endpoint == "repos/acme/sentinel/issues/1":
                    emit(issue(1))
                elif endpoint == "repos/acme/sentinel/issues/2":
                    emit(issue(2))
                elif endpoint == "repos/acme/sentinel/issues/1/sub_issues?per_page=100":
                    emit([issue(2)])
                elif endpoint == "repos/acme/sentinel/issues/1/comments?per_page=100":
                    expected = ["api", "--paginate", "--slurp", endpoint]
                    if arguments != expected:
                        raise SystemExit(
                            "fake gh requires recorded comment-list grammar, "
                            f"got {arguments!r}"
                        )
                    emit([comments()])
                else:
                    raise SystemExit(f"unexpected fake gh api invocation: {arguments!r}")
            elif arguments[:2] == ["issue", "edit"]:
                if arguments[:5] != [
                    "issue", "edit", "1", "--repo", "acme/sentinel"
                ] or len(arguments) != 7:
                    raise SystemExit(f"unexpected fake gh edit: {arguments!r}")
                (root / "master-body").write_text(body_file(), encoding="utf-8")
                print("https://github.com/acme/sentinel/issues/1")
            elif arguments[:2] == ["issue", "comment"]:
                if arguments[:6] != [
                    "issue", "comment", "1", "--repo", "acme/sentinel", "--body"
                ] or len(arguments) != 7:
                    raise SystemExit(f"unexpected fake gh comment: {arguments!r}")
                body = arguments[6]
                recorded = comments()
                comment_id = len(recorded) + 1
                comment = {
                    "id": comment_id,
                    "body": body,
                    "html_url": (
                        "https://github.com/acme/sentinel/issues/1"
                        f"#issuecomment-{comment_id}"
                    ),
                    "created_at": "2026-08-08T08:00:00Z",
                    "updated_at": "2026-08-08T08:00:00Z",
                    "user": {"login": "operator"},
                }
                with (root / "final-digests").open("a", encoding="utf-8") as handle:
                    handle.write(json.dumps(comment, separators=(",", ":")) + "\n")
                print(comment["html_url"])
            elif arguments[:2] == ["issue", "close"]:
                if len(arguments) != 5 or arguments[3:] != [
                    "--repo", "acme/sentinel"
                ]:
                    raise SystemExit(f"unexpected fake gh close: {arguments!r}")
                if arguments[2] == "1":
                    (root / "master-closed").touch()
                elif arguments[2] == "2":
                    (root / "subissue-closed").touch()
                else:
                    raise SystemExit(f"unexpected fake gh close: {arguments!r}")
                print(f"https://github.com/acme/sentinel/issues/{arguments[2]}")
            else:
                raise SystemExit(f"unexpected fake gh invocation: {arguments!r}")
          '';
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

            # An option example is the worked example a consumer copies. One
            # that renders argv the target binary rejects reproduces #244 in
            # the consumer's tree and the module accepts it, so the published
            # reference is guarded rather than fixed one declaration at a time.
            for page in "''${generated_pages[@]}"; do
              if grep -Fq -- '--ask-for-approval' "$page"; then
                echo "generated option reference publishes --ask-for-approval, which codex exec rejects: $page" >&2
                exit 1
              fi
            done

            core_json=${optionsJson coreOptionsDoc}
            home_json=${optionsJson homeManagerOptionsDoc}
            nixos_json=${optionsJson nixosOptionsDoc}
            jq -S 'keys' "$core_json" > core-option-keys.json
            jq -S 'keys' "$home_json" > home-option-keys.json
            jq -S 'keys' "$nixos_json" > nixos-option-keys.json
            cmp core-option-keys.json home-option-keys.json
            jq -S '
              keys - [
                "services.tally.campaignForge",
                "services.tally.campaignForge.enable",
                "services.tally.campaignForge.gitUserEmail",
                "services.tally.campaignForge.gitUserName",
                "services.tally.campaignForge.homeDir",
                "services.tally.campaignForge.login",
                "services.tally.campaignForge.tokenFile",
                "services.tally.group",
                "services.tally.user"
              ]
            ' "$nixos_json" > nixos-common-option-keys.json
            cmp core-option-keys.json nixos-common-option-keys.json
            # The three options the NixOS module owns alone: the account the
            # system service runs as, and the forge identity it acts as when it
            # executes campaigns. Home Manager needs neither -- it runs as the
            # operator and inherits the operator's own gh and git identity.
            jq -e '
              has("services.tally.group")
              and has("services.tally.user")
              and has("services.tally.campaignForge.enable")
              and has("services.tally.campaignForge.login")
              and has("services.tally.campaignForge.tokenFile")
              and has("services.tally.campaignForge.homeDir")
            ' "$nixos_json" >/dev/null
            jq -e '
              all(to_entries[];
                (.value.type | type == "string" and length > 0)
                and (.value.description | type == "string" and length > 0)
              )
              and has("services.tally.producers.<name>.enqueue.gateManifest.requiredGateIds")
              and has("services.tally.adapters.<name>.hardening")
              and has("services.tally.adapters.<name>.extraWritablePaths")
              and has("services.tally.transport.maxFrameBytes")
              and has("services.tally.scheduling.agingThresholdSec")
              and (
                [
                  keys[]
                  | select(startswith("services.tally.flows.<name>."))
                ]
                | length == 12
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
        hardeningDocDrift =
          pkgs.runCommand "tally-hardening-doc-drift" { nativeBuildInputs = [ pkgs.ripgrep ]; }
            ''
              properties="$PWD/hardening-properties"
              ${pkgs.ripgrep}/bin/rg --only-matching '`[^`]+=[^`]*`' \
                ${./doc/src/configuration/hardening-properties.md.inc} \
                | tr -d '`' \
                | sort -u > "$properties"
              test -s "$properties"
              while IFS= read -r property; do
                if ! ${pkgs.ripgrep}/bin/rg --fixed-strings --quiet -- "$property" \
                  ${./crates/tally-core/src/executor}; then
                  echo "documented hardening property is absent from executor source: $property" >&2
                  exit 1
                fi
              done < "$properties"
              touch "$out"
            '';
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
        campaignDrivers = import ./nix/lib/campaign-drivers.nix { inherit pkgs; };
        agencyNightlyDriver = pkgs.writeShellApplication {
          name = "agency-nightly-driver";
          runtimeInputs = [
            pkgs.gh
            pkgs.git
            pkgs.python3
          ];
          text = ''
            exec ${pkgs.python3}/bin/python3 ${campaignDrivers}/agency_nightly_driver.py "$@"
          '';
        };
        specBuildDriver = import ./nix/lib/spec-build-driver.nix { inherit pkgs; };
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
        # #382 HIGH-1's pinned cross-language vector: a pool that declares no
        # `resource` and one that declares `resource = "vram"` explicitly,
        # rendered through the exact `mkRuntimeConfig`/`renderPool` path the
        # daemon's config comes from — not the raw NixOS option, so a
        # regression in `renderPool`'s `optionalAttrs` guard is caught here,
        # not just in the option type. Compared against
        # `test/fixtures/pools/resource-declaration.golden.json` by the
        # `pool-resource-declaration` check below; `PoolConfig`'s Rust
        # `Deserialize` is pinned against the same checked-in file by
        # `config::tests::pool_config_reads_the_nix_rendered_declared_vs_undeclared_fixture_correctly`.
        # Nix's rendering and Rust's reading of it cannot drift apart
        # silently without both pins failing.
        poolResourceDeclarationFixture =
          (moduleCommon.mkRuntimeConfig
            (pkgs.lib.evalModules {
              modules = [
                {
                  options.services.tally = moduleCommon.mkOptions {
                    defaultPackage = null;
                    defaultDataDir = "/var/lib/tally/data";
                    defaultStateDir = "/var/lib/tally/state";
                  };
                }
                {
                  services.tally.pools.undeclared = {
                    capacity = 2;
                  };
                  services.tally.pools.declared = {
                    resource = "vram";
                    capacity = 2;
                  };
                }
              ];
            }).config.services.tally
          ).pools;
        # Representative arguments for the checked-in examples, so the flake
        # exercises each argsSchema instead of only its meta block. The agency
        # wave is the exception: it ships its own documented argument file,
        # because for that flow the arguments are the worklist.
        exampleArgs =
          builtins.mapAttrs
            (name: value: pkgs.writeText "tally-example-args-${name}.json" (builtins.toJSON value))
            {
              academic-ocr = {
                pages = [
                  {
                    paperId = "paper-1";
                    pageNumber = 1;
                    sourcePath = "/var/lib/ocr/paper-1/page-1.tif";
                  }
                ];
                protocols = [
                  {
                    id = "cheap-pass";
                    tier = "cheap";
                  }
                  {
                    id = "specialist-pass";
                    tier = "specialist";
                  }
                ];
                driver = {
                  adapter = "shell";
                  program = "/run/current-system/sw/bin/ocr-driver";
                  runtimeMaxSec = 900;
                };
                outputDir = "/var/lib/ocr/out";
                rasterDpi = 600;
                maxMutationIterations = 2;
                maxDisagreementPermille = 20;
              };
              domain-failure = {
                invoice = "INV-2026-0042";
              };
              fleet-deploy = {
                remote = "origin";
                revision = "main";
                coordinatorCheckout = "/var/lib/fleet/coordinator";
                workerCheckout = "/var/lib/fleet/worker";
              };
              monthly-review = {
                minimumValid = 2;
                publish = false;
                dotfilesUrl = "https://github.com/mecattaf/dotfiles";
                baseBranch = "main";
                period = "2026-07";
                driver = {
                  adapter = "shell";
                  program = "/run/current-system/sw/bin/review-driver";
                  stateDir = "/var/lib/review/state";
                  receiptPath = "/var/lib/review/receipt.json";
                  runtimeMaxSec = 1800;
                };
              };
              pooled-review = {
                subject = "https://example.invalid/change/42";
                minimumValid = 2;
              };
              worklist-fanout = {
                repository = "mecattaf/tally.nix";
                label = "ready";
                waveSize = 4;
              };
            };
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
        # The NixOS test disks are intentionally about 1 GiB. Keep the
        # production storage defaults covered by the module/evaluation checks,
        # while giving live VM daemons an explicit fixture-sized free-space
        # policy so their first enqueue exercises the intended test surface.
        vmStorageBudget = {
          dataDir = {
            warningFreeBytes = 134217728;
            minimumFreeBytes = 67108864;
          };
          stateDir = {
            warningFreeBytes = 134217728;
            minimumFreeBytes = 67108864;
          };
        };
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
            storage = vmStorageBudget;
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
                  codex-window = {
                    resource = "slot";
                    capacity = 16;
                  };
                  claude-window = {
                    resource = "slot";
                    capacity = 8;
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
                adapters.production = {
                  argv = [ "true" ];
                  hardening = "production";
                  extraWritablePaths = [ "/var/lib/tally-stock/agent-state" ];
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
                    reviewers = [ "tally-reviewer" ];
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
        campaignHome = home-manager.lib.homeManagerConfiguration {
          inherit pkgs;
          modules = [
            self.homeManagerModules.tally
            {
              home = {
                username = "tally-campaign";
                homeDirectory = "/tmp/tally-campaign-home";
                stateVersion = "26.11";
              };
              services.tally = {
                enable = true;
                campaignPoll = {
                  interval = "4min";
                  timeout = "2min";
                };
                adapters.narrator = {
                  argv = [
                    "/bin/sh"
                    "/srv/campaign-fixtures/narrate"
                  ];
                  # The adapter entry is what supplies the narrator's endpoint
                  # and credentials, so the seam must carry env into the brief;
                  # a narrator that got only argv would fail twice and narrate
                  # from the template for ever without saying so.
                  env.NARRATOR_ENDPOINT = "https://narrator.invalid/v1";
                  # And the proposal is read back from the capture the adapter
                  # declares, not from a pattern the driver hardcodes.
                  scrape.finalMessage = {
                    stream = "stdout";
                    mode = "regex";
                    pattern = "^NARRATOR_RESULT=(.*)$";
                  };
                };
                # Keep the campaign's implementation role separate from the
                # shell adapter used by deterministic flow nodes: declaring a
                # result projection on shell would make every ordinary command
                # wait for a final message it never emits.
                adapters.fixture-agent.scrape.finalMessage = {
                  stream = "stdout";
                  mode = "regex";
                  pattern = "^TALLY_FINAL_MESSAGE=(.*)$";
                };
                campaigns.fixture = {
                  enable = true;
                  repositories."acme/spec".checkout = toString ./test/fixtures/spec-build/repo;
                  label = "spec-campaign";
                  # A mention token is posted as a real comment and at-mentions
                  # whoever it names, so the fixture names the campaign's own
                  # trusted actor rather than an unrelated real account.
                  mention = "@operator build";
                  allowSelfTriggered = true;
                  allowedActors = [ "operator" ];
                  pollIntervalSec = 17;
                  postFailureEvidence = true;
                  postFailureStderr = true;
                  worklist = "specs/*/tasks.json";
                  maxTasks = 7;
                  maxParallel = 3;
                  gates = [
                    {
                      kind = "command";
                      id = "content";
                      # A fixture is copied, so it must not model a no-op probe.
                      # This one exercises what the gate below actually needs --
                      # the interpreter the gate argv names -- and asserts that
                      # the subject the gate tests for is genuinely absent on
                      # the pristine base, so a green post-change gate is proof
                      # the agent built it rather than proof it was always
                      # there.
                      preflightArgv = [
                        "/bin/sh"
                        "-eu"
                        "-c"
                        "test -x /bin/sh; test ! -d build"
                      ];
                      argv = [
                        "/bin/sh"
                        "-eu"
                        "-c"
                        "test -d build"
                      ];
                    }
                    {
                      kind = "forbidPaths";
                      id = "no-db-artifacts";
                      forbidPaths = [
                        "*.db"
                        "*.db-wal"
                        "*.db-shm"
                        "*.sqlite*"
                      ];
                      runtimeMaxSec = 11;
                    }
                  ];
                  agent = "fixture-agent";
                  agentArgv = [ "/bin/true" ];
                  agentApprovalPolicy = null;
                  agentSandboxPolicy = null;
                  agentDiagnosisSandboxPolicy = null;
                  agentRuntimeMaxSec = 120;
                  driverRuntimeMaxSec = 30;
                  mergeMethod = "merge";
                  # Advisory is the only posture this wave arms: the merge node
                  # records what the binding found and never fails on it. The
                  # externally provisioned binary is absent from this fixture,
                  # so what it records here is that fact.
                  gitAiBinding = "advisory";
                  # driverRuntimeMaxSec is 30 here, and the settlement barrier
                  # runs inside that node. The evaluated assertion requires the
                  # deadline to be at least twice this budget, so the fixture
                  # states a budget its own node can actually host.
                  gitAiAwaitSec = 12;
                  # The narrate seam is config-only: an adapter the estate adds
                  # to the open map, named as this campaign's catalog role. The
                  # rendered narration argv is the adapter's own argv with this
                  # campaign's stewardArgv appended, so swapping narrators is an
                  # adapter change and never a driver change.
                  steward = "narrator";
                  stewardArgv = [ "--narrate" ];
                  stewardRuntimeMaxSec = 45;
                  pool.name = "fixture-campaign";
                };
                # The two-repository seam, rendered: the worklist and the
                # campaign thread live on the spec repository, the lanes and
                # pull requests on the code repository. Each role names an
                # entry of the campaign's own repositories map.
                campaigns.split = {
                  enable = true;
                  # The spec corpus is the fixture repository that actually
                  # holds the worklist; the code repository is a separate
                  # operational checkout, which is what makes this a split.
                  # `checkout` is operational state rather than a store source,
                  # so the code role names a plain path the way an estate would.
                  repositories."acme/spec".checkout = toString ./test/fixtures/spec-build/repo;
                  repositories."acme/code".checkout = "/srv/campaign-fixtures/split-code";
                  codeRepository = "acme/code";
                  specRepository = "acme/spec";
                  # The worklist this glob resolves to carries seven tasks, so
                  # maxTasks admits seven. A fixture whose own bounds refuse
                  # its own worklist describes a configuration that could never
                  # run a pass.
                  worklist = "specs/001-toy/tasks.json";
                  maxTasks = 7;
                  gates = [
                    {
                      kind = "command";
                      id = "content";
                      preflightArgv = [
                        "/bin/sh"
                        "-eu"
                        "-c"
                        "test -x /bin/sh; test ! -d build"
                      ];
                      argv = [
                        "/bin/sh"
                        "-eu"
                        "-c"
                        "test -d build"
                      ];
                    }
                  ];
                  agentArgv = [ "/bin/true" ];
                  pool.name = "split-campaign";
                };
                campaigns.defaulted = {
                  enable = true;
                  repositories."acme/spec".checkout = toString ./test/fixtures/spec-build/repo;
                  maxTasks = 1;
                  gates = [
                    {
                      kind = "command";
                      id = "content";
                      preflightArgv = [
                        "/bin/sh"
                        "-eu"
                        "-c"
                        "test -x /bin/sh; test ! -d build"
                      ];
                      argv = [
                        "/bin/sh"
                        "-eu"
                        "-c"
                        "test -d build"
                      ];
                    }
                  ];
                  agentArgv = [ "/bin/true" ];
                  pool.name = "default-campaign";
                };
                # The generated campaign producer's projection literals are
                # mkDefault, so an estate tunes one campaign's public surface
                # with an ordinary override: no mkForce, no forked producer
                # builder. The sibling campaign keeps the shipped defaults, so
                # this also proves the override is scoped to one producer.
                # `kind` is the producer registry's discriminator, so an
                # override states it; nothing else here is mkForce.
                producers.campaign-defaulted = {
                  kind = "gh";
                  postEvidence = false;
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
        pollDisabledHome = home-manager.lib.homeManagerConfiguration {
          inherit pkgs;
          modules = [
            self.homeManagerModules.tally
            {
              home = {
                username = "tally-poll-disabled";
                homeDirectory = "/tmp/tally-poll-disabled-home";
                stateVersion = "26.11";
              };
              services.tally = {
                enable = true;
                campaignPoll.enable = false;
              };
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
                  bad-failure-stderr = {
                    kind = "gh";
                    postFailureStderr = true;
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
        # Gate fixtures that evaluate cleanly and are then rejected by a named
        # assertion, so `mkAssertions` can be read for the message.
        invalidCampaignGateFields = [
          {
            kind = "command";
            id = "mixed";
            # no-op-probe-allowed: this gate exists to be rejected. It declares
            # forbidPaths beside a command kind, so the module refuses the
            # campaign before any probe could run; a representative probe here
            # would only obscure what the fixture is testing.
            preflightArgv = [ "true" ];
            argv = [ "true" ];
            forbidPaths = [ "*.db" ];
          }
          {
            kind = "forbidPaths";
            id = "ambiguous-double-star";
            forbidPaths = [ "src/**.db" ];
          }
        ];
        # `kind` is an enum with no default, so a gate that omits it is refused
        # one layer earlier than an assertion: the option system itself throws
        # "The option `...kind' was accessed but has no value defined." That is
        # the exact failure an out-of-repo configuration written before gate
        # kinds became mandatory hits on its next deploy, and it is the
        # migration the CHANGELOG names, so the fixture set has to contain it.
        # It is kept out of the list above, and out of `invalidCampaignHome`,
        # because forcing it throws rather than yielding an assertion message:
        # a fixture carrying it fails at the option system before Home
        # Manager's assertion machinery runs, so it would take every readable
        # campaign-gate message down with it and leave
        # `!invalidCampaignAttempt.success` green even if the campaign-gate
        # assertions were unwired from the module entirely.
        missingKindCampaignGate = {
          id = "no-kind";
          # no-op-probe-allowed: this gate exists to be refused for the one
          # field it omits, so nothing here ever runs and a representative
          # probe would only obscure what the fixture tests.
          preflightArgv = [ "true" ];
          argv = [ "true" ];
        };
        # The same gate with the one missing field supplied. It is the control:
        # without it, the refusal below would be evidence only that *something*
        # about the fixture is wrong.
        suppliedKindCampaignGate = missingKindCampaignGate // {
          kind = "command";
        };
        invalidCampaignHome = home-manager.lib.homeManagerConfiguration {
          inherit pkgs;
          modules = [
            self.homeManagerModules.tally
            {
              home = {
                username = "tally-invalid-campaign";
                homeDirectory = "/tmp/tally-invalid-campaign-home";
                stateVersion = "26.11";
              };
              services.tally = {
                enable = true;
                campaigns.invalid = {
                  enable = true;
                  repositories."acme/spec".checkout = "/tmp/spec";
                  # Only the field fixtures. This activation is what proves the
                  # campaign-gate assertions are wired into Home Manager at all,
                  # and it can only prove that by failing *as an assertion*.
                  gates = invalidCampaignGateFields;
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
            enqueue ? { },
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
                  inherit enqueue;
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
                  config.services.tally.producers = moduleCommon.mkFlowProducers config.services.tally config.services.tally.flows;
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
            ''tally: {"name":"FlowDeterminismError","code":"determinism-violation","message":"banned global Math.random is unavailable in flow scripts because it would break replay; derive the choice from witnessed input, or let members() pick, instead","location":{"line":8,"column":1},"details":{"global":"Math.random"}}''
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
        flowSugarPoolsFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-sugar-pools-failure";
          drv = moduleCommon.mkCheckedConfig (mkFlowConfig {
            script = ./test/fixtures/flows/sugar-pools.js;
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            ''tally: {"name":"FlowSpecError","code":"sugar-option-conflict","message":"sugar option \"pools\" is fixed by claude() and cannot be set by the script","location":{"line":8,"column":36},"details":{"field":"pools"}}''
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
        flowFanoutWidthFailure = pkgs.testers.testBuildFailure' {
          name = "tally-flow-fanout-width-failure";
          drv = moduleCommon.mkCheckedConfig (mkFlowConfig {
            script = ./test/fixtures/flows/valid.js;
            args.task = "ship";
            catalog = catalogFixture;
            enqueue.fanoutCap = 4;
          });
          expectedBuilderExitCode = 1;
          expectedBuilderLogEntries = [
            "tally flow fixture script meta.maxNodes 5 exceeds enqueue.fanoutCap 4; raise services.tally.enqueue.fanoutCap or lower meta.maxNodes"
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
                bad-failure-stderr = {
                  kind = "gh";
                  postFailureStderr = true;
                  enqueue = {
                    argv = [ "gh-job" ];
                    pool = "slot";
                  };
                };
                bad-review = {
                  kind = "gh";
                  requestReview = true;
                  enqueue = {
                    argv = [ "gh-job" ];
                    pool = "slot";
                  };
                };
                bad-reviewer-login = {
                  kind = "gh";
                  requestReview = true;
                  reviewers = [ "not a login" ];
                  enqueue = {
                    argv = [ "gh-job" ];
                    pool = "slot";
                  };
                };
                # 40 characters: grammatical, one past GitHub's own bound. The
                # module used to accept this and the daemon then refused to
                # load the config it was deployed with.
                bad-reviewer-length = {
                  kind = "gh";
                  requestReview = true;
                  reviewers = [ "abcdefghijklmnopqrstuvwxyz0123456789abcd" ];
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
        invalidCampaignSchema = pkgs.lib.evalModules {
          modules = [
            {
              options.services.tally = moduleCommon.mkOptions {
                defaultPackage = tally;
                defaultDataDir = "/tmp/tally-data";
                defaultStateDir = "/tmp/tally-state";
              };
              config.services.tally.campaigns.invalid = {
                repositories."acme/spec".checkout = "/tmp/spec";
                gates = invalidCampaignGateFields;
              };
            }
          ];
        };
        mkCampaignGateSchema =
          gates:
          pkgs.lib.evalModules {
            modules = [
              {
                options.services.tally = moduleCommon.mkOptions {
                  defaultPackage = tally;
                  defaultDataDir = "/tmp/tally-data";
                  defaultStateDir = "/tmp/tally-state";
                };
                config.services.tally.campaigns.invalid = {
                  repositories."acme/spec".checkout = "/tmp/spec";
                  inherit gates;
                };
              }
            ];
          };
        forceCampaignGates =
          schema:
          builtins.tryEval (builtins.deepSeq schema.config.services.tally.campaigns.invalid.gates true);
        missingKindCampaignAttempt = forceCampaignGates (mkCampaignGateSchema [ missingKindCampaignGate ]);
        suppliedKindCampaignAttempt = forceCampaignGates (mkCampaignGateSchema [
          suppliedKindCampaignGate
        ]);
        nonCommittingCampaignSchema = pkgs.lib.evalModules {
          modules = [
            {
              options.services.tally = moduleCommon.mkOptions {
                defaultPackage = tally;
                defaultDataDir = "/tmp/tally-data";
                defaultStateDir = "/tmp/tally-state";
              };
              # workspace-write is a declared codex sandbox policy, so nothing
              # about the name is wrong; it is simply one the adapter says
              # cannot commit, which an implementation node must.
              config.services.tally = {
                adapters = moduleCommon.adapterDefaults;
                campaigns.writable-only = {
                  enable = true;
                  repositories."acme/spec".checkout = "/tmp/spec";
                  gates = [
                    {
                      kind = "command";
                      id = "content";
                      preflightArgv = [
                        "/bin/sh"
                        "-eu"
                        "-c"
                        "test -x /bin/sh; test ! -d build"
                      ];
                      argv = [
                        "/bin/sh"
                        "-eu"
                        "-c"
                        "test -d build"
                      ];
                    }
                  ];
                  agentSandboxPolicy = "workspace-write";
                };
              };
            }
          ];
        };
        reservedCampaignNameSchema = pkgs.lib.evalModules {
          modules = [
            {
              options.services.tally = moduleCommon.mkOptions {
                defaultPackage = tally;
                defaultDataDir = "/tmp/tally-data";
                defaultStateDir = "/tmp/tally-state";
              };
              # `campaigns.continuation` renders `producers.campaign-continuation`,
              # which is the name the campaign layer seeds the generic events-dir
              # drain under. The fold adds campaign producers on top of that seed,
              # so the campaign would replace the drain with a gh producer.
              config.services.tally.campaigns.continuation = {
                enable = true;
                repositories."acme/spec".checkout = "/tmp/spec";
                gates = [
                  {
                    kind = "command";
                    id = "content";
                    preflightArgv = [
                      "/bin/sh"
                      "-eu"
                      "-c"
                      "test -x /bin/sh; test ! -d build"
                    ];
                    argv = [
                      "/bin/sh"
                      "-eu"
                      "-c"
                      "test -d build"
                    ];
                  }
                ];
              };
            }
          ];
        };
        reservedCampaignNameMessages = map (entry: entry.message) (
          builtins.filter (entry: !entry.assertion) (
            moduleCommon.mkAssertions reservedCampaignNameSchema.config.services.tally
          )
        );
        nonCommittingCampaignMessages = map (entry: entry.message) (
          builtins.filter (entry: !entry.assertion) (
            moduleCommon.mkAssertions nonCommittingCampaignSchema.config.services.tally
          )
        );
        invalidCampaignAssertions = builtins.filter (entry: !entry.assertion) (
          moduleCommon.mkAssertions invalidCampaignSchema.config.services.tally
        );
        invalidCampaignMessages = map (entry: entry.message) invalidCampaignAssertions;
        invalidCampaignAttempt = builtins.tryEval (
          builtins.deepSeq invalidCampaignHome.activationPackage true
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
        # A NixOS host that executes forge-native campaigns. The token path is a
        # path, never a secret: activation reads it, the module never does.
        campaignNixos = nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.tally
            nixosBase
            {
              services.tally = {
                enable = true;
                campaignForge = {
                  enable = true;
                  login = "tally-fixture";
                  tokenFile = "/run/secrets/tally-campaign-forge-token";
                };
                campaignPoll.interval = "4min";
                campaignPoll.timeout = "2min";
              };
            }
          ];
        };
        campaignPollDisabledNixos = nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.tally
            nixosBase
            {
              services.tally = {
                enable = true;
                campaignForge = {
                  enable = true;
                  login = "tally-fixture";
                  tokenFile = "/run/secrets/tally-campaign-forge-token";
                };
                campaignPoll.enable = false;
              };
            }
          ];
        };
        # The identity is not optional on this module, so an operator who turns
        # the surface on without declaring one is refused at evaluation rather
        # than by a timer that fails every tick.
        anonymousCampaignNixos = nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.tally
            nixosBase
            { services.tally.campaignForge.enable = true; }
          ];
        };
        badLoginCampaignNixos = nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.tally
            nixosBase
            {
              services.tally = {
                enable = true;
                campaignForge = {
                  enable = true;
                  login = "not a login";
                  tokenFile = "/run/secrets/tally-campaign-forge-token";
                };
              };
            }
          ];
        };
        failedAssertionMessages =
          evaluated:
          builtins.map (entry: entry.message) (
            builtins.filter (entry: !entry.assertion) evaluated.config.assertions
          );
        anonymousCampaignMessages = failedAssertionMessages anonymousCampaignNixos;
        badLoginCampaignMessages = failedAssertionMessages badLoginCampaignNixos;
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
                campaigns.fixture = {
                  enable = true;
                  repositories."acme/spec".checkout = "/tmp/spec";
                  gates = [
                    {
                      kind = "command";
                      id = "tests";
                      # no-op-probe-allowed: this host is evaluated only to
                      # prove the unsupported-system assertion fires. The
                      # campaign never renders a flow, so the gate's content is
                      # irrelevant to what the fixture asserts.
                      preflightArgv = [ "true" ];
                      argv = [ "true" ];
                    }
                  ];
                };
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
                storage = vmStorageBudget;
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
                    storage = vmStorageBudget;
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
            machine.succeed(
              "cd /tmp && ${tally}/bin/tally --config /etc/tally/config.json --socket /run/tally/tally.sock adapter smoke shell "
              "| ${pkgs.jq}/bin/jq -e '.adapter == \"shell\" and .pool == \"stock\" and .verdict == \"pass\" and .captureStatus == \"not-declared\"'"
            )

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
              system.extraDependencies = [ campaignHostProbeFlow ];
              virtualisation.memorySize = 2048;

              systemd.tmpfiles.rules = [
                "d /srv/tally/production-agent 0700 tally tally -"
                "d /srv/tally/campaign-host 0700 tally tally -"
              ];
              system.activationScripts.campaignHostToken = {
                deps = [ "users" ];
                text = ''
                  printf 'fixture-token\n' > /run/tally-campaign-host-token
                  chmod 0600 /run/tally-campaign-host-token
                '';
              };
              system.activationScripts.tallyCampaignForgeIdentity.deps = pkgs.lib.mkAfter [
                "campaignHostToken"
              ];

              services.tally = {
                enable = true;
                dataDir = "/srv/tally/data";
                stateDir = "/srv/tally/state";
                retention.enable = false;
                storage = vmStorageBudget;
                transport.maxFrameBytes = 20971520;
                campaignForge = {
                  enable = true;
                  login = "operator";
                  tokenFile = "/run/tally-campaign-host-token";
                };
                campaignPoll.enable = false;
                pools.stock = {
                  resource = "build-slot";
                  capacity = 1;
                  enforce = "cooperative";
                };
                pools.sentinel-campaign = {
                  resource = "mutex";
                  capacity = 1;
                  enforce = "cooperative";
                  predicate.co-residency = { };
                };
                adapters.shell.env.PATH = pkgs.lib.makeBinPath [
                  campaignHostFakeGh
                  pkgs.coreutils
                  pkgs.git
                  pkgs.nix
                ];
                adapters.production-probe = {
                  hardening = "production";
                  extraWritablePaths = [ "/srv/tally/production-agent" ];
                };
              };
            };
          testScript = ''
            import json
            import re
            import shlex

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

            machine.succeed("install -o tally -g tally -m 0600 /dev/null /srv/tally/state/forbidden")
            machine.succeed("printf 'sentinel\\n' > /srv/tally/state/forbidden")
            machine.succeed("install -d -o tally -g tally -m 0700 /srv/tally/state/capture")
            machine.succeed("install -o tally -g tally -m 0600 /dev/null /srv/tally/state/capture/foreign.out")
            machine.succeed("printf 'sentinel\\n' > /srv/tally/state/capture/foreign.out")

            submitted = json.loads(machine.succeed(
              "${tally}/bin/tally --socket /run/tally/tally.sock "
              "enqueue --pool stock --adapter production-probe -- "
              "${hardeningProbe}/bin/tally-hardening-probe"
            ))
            hardened_task = submitted["task_uuid"]
            unit = f"tally-job-{hardened_task}.service"
            userctl = (
              f"runuser -u tally -- env XDG_RUNTIME_DIR=/run/user/{uid} "
              "systemctl --user"
            )
            machine.wait_until_succeeds(userctl + f" is-active {unit}")
            machine.wait_until_succeeds("test -s /srv/tally/production-agent/ready")

            def unit_property(name):
                return machine.succeed(
                  userctl + f" show {unit} --property={name} --value"
                ).strip()

            expected_properties = {
              "ProtectHome": "read-only",
              "PrivateTmp": "yes",
              "ProtectSystem": "strict",
              "NoNewPrivileges": "yes",
              "PrivateDevices": "yes",
              "ProtectKernelTunables": "yes",
              "ProtectKernelModules": "yes",
              "ProtectKernelLogs": "yes",
              "ProtectControlGroups": "yes",
              "ProtectClock": "yes",
              "RestrictSUIDSGID": "yes",
              "LockPersonality": "yes",
              "RestrictRealtime": "yes",
              "CapabilityBoundingSet": "",
              "ProtectProc": "invisible",
            }
            for name, expected in expected_properties.items():
                actual = unit_property(name)
                assert actual == expected, (name, actual, expected)

            address_families = set(unit_property("RestrictAddressFamilies").split())
            assert address_families == {"AF_UNIX", "AF_INET", "AF_INET6"}, address_families
            system_calls = set(unit_property("SystemCallFilter").split())
            assert {"read", "write", "socket"} <= system_calls, system_calls
            assert {"mount", "reboot", "kexec_load"}.isdisjoint(system_calls), system_calls

            writable_paths = set(unit_property("ReadWritePaths").split())
            expected_writable_paths = {
              "/srv/tally/state/unit-exit",
              f"/srv/tally/state/capture/{hardened_task}.out",
              f"/srv/tally/state/capture/{hardened_task}.adapter.err",
              "/srv/tally/state/exec-attestations.jsonl",
              "/srv/tally/production-agent",
            }
            assert writable_paths == expected_writable_paths, writable_paths

            machine.succeed("touch /srv/tally/production-agent/release")
            hardened = json.loads(machine.succeed(
              "${tally}/bin/tally --socket /run/tally/tally.sock --rpc-timeout-sec 60 "
              f"queue await-job {hardened_task}"
            ))
            assert hardened["verdict"] == "pass", hardened
            assert hardened["exit_code"] == 0, hardened
            machine.succeed("test \"$(cat /srv/tally/production-agent/allowed)\" = allowed")
            machine.succeed("test \"$(cat /srv/tally/production-agent/socket)\" = reachable")
            machine.succeed("test \"$(cat /srv/tally/state/forbidden)\" = sentinel")
            machine.succeed("test \"$(cat /srv/tally/state/capture/foreign.out)\" = sentinel")
            machine.succeed(
              "${pkgs.jq}/bin/jq -e --arg task " + hardened_task +
              " 'select(.payload.taskUuid == $task and .payload.adapter == \"production-probe\")' "
              "/srv/tally/state/exec-attestations.jsonl"
            )

            # #442: the system module has one config, at /etc/tally. Both the
            # initial campaign flow and its continuation must inherit that
            # exact locator; the service account deliberately has no XDG copy.
            campaign_root = "/srv/tally/campaign-host"
            campaign_state = "/srv/tally/state"
            campaign_socket = "/run/tally/tally.sock"
            issue = "https://github.com/acme/sentinel/issues/1"
            git = "${pkgs.git}/bin/git"
            machine.succeed(
              "${pkgs.coreutils}/bin/install -o tally -g tally -m 0600 /dev/null "
              + campaign_root + "/master-body"
            )
            machine.succeed(
              "${pkgs.jq}/bin/jq -r .body ${campaignHostMaster} > "
              + campaign_root + "/master-body"
            )
            campaign_user = (
              "runuser -u tally -- env HOME=/var/lib/tally/forge "
              f"XDG_RUNTIME_DIR=/run/user/{uid} "
              "TALLY_GH_PROGRAM=${campaignHostFakeGh}/bin/gh"
            )
            campaign_cli = (
              campaign_user + " ${tally}/bin/tally"
              " --config /etc/tally/config.json"
              " --socket " + campaign_socket +
              " --rpc-timeout-sec 240"
            )

            machine.succeed(
              "${pkgs.jq}/bin/jq -e '"
              ".maxFrameBytes == 20971520 and "
              ".pools[\"sentinel-campaign\"].resource == \"mutex\" and "
              ".pools[\"sentinel-campaign\"].capacity == 1' "
              "/etc/tally/config.json"
            )
            machine.succeed("test ! -e /var/lib/tally/forge/.config/tally/config.json")

            checkout = campaign_root + "/checkout"
            remote = campaign_root + "/remote.git"
            machine.succeed(
              "runuser -u tally -- " + git + " init --bare --quiet --initial-branch=main " + remote
            )
            machine.succeed(
              "runuser -u tally -- " + git + " init --quiet --initial-branch=main " + checkout
            )
            machine.succeed(
              "runuser -u tally -- " + git + " -C " + checkout +
              " config user.name 'Campaign Host Test'"
            )
            machine.succeed(
              "runuser -u tally -- " + git + " -C " + checkout +
              " config user.email campaign-host@example.invalid"
            )
            machine.succeed(
              "runuser -u tally -- ${pkgs.bash}/bin/bash -c " +
              shlex.quote("printf 'base\\n' > " + checkout + "/README.md")
            )
            machine.succeed("runuser -u tally -- " + git + " -C " + checkout + " add README.md")
            machine.succeed(
              "runuser -u tally -- " + git + " -C " + checkout +
              " commit --quiet -m 'campaign host fixture'"
            )
            machine.succeed(
              "runuser -u tally -- " + git + " -C " + checkout + " remote add origin " + remote
            )
            machine.succeed(
              "runuser -u tally -- " + git + " -C " + checkout +
              " push --quiet --set-upstream origin main"
            )
            base_before = machine.succeed(
              "runuser -u tally -- " + git + " --git-dir=" + remote +
              " rev-parse refs/heads/main"
            ).strip()
            # Hold the flow-authored event so the successor has to enter
            # through the same plain public poll an operator runs.
            machine.succeed("systemctl stop tally-drain.timer tally-drain.service")

            arm = (
              campaign_cli + " campaign arm " + issue +
              " --allow-test-local-forge"
              " --flow ${campaignHostProbeFlow}"
              " --state-dir " + campaign_state +
              " --workspace-root " + campaign_root + "/workspaces"
              " --wait"
            )
            armed = json.loads(machine.succeed(arm))
            assert armed["verdict"] == "pass", armed
            assert armed["projection"] == "degraded-checkboxes", armed
            assert machine.succeed(
              "wc -l < " + campaign_root + "/frame-probes"
            ).strip() == "1"
            base_after = machine.succeed(
              "runuser -u tally -- " + git + " --git-dir=" + remote +
              " rev-parse refs/heads/main"
            ).strip()
            assert base_after != base_before, (base_before, base_after)
            machine.succeed("test ! -e " + campaign_root + "/master-closed")
            machine.succeed("test ! -e " + campaign_root + "/final-digests")
            machine.succeed("test ! -e " + campaign_root + "/body-file-records")

            # Exact public convergence boundary: no token argument and no
            # daemon-drained event. The forge remains unchanged until the
            # Git-derived successor performs terminal closeout.
            plain_poll = (
              campaign_cli + " campaign poll --once --wait --state-dir " + campaign_state
            )

            # An operator edit changes the executable digest. Poll must still
            # refuse to dispatch it, but that correct admission stop is a
            # structured blocked state rather than a failed periodic service.
            machine.succeed(
              "grep -F '\"name\":\"sentinel-config\"' "
              + campaign_root + "/master-body"
            )
            machine.succeed(
              "sed -i 's/\"name\":\"sentinel-config\"/"
              "\"name\":\"sentinel-config-edited\"/' "
              + campaign_root + "/master-body"
            )
            blocked_poll = json.loads(machine.succeed(plain_poll))
            assert blocked_poll["observed"] == 1, blocked_poll
            assert blocked_poll["dispatched"] == 0, blocked_poll
            assert blocked_poll["pruned"] == 0, blocked_poll
            assert blocked_poll["failures"] == [], blocked_poll
            assert len(blocked_poll["blocked"]) == 1, blocked_poll
            blocked = blocked_poll["blocked"][0]
            assert blocked["issue"] == issue, blocked
            assert blocked["reason"] == "rearm-required", blocked
            assert blocked["approvedGraphDigest"] != blocked["liveGraphDigest"], blocked
            assert machine.succeed(
              "wc -l < " + campaign_root + "/frame-probes"
            ).strip() == "1"

            # Restore the admitted graph; the ordinary poll path can now
            # observe the local merge and drive terminal closeout.
            machine.succeed(
              "${pkgs.jq}/bin/jq -r .body ${campaignHostMaster} > "
              + campaign_root + "/master-body"
            )
            poll_results = []
            for _ in range(8):
              poll_results.append(json.loads(machine.succeed(plain_poll)))
              if machine.execute("test -e " + campaign_root + "/master-closed")[0] == 0:
                break
            else:
              raise AssertionError(("plain campaign polls did not reach closeout", poll_results))
            assert any(result["dispatched"] == 1 for result in poll_results), poll_results
            assert machine.succeed(
              "wc -l < " + campaign_root + "/frame-probes"
            ).strip() == "2"
            assert machine.succeed(
              "wc -l < " + campaign_root + "/final-digests"
            ).strip() == "1"
            fake_gh = campaign_user + " ${campaignHostFakeGh}/bin/gh"
            closed_master = json.loads(machine.succeed(
              fake_gh + " api repos/acme/sentinel/issues/1"
            ))
            digest_pages = json.loads(machine.succeed(
              fake_gh + " api --paginate --slurp "
              "repos/acme/sentinel/issues/1/comments?per_page=100"
            ))
            digests = [comment for page in digest_pages for comment in page]
            assert closed_master["state"] == "closed", closed_master
            assert len(digests) == 1, digests
            assert "tally:campaign-complete:v1" in digests[0]["body"], digests
            assert re.search(
              r"\bsha256:[0-9a-f]{64}\b", digests[0]["body"]
            ), digests
            assert "### Campaign complete" in digests[0]["body"], digests
            assert "- [x] <!-- tally:campaign-task:v1 id=sentinel-checkpoint -->" in (
              closed_master["body"]
            ), closed_master
            body_files = [
              json.loads(line)
              for line in machine.succeed(
                "cat " + campaign_root + "/body-file-records"
              ).splitlines()
            ]
            assert len(body_files) == 1, body_files
            assert body_files[0]["argument"] != "-", body_files
            assert "- [x]" in body_files[0]["body"], body_files

            # Now drain the held event. It still exercises the packaged
            # continuation child and host argv, but the closed master makes it
            # a no-op rather than the source of convergence.
            machine.succeed("systemctl start tally-drain.service")

            def jobs():
              return json.loads(machine.succeed(campaign_cli + " query jobs"))["items"]

            machine.wait_until_succeeds(
              campaign_cli + " query jobs | ${pkgs.jq}/bin/jq -e '"
              "[.items[] | select(.argv[5:7] == [\"flow\",\"run\"] and "
              ".argv[7] == \"${campaignHostProbeFlow}\" and .terminalVerdict == \"pass\")] "
              "| length == 2'"
            )
            machine.wait_until_succeeds(
              campaign_cli + " query jobs | ${pkgs.jq}/bin/jq -e '"
              "[.items[] | select(.argv[5:7] == [\"campaign\",\"poll\"] and "
              ".argv[-2:] == [\"--state-dir\",\"" + campaign_state + "\"] and "
              ".terminalVerdict == \"pass\")] | length == 1'"
            )
            observed = jobs()
            flow_argvs = [
              item for item in observed
              if item["argv"][5:7] == ["flow", "run"]
              and item["argv"][7] == "${campaignHostProbeFlow}"
            ]
            continuation_argvs = [
              item for item in observed
              if item["argv"][5:7] == ["campaign", "poll"]
              and item["argv"][-2:] == ["--state-dir", campaign_state]
            ]
            frame_jobs = [
              item for item in observed
              if item["argv"] == ["${campaignHostProbe}/bin/tally-campaign-host-probe"]
            ]
            assert len(flow_argvs) == 2, flow_argvs
            assert len(continuation_argvs) == 1, continuation_argvs
            assert len(frame_jobs) == 2, frame_jobs
            for item in flow_argvs + continuation_argvs:
              assert item["argv"][1:5] == [
                "--config", "/etc/tally/config.json", "--socket", campaign_socket
              ], item
              assert item["terminalVerdict"] == "pass", item
            continuation = continuation_argvs[0]
            assert continuation["argv"][5:] == [
              "campaign", "poll", "--once", "--state-dir", campaign_state
            ], continuation
            for item in flow_argvs:
              assert set(item["pool"]) == {"flow", "sentinel-campaign"}, item
            assert all(item["terminalVerdict"] == "pass" for item in frame_jobs), frame_jobs
            assert len({item["briefHash"] for item in frame_jobs}) == 1, frame_jobs
            frame_digest = frame_jobs[0]["briefHash"].removeprefix("sha256:")
            machine.succeed(
              "test \"$(stat -c %s /srv/tally/data/briefs/" + frame_digest +
              ".json)\" -eq 16776714"
            )
            machine.succeed(
              "grep -F -- '--config /etc/tally/config.json --socket " + campaign_socket +
              " campaign poll --once --state-dir " + campaign_state + "' " +
              campaign_root + "/gh-callers"
            )

            # Mutation one: replay the real initial argv and real campaign
            # brief after deleting only its serialized --config/path pair.
            # The XDG lookup must fail before configured-frame can be admitted.
            probes_before = machine.succeed("wc -l < " + campaign_root + "/frame-probes").strip()
            initial = flow_argvs[0]
            assert initial["argv"][1:3] == ["--config", "/etc/tally/config.json"], initial
            initial_without_config = initial["argv"][:1] + initial["argv"][3:]
            initial_brief = (
              "/srv/tally/data/briefs/" + initial["briefHash"].removeprefix("sha256:") + ".json"
            )
            status, output = machine.execute(
              campaign_cli +
              " enqueue --pool flow --pool sentinel-campaign --adapter shell"
              " --brief-path " + initial_brief + " --wait -- " +
              shlex.join(initial_without_config)
            )
            assert status != 0, output
            assert machine.succeed("wc -l < " + campaign_root + "/frame-probes").strip() == probes_before
            mutated_initial = next(item for item in jobs() if item["argv"] == initial_without_config)
            assert mutated_initial["terminalVerdict"] == "failed", mutated_initial
            machine.succeed(
              "grep -F 'cannot read config /var/lib/tally/forge/.config/tally/config.json' "
              "/srv/tally/state/capture/" + mutated_initial["taskUuid"] + ".adapter.err"
            )

            # Mutation two: arm a fresh no-enqueue registry, then replay the
            # real continuation argv against it with only the same pair
            # removed. Poll must fail validation before dispatching a flow.
            # Re-open the fixture after the closeout assertions so this poll
            # reaches host validation instead of pruning a closed campaign.
            machine.succeed("rm " + campaign_root + "/master-closed")
            mutation_state = campaign_root + "/mutation-state"
            no_enqueue = (
              campaign_cli + " campaign arm " + issue +
              " --allow-test-local-forge --no-enqueue"
              " --flow ${campaignHostProbeFlow}"
              " --state-dir " + mutation_state +
              " --workspace-root " + campaign_root + "/mutation-workspaces"
            )
            registered = json.loads(machine.succeed(no_enqueue))
            assert registered["status"] == "armed" and not registered["enqueued"], registered
            assert continuation["argv"][1:3] == ["--config", "/etc/tally/config.json"], continuation
            continuation_without_config = continuation["argv"][:1] + continuation["argv"][3:]
            continuation_without_config[-1] = mutation_state
            status, output = machine.execute(
              campaign_cli + " enqueue --pool campaign-control --adapter shell --wait -- " +
              shlex.join(continuation_without_config)
            )
            assert status != 0, output
            assert machine.succeed("wc -l < " + campaign_root + "/frame-probes").strip() == probes_before
            mutated_continuation = next(
              item for item in jobs() if item["argv"] == continuation_without_config
            )
            assert mutated_continuation["terminalVerdict"] == "failed", mutated_continuation
            # Poll keeps each registration's cause in its bounded JSON summary
            # while stderr carries only the aggregate non-zero diagnosis.
            machine.succeed(
              "grep -F 'cannot read config /var/lib/tally/forge/.config/tally/config.json' "
              "/srv/tally/state/capture/" + mutated_continuation["taskUuid"] + ".out"
            )
            machine.succeed("touch " + campaign_root + "/master-closed")
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
                    storage = vmStorageBudget;
                    pools.stock = {
                      resource = "build-slot";
                      enforce = "cooperative";
                    };
                  };
                };
              };
            };
          testScript = ''
            import json

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

            # Real aged files in the real state directory. Age is injected with
            # mtimes rather than slept for, so the sweep is exercised against a
            # live system without a wall-clock dependency.
            state = "/var/lib/tally-retention/.local/state/tally"
            coreutils = "${pkgs.coreutils}/bin"
            archive_unit = state + "/capture/archive/00000000-0000-4000-8000-000000000001"
            for directory in [
              archive_unit,
              state + "/events/done",
              state + "/events/rejected",
              state + "/events/processing",
            ]:
              machine.succeed(user + " " + coreutils + "/mkdir -p " + directory)

            aged = [
              (archive_unit + "/attempt-0000000001-epoch-00000000000000000001.out", "60 days ago"),
              (archive_unit + "/attempt-0000000001-epoch-00000000000000000001.err", "60 days ago"),
              (archive_unit + "/attempt-0000000002-epoch-00000000000000000001.out", "1 hour ago"),
              (state + "/events/done/expired.json", "200 days ago"),
              (state + "/events/done/recent.json", "10 days ago"),
              (state + "/events/rejected/expired.json", "60 days ago"),
              (state + "/events/rejected/recent.json", "1 hour ago"),
              (state + "/events/processing/inflight.json", "400 days ago"),
            ]
            for path, age in aged:
              machine.succeed(user + " " + coreutils + "/touch " + path)
              machine.succeed(user + " " + coreutils + "/touch -d " + repr(age) + " " + path)

            report = machine.succeed(
              user + " ${tally}/bin/tally gc --horizon 3s --collect --data-dir " + data +
              " --state-dir " + state
            )
            swept = json.loads(report)
            assert swept["rootsPruned"] == 1, report
            assert swept["stateDirSwept"], report
            # The daemon may archive captures of its own; only the two files
            # aged past the horizon are eligible.
            assert swept["captureArchivesExamined"] >= 3, report
            assert swept["captureArchivesPruned"] == 2, report
            assert swept["eventsDoneExamined"] == 2, report
            assert swept["eventsDonePruned"] == 1, report
            assert swept["eventsRejectedExamined"] == 2, report
            assert swept["eventsRejectedPruned"] == 1, report
            assert ledger_hash == machine.succeed(
              "${pkgs.coreutils}/bin/sha256sum " + data + "/witness.jsonl"
            ).split()[0]
            machine.fail(nix_store + " --check-validity " + old_only)
            machine.succeed(nix_store + " --check-validity " + shared)

            for expired in [
              archive_unit + "/attempt-0000000001-epoch-00000000000000000001.out",
              archive_unit + "/attempt-0000000001-epoch-00000000000000000001.err",
              state + "/events/done/expired.json",
              state + "/events/rejected/expired.json",
            ]:
              machine.fail("test -e " + expired)
            for retained in [
              archive_unit + "/attempt-0000000002-epoch-00000000000000000001.out",
              state + "/events/done/recent.json",
              state + "/events/rejected/recent.json",
              state + "/events/processing/inflight.json",
            ]:
              machine.succeed("test -e " + retained)

            # The count bound is the other half of the rejected envelope.
            for index in range(4):
              machine.succeed(
                user + " " + coreutils + "/touch " + state +
                "/events/rejected/hostile-" + str(index) + ".json"
              )
            capped = machine.succeed(
              user + " ${tally}/bin/tally gc --horizon 3s --data-dir " + data +
              " --state-dir " + state + " --events-rejected-max-count 2"
            )
            capped_report = json.loads(capped)
            assert capped_report["eventsRejectedExamined"] == 5, capped
            assert capped_report["eventsRejectedPruned"] == 3, capped
            surviving = machine.succeed(
              coreutils + "/ls " + state + "/events/rejected | " + coreutils + "/wc -l"
            ).strip()
            assert surviving == "2", surviving
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
                      storage = vmStorageBudget;
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
            # Home Manager can finish while the user manager is still coming
            # up and skip its reload. Load and start the newly installed unit
            # explicitly before asserting daemon liveness.
            coordinator.succeed(
              "runuser -u tally -- env HOME=/var/lib/tally-coordinator XDG_RUNTIME_DIR=/run/user/1000 systemctl --user daemon-reload"
            )
            coordinator.succeed(
              "runuser -u tally -- env HOME=/var/lib/tally-coordinator XDG_RUNTIME_DIR=/run/user/1000 systemctl --user start tally-daemon.service"
            )
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
        campaignAssertionMessages = map (entry: entry.message) (
          builtins.filter (entry: !entry.assertion) campaignHome.config.assertions
        );
        campaignHomeServices = campaignHome.config.systemd.user.services;
        campaignHomeTimers = campaignHome.config.systemd.user.timers;
        campaignPollScript = campaignHomeServices.tally-campaign-poll.Service.ExecStart;
        campaignSystemPollScript =
          campaignNixos.config.systemd.services.tally-campaign-poll.serviceConfig.ExecStart;
        checkedHomeConfig = stockHome.config.xdg.configFile."tally/config.json".source;
        checkedCampaignConfig = campaignHome.config.xdg.configFile."tally/config.json".source;
        systemServices = stockNixos.config.systemd.services;
        systemTimers = stockNixos.config.systemd.timers;
        campaignSystemConfig = campaignNixos.config.services.tally;
        campaignSystemServices = campaignNixos.config.systemd.services;
        campaignSystemTimers = campaignNixos.config.systemd.timers;
        campaignSystemActivation =
          campaignNixos.config.system.activationScripts.tallyCampaignForgeIdentity.text;
        systemServiceExec = name: systemServices.${name}.serviceConfig.ExecStart;
        systemDaemon = systemServices.tally-daemon;
        systemWitnessEmitter = systemServices."tally-witness-emit@";
        moduleContract =
          assert
            stockHome.config.services.tally.retention == {
              enable = true;
              horizon = "30d";
              onCalendar = "daily";
              captureArchiveHorizon = "30d";
              eventsDoneHorizon = "180d";
              eventsRejectedHorizon = "30d";
              eventsRejectedMaxCount = 10000;
              producerMarkerHorizon = "180d";
              lifecycleHorizon = "30d";
              lifecycleMaxBytes = 268435456;
            };
          assert
            stockHome.config.services.tally.storage == {
              pollIntervalSec = 60;
              dataDir = {
                warningBytes = 34359738368;
                hardBytes = 68719476736;
                warningFreeBytes = 17179869184;
                minimumFreeBytes = 8589934592;
              };
              stateDir = {
                warningBytes = 34359738368;
                hardBytes = 68719476736;
                warningFreeBytes = 17179869184;
                minimumFreeBytes = 8589934592;
              };
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
          assert pkgs.lib.hasInfix
            "--capture-archive-horizon 30d --events-done-horizon 180d --events-rejected-horizon 30d --events-rejected-max-count 10000 --producer-marker-horizon 180d"
            (homeServiceExec "tally-retention");
          # Forge-native campaigns post no continuation comment, so this timer
          # is the only thing that carries a campaign past its first pass.
          assert homeServices ? tally-campaign-poll;
          assert homeTimers ? tally-campaign-poll;
          assert homeTimers.tally-campaign-poll.Timer.OnUnitActiveSec == "60s";
          assert homeTimers.tally-campaign-poll.Timer.Unit == "tally-campaign-poll.service";
          assert homeServices.tally-campaign-poll.Service.TimeoutStartSec == "90s";
          assert campaignHomeTimers.tally-campaign-poll.Timer.OnUnitActiveSec == "4min";
          assert campaignHomeServices.tally-campaign-poll.Service.TimeoutStartSec == "2min";
          assert !(pollDisabledHome.config.systemd.user.timers ? tally-campaign-poll);
          assert !(pollDisabledHome.config.systemd.user.services ? tally-campaign-poll);
          # campaignMaxNodes and the CLI's max_flow_nodes are two independent
          # implementations of one budget, and nothing else makes them agree.
          # The fixture campaign has the shape the CLI unit test uses --
          # maxParallel 3, two gates, one of them a command gate -- so both
          # sides must land on 3 + (2 + 2*1) + 3*(12 + 2*2) = 55. Drift on
          # either side breaks a test rather than silently capping a run below
          # its own worst case.
          assert campaignHome.config.services.tally.flows.fixture.maxNodes == 55;
          # The generated producer's projection literals are mkDefault: an
          # ordinary estate override wins without mkForce, and every campaign
          # that states no opinion keeps the shipped defaults bit for bit.
          assert campaignHome.config.services.tally.producers.campaign-defaulted.postEvidence == false;
          assert campaignHome.config.services.tally.producers.campaign-defaulted.postReceipt == true;
          assert campaignHome.config.services.tally.producers.campaign-fixture.postEvidence == true;
          assert campaignHome.config.services.tally.producers.campaign-fixture.postReceipt == true;
          assert campaignHome.config.services.tally.producers.campaign-fixture.postGateSummary == false;
          assert campaignHome.config.services.tally.producers.campaign-fixture.requestReview == false;
          assert campaignHome.config.services.tally.producers.campaign-fixture.closeOnAcceptance == false;
          assert campaignHome.config.services.tally.producers.campaign-fixture.closeOnPass == false;
          assert campaignHome.config.services.tally.producers.campaign-fixture.neverMutate == false;
          # One drainer, not two. tally-drain.timer already claimed the whole
          # events directory at this cadence on every tally home, and the drain
          # RPC drains the directory whoever calls it, so the campaign
          # continuation producer stays a registry entry and renders no unit:
          # two timers at the same cadence raced for the right to stamp the
          # durable admission origin of every event dropped in that directory.
          assert homeTimers.tally-drain.Timer.OnUnitActiveSec == "5s";
          assert campaignHomeTimers.tally-drain.Timer.OnUnitActiveSec == "5s";
          assert campaignHome.config.services.tally.producers.campaign-continuation.selfDrain == false;
          assert !(campaignHomeTimers ? tally-producer-campaign-continuation);
          assert !(campaignHomeServices ? tally-producer-campaign-continuation);
          # An ordinary events-dir producer keeps its own unit pair.
          assert homeTimers ? tally-producer-drop;
          assert homeServices ? tally-producer-drop;
          # The continue node's write into that directory is a declared write
          # dependency of the driver adapter, so hardening the adapter cannot
          # silently break a campaign's self-continuation.
          assert
            campaignHome.config.services.tally.adapters.spec-build-driver.extraWritablePaths == [
              "/tmp/tally-campaign-home/.local/state/tally/events"
              "/tmp/tally-campaign-home/.local/state/tally/capture/archive"
            ];
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
          assert pkgs.lib.hasInfix
            "gc --horizon 30d --collect --data-dir /var/lib/tally/data --state-dir /var/lib/tally/state"
            (systemServiceExec "tally-retention");
          assert pkgs.lib.hasInfix
            "--capture-archive-horizon 30d --events-done-horizon 180d --events-rejected-horizon 30d --events-rejected-max-count 10000 --producer-marker-horizon 180d"
            (systemServiceExec "tally-retention");
          assert
            systemServices.tally-retention.serviceConfig.ReadWritePaths == [
              "/var/lib/tally/data"
              "/var/lib/tally/state"
            ];
          assert systemServices.tally-retention.serviceConfig.User == "tally";
          assert builtins.elem
            "services.tally.producers must be empty in the NixOS module; configure producers with the Home Manager module (tally.homeManagerModules.tally)"
            unsupportedSystemMessages;
          assert builtins.elem
            "services.tally.flows must be empty in the NixOS module; configure flows with the Home Manager module (tally.homeManagerModules.tally)"
            unsupportedSystemMessages;
          assert builtins.elem
            "services.tally.campaigns must be empty in the NixOS module: a declared campaign is driven by a managed GitHub producer unit and only the Home Manager module renders producer units. Set services.tally.campaignForge.enable = true and arm forge-native campaigns with `tally campaign arm`, or configure declared campaigns with the Home Manager module (tally.homeManagerModules.tally)"
            unsupportedSystemMessages;
          # The campaign execution surface on NixOS. A stock host renders none
          # of it: the switch is off, so the module deploys the daemon exactly
          # as it did before the option existed.
          assert stockNixos.config.services.tally.campaignForge.enable == false;
          assert stockNixos.config.services.tally.producers == { };
          assert !(stockNixos.config.services.tally.pools ? campaign);
          assert !(stockNixos.config.services.tally.pools ? campaign-agent);
          assert !(stockNixos.config.services.tally.pools ? campaign-control);
          assert !(stockNixos.config.services.tally.pools ? flow);
          assert !(stockNixos.config.services.tally.adapters ? spec-build-driver);
          assert !(systemServices ? tally-campaign-poll);
          assert !(systemTimers ? tally-campaign-poll);
          assert stockNixos.config.users.users.tally.home == "/var/empty";
          # With the switch on, the host renders exactly what `tally campaign
          # arm` validates a host against: the four pools, the driver adapter,
          # and a fanout cap that admits a worst-case pass.
          assert campaignSystemConfig.pools.campaign.resource == "mutex";
          assert campaignSystemConfig.pools.campaign.capacity == 1;
          assert campaignSystemConfig.pools.campaign-agent.resource == "slot";
          assert campaignSystemConfig.pools.campaign-control.resource == "cpu-slot";
          assert campaignSystemConfig.pools.flow.resource == "cpu-slot";
          assert campaignSystemConfig.enqueue.fanoutCap == 64;
          # The events directory the continuation payload is written into, plus
          # the home the driver's own forge identity lives in: a hardened
          # driver adapter has to keep both writable on a system host, because
          # `gh` rewrites its own configuration file on first use.
          assert
            campaignSystemConfig.adapters.spec-build-driver.extraWritablePaths == [
              "/var/lib/tally/forge"
              "/var/lib/tally/state/events"
              "/var/lib/tally/state/capture/archive"
            ];
          assert
            campaignSystemConfig.adapters.spec-build-driver.scrape.finalMessage.pattern
            == "^TALLY_FINAL_MESSAGE=(.*)$";
          # One registry entry, no producer unit: this module renders no
          # producer units at all, and tally-drain.timer already drains that
          # directory at the same cadence the entry declares.
          assert campaignSystemConfig.producers.campaign-continuation.kind == "events-dir";
          assert campaignSystemConfig.producers.campaign-continuation.selfDrain == false;
          assert !(campaignSystemServices ? tally-producer-campaign-continuation);
          assert !(campaignSystemTimers ? tally-producer-campaign-continuation);
          assert campaignSystemTimers.tally-drain.timerConfig.OnUnitActiveSec == "5s";
          # The continuation payload's directory exists before the first job
          # that names it starts, on this module too.
          assert pkgs.lib.hasInfix "/var/lib/tally/state/events" systemDaemon.serviceConfig.ExecStartPre;
          assert pkgs.lib.hasInfix "/var/lib/tally/state/capture/archive"
            systemDaemon.serviceConfig.ExecStartPre;
          assert pkgs.lib.hasInfix "/var/lib/tally/state/events"
            stockNixos.config.system.activationScripts.tallyRuntimeDirectories.text;
          assert pkgs.lib.hasInfix "/var/lib/tally/state/capture/archive"
            stockNixos.config.system.activationScripts.tallyRuntimeDirectories.text;
          # The poll units, with system-service paths.
          assert campaignSystemServices.tally-campaign-poll.serviceConfig.User == "tally";
          assert campaignSystemServices.tally-campaign-poll.serviceConfig.TimeoutStartSec == "2min";
          assert
            campaignSystemServices.tally-campaign-poll.serviceConfig.ReadWritePaths == [
              "/var/lib/tally/state"
              "/var/lib/tally/forge"
            ];
          assert
            campaignSystemServices.tally-campaign-poll.serviceConfig.Environment == [
              "HOME=/var/lib/tally/forge"
            ];
          assert campaignSystemTimers.tally-campaign-poll.timerConfig.OnUnitActiveSec == "4min";
          assert campaignSystemTimers.tally-campaign-poll.timerConfig.Unit == "tally-campaign-poll.service";
          assert !(campaignPollDisabledNixos.config.systemd.services ? tally-campaign-poll);
          assert !(campaignPollDisabledNixos.config.systemd.timers ? tally-campaign-poll);
          # An `After=` on a target nothing pulls in orders against nothing, and
          # the scan's first act is an authenticated forge read 15s after boot.
          assert
            campaignSystemServices.tally-campaign-poll.after == [
              "network-online.target"
              "tally-daemon.service"
            ];
          assert campaignSystemServices.tally-campaign-poll.wants == [ "network-online.target" ];
          # A host whose secret is not provisioned yet skips the scan instead of
          # failing a unit every tick.
          assert
            campaignSystemServices.tally-campaign-poll.unitConfig.ConditionPathExists == [
              "/etc/tally/config.json"
              "/var/lib/tally/forge/.config/gh/hosts.yml"
            ];
          # The identity. The account gets a real home because campaign jobs are
          # transient units in its own user manager and read gh and git
          # configuration from HOME; the token is piped in from its declared
          # path and appears nowhere in the store.
          assert campaignNixos.config.users.users.tally.home == "/var/lib/tally/forge";
          assert campaignNixos.config.users.users.tally.createHome;
          assert pkgs.lib.hasInfix "--login tally-fixture" campaignSystemActivation;
          assert pkgs.lib.hasInfix "--git-name tally-fixture" campaignSystemActivation;
          assert pkgs.lib.hasInfix "--git-email tally-fixture@users.noreply.github.com"
            campaignSystemActivation;
          assert pkgs.lib.hasInfix "< /run/secrets/tally-campaign-forge-token" campaignSystemActivation;
          # A missing secret is named by the option it comes from, rather than
          # arriving as a bare shell redirection error.
          assert pkgs.lib.hasInfix "if [ ! -r /run/secrets/tally-campaign-forge-token ]"
            campaignSystemActivation;
          assert pkgs.lib.hasInfix "services.tally.campaignForge.tokenFile is unreadable"
            campaignSystemActivation;
          # No estate here runs sops-nix or agenix, so the ordering list is the
          # account record alone; naming a snippet that does not exist is an
          # evaluation error, which is why the dependency is conditional.
          assert
            campaignNixos.config.system.activationScripts.tallyCampaignForgeIdentity.deps == [
              "users"
            ];
          # Turning the surface off removes the token this writer left behind,
          # and a host that never enabled it still runs the same no-op snippet.
          assert !(campaignNixos.config.system.activationScripts ? tallyCampaignForgeTeardown);
          assert !(stockNixos.config.system.activationScripts ? tallyCampaignForgeIdentity);
          assert pkgs.lib.hasInfix "--remove --home /var/lib/tally/forge"
            stockNixos.config.system.activationScripts.tallyCampaignForgeTeardown.text;
          assert builtins.elem
            "services.tally.campaignForge.login must name the GitHub account the tally system service acts as; unlike the Home Manager module there is no ambient operator identity to inherit"
            anonymousCampaignMessages;
          assert builtins.elem
            "services.tally.campaignForge.tokenFile must be the absolute path of a file holding that account's GitHub token; it is read at activation and never enters the Nix store"
            anonymousCampaignMessages;
          assert builtins.elem "services.tally.campaignForge.enable requires services.tally.enable"
            anonymousCampaignMessages;
          assert builtins.elem
            "services.tally.campaignForge.login must be a GitHub login: alphanumerics and interior hyphens, at most 39 characters"
            badLoginCampaignMessages;
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
          assert builtins.elem
            "services.tally.flows.bad-budget.budgetPool has been removed: flows are excluded from windowed-consumption admission by design; use node priorities for contention, or workloadMutex for a process-scoped capacity-1 runner mutex"
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
          # The periodic spelling, which is the only one that absorbs an
          # absent socket (#411) or an expired `queue.drain` deadline (#427).
          # The NixOS side is pinned above; this is the user-unit half, and it
          # is the half whose failures the fleet's per-user-unit watcher
          # reports. `queue drain` here would leave both absorptions inert.
          assert pkgs.lib.hasInfix "daemon drain" (homeServiceExec "tally-drain");
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
          # #416: the module exports the data directory it configures beside,
          # so a direct-file verb resolving its default through TALLY_DATA_DIR
          # aims at the deployment's store.
          assert builtins.elem "TALLY_DATA_DIR=/tmp/tally-stock-home/.local/share/tally"
            homeServices.tally-daemon.Service.Environment;
          assert builtins.elem "TALLY_DATA_DIR=/tmp/tally-stock-home/.local/share/tally"
            homeServices."tally-witness-emit@".Service.Environment;
          assert builtins.elem "TALLY_DATA_DIR=/tmp/tally-stock-home/.local/share/tally"
            homeServices.tally-retention.Service.Environment;
          # And, on the Home Manager module, into the operator's own session
          # too: this module's direct-file verbs run from the operator's
          # shell, and the units' environments never reach it.
          assert
            stockHome.config.home.sessionVariables.TALLY_DATA_DIR == "/tmp/tally-stock-home/.local/share/tally";
          assert builtins.elem tallyWitnessEmit stockHome.config.home.packages;
          assert builtins.elem tallyWitnessEmit stockNixos.config.environment.systemPackages;
          assert builtins.elem "TALLY_ATTESTATION_LEDGER=/var/lib/tally/data/attestations.jsonl"
            systemWitnessEmitter.serviceConfig.Environment;
          assert builtins.elem "TALLY_DATA_DIR=/var/lib/tally/data" systemDaemon.serviceConfig.Environment;
          assert builtins.elem "TALLY_DATA_DIR=/var/lib/tally/data"
            systemWitnessEmitter.serviceConfig.Environment;
          assert builtins.elem "TALLY_DATA_DIR=/var/lib/tally/data"
            systemServices.tally-retention.serviceConfig.Environment;
          # And into the login environment, the same reason as the Home
          # Manager module's session export: the units are handed
          # `--data-dir` anyway, and the operator's own shell is where an
          # omitted one used to resolve to the wrong store.
          assert stockNixos.config.environment.variables.TALLY_DATA_DIR == "/var/lib/tally/data";
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
          # The daemon's watchdog keepalive derives every liveness budget from
          # this period, and pins those derivations at 30s in daemon::notify.
          assert systemDaemon.serviceConfig.WatchdogSec == "30s";
          assert homeServices.tally-daemon.Service.WatchdogSec == "30s";
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
            grep -F -- '--data-dir' ${homeServiceExec "tally-producer-daily"}
            grep -F -- '${toString stockHome.config.services.tally.dataDir}' ${homeServiceExec "tally-producer-daily"}
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
            jq -e --arg tally ${tally}/bin/tally '
              .maxFrameBytes == 33554432 and
              .agingThresholdSec == 900 and
              .enqueue.depthCap == 3 and
              .enqueue.fanoutCap == 64 and
              .lease.yieldGraceSec == 20 and
              .retention == {"enable":true,"horizon":"30d","onCalendar":"daily","captureArchiveHorizon":"30d","eventsDoneHorizon":"180d","eventsRejectedHorizon":"30d","eventsRejectedMaxCount":10000,"producerMarkerHorizon":"180d","lifecycleHorizon":"30d","lifecycleMaxBytes":268435456} and
              .storage == {"pollIntervalSec":60,"dataDir":{"warningBytes":34359738368,"hardBytes":68719476736,"warningFreeBytes":17179869184,"minimumFreeBytes":8589934592},"stateDir":{"warningBytes":34359738368,"hardBytes":68719476736,"warningFreeBytes":17179869184,"minimumFreeBytes":8589934592}} and
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
              .pools["codex-window"].resource == "slot" and
              .pools["codex-window"].capacity == 16 and
              .pools["claude-window"].resource == "slot" and
              .pools["claude-window"].capacity == 8 and
              .producers["flow-fixture"].kind == "calendar" and
              .producers["flow-fixture"].onCalendar == "daily" and
              .producers["flow-fixture"].enqueue.argv[0:3] == [$tally, "flow", "run"] and
              .producers["flow-fixture"].enqueue.argv[4:7] == ["--args-from-brief", "--max-nodes", "1000"] and
              .producers["flow-fixture"].enqueue.argv[7] == "--catalog" and
              .producers["flow-fixture"].enqueue.brief == {"task":"ship"} and
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
              .producers.github.postEvidence == false and
              .producers.github.postFailureEvidence == false and
              .producers.github.postFailureStderr == false and
              .producers.github.postGateSummary == true and
              .producers.github.requestReview == true and
              .producers.github.reviewers == ["tally-reviewer"] and
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
              .adapters.production.hardening == "production" and
              .adapters.production.extraWritablePaths == ["/var/lib/tally-stock/agent-state"] and
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
          final-conformance-bar = finalConformanceBar;
          doc = documentation;
          agency-nightly-driver = agencyNightlyDriver;
          spec-build-driver = specBuildDriver;
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
          final-conformance-bar = flake-utils.lib.mkApp { drv = finalConformanceBar; };
        };
        checks = {
          inherit tally;
          rustfmt = rustfmtCheck;
          clippy = clippyCheck;
          nixfmt-check = nixfmtCheck;
          doc = documentation;
          hardening-doc-drift = hardeningDocDrift;
          stock-home-activation = stockHome.activationPackage;
          module-layer = moduleContract;
          final-conformance-bar-harness = pkgs.runCommand "tally-final-conformance-bar-harness" { } ''
            ${finalConformanceBar}/bin/tally-final-conformance-bar --list > cases.txt
            grep -Fq 'campaign-full-pipeline' cases.txt
            grep -Fq 'parallel-population-wave' cases.txt
            grep -Fq 'usage-codex-cumulative-delta' cases.txt
            cp cases.txt "$out"
          '';
          spec-build-driver-tests =
            pkgs.runCommand "tally-spec-build-driver-tests"
              {
                nativeBuildInputs = [
                  pkgs.git
                  pkgs.python3
                ];
              }
              ''
                export HOME="$TMPDIR/home"
                mkdir -p "$HOME"
                export SPEC_BUILD_DRIVER_SOURCE=${campaignDrivers}/spec_build_driver.py
                export SPEC_BUILD_REDACTION_VECTORS=${./test/fixtures/redaction/vectors.json}
                python3 ${./test/spec_build_driver_test.py}
                touch "$out"
              '';
          campaign-render =
            pkgs.runCommand "tally-campaign-render"
              {
                activationPackage = campaignHome.activationPackage;
                checkedConfig = checkedCampaignConfig;
                stockConfig = checkedHomeConfig;
                pollScript = campaignPollScript;
                systemPollScript = campaignSystemPollScript;
                nativeBuildInputs = [
                  pkgs.git
                  pkgs.jq
                ];
              }
              ''
                trap 'echo "campaign-render: failed at line $LINENO: $BASH_COMMAND" >&2' ERR
                test -e "$activationPackage"
                ${tally}/bin/tally --mode check-config --config "$checkedConfig" >/dev/null
                # The poll holds the registry lock exclusively for its whole
                # run, so the timer must scan once and return rather than wait
                # out a campaign pass under the lock; --wait would block
                # interactive arm, disarm, and list for the pass duration.
                # Both modules render their own poll program, so both are held
                # to this; the system one is also held to the state directory
                # the module deploys, because an interactive arm and this timer
                # disagreeing about the registry root is a campaign that
                # registers and then never polls.
                for script in "$pollScript" "$systemPollScript"; do
                  test -x "$script"
                  grep -Fq -- "campaign poll --once" "$script"
                  # Poll reconciles indirect roots in exactly the same registry
                  # lifecycle as interactive arm, so both module wrappers must
                  # make nix-store available through their generated PATH.
                  grep -Fq -- "${pkgs.nix}/bin" "$script"
                  if grep -Fq -- "--wait" "$script"; then
                    echo "campaign-render: poll timer must not pass --wait: $script" >&2
                    exit 1
                  fi
                done
                grep -Fq -- "--config /etc/tally/config.json" "$systemPollScript"
                grep -Fq -- "--socket /run/tally/tally.sock" "$systemPollScript"
                grep -Fq -- "--state-dir /var/lib/tally/state" "$systemPollScript"
                jq -e '
                  .producers["campaign-fixture"].enqueue.brief as $fixtureArgs |
                  .producers["campaign-defaulted"].enqueue.brief as $defaultedArgs |
                  .producers["campaign-split"].enqueue.brief as $splitArgs |
                  .enqueue.fanoutCap == 64 and
                  .pools["fixture-campaign"].resource == "mutex" and
                  .pools["fixture-campaign"].capacity == 1 and
                  .pools["campaign-control"].resource == "cpu-slot" and
                  .pools["campaign-control"].capacity == 4 and
                  .pools["campaign-agent"].resource == "slot" and
                  .pools["campaign-agent"].capacity == 4 and
                  .adapters["spec-build-driver"].scrape.finalMessage.mode == "regex" and
                  .adapters["spec-build-driver"].scrape.finalMessage.pattern == "^TALLY_FINAL_MESSAGE=(.*)$" and
                  ([.adapters.codex.launch.approvalPolicies[][0]] | unique) == ["-c"] and
                  .adapters.codex.launch.approvalPolicies.never == ["-c", "approval_policy=\"never\""] and
                  ([.adapters.codex.launch.approvalPolicies[][]] | any(. == "--ask-for-approval") | not) and
                  .adapters.codex.launch.commitCapableSandboxPolicies == ["danger-full-access", "dangerously-bypass"] and
                  .flows.fixture.workloadMutex == "fixture-campaign" and
                  (.flows.fixture.script | endswith("spec-build.js")) and
                  $fixtureArgs.tally == "${tally}/bin/tally" and
                  $fixtureArgs.agent.approvalPolicy == null and
                  $fixtureArgs.agent.sandboxPolicy == null and
                  $fixtureArgs.agent.diagnosisSandboxPolicy == null and
                  $fixtureArgs.gates[0].kind == "command" and
                  $fixtureArgs.gates[0].preflightArgv ==
                    ["/bin/sh", "-eu", "-c", "test -x /bin/sh; test ! -d build"] and
                  $fixtureArgs.gates[0].runtimeMaxSec == 900 and
                  ($fixtureArgs.gates[0] | has("forbidPaths") | not) and
                  $fixtureArgs.gates[1].kind == "forbidPaths" and
                  $fixtureArgs.gates[1].forbidPaths == ["*.db", "*.db-wal", "*.db-shm", "*.sqlite*"] and
                  ($fixtureArgs.gates[1] | has("preflightArgv") | not) and
                  ($fixtureArgs.gates[1] | has("argv") | not) and
                  $fixtureArgs.gates[1].runtimeMaxSec == 11 and
                  $fixtureArgs.mergeMethod == "merge" and
                  $fixtureArgs.gitAiBinding == "advisory" and
                  $fixtureArgs.gitAiAwaitSec == 12 and
                  ($fixtureArgs.driverRuntimeMaxSec >= 2 * $fixtureArgs.gitAiAwaitSec) and
                  ($fixtureArgs.captureRoot | endswith("/capture/archive")) and
                  $fixtureArgs.postFailureEvidence == true and
                  $fixtureArgs.postFailureStderr == true and
                  $fixtureArgs.steward.adapter == "narrator" and
                  $fixtureArgs.steward.argv == [
                    "/bin/sh", "/srv/campaign-fixtures/narrate", "--narrate"
                  ] and
                  $fixtureArgs.steward.runtimeMaxSec == 45 and
                  $fixtureArgs.steward.env
                    == {"NARRATOR_ENDPOINT": "https://narrator.invalid/v1"} and
                  ($fixtureArgs.steward.env == .adapters.narrator.env) and
                  ($fixtureArgs.steward.finalMessagePattern
                    == .adapters.narrator.scrape.finalMessage.pattern) and
                  $fixtureArgs.steward.finalMessagePattern == "^NARRATOR_RESULT=(.*)$" and
                  .adapters["spec-build-driver"].scrape.finalMessage.pattern
                    == "^TALLY_FINAL_MESSAGE=(.*)$" and
                  # The seam is rendered only where it is configured. A role
                  # left null is absent from the args, so a single-repository
                  # campaign carries the same brief it always did.
                  $splitArgs.codeRepository == "acme/code" and
                  $splitArgs.specRepository == "acme/spec" and
                  ($splitArgs | has("issueRepository") | not) and
                  ($splitArgs.repositories | keys) == ["acme/code", "acme/spec"] and
                  ($splitArgs.repositories["acme/code"].checkout
                    != $splitArgs.repositories["acme/spec"].checkout) and
                  $splitArgs.worklist == "specs/001-toy/tasks.json" and
                  $splitArgs.maxTasks == 7 and
                  ($defaultedArgs | has("codeRepository") | not) and
                  ($defaultedArgs | has("specRepository") | not) and
                  ($defaultedArgs | has("issueRepository") | not) and
                  ($fixtureArgs | has("specRepository") | not) and
                  $defaultedArgs.mergeMethod == "squash" and
                  $defaultedArgs.gitAiBinding == "off" and
                  $defaultedArgs.gitAiAwaitSec == 60 and
                  $defaultedArgs.postFailureEvidence == false and
                  $defaultedArgs.postFailureStderr == false and
                  $defaultedArgs.steward == null and
                  $defaultedArgs.agent.adapter == "codex" and
                  $defaultedArgs.agent.approvalPolicy == "never" and
                  $defaultedArgs.agent.sandboxPolicy == "danger-full-access" and
                  $defaultedArgs.agent.diagnosisSandboxPolicy == "read-only" and
                  .producers["campaign-fixture"].kind == "gh" and
                  .producers["campaign-fixture"].enable == true and
                  .producers["campaign-fixture"].sources[0].search.repositories == ["acme/spec"] and
                  .producers["campaign-fixture"].sources[0].search.labels == ["spec-campaign"] and
                  .producers["campaign-fixture"].sources[0].search.state == "open" and
                  .producers["campaign-fixture"].sources[0].search.kinds == ["issue"] and
                  .producers["campaign-fixture"].triggers.mentions == ["@operator build"] and
                  .producers["campaign-fixture"].allowSelfTriggered == true and
                  .producers["campaign-fixture"].allowedActors == ["operator"] and
                  .producers["campaign-fixture"].pollIntervalSec == 17 and
                  .producers["campaign-fixture"].postReceipt == true and
                  .producers["campaign-fixture"].postEvidence == true and
                  .producers["campaign-fixture"].postFailureEvidence == true and
                  .producers["campaign-fixture"].postFailureStderr == true and
                  .producers["campaign-fixture"].postGateSummary == false and
                  .producers["campaign-fixture"].closeOnAcceptance == false and
                  .producers["campaign-fixture"].closeOnPass == false and
                  .producers["campaign-fixture"].enqueue.pool == ["fixture-campaign", "flow"] and
                  .producers["campaign-fixture"].enqueue.adapter == "shell" and
                  .producers["campaign-fixture"].enqueue.evidence == ["exit:0"] and
                  .producers["campaign-fixture"].enqueue.noEnqueue == false and
                  .producers["campaign-fixture"].enqueue.argv[0:3] == [
                    "${tally}/bin/tally", "flow", "run"
                  ] and
                  .producers["campaign-fixture"].enqueue.argv[4:7] == ["--args-from-brief", "--max-nodes", "55"] and
                  ([.producers | keys[] | select(test("reconcile"))] == []) and
                  ([.producers | keys[] | select(startswith("campaign-"))]
                    == [
                      "campaign-continuation",
                      "campaign-defaulted",
                      "campaign-fixture",
                      "campaign-split"
                    ]) and
                  .producers["campaign-continuation"].kind == "events-dir" and
                  .producers["campaign-continuation"].pollIntervalSec == 5 and
                  ($fixtureArgs.continuation.argv
                    == .producers["campaign-fixture"].enqueue.argv) and
                  $fixtureArgs.continuation.pool == ["flow", "fixture-campaign"] and
                  $fixtureArgs.continuation.priority == "low" and
                  ($fixtureArgs.continuation.eventsDir | endswith("/events")) and
                  ($fixtureArgs | has("reconcileCommand") | not) and
                  .producers["campaign-defaulted"].allowSelfTriggered == false
                  and .producers["campaign-defaulted"].postFailureEvidence == false
                  and .producers["campaign-defaulted"].postFailureStderr == false
                ' "$checkedConfig" >/dev/null

                # The generic drain is installed once and unconditionally, so a
                # host with only the poll surface and no declared campaign
                # still renders it: arming needs no Nix change.
                jq -e '
                  ([.producers | keys[] | select(startswith("campaign-"))]
                    == ["campaign-continuation"]) and
                  .producers["campaign-continuation"].kind == "events-dir"
                ' "$stockConfig" >/dev/null

                cp -R ${./test/fixtures/spec-build/repo} "$TMPDIR/spec"
                chmod -R u+w "$TMPDIR/spec"
                git -C "$TMPDIR/spec" init --quiet --initial-branch=main
                git -C "$TMPDIR/spec" config user.name "Tally Fixture"
                git -C "$TMPDIR/spec" config user.email "tally-fixture@invalid"
                printf '%s\n' legacy > "$TMPDIR/spec/legacy.db"
                printf '%s\n' source > "$TMPDIR/spec/rename-source"
                git -C "$TMPDIR/spec" add --all
                git -C "$TMPDIR/spec" commit --quiet -m "fixture: frozen spec"
                git init --bare --quiet --initial-branch=main "$TMPDIR/spec-source-remote.git"
                git -C "$TMPDIR/spec" remote add origin "$TMPDIR/spec-source-remote.git"
                git -C "$TMPDIR/spec" push --quiet --set-upstream origin main
                jq -n --arg checkout "$TMPDIR/spec" '{
                  repository: "acme/spec",
                  repositoryConfig: {
                    checkout: $checkout,
                    baseBranch: "main",
                    remote: "origin",
                    forge: "local"
                  },
                  worklist: "specs/*/tasks.json",
                  maxTasks: 7,
                  maxParallel: 3
                }' > "$TMPDIR/worklist-brief.json"
                export TALLY_BRIEF="$TMPDIR/worklist-brief.json"
                ${specBuildDriver}/bin/spec-build-driver worklist \
                  | sed 's/^TALLY_FINAL_MESSAGE=//' > worklist.json
                jq -e '
                  .schemaVersion == 1 and
                  .repository == "acme/spec" and
                  .source.path == "specs/001-toy/tasks.json" and
                  (.source.sha256 | test("^sha256:[0-9a-f]{64}$")) and
                  (.source.revision | test("^[0-9a-f]{40,64}$")) and
                  [.tasks[].id] == ["task-1", "phase-one-checkpoint", "task-2", "task-3", "task-4", "task-5", "task-6"] and
                  all(.tasks[] | select(.kind == "implementation");
                    (.goal | length) > 0 and
                    (.deliveredBehaviors | length) > 0 and
                    (.readFirst.specSections | length) > 0 and
                    (.conflictDomains | length) > 0 and
                    (.acceptanceCriteria | length) > 0 and
                    all(.acceptanceCriteria[]; (.argv | length) > 0)
                  ) and
                  .tasks[1].id == "phase-one-checkpoint" and
                  .tasks[1].kind == "checkpoint" and
                  .tasks[1].title == "Validate the accumulated first phase" and
                  .tasks[1].argv[0:3] == ["sh", "-eu", "-c"] and
                  (.tasks[1].argv[3] |
                    contains("checkpoint-red") and contains("TALLY_BRIEF")) and
                  .tasks[1].runtimeMaxSec == 10 and
                  .tasks[1].dependencies == ["task-1"] and
                  .tasks[2].dependencies == ["phase-one-checkpoint"]
                ' worklist.json >/dev/null

                worklistPath="$TMPDIR/spec/specs/001-toy/tasks.json"
                publish_worklist_change() {
                  git -C "$TMPDIR/spec" add "$worklistPath"
                  git -C "$TMPDIR/spec" commit --quiet -m "$1"
                  git -C "$TMPDIR/spec" push --quiet origin main
                }
                restore_worklist() {
                  cp ${./test/fixtures/spec-build/repo/specs/001-toy/tasks.json} \
                    "$worklistPath"
                  publish_worklist_change "$1"
                }
                jq 'del(.tasks[0].kind)' "$worklistPath" \
                  > "$TMPDIR/missing-task-kind.json"
                mv "$TMPDIR/missing-task-kind.json" "$worklistPath"
                publish_worklist_change "fixture: remove task kind"
                if ${specBuildDriver}/bin/spec-build-driver worklist \
                  > /dev/null 2> "$TMPDIR/missing-task-kind.err"; then
                  echo "worklist implementation without a kind unexpectedly passed" >&2
                  exit 1
                fi
                grep -F 'tasks[0].kind must equal implementation or checkpoint' \
                  "$TMPDIR/missing-task-kind.err" >/dev/null
                restore_worklist "fixture: restore task kind"

                jq '.tasks[1].goal = "checkpoints do not implement"' "$worklistPath" \
                  > "$TMPDIR/checkpoint-with-agent-field.json"
                mv "$TMPDIR/checkpoint-with-agent-field.json" "$worklistPath"
                publish_worklist_change "fixture: add invalid checkpoint field"
                if ${specBuildDriver}/bin/spec-build-driver worklist \
                  > /dev/null 2> "$TMPDIR/checkpoint-with-agent-field.err"; then
                  echo "checkpoint with an implementation field unexpectedly passed" >&2
                  exit 1
                fi
                grep -F 'tasks[1] has unknown fields: goal' \
                  "$TMPDIR/checkpoint-with-agent-field.err" >/dev/null
                restore_worklist "fixture: restore checkpoint shape"

                jq 'del(.tasks[0].conflictDomains)' "$worklistPath" \
                  > "$TMPDIR/missing-conflict-domains.json"
                mv "$TMPDIR/missing-conflict-domains.json" "$worklistPath"
                publish_worklist_change "fixture: remove conflict domains"
                if ${specBuildDriver}/bin/spec-build-driver worklist \
                  > /dev/null 2> "$TMPDIR/missing-conflict-domains.err"; then
                  echo "parallel worklist without conflictDomains unexpectedly passed" >&2
                  exit 1
                fi
                grep -F 'tasks[0].conflictDomains must be a non-empty array' \
                  "$TMPDIR/missing-conflict-domains.err"
                restore_worklist "fixture: restore conflict domains"

                base_rev="$(git -C "$TMPDIR/spec" rev-parse HEAD)"
                write_constraint_brief() {
                  jq -n \
                    --arg base "$base_rev" \
                    --arg branch "$1" \
                    --argjson patterns "$2" \
                    --arg worktree "$TMPDIR/spec" \
                    '{
                      gate: {
                        kind: "forbidPaths",
                        id: "no-db-artifacts",
                        forbidPaths: $patterns,
                        runtimeMaxSec: 11
                      },
                      repositoryConfig: {
                        checkout: $worktree,
                        baseBranch: "main",
                        remote: "origin",
                        forge: "local"
                      },
                      workspace: {
                        taskId: "task-1",
                        baseRev: $base,
                        branch: $branch,
                        worktreePath: $worktree
                      }
                    }' > "$TMPDIR/constraint-brief.json"
                }

                mkdir -p "$TMPDIR/spec/build/nested"
                printf '%s\n' transient > "$TMPDIR/spec/build/nested/transient.db"
                git -C "$TMPDIR/spec" add build/nested/transient.db
                git -C "$TMPDIR/spec" commit --quiet -m "fixture: add forbidden artifact"
                write_constraint_brief main '["*.db", "*.db-wal", "*.db-shm", "*.sqlite*"]'
                export TALLY_BRIEF="$TMPDIR/constraint-brief.json"
                if ${specBuildDriver}/bin/spec-build-driver constraint \
                  > "$TMPDIR/constraint-fail.out" 2> "$TMPDIR/constraint-fail.err"; then
                  echo "forbidPaths constraint accepted a committed .db path" >&2
                  exit 1
                fi
                grep -F 'forbidPaths gate' "$TMPDIR/constraint-fail.err" >/dev/null
                grep -F 'build/nested/transient.db' "$TMPDIR/constraint-fail.err" >/dev/null
                git -C "$TMPDIR/spec" rm --quiet build/nested/transient.db
                git -C "$TMPDIR/spec" commit --quiet -m "fixture: remove forbidden artifact"
                if ${specBuildDriver}/bin/spec-build-driver constraint \
                  > "$TMPDIR/constraint-history-fail.out" \
                  2> "$TMPDIR/constraint-history-fail.err"; then
                  echo "forbidPaths constraint forgot an artifact deleted by a later commit" >&2
                  exit 1
                fi
                grep -F 'build/nested/transient.db' \
                  "$TMPDIR/constraint-history-fail.err" >/dev/null

                git -C "$TMPDIR/spec" switch --detach "$base_rev" >/dev/null
                git -C "$TMPDIR/spec" switch -c deletion-only >/dev/null
                git -C "$TMPDIR/spec" rm --quiet legacy.db
                printf '%s\n' allowed > "$TMPDIR/spec/allowed.txt"
                git -C "$TMPDIR/spec" add allowed.txt
                git -C "$TMPDIR/spec" commit --quiet -m "fixture: delete legacy artifact"
                deletion_head="$(git -C "$TMPDIR/spec" rev-parse HEAD)"
                write_constraint_brief deletion-only \
                  '["*.db", "*.db-wal", "*.db-shm", "*.sqlite*"]'
                ${specBuildDriver}/bin/spec-build-driver constraint \
                  | sed 's/^TALLY_FINAL_MESSAGE=//' > "$TMPDIR/constraint-pass.json"
                jq -e --arg base "$base_rev" --arg head "$deletion_head" '
                  .gateId == "no-db-artifacts" and
                  .kind == "forbidPaths" and
                  .patterns == ["*.db", "*.db-wal", "*.db-shm", "*.sqlite*"] and
                  .checkedPaths == 1 and
                  .baseRev == $base and
                  .head == $head
                ' "$TMPDIR/constraint-pass.json" >/dev/null

                git -C "$TMPDIR/spec" switch --detach "$base_rev" >/dev/null
                git -C "$TMPDIR/spec" switch -c case-and-double-star >/dev/null
                mkdir -p "$TMPDIR/spec/src/deep"
                printf '%s\n' direct > "$TMPDIR/spec/src/App.SQLite"
                printf '%s\n' nested > "$TMPDIR/spec/src/deep/App.SQLite"
                git -C "$TMPDIR/spec" add src
                git -C "$TMPDIR/spec" commit --quiet -m "fixture: add mixed-case sqlite paths"
                write_constraint_brief case-and-double-star '["src/**/*.sqlite*"]'
                if ${specBuildDriver}/bin/spec-build-driver constraint \
                  > "$TMPDIR/constraint-case-fail.out" 2> "$TMPDIR/constraint-case-fail.err"; then
                  echo "forbidPaths constraint matched paths case-sensitively" >&2
                  exit 1
                fi
                grep -F 'src/App.SQLite' "$TMPDIR/constraint-case-fail.err" >/dev/null
                grep -F 'src/deep/App.SQLite' "$TMPDIR/constraint-case-fail.err" >/dev/null

                git -C "$TMPDIR/spec" switch --detach "$base_rev" >/dev/null
                git -C "$TMPDIR/spec" switch -c trailing-double-star >/dev/null
                printf '%s\n' tracked-file > "$TMPDIR/spec/tracked-root"
                git -C "$TMPDIR/spec" add tracked-root
                git -C "$TMPDIR/spec" commit --quiet -m "fixture: add path named tracked-root"
                write_constraint_brief trailing-double-star '["tracked-root/**"]'
                if ${specBuildDriver}/bin/spec-build-driver constraint \
                  > "$TMPDIR/constraint-trailing-fail.out" \
                  2> "$TMPDIR/constraint-trailing-fail.err"; then
                  echo "trailing ** did not span zero path components" >&2
                  exit 1
                fi
                grep -F '"tracked-root"' "$TMPDIR/constraint-trailing-fail.err" >/dev/null

                git -C "$TMPDIR/spec" switch --detach "$base_rev" >/dev/null
                git -C "$TMPDIR/spec" switch -c rename-case >/dev/null
                mkdir -p "$TMPDIR/spec/nested"
                git -C "$TMPDIR/spec" mv rename-source nested/RENAMED.DB
                git -C "$TMPDIR/spec" commit --quiet -m "fixture: rename to forbidden path"
                write_constraint_brief rename-case '["nested/*.db"]'
                if ${specBuildDriver}/bin/spec-build-driver constraint \
                  > "$TMPDIR/constraint-rename-fail.out" \
                  2> "$TMPDIR/constraint-rename-fail.err"; then
                  echo "forbidPaths constraint missed a renamed mixed-case path" >&2
                  exit 1
                fi
                grep -F 'nested/RENAMED.DB' "$TMPDIR/constraint-rename-fail.err" >/dev/null

                write_constraint_brief rename-case '["src/**.db"]'
                if ${specBuildDriver}/bin/spec-build-driver constraint \
                  > "$TMPDIR/constraint-pattern-fail.out" \
                  2> "$TMPDIR/constraint-pattern-fail.err"; then
                  echo "forbidPaths constraint accepted an ambiguous ** component" >&2
                  exit 1
                fi
                grep -F "may use '**' only as a complete path component" \
                  "$TMPDIR/constraint-pattern-fail.err" >/dev/null

                git -C "$TMPDIR/spec" switch --detach "$base_rev" >/dev/null
                git -C "$TMPDIR/spec" switch -c publication-stale >/dev/null
                mkdir -p "$TMPDIR/spec/build"
                printf '%s\n' allowed > "$TMPDIR/spec/build/allowed.txt"
                git -C "$TMPDIR/spec" add build/allowed.txt
                git -C "$TMPDIR/spec" commit --quiet -m "fixture: clean publication head"
                witnessed_head="$(git -C "$TMPDIR/spec" rev-parse HEAD)"
                write_constraint_brief publication-stale \
                  '["*.db", "*.db-wal", "*.db-shm", "*.sqlite*"]'
                ${specBuildDriver}/bin/spec-build-driver constraint \
                  | sed 's/^TALLY_FINAL_MESSAGE=//' > "$TMPDIR/publication-constraint.json"

                printf '%s\n' late > "$TMPDIR/spec/build/LATE.SQLite"
                git -C "$TMPDIR/spec" add build/LATE.SQLite
                git -C "$TMPDIR/spec" commit --quiet -m "fixture: mutate head after constraint"
                git init --bare --quiet --initial-branch=main "$TMPDIR/publication-remote.git"
                git -C "$TMPDIR/spec" remote set-url origin "$TMPDIR/publication-remote.git"
                git -C "$TMPDIR/spec" push --quiet origin \
                  "$base_rev:refs/heads/main"
                jq -n \
                  --arg base "$base_rev" \
                  --arg checkout "$TMPDIR/spec" \
                  --arg workspaceRoot "$TMPDIR/workspaces" \
                  --slurpfile constraints "$TMPDIR/publication-constraint.json" \
                  --slurpfile worklist worklist.json \
                  '{
                    campaign: "fixture",
                    repository: "acme/spec",
                    repositoryConfig: {
                      checkout: $checkout,
                      baseBranch: "main",
                      remote: "origin",
                      forge: "local"
                    },
                    issue: {
                      number: "7",
                      url: "https://github.com/acme/spec/issues/7"
                    },
                    runId: "stale-publication",
                    workspaceRoot: $workspaceRoot,
                    task: ($worklist[0].tasks[0] | .conflictDomains = [
                      "build/allowed.txt",
                      "build/LATE.SQLite"
                    ]),
                    domainsRequired: true,
                    gates: [{
                      kind: "forbidPaths",
                      id: "no-db-artifacts",
                      forbidPaths: ["*.db", "*.db-wal", "*.db-shm", "*.sqlite*"],
                      runtimeMaxSec: 11
                    }],
                    workspace: {
                      taskId: "task-1",
                      baseRev: $base,
                      branch: "publication-stale",
                      publishBranch: "tally/fixture-issue-7/task-1",
                      worktreePath: $checkout
                    },
                    constraints: $constraints
                  }' > "$TMPDIR/publication-brief.json"
                export TALLY_BRIEF="$TMPDIR/publication-brief.json"
                if ${specBuildDriver}/bin/spec-build-driver publish \
                  > "$TMPDIR/publication-stale.out" \
                  2> "$TMPDIR/publication-stale.err"; then
                  echo "publication reused a constraint receipt from a stale head" >&2
                  exit 1
                fi
                grep -F 'build/LATE.SQLite' "$TMPDIR/publication-stale.err" >/dev/null
                if git -C "$TMPDIR/spec" ls-remote --exit-code origin \
                  refs/heads/tally/fixture-issue-7/task-1 >/dev/null 2>&1; then
                  echo "stale constrained head reached the remote" >&2
                  exit 1
                fi

                git -C "$TMPDIR/spec" switch --detach "$witnessed_head" >/dev/null
                git -C "$TMPDIR/spec" branch --force publication-stale \
                  "$witnessed_head" >/dev/null
                git -C "$TMPDIR/spec" switch publication-stale >/dev/null
                ${specBuildDriver}/bin/spec-build-driver publish \
                  | sed 's/^TALLY_FINAL_MESSAGE=//' > "$TMPDIR/publication-pass.json"
                jq -e --arg head "$witnessed_head" '
                  .taskId == "task-1" and
                  .head == $head and
                  .branch == "tally/fixture-issue-7/task-1" and
                  .ownership.domainsRequired == true and
                  .ownership.conflictDomains == [
                    "build/allowed.txt",
                    "build/LATE.SQLite"
                  ] and
                  .ownership.ownedPaths == ["build/allowed.txt"] and
                  .ownership.head == $head
                ' "$TMPDIR/publication-pass.json" >/dev/null
                test "$(git -C "$TMPDIR/spec" ls-remote origin \
                  refs/heads/tally/fixture-issue-7/task-1 | cut -f1)" = \
                  "$witnessed_head"

                # `campaigns.defaulted` declares no mention, so its trigger
                # grammar is whatever the module ships. Reading that grammar
                # back out of the rendered config keeps the event honest under a
                # future default change and keeps this file from repeating a
                # mention literal, which is a live GitHub at-mention wherever it
                # is copied from.
                default_mention="$(jq -r \
                  '.producers["campaign-defaulted"].triggers.mentions[0]' \
                  "$checkedConfig")"
                # And the default itself is pinned, because #319 made it a
                # back-compat contract: it is deliberately left naming a real,
                # unrelated GitHub account so that campaigns relying on it do
                # not silently lose their trigger grammar, and a promise nothing
                # checks is not a promise. Retiring it is a release-boundary
                # decision, and this pin is what makes that decision explicit
                # rather than a diff nobody noticed.
                #
                # The pin is the digest and not the literal on purpose. #319's
                # first acceptance criterion is that grepping this file and
                # doc/ for the account that default names returns nothing, and
                # a pin that spelled it would fail that criterion in the act of
                # protecting it. The digest moves if and only if the default
                # does; the CHANGELOG carries the literal, which is outside
                # that grep's scope and is where a migrating operator looks.
                default_mention_sha256="$(printf '%s' "$default_mention" \
                  | sha256sum | cut -d' ' -f1)"
                if [ "$default_mention_sha256" != \
                  "bd13b18429fdb18e7b981e1a552dd2c97d375fc33557e59855616d4e6b956d2d" ]; then
                  echo "the shipped campaigns.<name>.mention default changed" >&2
                  echo "  rendered digest: $default_mention_sha256" >&2
                  echo "  if that change is intended, update this pin and give" >&2
                  echo "  the CHANGELOG the migration note the old default is" >&2
                  echo "  owed; if it is not, the module default regressed" >&2
                  exit 1
                fi
                default_event="$(printf '%s' \
                  '{"kind":"gh","source":"search","repo":"acme/spec","number":6,"htmlUrl":"https://github.com/acme/spec/issues/6","itemType":"issue","nodeId":"I-campaign-6","itemAuthor":"operator","triggerActor":"operator","selfActor":"operator","triggerKind":"mention","eventId":"comment-6","commentId":"comment-6","triggerTimestamp":"2026-07-31T08:59:00Z","context":{"schemaVersion":2,"title":"Build the frozen spec","body":"The work lives in the spec repository.","state":"open","labels":["campaign"],"assignees":[],"triggeringComment":{"id":"comment-6","author":"operator","body":"PLACEHOLDER"}}}' \
                  | jq -c --arg mention "$default_mention" \
                    '.context.triggeringComment.body = $mention')"
                mkdir -p "$TMPDIR/data"
                default_dispatch="$(${tally}/bin/tally --config "$checkedConfig" \
                  __producer-dispatch campaign-defaulted --state-dir "$TMPDIR/state" \
                  --data-dir "$TMPDIR/data" \
                  --event "$default_event")"
                test "$(printf '%s' "$default_dispatch" | jq -r '.filtered.reason')" = \
                  self-trigger-disabled

                event='{"kind":"gh","source":"search","repo":"acme/spec","number":7,"htmlUrl":"https://github.com/acme/spec/issues/7","itemType":"issue","nodeId":"I-campaign-7","itemAuthor":"operator","triggerActor":"operator","selfActor":"operator","triggerKind":"mention","eventId":"comment-7","commentId":"comment-7","triggerTimestamp":"2026-07-31T09:00:00Z","context":{"schemaVersion":2,"title":"Build the frozen spec","body":"The work lives in the spec repository.","state":"open","labels":["spec-campaign"],"assignees":[],"triggeringComment":{"id":"comment-7","author":"operator","body":"@operator build"}}}'
                dispatch="$(${tally}/bin/tally --config "$checkedConfig" \
                  __producer-dispatch campaign-fixture --state-dir "$TMPDIR/state" \
                  --data-dir "$TMPDIR/data" \
                  --event "$event")"
                payload="$(printf '%s' "$dispatch" | jq -r '.emitted')"
                runtime_args="$(jq -r '.briefPath' "$payload")"
                test -f "$runtime_args"
                test "$(dirname "$(dirname "$runtime_args")")" = "$TMPDIR/data"
                test ! -e "$TMPDIR/state/briefs"
                jq -e '
                  .campaign == "fixture" and
                  .repository == "acme/spec" and
                  .issue == {
                    "number": "7",
                    "url": "https://github.com/acme/spec/issues/7"
                  } and
                  .runId == "comment-7" and
                  .worklist == "specs/*/tasks.json" and
                  .maxTasks == 7 and
                  .maxParallel == 3 and
                  (.continuation.argv | index("--args-from-brief")) == 4 and
                  .continuation.argv[6] == "55" and
                  .continuation.pool == ["flow", "fixture-campaign"] and
                  .continuation.priority == "low" and
                  (.continuation.eventsDir | endswith("/events")) and
                  (has("reconcileCommand") | not) and
                  .tally == "${tally}/bin/tally" and
                  .repositories["acme/spec"].baseBranch == "main" and
                  .repositories["acme/spec"].forge == "github" and
                  .agent.adapter == "fixture-agent" and
                  .agent.argv == ["/bin/true"] and
                  .agent.approvalPolicy == null and
                  .agent.sandboxPolicy == null and
                  .agent.diagnosisSandboxPolicy == null and
                  [.gates[].id] == ["content", "no-db-artifacts"] and
                  .gates[0].kind == "command" and
                  .gates[0].preflightArgv ==
                    ["/bin/sh", "-eu", "-c", "test -x /bin/sh; test ! -d build"] and
                  .gates[0].argv == ["/bin/sh", "-eu", "-c", "test -d build"] and
                  .gates[0].runtimeMaxSec == 900 and
                  (.gates[0] | has("forbidPaths") | not) and
                  .gates[1].kind == "forbidPaths" and
                  .gates[1].forbidPaths == ["*.db", "*.db-wal", "*.db-shm", "*.sqlite*"] and
                  (.gates[1] | has("preflightArgv") | not) and
                  (.gates[1] | has("argv") | not) and
                  .gates[1].runtimeMaxSec == 11
                ' "$runtime_args" >/dev/null
                jq -e --arg args "$runtime_args" \
                  '.briefPath == $args and (has("brief") | not)' "$payload" >/dev/null
                ${tally}/bin/tally --config "$checkedConfig" flow check \
                  ${./examples/flows/spec-build.js} --args-path "$runtime_args" >/dev/null
                test "$(jq -r '.argv[3]' "$payload")" = \
                  "$(jq -r '.flows.fixture.script' "$checkedConfig")"

                # The self-nudge is local now: no per-campaign reconcile
                # producer exists to poll a public comment back, and the one
                # generic drain that replaced it is an events-dir producer.
                jq -e '
                  (.producers | has("campaign-fixture-reconcile") | not) and
                  (.producers | has("campaign-defaulted-reconcile") | not)
                ' "$checkedConfig" >/dev/null
                calendar_event='{"kind":"calendar"}'
                if ${tally}/bin/tally --config "$checkedConfig" \
                  __producer-dispatch campaign-continuation \
                  --state-dir "$TMPDIR/state" --data-dir "$TMPDIR/data" \
                  --event "$calendar_event" 2>"$TMPDIR/continuation-kind.err"; then
                  echo "campaign-render: campaign-continuation accepted a calendar observation" >&2
                  exit 1
                fi
                grep -Fq 'has kind "events-dir"' "$TMPDIR/continuation-kind.err"
                touch "$out"
              '';
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
                # Checking an example without --args never exercises its
                # argsSchema, which is most of what the example teaches. Every
                # example is checked bare and again against representative
                # arguments; the two that declare selectors also get the catalog.
                for example in \
                  ${./examples/flows/academic-ocr.js} \
                  ${./examples/flows/agency-nightly.js} \
                  ${./examples/flows/domain-failure.js} \
                  ${./examples/flows/fleet-deploy.js} \
                  ${./examples/flows/monthly-review.js} \
                  ${./examples/flows/pooled-review.js} \
                  ${./examples/flows/spec-build.js} \
                  ${./examples/flows/worklist-fanout.js}; do
                  ${tally}/bin/tally flow check "$example" >/dev/null
                done
                ${tally}/bin/tally flow check ${./examples/flows/academic-ocr.js} \
                  --args-path ${exampleArgs.academic-ocr} >/dev/null
                ${tally}/bin/tally flow check ${./examples/flows/domain-failure.js} \
                  --args-path ${exampleArgs.domain-failure} >/dev/null
                ${tally}/bin/tally flow check ${./examples/flows/fleet-deploy.js} \
                  --args-path ${exampleArgs.fleet-deploy} >/dev/null
                ${tally}/bin/tally flow check ${./examples/flows/monthly-review.js} \
                  --args-path ${exampleArgs.monthly-review} --catalog ${catalogFixture} >/dev/null
                ${tally}/bin/tally flow check ${./examples/flows/pooled-review.js} \
                  --args-path ${exampleArgs.pooled-review} --catalog ${catalogFixture} >/dev/null
                ${tally}/bin/tally flow check ${./examples/flows/worklist-fanout.js} \
                  --args-path ${exampleArgs.worklist-fanout} >/dev/null
                # The agency wave's worklist IS its arguments, so its
                # representative arguments are the checked-in documented wave
                # rather than an inline attrset, and that file has to satisfy
                # the flow's own argsSchema.
                agency_meta="$(${tally}/bin/tally flow check ${./examples/flows/agency-nightly.js} \
                  --args-path ${./examples/flows/agency-nightly.args.json})"
                test "$(printf '%s' "$agency_meta" | jq -r '.name')" = agency-nightly
                test "$(printf '%s' "$agency_meta" | jq -r '.maxNodes')" = 20
                test "$(printf '%s' "$agency_meta" | jq -r '.iterationCap')" = 8
                test "$(printf '%s' "$agency_meta" | jq -c '.pools')" = \
                  '["agency-control","codex-window","claude-window"]'
                touch "$out"
              '';
          agency-nightly-driver =
            pkgs.runCommand "tally-agency-nightly-driver"
              {
                nativeBuildInputs = [
                  pkgs.git
                  pkgs.python3
                ];
                AGENCY_NIGHTLY_DRIVER = "${campaignDrivers}/agency_nightly_driver.py";
              }
              ''
                export HOME="$TMPDIR/home"
                mkdir -p "$HOME"
                ${pkgs.python3}/bin/python3 ${./test/agency_nightly_driver_test.py}
                touch "$out"
              '';
          spec-build-task-ref-identity =
            pkgs.runCommand "tally-spec-build-task-ref-identity" { nativeBuildInputs = [ pkgs.ripgrep ]; }
              ''
                # Every taskRef must come from taskRefFor(), which resolves to
                # the issue-scoped container in forge-native mode. Interpolating
                # effective.campaign instead hides those nodes from the
                # cross-run blocking filter, and the failure path (diff,
                # diagnose, steer) is exactly where that matters most.
                if rg -n 'taskRef[^;]*\$\{effective\.campaign\}' \
                  ${./examples/flows/spec-build.js}; then
                  echo "spec-build.js must build taskRef with taskRefFor()" >&2
                  exit 1
                fi
                touch "$out"
              '';
          spec-build-task-steering-threads =
            pkgs.runCommand "tally-spec-build-task-steering-threads"
              {
                nativeBuildInputs = [
                  pkgs.python3
                  pkgs.ripgrep
                ];
                SPEC_BUILD_DRIVER_SOURCE = "${campaignDrivers}/spec_build_driver.py";
                SPEC_BUILD_FLOW_SOURCE = "${./examples/flows/spec-build.js}";
              }
              ''
                # A task brief's authorizedComments must be composed by
                # authorizedComments(task): the campaign-wide master thread
                # plus that task's own sub-issue thread. Passing args.steering
                # straight through silently drops per-task steering, and the
                # failure is invisible until an operator's comment on a task
                # sub-issue never reaches its agent.
                if rg -n 'authorizedComments: args\.steering' \
                  ${./examples/flows/spec-build.js}; then
                  echo "spec-build.js must compose authorizedComments with authorizedComments(task)" >&2
                  exit 1
                fi
                # Every brief that reads or writes a forge surface carries the
                # arm-time capability record, so one pass cannot mix the native
                # and degraded projections: the helper itself plus the
                # reconcile, checkpoint, merge, retry, and steer briefs.
                wrapped="$(rg -c 'withCapabilities\(' ${./examples/flows/spec-build.js})"
                if [ "$wrapped" -lt 6 ]; then
                  echo "spec-build.js dropped a capability-carrying brief (found $wrapped)" >&2
                  exit 1
                fi
                PYTHONDONTWRITEBYTECODE=1 \
                  ${pkgs.python3}/bin/python3 \
                  ${./test/spec_build_task_steering_threads_test.py}
                touch "$out"
              '';
          campaign-timer-doc-drift =
            pkgs.runCommand "tally-campaign-timer-doc-drift" { nativeBuildInputs = [ pkgs.ripgrep ]; }
              ''
                # tally-campaign-poll.timer ships, so the campaign docs must
                # not carry the unscoped claim that no periodic campaign timer
                # exists. Since #306 both campaign classes continue themselves
                # through a JSON drop in the shipped events directory, and this
                # timer is the recovery path for a lost continuation event plus
                # the way an outside edit to an armed issue graph is noticed. A
                # blanket denial is false of that arrangement too, and a drift
                # check whose own rationale describes a deleted mechanism is
                # exactly what the next reader trusts.
                if rg -n 'there is no periodic campaign timer' \
                  ${./doc/src/flows/campaigns.md}; then
                  echo "campaigns.md contradicts the shipped tally-campaign-poll.timer" >&2
                  exit 1
                fi
                touch "$out"
              '';
          campaign-preflight-probe-drift =
            pkgs.runCommand "tally-campaign-preflight-probe-drift"
              {
                nativeBuildInputs = [
                  pkgs.ripgrep
                  pkgs.python3
                ];
              }
              ''
                # The copyable blocks are what an operator pastes, and a probe
                # that only asks a tool for its version is exactly what this
                # document's own warning calls insufficient.
                if rg -n 'cargo --version|cargo fmt --version' \
                  ${./doc/src/flows/campaigns.md}; then
                  echo "campaigns.md still ships a version-only preflight probe" >&2
                  exit 1
                fi
                # Every command gate's probe on the shipped Nix surface is read
                # and judged, rather than one exact spelling being grepped for.
                # The first version of this guard matched only `[ "/bin/true" ]`
                # and was green while this very file shipped `[ "true" ]` twice.
                ${pkgs.python3}/bin/python3 ${./test/preflight_probe_drift.py} \
                  ${./flake.nix} ${./nix/modules/common.nix}
                # The replacements have to be present, not merely the absence
                # of the bad ones: a probe that reaches the compiler driver and
                # resolves the workspace offline, and one that makes rustfmt
                # format something. The shipped Nix fixture's own replacement is
                # asserted against the rendered campaign args in
                # checks.campaign-render, which is stronger than a grep.
                if ! rg -qF \
                  'command -v cargo >/dev/null; command -v cc >/dev/null; cargo metadata --offline --format-version 1 >/dev/null' \
                  ${./doc/src/flows/campaigns.md}; then
                  echo "campaigns.md lost its representative test-gate preflight probe" >&2
                  exit 1
                fi
                if ! rg -qF 'rustfmt --emit stdout >/dev/null' ${./doc/src/flows/campaigns.md}; then
                  echo "campaigns.md lost its representative format-gate preflight probe" >&2
                  exit 1
                fi
                # The non-gating witness is the whole point of #320: the real
                # merge-criterion argv has to run at t=0 beside the proxy.
                if ! rg -qF 'preflight-witness-' ${./examples/flows/spec-build.js}; then
                  echo "spec-build.js dropped the non-gating preflight witness node" >&2
                  exit 1
                fi
                touch "$out"
              '';
          spec-build-conflict-domains =
            pkgs.runCommand "tally-spec-build-conflict-domains"
              {
                nativeBuildInputs = [
                  pkgs.git
                  pkgs.python3
                ];
                SPEC_BUILD_DRIVER = "${campaignDrivers}/spec_build_driver.py";
              }
              ''
                ${pkgs.python3}/bin/python3 ${./test/spec_build_conflict_domains_test.py}
                touch "$out"
              '';
          spec-build-two-repo =
            pkgs.runCommand "tally-spec-build-two-repo"
              {
                nativeBuildInputs = [
                  pkgs.git
                  pkgs.python3
                ];
                SPEC_BUILD_DRIVER = "${campaignDrivers}/spec_build_driver.py";
              }
              ''
                export HOME="$TMPDIR/home"
                mkdir -p "$HOME"
                ${pkgs.python3}/bin/python3 ${./test/spec_build_two_repo_test.py}
                touch "$out"
              '';
          spec-build-checkpoint-receipts =
            pkgs.runCommand "tally-spec-build-checkpoint-receipts"
              {
                nativeBuildInputs = [
                  pkgs.git
                  pkgs.python3
                ];
                SPEC_BUILD_DRIVER = "${campaignDrivers}/spec_build_driver.py";
                SPEC_BUILD_CHECKPOINT_REF_VECTORS = "${./test/fixtures/spec-build/checkpoint-refs.json}";
              }
              ''
                ${pkgs.python3}/bin/python3 ${./test/spec_build_checkpoint_receipts_test.py}
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
                sugarFailure = flowSugarPoolsFailure;
                closureFailure = flowPoolClosureFailure;
                windowedFailure = flowWindowedConsumptionFailure;
                reservedFailure = flowReservedPoolFailure;
                reservedBuildFailure = flowReservedBuildPoolFailure;
                maxNodesFailure = flowMaxNodesFailure;
                fanoutWidthFailure = flowFanoutWidthFailure;
              }
              ''
                test -e "$lintFailure"
                test -e "$sugarFailure"
                test -e "$closureFailure"
                test -e "$windowedFailure"
                test -e "$reservedFailure"
                test -e "$reservedBuildFailure"
                test -e "$maxNodesFailure"
                test -e "$fanoutWidthFailure"
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
          pool-resource-declaration =
            pkgs.runCommand "tally-pool-resource-declaration"
              {
                nativeBuildInputs = [ pkgs.jq ];
                rendered = pkgs.writeText "pool-resource-declaration-rendered.json" (
                  builtins.toJSON poolResourceDeclarationFixture
                );
              }
              ''
                jq -S . "$rendered" > rendered.json
                jq -S . ${./test/fixtures/pools/resource-declaration.golden.json} > golden.json
                cmp rendered.json golden.json
                jq -e '(.undeclared | has("resource")) | not' rendered.json >/dev/null
                jq -e '.declared.resource == "vram"' rendered.json >/dev/null
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
            assert builtins.elem
              "gh producer bad-failure-stderr postFailureStderr=true requires postFailureEvidence=true"
              invalidProducerMessages;
            assert builtins.elem "gh producer bad-review requestReview=true requires a non-empty reviewers list"
              invalidProducerMessages;
            assert builtins.elem "gh producer bad-reviewer-login reviewers must be unique GitHub logins"
              invalidProducerMessages;
            assert builtins.elem "gh producer bad-reviewer-length reviewers must be unique GitHub logins"
              invalidProducerMessages;
            assert !invalidProducerAttempt.success;
            pkgs.runCommand "tally-producer-kind-required" { } ''
              touch "$out"
            '';
          campaign-gates-rejected =
            assert builtins.any (
              message: nixpkgs.lib.hasInfix "fields must agree with kind" message
            ) invalidCampaignMessages;
            assert builtins.any (
              message: nixpkgs.lib.hasInfix "use '**' only as a complete path component" message
            ) invalidCampaignMessages;
            # A gate that omits `kind` is refused by the option system rather
            # than by an assertion, so it is proved by evaluation failure and
            # not by a message. The control below is what makes that failure
            # mean "the missing kind" and not "something about this fixture":
            # the same gate with `kind = "command"` supplied evaluates.
            assert !missingKindCampaignAttempt.success;
            assert suppliedKindCampaignAttempt.success;
            assert !invalidCampaignAttempt.success;
            pkgs.runCommand "tally-campaign-gates-rejected" { } ''
              touch "$out"
            '';
          campaign-reserved-name-rejected =
            # The Home Manager assertion that used to be the only thing
            # catching this named an internal producer and never the campaign
            # that collided with it, and it fails closed only for as long as it
            # exists: the fold *seeds* the generic drain rather than overlaying
            # it, so relaxing that assertion would make the collision silent.
            assert builtins.any (
              message: nixpkgs.lib.hasInfix "tally campaign name continuation is reserved" message
            ) reservedCampaignNameMessages;
            # An ordinary campaign name is not caught by it.
            assert builtins.all (
              message: !nixpkgs.lib.hasInfix "is reserved" message
            ) campaignAssertionMessages;
            pkgs.runCommand "tally-campaign-reserved-name-rejected" { } ''
              touch "$out"
            '';
          campaign-noncommitting-sandbox-rejected =
            assert builtins.any (
              message:
              nixpkgs.lib.hasInfix "tally campaign writable-only agentSandboxPolicy must be one of adapter codex commitCapableSandboxPolicies" message
            ) nonCommittingCampaignMessages;
            # The shipped defaults are the counter-case: they must not appear.
            assert builtins.all (
              message: !nixpkgs.lib.hasInfix "commitCapableSandboxPolicies" message
            ) campaignAssertionMessages;
            pkgs.runCommand "tally-campaign-noncommitting-sandbox-rejected" { } ''
              touch "$out"
            '';
          gh-login-grammar =
            assert ghLoginVectors.maxLength == ghLoginLibrary.maxLength;
            assert builtins.length ghLoginVectors.cases >= 20;
            if ghLoginVectorFailures == [ ] then
              pkgs.runCommand "tally-gh-login-grammar" { } ''
                touch "$out"
              ''
            else
              throw "nix/lib/gh-login.nix disagrees with test/fixtures/gh-login/vectors.json on: ${
                nixpkgs.lib.concatMapStringsSep ", " (case: case.name) ghLoginVectorFailures
              }";
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
            test "$(jq -r '.adapters.production.hardening' ${checkedHomeConfig})" = production
            test "$(jq -c '.adapters.production.extraWritablePaths' ${checkedHomeConfig})" = '["/var/lib/tally-stock/agent-state"]'
            test "$(jq -r '.adapters.pi.hardening // "absent"' ${checkedHomeConfig})" = absent
            strict_launch="$(${tally}/bin/tally --config ${checkedHomeConfig} __adapter-render project-codex -- payload)"
            test "$(printf '%s' "$strict_launch" | jq -r '.hardening')" = strict
            workspace_launch="$(${tally}/bin/tally --config ${checkedHomeConfig} __adapter-render shell -- /bin/true)"
            test "$(printf '%s' "$workspace_launch" | jq -r '.hardening')" = workspace
            none_launch="$(${tally}/bin/tally --config ${checkedHomeConfig} __adapter-render explicit-none -- payload)"
            test "$(printf '%s' "$none_launch" | jq -r '.hardening')" = none
            production_launch="$(${tally}/bin/tally --config ${checkedHomeConfig} __adapter-render production -- payload)"
            test "$(printf '%s' "$production_launch" | jq -r '.hardening')" = production
            grep -F '"nix-custom"' ${adapterConfig} >/dev/null
            grep -F '"claude-code"' ${adapterConfig} >/dev/null
            grep -F '"codex"' ${adapterConfig} >/dev/null
            grep -F '"pi"' ${adapterConfig} >/dev/null
            grep -F '"shell"' ${adapterConfig} >/dev/null
            # No trailing `--` in either: pi has no end-of-options separator
            # and exits 1 on one, writing nothing to stdout, so a `--`-suffixed
            # argv could never produce the stream the trace declaration below
            # describes.
            test "$(jq -c '.adapters.pi.argv' ${adapterConfig})" = '["pi","--mode","json"]'
            # Mutation guard: removing this Pi-only declaration falls back to
            # opaque and makes the unsafe render commands below succeed.
            test "$(jq -r '.adapters.pi.launch.rejectOptionLikeWorkloadHead' ${adapterConfig})" = true
            test "$(jq -c '.adapters.pi.resume' ${adapterConfig})" = '["pi","--mode","json","--session","%<sessionRef>%","--model","%<model>%"]'
            test "$(jq -c '.adapters["claude-code"].argv' ${adapterConfig})" = '["claude","--print","--verbose","--output-format","stream-json","--"]'
            test "$(jq -c '.adapters["claude-code"].resume' ${adapterConfig})" = '["claude","--resume","%<sessionRef>%","--model","%<model>%","--print","--verbose","--output-format","stream-json","--"]'
            test "$(jq -c '.adapters["claude-code"].trace' ${adapterConfig})" = '{"framing":"json-lines","stream":"stdout"}'
            test "$(jq -c '.adapters.codex.trace' ${adapterConfig})" = '{"framing":"json-lines","stream":"stdout"}'
            test "$(jq -c '.adapters.shell.trace' ${adapterConfig})" = 'null'
            test "$(jq -c '.adapters.pi.trace' ${adapterConfig})" = '{"framing":"json-lines","stream":"stdout"}'
            test "$(jq -c '.adapters.codex.argv' ${adapterConfig})" = '["codex","exec","--json","--"]'
            test "$(jq -c '.adapters.codex.resume' ${adapterConfig})" = '["codex","-C","%<cwd>%","exec","resume","--json","%<sessionRef>%","--"]'
            test "$(jq -r '.adapters.codex.launch.resumeOptionsBeforeCapture' ${adapterConfig})" = sessionRef
            test "$(jq -c '.adapters.codex.launch.cwdArgv' ${adapterConfig})" = '["-C","%<cwd>%"]'
            test "$(jq -c '.adapters.codex.launch.sandboxPolicies["dangerously-bypass"]' ${adapterConfig})" = '["--dangerously-bypass-approvals-and-sandbox"]'
            test "$(jq -c '.adapters.shell' ${adapterConfig})" = '{"argv":[],"env":{},"extraConfig":{},"extraWritablePaths":[],"launch":{},"resume":null,"resumeRequiresLaunchCwd":false,"scrape":{},"trace":null,"usageCounterScope":"attempt","yieldHook":null}'
            # The cross-cwd resume invariant is a declaration, and pi is the
            # only preset with the measurement behind it: pi's SessionManager
            # filters by exact resolved-path equality, so a resume from another
            # directory prints `Session found in different project`, prompts on
            # stderr, and exits 0 having done no work. codex re-presents the
            # directory in its own resume argv (`-C %<cwd>%`) and claude-code
            # has not been measured here, so neither declares it -- silence is
            # "unmeasured", never "safe".
            test "$(jq -r '.adapters.pi.resumeRequiresLaunchCwd' ${adapterConfig})" = true
            test "$(jq -r '.adapters.codex.resumeRequiresLaunchCwd' ${adapterConfig})" = false
            test "$(jq -r '.adapters["claude-code"].resumeRequiresLaunchCwd' ${adapterConfig})" = false
            test "$(jq -r '.adapters.codex.usageCounterScope' ${adapterConfig})" = session-cumulative
            test "$(jq -r '.adapters.codex.scrape.usage.counterScope' ${adapterConfig})" = session-cumulative
            for preset in pi claude-code shell nix-custom; do
              test "$(jq -r --arg preset "$preset" '.adapters[$preset].usageCounterScope' ${adapterConfig})" = attempt
            done
            for preset in pi claude-code codex; do
              test "$(jq -c --arg preset "$preset" '.adapters[$preset].yieldHook' ${adapterConfig})" = '["tally","lease","status"]'
              test "$(jq -r --arg preset "$preset" '.adapters[$preset].scrape.sessionRef.mode' ${adapterConfig})" = jsonPath
              test "$(jq -r --arg preset "$preset" '.adapters[$preset].scrape.usage.mode' ${adapterConfig})" = jsonPath
              test "$(jq -r --arg preset "$preset" '.adapters[$preset].scrape.finalMessage.mode' ${adapterConfig})" = jsonPathLast
            done
            # `model` is the one capture whose mode is not uniform. codex and
            # claude-code read a document-scoped `$..model`; pi's must select
            # across the whole stream to exclude an invalid turn, and a
            # `$[?...]` filter over the document array is `jsonPathLast`.
            test "$(jq -r '.adapters.codex.scrape.model.mode' ${adapterConfig})" = jsonPath
            test "$(jq -r '.adapters["claude-code"].scrape.model.mode' ${adapterConfig})" = jsonPath
            test "$(jq -r '.adapters.pi.scrape.model.mode' ${adapterConfig})" = jsonPathLast
            # The per-harness usage key mapping is a declaration, and these are
            # the declarations. `crates/tally-core/src/usage.rs` mirrors these
            # exact strings in its fixture tests, so a preset that drifts from
            # the normalizer's expectations fails here rather than passing two
            # agreeing-but-wrong suites.
            test "$(jq -c '.adapters.codex.scrape.usage.fields' ${adapterConfig})" = '{"cacheReadTokens":["cached_input_tokens"],"cacheWriteTokens":["cache_write_input_tokens"],"inputTokensWithCacheRead":["input_tokens"],"outputTokens":["output_tokens"],"reasoningTokens":["reasoning_output_tokens"]}'
            test "$(jq -c '.adapters["claude-code"].scrape.usage.fields' ${adapterConfig})" = '{"cacheReadTokens":["cache_read_input_tokens"],"cacheWriteTokens":["cache_creation_input_tokens"],"inputTokens":["input_tokens"],"outputTokens":["output_tokens"]}'
            test "$(jq -c '.adapters["claude-code"].scrape.usageCost' ${adapterConfig})" = '{"fields":{"costUsd":["$"]},"mode":"jsonPathLast","pattern":"$[?@.type == '"'"'result'"'"'].total_cost_usd","stream":"stdout"}'
            test "$(jq -c '.adapters["claude-code"].scrape.contextWindow' ${adapterConfig})" = '{"fields":{"contextWindow":["$"]},"mode":"jsonPathLast","pattern":"$[?@.type == '"'"'result'"'"'].modelUsage.*.contextWindow","stream":"stdout"}'
            # Occupancy is a narrower capture than usage: scoped to only
            # assistant-turn events, under field names of its own so a lookup
            # for one concern can never resolve against the other's capture.
            test "$(jq -c '.adapters["claude-code"].scrape.occupancy.fields' ${adapterConfig})" = '{"residentCacheReadTokens":["cache_read_input_tokens"],"residentCacheWriteTokens":["cache_creation_input_tokens"],"residentInputTokens":["input_tokens"]}'
            test "$(jq -r '.adapters["claude-code"].scrape.occupancy.pattern' ${adapterConfig})" = "\$[?@.type == 'assistant'].message.usage"
            # pi's key names are known now -- test/fixtures/traces/pi.jsonl is
            # a real `pi --mode json` capture -- but the same capture shows pi
            # states usage per assistant message and never per attempt, so a
            # declared mapping here would report one turn as the attempt's
            # spend. It stays undeclared for that reason, not for want of a
            # capture; the per-turn reading it does support is declared as
            # occupancy below.
            test "$(jq -r '.adapters.pi.scrape.usage.fields // "absent"' ${adapterConfig})" = absent
            test "$(jq -r '.adapters.shell.scrape.finalMessage // "absent"' ${adapterConfig})" = absent
            # No real codex or pi capture has ever stated a context window,
            # so neither preset declares the scrape -- an operator who knows
            # the ceiling can still assert it via extraConfig.contextWindow.
            test "$(jq -r '.adapters.codex.scrape.contextWindow // "absent"' ${adapterConfig})" = absent
            test "$(jq -r '.adapters.pi.scrape.contextWindow // "absent"' ${adapterConfig})" = absent
            # codex exec --json states no per-turn resident figure, only a
            # cumulative one, so it declares no occupancy capture either. pi
            # states the opposite -- per-message figures and no cumulative one
            # -- so occupancy is precisely what its usage objects are.
            test "$(jq -r '.adapters.codex.scrape.occupancy // "absent"' ${adapterConfig})" = absent
            test "$(jq -c '.adapters.pi.scrape.occupancy.fields' ${adapterConfig})" = '{"residentCacheReadTokens":["cacheRead"],"residentCacheWriteTokens":["cacheWrite"],"residentInputTokens":["input"]}'
            test "$(jq -r '.adapters.pi.scrape.occupancy.pattern' ${adapterConfig})" = "\$[?@.type == 'message_end' && @.message.role == 'assistant' && @.message.stopReason != 'aborted' && @.message.stopReason != 'error'].message.usage"
            # The valid-turn guard is applied to every capture an operator
            # reads, not only to occupancy: `finalMessage` would otherwise
            # report an aborted turn's partial text as the node's answer and
            # `model` would pin a model no valid turn used. All three are
            # scoped to assistant `message_end` -- that scoping is load
            # bearing, not stylistic, because pi repeats the same message
            # (same role, same model) on `message_start`/`message_update`
            # with `stopReason: pending`, so a filter that matches those
            # records reads an excluded turn's model out of them. `usage`
            # stays unguarded on purpose -- see the aborted-fixture render.
            test "$(jq -r '.adapters.pi.scrape.finalMessage.pattern' ${adapterConfig})" = "\$[?@.type == 'message_end' && @.message.role == 'assistant' && @.message.stopReason != 'aborted' && @.message.stopReason != 'error'].message.content[?@.type == 'text'].text"
            test "$(jq -r '.adapters.pi.scrape.model.pattern' ${adapterConfig})" = "\$[?@.type == 'message_end' && @.message.role == 'assistant' && @.message.stopReason != 'aborted' && @.message.stopReason != 'error'].message.model"
            test "$(jq -r '.adapters.pi.scrape.sessionRef.pattern' ${adapterConfig})" = '$.id'
            test "$(jq -r '.adapters["claude-code"].scrape.sessionRef.pattern' ${adapterConfig})" = '$..session_id'
            test "$(jq -r '.adapters.codex.scrape.sessionRef.pattern' ${adapterConfig})" = '$..thread_id'
            test "$(jq -r '.adapters.codex.scrape.finalMessage.mode' ${adapterConfig})" = jsonPathLast
            test "$(jq -r '.adapters.codex.scrape.finalMessage.pattern' ${adapterConfig})" = "\$[?@.type == 'item.completed' && @.item.type == 'agent_message'].item.text"
            test "$(jq -r '.adapters.codex.extraConfig.modelFlag' ${adapterConfig})" = '--model'
            jq -e '.adapters["nix-custom"].skillBundle == "review protocol α\n"' ${adapterConfig} >/dev/null
            test "$(jq -r '.adapters["nix-custom"].env.CUSTOM_AGENT_MODE' ${adapterConfig})" = batch
            test "$(jq -r '.adapters["nix-custom"].hardening' ${adapterConfig})" = production
            test "$(jq -c '.adapters["nix-custom"].extraWritablePaths' ${adapterConfig})" = '["/var/lib/custom-agent"]'
            launch="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render nix-custom -- 'payload arg' "")"
            test "$(printf '%s' "$launch" | jq -c '.argv')" = '["custom-agent","--structured","payload arg",""]'
            test "$(printf '%s' "$launch" | jq -r '.env.CUSTOM_AGENT_MODE')" = batch
            test "$(printf '%s' "$launch" | jq -r '.hardening')" = production
            resume="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render nix-custom --captures '{"sessionRef":"nix-session"}' -- '--option-looking')"
            test "$(printf '%s' "$resume" | jq -c '.argv')" = '["custom-agent","--resume","nix-session","--option-looking"]'
            pi_launch="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render pi -- work)"
            test "$(printf '%s' "$pi_launch" | jq -c '.argv')" = '["pi","--mode","json","work"]'
            for unsafe_head in --version -p; do
              if ${tally}/bin/tally --config ${adapterConfig} __adapter-render pi -- "$unsafe_head" >pi-unsafe.out 2>pi-unsafe.err; then
                echo "pi launch admitted unsafe workload head $unsafe_head" >&2
                exit 1
              fi
              grep -F "adapter \"pi\" pre-launch refusal option-like-workload-head at index 0: \"$unsafe_head\"" pi-unsafe.err >/dev/null
              if ${tally}/bin/tally --config ${adapterConfig} __adapter-render pi --captures '{"sessionRef":"pi-session","model":"Pi/Exact.Model"}' -- "$unsafe_head" >pi-unsafe.out 2>pi-unsafe.err; then
                echo "pi resume admitted unsafe workload head $unsafe_head" >&2
                exit 1
              fi
              grep -F "adapter \"pi\" pre-launch refusal option-like-workload-head at index 0: \"$unsafe_head\"" pi-unsafe.err >/dev/null
            done
            : > empty.err
            # SYNTHETIC, and deliberately not pi's key set. This block
            # predates the real-capture block below and exists only to pin
            # the preset's *selection* rules -- `$.id` over the session
            # header, the last assistant `message_end` for `finalMessage`, a
            # `user` message ignored in between -- against a stream small
            # enough to read in one screen. Its `usage` keys are
            # `input_tokens`, which pi does not emit: pi's real keys are
            # `input`/`output`/`cacheRead`/`cacheWrite`, and they are
            # asserted from the recorded bytes further down. Nothing here
            # should be read as evidence about pi's wire format.
            printf '%s\n' \
              '{"type":"session","id":"pi-session","model":"Pi/Exact.Model"}' \
              '{"type":"message_end","message":{"role":"assistant","model":"Pi/Exact.Model","content":[{"type":"text","text":"pi first"}],"usage":{"input_tokens":5}}}' \
              '{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"ignore user"}]}}' \
              '{"type":"message_end","message":{"role":"assistant","model":"Pi/Exact.Model","content":[{"type":"text","text":"pi final"}],"usage":{"input_tokens":11}}}' > pi.jsonl
            pi_render="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render pi --scrape-stdout "$PWD/pi.jsonl" --scrape-stderr "$PWD/empty.err" -- work)"
            test "$(printf '%s' "$pi_render" | jq -c '.argv')" = '["pi","--mode","json","--session","pi-session","--model","Pi/Exact.Model","work"]'
            test "$(printf '%s' "$pi_render" | jq -c '.captures.usage')" = '{"input_tokens":11}'
            test "$(printf '%s' "$pi_render" | jq -r '.captures.finalMessage')" = 'pi final'
            test "$(printf '%s' "$pi_render" | jq -r '.defaultGateManifest')" = false
            # The same preset against a real `pi --mode json` capture rather
            # than a stream written to agree with it. Every pi capture is
            # resolved here from the recorded bytes: the session header's
            # `$.id`, the model, the last assistant turn's usage object with
            # the exact key set pi emits, the occupancy capture landing on
            # that same turn rather than on the zero-filled `message_start`
            # placeholder, and the final assistant text. See
            # test/fixtures/traces/README.md for the capture's provenance.
            pi_real="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render pi --scrape-stdout ${./test/fixtures/traces/pi.jsonl} --scrape-stderr "$PWD/empty.err" -- work)"
            test "$(printf '%s' "$pi_real" | jq -c '.argv')" = '["pi","--mode","json","--session","019f0000-0000-7000-8000-000000000001","--model","qwen3.6-35b-a3b","work"]'
            pi_last_turn_usage='{"input":190,"output":46,"cacheRead":842,"cacheWrite":0,"reasoning":0,"totalTokens":1078,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}'
            test "$(printf '%s' "$pi_real" | jq -c '.captures.occupancy')" = "$pi_last_turn_usage"
            # The two captures coincide for pi, and that is the finding, not
            # an accident: pi's stream carries no attempt-level roll-up, so
            # the last `usage` object anywhere in it is the same last-turn
            # object occupancy reads. That is why occupancy is declared and a
            # spend mapping is not. (For claude-code the two deliberately
            # differ: its `usage` lands on the `result` roll-up.)
            test "$(printf '%s' "$pi_real" | jq -c '.captures.usage')" = "$pi_last_turn_usage"
            test "$(printf '%s' "$pi_real" | jq -r '.captures.finalMessage')" = 'The file notes.txt contains 42.'
            # A stream that ends on a turn pi marked `aborted` must resolve
            # occupancy to the last VALID turn, not to the aborted turn's
            # zero-filled usage object: three resolved zeroes are `Some(0)`,
            # which reads as an empty context for a session that was over a
            # thousand tokens full. The fixture is the capture above with one
            # real aborted assistant turn appended, carrying the
            # `message_start` / `message_update` / `message_end` lifecycle pi
            # emits for every message -- see test/fixtures/traces/README.md.
            # That lifecycle is why this render can see the `model` defect at
            # all: the mid-stream records repeat the aborted turn's model
            # under `stopReason: pending`, so a guard that is not scoped to
            # `message_end` reads it back out of them and the assertions
            # below pass on a bare spliced `message_end` while failing on
            # every shape pi can actually emit.
            pi_aborted="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render pi --scrape-stdout ${./test/fixtures/traces/pi-aborted-turn.jsonl} --scrape-stderr "$PWD/empty.err" -- work)"
            test "$(printf '%s' "$pi_aborted" | jq -c '.captures.occupancy')" = "$pi_last_turn_usage"
            test "$(printf '%s' "$pi_aborted" | jq -r '.captures.occupancy.input')" != 0
            # The guard is on every capture an operator reads, not just on
            # occupancy. `finalMessage` must be the last VALID turn's answer:
            # the aborted turn in this fixture carries partial text
            # (`The file notes.txt cont`), and reporting that truncated
            # fragment as the node's answer -- unmarked, indistinguishable
            # from a complete one -- is what the clauses on that capture
            # prevent.
            test "$(printf '%s' "$pi_aborted" | jq -r '.captures.finalMessage')" = 'The file notes.txt contains 42.'
            # And this argv is where the same defect on `model` became
            # operator-visible. The spliced aborted turn genuinely came from
            # another session and states `qwen3-vl-8b-ocr`; an unguarded
            # `$..model` pinned it, so the resume tally would have run named
            # a model no valid turn of this session ever used. Asserting the
            # whole argv rather than the capture is deliberate: the argv is
            # the surface an operator sees.
            test "$(printf '%s' "$pi_aborted" | jq -c '.argv')" = '["pi","--mode","json","--session","019f0000-0000-7000-8000-000000000001","--model","qwen3.6-35b-a3b","work"]'
            test "$(printf '%s' "$pi_aborted" | jq -r '.argv | index("qwen3-vl-8b-ocr") // "absent"')" = absent
            # `usage` is deliberately unguarded, and this is what that costs:
            # the stream-wide `$..usage` does land on the aborted turn. It is
            # never read as occupancy, and no spend mapping is declared for
            # pi, so it states nothing -- but the asymmetry is asserted here
            # rather than left for a reader to discover.
            test "$(printf '%s' "$pi_aborted" | jq -r '.captures.usage.totalTokens')" = 0
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
            # This is the committed excerpt of a real default-model
            # `codex exec --json` capture. It states the session, final answer,
            # and all five observed usage keys, but no model. The stock preset
            # must keep that absence honest and still render a resumable argv.
            codex_render="$(${tally}/bin/tally --config ${adapterConfig} __adapter-render codex --cwd "$PWD" --scrape-stdout ${./test/fixtures/usage/codex.jsonl} --scrape-stderr "$PWD/empty.err" -- work)"
            expected_codex="$(jq -cn --arg cwd "$PWD" '["codex","-C",$cwd,"exec","resume","--json","codex-usage-thread","--","work"]')"
            test "$(printf '%s' "$codex_render" | jq -c '.argv')" = "$expected_codex"
            test "$(printf '%s' "$codex_render" | jq -r '.captures.sessionRef')" = codex-usage-thread
            test "$(printf '%s' "$codex_render" | jq -e '.captures | has("model") | not')" = true
            test "$(printf '%s' "$codex_render" | jq -c '.captures.usage')" = '{"input_tokens":7060166,"cached_input_tokens":6798080,"cache_write_input_tokens":0,"output_tokens":32842,"reasoning_output_tokens":15163}'
            test "$(printf '%s' "$codex_render" | jq -r '.captures.finalMessage')" = '<redacted text>'
            test "$(printf '%s' "$codex_render" | jq -r '.defaultGateManifest')" = true
            # Mutation guard: restoring the old required placeholder makes the
            # same real capture fail because it never stated a model.
            jq '.adapters.codex.resume = ["codex", "-C", "%<cwd>%", "exec", "resume", "--json", "--model", "%<model>%", "%<sessionRef>%", "--"]' ${adapterConfig} > codex-required-model.json
            if ${tally}/bin/tally --config "$PWD/codex-required-model.json" __adapter-render codex --cwd "$PWD" --scrape-stdout ${./test/fixtures/usage/codex.jsonl} --scrape-stderr "$PWD/empty.err" -- work >codex-required-model.out 2>codex-required-model.err; then
              echo "required Codex model placeholder accepted a capture with no model" >&2
              exit 1
            fi
            grep -F 'resume capture "model" is absent for adapter "codex"' codex-required-model.err >/dev/null
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
                producer_data="$PWD/data"
                mkdir -p "$producer_data"
                daily="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch daily --state-dir "$producer_state" --data-dir "$producer_data" --event '{"kind":"calendar"}')"
                test "$(printf '%s' "$daily" | jq -r 'keys[0]')" = emitted
                own_event='{"kind":"gh","source":"search","repo":"agency-agency/spec","number":21,"htmlUrl":"https://github.com/agency-agency/spec/issues/21","itemType":"issue","nodeId":"I-self","itemAuthor":"tally-bot","triggerActor":"tally-bot","selfActor":"tally-bot","triggerKind":"command-comment","eventId":"comment-42","commentId":"comment-42","triggerTimestamp":"2026-07-20T12:30:00Z","context":{"schemaVersion":2,"title":"Self-authored issue","body":"untrusted $(must-not-run)","state":"open","labels":["agency:codex-ready"],"assignees":["tally-bot"],"triggeringComment":{"id":"comment-42","author":"tally-bot","body":"/tally run"}}}'
                own="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch github --state-dir "$producer_state" --data-dir "$producer_data" --event "$own_event")"
                test "$(printf '%s' "$own" | jq -r 'keys[0]')" = emitted
                own_path="$(printf '%s' "$own" | jq -r '.emitted')"
                jq -e '.argv == ["gh-job"] and .noEnqueue == true' "$own_path" >/dev/null
                flow_event='{"kind":"gh","source":"search","repo":"agency-agency/spec","number":61,"htmlUrl":"https://github.com/agency-agency/spec/issues/61","itemType":"issue","nodeId":"I-flow-61","itemAuthor":"flow-author","triggerActor":"tally-bot","selfActor":"tally-bot","triggerKind":"command-comment","eventId":"notification-61","commentId":"comment-61","triggerTimestamp":"2026-07-26T12:30:00Z","context":{"schemaVersion":2,"title":"Pooled review","body":"untrusted $(must-not-run)","state":"open","labels":["agency:codex-ready"],"assignees":["tally-bot"],"triggeringComment":{"id":"comment-61","author":"tally-bot","body":"/pooled-review"}}}'
                flow="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch github-flow --state-dir "$producer_state" --data-dir "$producer_data" --event "$flow_event")"
                test "$(printf '%s' "$flow" | jq -r 'keys[0]')" = emitted
                flow_path="$(printf '%s' "$flow" | jq -r '.emitted')"
                flow_args="$(jq -r '.briefPath' "$flow_path")"
                test -f "$flow_args"
                jq -e \
                  --arg script ${./examples/flows/pooled-review.js} \
                  --arg catalog ${catalogFixture} \
                  --arg args "$flow_args" '
                    .source == "gh" and
                    .noEnqueue == false and
                    .argv[0:3] == ["tally", "flow", "run"] and
                    .argv[3] == $script and
                    .argv[4:7] == ["--args-from-brief", "--max-nodes", "1000"] and
                    .briefPath == $args and
                    (has("brief") | not) and
                    .argv[7] == "--catalog" and
                    .argv[8] == $catalog and
                    ([.argv[] | contains("must-not-run")] | any | not)
                  ' "$flow_path" >/dev/null
                jq -e '. == {
                  "subject": "https://github.com/agency-agency/spec/issues/61",
                  "minimumValid": 2
                }' "$flow_args" >/dev/null
                test "$(dirname "$(dirname "$flow_args")")" = "$producer_data"
                test ! -e "$producer_state/briefs"
                flow_duplicate="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch github-flow --state-dir "$producer_state" --data-dir "$producer_data" --event "$flow_event")"
                test "$(printf '%s' "$flow_duplicate" | jq -r '.')" = duplicate
                rejected_event='{"kind":"gh","source":"search","repo":"agency-agency/spec","number":21,"htmlUrl":"https://github.com/agency-agency/spec/issues/21","itemType":"issue","nodeId":"I-self","itemAuthor":"tally-bot","triggerActor":"untrusted-user","selfActor":"tally-bot","triggerKind":"command-comment","eventId":"comment-43","commentId":"comment-43","triggerTimestamp":"2026-07-20T12:31:00Z","context":{"schemaVersion":2,"title":"Self-authored issue","body":"untrusted","state":"open","labels":["agency:codex-ready"],"assignees":["tally-bot"],"triggeringComment":{"id":"comment-43","author":"untrusted-user","body":"/tally run"}}}'
                rejected="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch github --state-dir "$producer_state" --data-dir "$producer_data" --event "$rejected_event")"
                test "$(printf '%s' "$rejected" | jq -r '.filtered.reason')" = trigger-actor-not-allowed
                effect="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch effects --state-dir "$producer_state" --data-dir "$producer_data" --event '{"kind":"build-effect","storePath":"${pkgs.hello}"}')"
                test "$(printf '%s' "$effect" | jq -r '.[0] | keys[0]')" = emitted
                duplicate="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch effects --state-dir "$producer_state" --data-dir "$producer_data" --event '{"kind":"build-effect","storePath":"${pkgs.hello}"}')"
                test "$(printf '%s' "$duplicate" | jq -r '.[0]')" = duplicate
                for probe in 1 2; do
                  lost="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch health --engine-only --state-dir "$producer_state" --data-dir "$producer_data" --event '{"kind":"pool-reachability","reachable":false}')"
                  test "$(printf '%s' "$lost" | jq -r '.transition')" = null
                done
                lost="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch health --engine-only --state-dir "$producer_state" --data-dir "$producer_data" --event '{"kind":"pool-reachability","reachable":false}')"
                test "$(printf '%s' "$lost" | jq -r '.transition')" = lost
                test "$(printf '%s' "$lost" | jq -r '.emitted | length')" = 1
                for probe in 1 2; do
                  returned="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch health --engine-only --state-dir "$producer_state" --data-dir "$producer_data" --event '{"kind":"pool-reachability","reachable":true}')"
                  test "$(printf '%s' "$returned" | jq -r '.transition')" = null
                done
                returned="$(${tally}/bin/tally --config ${producerConfig} __producer-dispatch health --engine-only --state-dir "$producer_state" --data-dir "$producer_data" --event '{"kind":"pool-reachability","reachable":true}')"
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
          # The campaign surface on a system host has to survive the same
          # config check the daemon runs at load: campaign pools and the driver
          # adapter are rendered here without a single declared flow.
          campaign-nixos-activation = campaignNixos.config.system.build.toplevel;
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
          ];
        };
        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
