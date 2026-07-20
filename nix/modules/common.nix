{
  self,
  lib,
  pkgs,
}:

let
  inherit (lib)
    concatLists
    filterAttrs
    flatten
    mapAttrs
    mapAttrsToList
    mkOption
    optionalAttrs
    types
    unique
    ;

  adapterLibrary = import ../lib/adapters.nix { inherit lib; };
  adapterDefaults = mapAttrs (_: value: lib.mkDefault value) adapterLibrary.presets;

  priorityRanks = import ../lib/priority-ranks.nix;

  internalAssertionsOption = mkOption {
    type = types.listOf types.raw;
    default = [ ];
    example = [ ];
    internal = true;
    description = "Assertions contributed by this typed tally submodule.";
  };

  credentialType = types.attrsOf types.externalPath;

  validComponent =
    value: builtins.match "[A-Za-z0-9_][A-Za-z0-9_.-]*" value != null && value != "." && value != "..";

  validEnvironmentName = value: builtins.match "[A-Za-z_][A-Za-z0-9_]*" value != null;

  validCredentialName = value: builtins.stringLength value <= 255 && validComponent value;

  mkScrapeCaptureType = types.submodule (
    { config, name, ... }: {
      options = {
        stream = mkOption {
          type = types.enum [
            "stdout"
            "stderr"
          ];
          default = "stdout";
          example = "stderr";
          description = "Captured stream read by this named scrape.";
        };
        mode = mkOption {
          type = types.enum [
            "regex"
            "jsonPath"
          ];
          default = "regex";
          example = "jsonPath";
          description = "Structured scrape mode.";
        };
        pattern = mkOption {
          type = types.str;
          default = "";
          example = "$..session_id";
          description = "Regex or RFC 9535 JSONPath expression.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = [
        {
          assertion = config.pattern != "";
          message = "tally adapter scrape ${name} requires a non-empty pattern";
        }
      ];
    }
  );

  mkAdapterType = types.submodule (
    { config, name, ... }: {
      options = {
        argv = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [
            "agent"
            "--json"
            "--"
          ];
          description = "Direct argv prefix for a fresh invocation; no shell string is accepted.";
        };
        resume = mkOption {
          type = types.nullOr (types.listOf types.str);
          default = null;
          example = [
            "agent"
            "resume"
            "%<sessionRef>%"
            "--"
          ];
          description = "Optional direct argv template for recovery resume.";
        };
        scrape = mkOption {
          type = types.attrsOf mkScrapeCaptureType;
          default = { };
          example.sessionRef = {
            mode = "jsonPath";
            pattern = "$..session_id";
          };
          description = "Named stdout/stderr captures used by resume and advisory attestations.";
        };
        yieldHook = mkOption {
          type = types.nullOr (types.listOf types.str);
          default = null;
          example = [
            "tally"
            "lease"
            "status"
          ];
          description = "Optional direct argv cooperative-yield checkpoint.";
        };
        env = mkOption {
          type = types.attrsOf types.str;
          default = { };
          example.NO_COLOR = "1";
          description = "Non-reserved environment added to adapter invocations.";
        };
        extraConfig = mkOption {
          type = types.attrsOf types.raw;
          default = { };
          example.modelFlag = "--model";
          description = "JSON-serializable adapter-specific extension data.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = flatten [
        {
          assertion = config.argv == [ ] || builtins.head config.argv != "";
          message = "tally adapter ${name} argv must start with a non-empty executable";
        }
        {
          assertion = config.resume == null || config.resume != [ ];
          message = "tally adapter ${name} resume must be null or a non-empty argv";
        }
        {
          assertion = config.resume == null || builtins.head config.resume != "";
          message = "tally adapter ${name} resume must start with a non-empty executable";
        }
        {
          assertion = config.yieldHook == null || config.yieldHook != [ ];
          message = "tally adapter ${name} yieldHook must be null or a non-empty argv";
        }
        {
          assertion = config.yieldHook == null || builtins.head config.yieldHook != "";
          message = "tally adapter ${name} yieldHook must start with a non-empty executable";
        }
        (mapAttrsToList (capture: value: value._tallyAssertions) config.scrape)
        (mapAttrsToList (environment: _: {
          assertion =
            validEnvironmentName environment
            && !(lib.hasPrefix "TALLY_" environment)
            && environment != "CREDENTIALS_DIRECTORY";
          message = "tally adapter ${name} environment name ${environment} is invalid or reserved";
        }) config.env)
      ];
    }
  );

  mkEnqueueType = types.submodule (
    { config, name, ... }: {
      options = {
        argv = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [
            "agent"
            "run"
          ];
          description = "Leaf argv executed directly after the adapter prefix.";
        };
        adapter = mkOption {
          type = types.str;
          default = "shell";
          example = "codex-project";
          description = "Open-map adapter name.";
        };
        pool = mkOption {
          type = types.coercedTo types.str (pool: [ pool ]) (types.listOf types.str);
          default = [ ];
          example = [
            "worker-build"
            "programmatic-budget"
          ];
          description = "Required non-empty set of target pool names; a legacy singleton string is accepted.";
        };
        priority = mkOption {
          type = types.enum [
            "interrupt"
            "high"
            "medium"
            "low"
          ];
          default = "low";
          example = "high";
          description = "Canonical priority tier.";
        };
        dedupKey = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "daily-%Y%m%d";
          description = "Optional strftime-expanded existence key.";
        };
        evidence = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [
            "artifact:/var/lib/result.json"
            "exit:0"
          ];
          description = "Canonical evidence specifications.";
        };
        evidenceClass = mkOption {
          type = types.raw;
          default = null;
          example = {
            source = "module";
          };
          description = "Optional opaque JSON evidence class passed through verbatim.";
        };
        manifestHash = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "sha256:opaque-to-tally";
          description = "Optional opaque manifest hash passed through verbatim.";
        };
        consumptionEstimate = mkOption {
          type = types.nullOr types.ints.unsigned;
          default = null;
          example = 900;
          description = "Non-negative authoritative admission estimate for a windowed budget.";
        };
        runtimeMaxSec = mkOption {
          type = types.nullOr types.ints.positive;
          default = null;
          example = 3600;
          description = "Optional RuntimeMaxSec bound for the transient job unit.";
        };
        noEnqueue = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Give this leaf the one-hop capability that refuses child enqueue.";
        };
        credentials = mkOption {
          type = credentialType;
          default = { };
          example.API_TOKEN = "/run/credentials/api-token";
          description = "Credential name to absolute source path, passed only by LoadCredential reference.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = flatten [
        {
          assertion = config.argv != [ ] && builtins.head config.argv != "";
          message = "tally producer enqueue ${name} requires a non-empty direct argv";
        }
        {
          assertion = config.pool != [ ];
          message = "tally producer enqueue ${name} requires a non-empty pool set";
        }
        {
          assertion = builtins.length (unique config.pool) == builtins.length config.pool;
          message = "tally producer enqueue ${name} pool set contains duplicates";
        }
        {
          assertion = config.adapter != "";
          message = "tally producer enqueue ${name} requires an adapter";
        }
        {
          assertion = config.dedupKey == null || config.dedupKey != "";
          message = "tally producer enqueue ${name} dedupKey must be null or non-empty";
        }
        (mapAttrsToList (credential: _: {
          assertion = validCredentialName credential;
          message = "tally producer enqueue ${name} has invalid credential name ${credential}";
        }) config.credentials)
      ];
    }
  );

  producerCommonOptions = kind: {
    kind = mkOption {
      type = types.enum [ kind ];
      default = kind;
      example = kind;
      description = "Producer discriminator; this entry is the ${kind} submodule.";
    };
    credentials = mkOption {
      type = credentialType;
      default = { };
      example.PRODUCER_TOKEN = "/run/credentials/producer-token";
      description = "Credential references made available to this producer unit.";
    };
    _tallyAssertions = internalAssertionsOption;
  };

  mkProducerModule =
    kind: extraOptions: assertions:
    ({ config, name, ... }: {
      options = producerCommonOptions kind // extraOptions;
      config._tallyAssertions = flatten [
        (mapAttrsToList (credential: _: {
          assertion = validCredentialName credential;
          message = "tally producer ${name} has invalid credential name ${credential}";
        }) config.credentials)
        (assertions config name)
      ];
    });

  calendarProducerType = types.submodule (
    mkProducerModule "calendar"
      {
        onCalendar = mkOption {
          type = types.str;
          default = "daily";
          example = "Mon..Fri 09:00";
          description = "systemd OnCalendar expression.";
        };
        enqueue = mkOption {
          type = mkEnqueueType;
          default = { };
          example = {
            argv = [ "daily-job" ];
            pool = "worker-build";
          };
          description = "Payload emitted on each calendar firing.";
        };
      }
      (
        config: name: [
          {
            assertion = config.onCalendar != "";
            message = "calendar producer ${name} requires a non-empty onCalendar";
          }
          config.enqueue._tallyAssertions
        ]
      )
  );

  eventsDirProducerType = types.submodule (
    mkProducerModule "events-dir" {
      pollIntervalSec = mkOption {
        type = types.ints.positive;
        default = 60;
        example = 15;
        description = "Polling cadence for the events-directory drain unit.";
      };
    } (_: _: [ ])
  );

  ghProducerType = types.submodule (
    mkProducerModule "gh"
      {
        enable = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Generate and run the GitHub polling producer.";
        };
        sources = mkOption {
          type = types.listOf (
            types.enum [
              "notifications"
              "search"
            ]
          );
          default = [ ];
          example = [
            "notifications"
            "search"
          ];
          description = "Explicit GitHub intake sources.";
        };
        actorExclude = mkOption {
          type = types.str;
          default = "self";
          example = "tally-bot";
          description = "Actor refused by the GitHub intake narrower.";
        };
        pollIntervalSec = mkOption {
          type = types.ints.positive;
          default = 60;
          example = 120;
          description = "GitHub polling cadence.";
        };
        postEvidence = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Enable the idempotent COMPLETED mutation half.";
        };
        enqueue = mkOption {
          type = mkEnqueueType;
          default = { };
          example = {
            argv = [ "handle-gh-item" ];
            pool = "worker-build";
          };
          description = "GitHub item enqueue payload.";
        };
      }
      (
        config: name: [
          {
            assertion = !config.enable || config.sources != [ ];
            message = "enabled gh producer ${name} requires at least one source";
          }
          {
            assertion = builtins.length config.sources == builtins.length (unique config.sources);
            message = "gh producer ${name} sources must not contain duplicates";
          }
          {
            assertion = config.actorExclude != "";
            message = "gh producer ${name} actorExclude must be non-empty";
          }
          config.enqueue._tallyAssertions
        ]
      )
  );

  buildEffectProducerType = types.submodule (
    mkProducerModule "build-effect" {
      watch = mkOption {
        type = types.enum [
          "gc-roots-dir"
          "jsonl"
          "post-build-hook"
        ];
        default = "gc-roots-dir";
        example = "jsonl";
        description = "Bounded store-path observation surface.";
      };
      path = mkOption {
        type = types.externalPath;
        default = "/var/empty/tally-build-effects";
        example = "/var/lib/tally/build-effects.jsonl";
        description = "Absolute observation path; tally never invokes nix build.";
      };
      onKey = mkOption {
        type = mkEnqueueType;
        default = { };
        example = {
          argv = [ "consume-store-path" ];
          pool = "worker-build";
        };
        description = "Payload emitted once per distinct observed store path.";
      };
    } (config: _: [ config.onKey._tallyAssertions ])
  );

  poolReachabilityProducerType = types.submodule (
    mkProducerModule "pool-reachability"
      {
        probePool = mkOption {
          type = types.str;
          default = "";
          example = "worker-gpu";
          description = "Configured pool whose local availability is probed.";
        };
        intervalSec = mkOption {
          type = types.ints.positive;
          default = 30;
          example = 15;
          description = "Reachability probe cadence.";
        };
        hysteresis = mkOption {
          type = types.ints.positive;
          default = 3;
          example = 5;
          description = "Consecutive observations required before a transition.";
        };
        onLost = mkOption {
          type = types.nullOr mkEnqueueType;
          default = null;
          example = {
            argv = [ "record-pool-loss" ];
            pool = "controller";
          };
          description = "Optional fresh enqueue on confirmed loss.";
        };
        onReturn = mkOption {
          type = types.nullOr mkEnqueueType;
          default = null;
          example = {
            argv = [ "record-pool-return" ];
            pool = "controller";
          };
          description = "Optional fresh enqueue on confirmed return.";
        };
        onReturnAttest = mkOption {
          type = types.nullOr mkEnqueueType;
          default = null;
          example = {
            argv = [ "assess-return" ];
            pool = "controller";
            noEnqueue = true;
          };
          description = "Optional advisory return assessor; noEnqueue must be true.";
        };
      }
      (
        config: name:
        flatten [
          {
            assertion = config.probePool != "";
            message = "pool-reachability producer ${name} requires probePool";
          }
          {
            assertion = config.onReturnAttest == null || config.onReturnAttest.noEnqueue;
            message = "pool-reachability producer ${name} onReturnAttest requires noEnqueue = true";
          }
          (map (field: if config.${field} == null then [ ] else config.${field}._tallyAssertions) [
            "onLost"
            "onReturn"
            "onReturnAttest"
          ])
        ]
      )
  );

  producerKinds = [
    "calendar"
    "build-effect"
    "pool-reachability"
    "gh"
    "events-dir"
  ];

  producerTypeFor = kind: type: types.addCheck type (value: value ? kind && value.kind == kind);

  invalidProducerType = types.addCheck (types.submodule (
    { config, name, ... }:
    {
      freeformType = types.attrsOf types.raw;
      options = {
        kind = mkOption {
          type = types.raw;
          default = null;
          example = "calendar";
          description = "Invalid producer discriminator retained only to report a complete assertion error.";
        };
        credentials = mkOption {
          type = credentialType;
          default = { };
          example.PRODUCER_TOKEN = "/run/credentials/producer-token";
          description = "Credential references retained while reporting an invalid producer discriminator.";
        };
        _tallyAssertions = internalAssertionsOption;
      };
      config._tallyAssertions = [
        {
          assertion = false;
          message =
            if config.kind == null then
              "tally producer ${name} requires an explicit kind; expected one of ${lib.concatStringsSep ", " producerKinds}"
            else
              "tally producer ${name} has unknown kind ${builtins.toJSON config.kind}; expected one of ${lib.concatStringsSep ", " producerKinds}";
        }
      ];
    }
  )) (value: !(value ? kind) || !(builtins.elem value.kind producerKinds));

  mkProducerType = types.oneOf [
    (producerTypeFor "calendar" calendarProducerType)
    (producerTypeFor "build-effect" buildEffectProducerType)
    (producerTypeFor "pool-reachability" poolReachabilityProducerType)
    (producerTypeFor "gh" ghProducerType)
    (producerTypeFor "events-dir" eventsDirProducerType)
    invalidProducerType
  ];

  mkUsageMeterType = types.submodule (
    { config, name, ... }: {
      options = {
        argv = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [
            "usage-meter"
            "poll"
          ];
          description = "Direct feeder argv; no shell string is accepted.";
        };
        pollIntervalSec = mkOption {
          type = types.ints.positive;
          default = 120;
          example = 300;
          description = "Meter observation and freshness cadence.";
        };
        budgetClass = mkOption {
          type = types.enum [ "programmatic" ];
          default = "programmatic";
          example = "programmatic";
          description = "The sole externally metered budget class.";
        };
        _tallyAssertions = internalAssertionsOption;
      };
      config._tallyAssertions = [
        {
          assertion = config.argv != [ ] && builtins.head config.argv != "";
          message = "tally usage meter ${name} requires a non-empty direct argv";
        }
      ];
    }
  );

  mkPoolType = types.submodule (
    { config, name, ... }: {
      options = {
        resource = mkOption {
          type = types.enum [
            "vram"
            "build-slot"
            "cpu-slot"
            "budget"
            "mutex"
          ];
          default = "vram";
          example = "build-slot";
          description = "Generalized scarce resource axis.";
        };
        capacity = mkOption {
          type = types.ints.positive;
          default = 1;
          example = 2;
          description = "Maximum simultaneous co-resident holders.";
        };
        budgetGb = mkOption {
          type = types.nullOr types.ints.positive;
          default = null;
          example = 24;
          description = "VRAM co-residency budget in GB, distinct from a window consumptionCap.";
        };
        predicate = mkOption {
          type = types.attrTag {
            co-residency = mkOption {
              type = types.submodule { options = { }; };
              default = { };
              example = { };
              description = "Counted co-residency admission.";
            };
            windowed-consumption = mkOption {
              type = types.submodule {
                options = {
                  windowSec = mkOption {
                    type = types.ints.positive;
                    default = 604800;
                    example = 86400;
                    description = "Rolling consumption window in seconds.";
                  };
                  consumptionCap = mkOption {
                    type = types.ints.positive;
                    default = 1;
                    example = 18000;
                    description = "Authoritative spend cap in the resource's native unit.";
                  };
                };
              };
              default = {
                windowSec = 604800;
                consumptionCap = 1;
              };
              example = {
                windowSec = 604800;
                consumptionCap = 18000;
              };
              description = "Rolling-window consumption admission.";
            };
          };
          default = {
            co-residency = { };
          };
          example.windowed-consumption = {
            windowSec = 604800;
            consumptionCap = 18000;
          };
          description = "Exactly one admission predicate tag.";
        };
        enforce = mkOption {
          type = types.enum [ "cooperative" ];
          default = "cooperative";
          example = "cooperative";
          description = "Portable cooperative enforcement; this is the complete accepted enum.";
        };
        hardPreempt = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Hard-reclaim a non-yielding lower-priority holder after yieldGraceSec.";
        };
        autoResume = mkOption {
          type = types.nullOr types.bool;
          default = null;
          example = true;
          description = "Override automatic same-row pool-return recovery; null uses the resource default.";
        };
        priority = mkOption {
          type = types.int;
          default = 0;
          example = -10;
          description = "Pool ordering rank; lower values are considered first.";
        };
        credentials = mkOption {
          type = credentialType;
          default = { };
          example.MODEL_TOKEN = "/run/credentials/model-token";
          description = "Credential references inherited by jobs leasing this pool.";
        };
        usageMeter = mkOption {
          type = types.nullOr mkUsageMeterType;
          default = null;
          example = {
            argv = [ "meter-feeder" ];
            pollIntervalSec = 120;
            budgetClass = "programmatic";
          };
          description = "Optional supervised external meter for a programmatic windowed budget.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = flatten [
        {
          assertion = config.resource != "mutex" || (config.capacity == 1 && config.predicate ? co-residency);
          message = "mutex pool ${name} must use co-residency with capacity 1";
        }
        {
          assertion =
            config.budgetGb == null
            || (config.resource == "vram" && config.capacity > 1 && config.predicate ? co-residency);
          message = "pool ${name} budgetGb is valid only for a co-resident vram pool with capacity > 1";
        }
        {
          assertion = !(config.predicate ? windowed-consumption) || config.resource == "budget";
          message = "pool ${name} windowed-consumption predicate requires resource = budget";
        }
        {
          assertion =
            config.usageMeter == null
            || (config.resource == "budget" && config.predicate ? windowed-consumption);
          message = "pool ${name} usageMeter requires a windowed-consumption budget pool";
        }
        (if config.usageMeter == null then [ ] else config.usageMeter._tallyAssertions)
        (mapAttrsToList (credential: _: {
          assertion = validCredentialName credential;
          message = "tally pool ${name} has invalid credential name ${credential}";
        }) config.credentials)
      ];
    }
  );

  mkOptions =
    {
      defaultPackage,
      defaultDataDir,
      defaultStateDir,
    }:
    {
      enable = mkOption {
        type = types.bool;
        default = false;
        example = true;
        description = "Enable the tally contention-and-proof daemon.";
      };
      package = mkOption {
        type = types.package;
        default = defaultPackage;
        defaultText = lib.literalExpression "inputs.tally.packages.${pkgs.stdenv.hostPlatform.system}.tally";
        example = lib.literalExpression "pkgs.tally";
        description = "Combined tally daemon and CLI package.";
      };
      installTallydSymlink = mkOption {
        type = types.bool;
        default = true;
        example = false;
        description = "Expose the tallyd argv[0] alias alongside the CLI.";
      };
      dataDir = mkOption {
        type = types.path;
        default = defaultDataDir;
        example = "/var/lib/tally/data";
        description = "Durable witness, attestation, and rebuildable TaskChampion data.";
      };
      stateDir = mkOption {
        type = types.path;
        default = defaultStateDir;
        example = "/var/lib/tally/state";
        description = "Mutable events, capture, exit-record, epoch, and producer state.";
      };
      journald.native = mkOption {
        type = types.bool;
        default = false;
        example = true;
        description = "Emit native journal-protocol datagrams instead of JSON stdout records.";
      };
      enqueue = mkOption {
        type = types.submodule {
          options = {
            depthCap = mkOption {
              type = types.ints.positive;
              default = 3;
              example = 5;
              description = "Maximum job-originated parent-to-child enqueue depth.";
            };
            fanoutCap = mkOption {
              type = types.ints.positive;
              default = 64;
              example = 16;
              description = "Maximum accepted children for one parent job.";
            };
            requireDedupKey = mkOption {
              type = types.bool;
              default = true;
              example = false;
              description = "Require dedupKey on job-originated enqueue.";
            };
          };
        };
        default = { };
        example = {
          depthCap = 4;
          fanoutCap = 32;
          requireDedupKey = true;
        };
        description = "Server-side enqueue capability guardrails.";
      };
      lease = mkOption {
        type = types.submodule {
          options = {
            graceSec = mkOption {
              type = types.ints.positive;
              default = 90;
              example = 120;
              description = "Epoch-keyed recovery grace bound.";
            };
            yieldPollSec = mkOption {
              type = types.ints.positive;
              default = 5;
              example = 2;
              description = "Cooperative-yield checkpoint cadence.";
            };
            yieldGraceSec = mkOption {
              type = types.ints.positive;
              default = 20;
              example = 30;
              description = "Grace before an opted-in hard reclaim.";
            };
          };
        };
        default = { };
        example = {
          graceSec = 90;
          yieldPollSec = 5;
          yieldGraceSec = 20;
        };
        description = "Local lease and cooperative-yield timing guardrails.";
      };
      pools = mkOption {
        type = types.attrsOf mkPoolType;
        default = { };
        example.worker-build = {
          resource = "build-slot";
          capacity = 1;
          enforce = "cooperative";
        };
        description = "Named local resource gates.";
      };
      producers = mkOption {
        type = types.attrsOf mkProducerType;
        default = { };
        example.daily = {
          kind = "calendar";
          onCalendar = "daily";
          enqueue = {
            argv = [ "daily-job" ];
            pool = "worker-build";
          };
        };
        description = "Five-kind producer registry with discriminator-specific submodules.";
      };
      adapters = mkOption {
        type = types.attrsOf mkAdapterType;
        default = { };
        defaultText = lib.literalExpression "inputs.tally.lib.adapters.presets";
        example.project-codex = {
          argv = [
            "codex"
            "exec"
            "-C"
            "/work/project"
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
        };
        description = "Open map of structured direct-argv adapters; extra adapters require no recompile.";
      };
    };

  renderEnqueue =
    enqueue:
    let
      pools = builtins.sort builtins.lessThan enqueue.pool;
    in
    {
      inherit (enqueue)
        argv
        adapter
        priority
        dedupKey
        evidence
        evidenceClass
        manifestHash
        consumptionEstimate
        runtimeMaxSec
        noEnqueue
        ;
      pool = if builtins.length pools == 1 then builtins.head pools else pools;
      credentials = mapAttrs (_: toString) enqueue.credentials;
    };

  renderProducer =
    _: producer:
    (
      {
        inherit (producer) kind;
        credentials = mapAttrs (_: toString) producer.credentials;
      }
      // (
        if !(builtins.elem producer.kind producerKinds) then
          { }
        else if producer.kind == "calendar" then
          {
            inherit (producer) onCalendar;
            enqueue = renderEnqueue producer.enqueue;
          }
        else if producer.kind == "events-dir" then
          {
            inherit (producer) pollIntervalSec;
          }
        else if producer.kind == "gh" then
          {
            inherit (producer)
              enable
              sources
              actorExclude
              pollIntervalSec
              postEvidence
              ;
            enqueue = renderEnqueue producer.enqueue;
          }
        else if producer.kind == "build-effect" then
          {
            inherit (producer) watch;
            path = toString producer.path;
            onKey = renderEnqueue producer.onKey;
          }
        else
          {
            inherit (producer) probePool intervalSec hysteresis;
            onLost = if producer.onLost == null then null else renderEnqueue producer.onLost;
            onReturn = if producer.onReturn == null then null else renderEnqueue producer.onReturn;
            onReturnAttest =
              if producer.onReturnAttest == null then null else renderEnqueue producer.onReturnAttest;
          }
      )
    );

  renderAdapter = _: adapter: {
    inherit (adapter)
      argv
      resume
      yieldHook
      env
      extraConfig
      ;
    scrape = mapAttrs (_: capture: {
      inherit (capture) stream mode pattern;
    }) adapter.scrape;
  };

  renderPool = _: pool: {
    inherit (pool)
      resource
      capacity
      budgetGb
      predicate
      enforce
      hardPreempt
      autoResume
      priority
      ;
    credentials = mapAttrs (_: toString) pool.credentials;
    usageMeter =
      if pool.usageMeter == null then
        null
      else
        {
          inherit (pool.usageMeter) argv pollIntervalSec budgetClass;
        };
  };

  mkRuntimeConfig = cfg: {
    enqueue = {
      inherit (cfg.enqueue) depthCap fanoutCap requireDedupKey;
    };
    lease = {
      inherit (cfg.lease) graceSec yieldPollSec yieldGraceSec;
    };
    pools = mapAttrs renderPool cfg.pools;
    producers = mapAttrs renderProducer cfg.producers;
    adapters = mapAttrs renderAdapter cfg.adapters;
    journald = { inherit (cfg.journald) native; };
  };

  producerEnqueues =
    producer:
    if producer.kind == "calendar" || producer.kind == "gh" then
      [ producer.enqueue ]
    else if producer.kind == "build-effect" then
      [ producer.onKey ]
    else if producer.kind == "pool-reachability" then
      builtins.filter (value: value != null) [
        producer.onLost
        producer.onReturn
        producer.onReturnAttest
      ]
    else
      [ ];

  mkAssertions =
    cfg:
    flatten [
      (mapAttrsToList (name: pool: [
        {
          assertion = validComponent name;
          message = "tally pool name ${name} is not a safe unit/file component";
        }
        pool._tallyAssertions
      ]) cfg.pools)
      (mapAttrsToList (name: adapter: [
        {
          assertion = name != "" && !lib.hasInfix "\u0000" name;
          message = "tally adapter names must be non-empty and contain no NUL bytes";
        }
        adapter._tallyAssertions
      ]) cfg.adapters)
      (mapAttrsToList (name: producer: [
        {
          assertion = validComponent name;
          message = "tally producer name ${name} is not a safe unit/file component";
        }
        producer._tallyAssertions
        (
          if producer.kind == "pool-reachability" then
            {
              assertion = builtins.hasAttr producer.probePool cfg.pools;
              message = "tally producer ${name} references unknown probePool ${producer.probePool}";
            }
          else
            [ ]
        )
        (map (enqueue:
          (map (pool: {
            assertion = builtins.hasAttr pool cfg.pools;
            message = "tally producer ${name} references unknown pool ${pool}";
          }) enqueue.pool)
          ++ [
            {
              assertion = builtins.hasAttr enqueue.adapter cfg.adapters;
              message = "tally producer ${name} references unknown adapter ${enqueue.adapter}";
            }
          ]
        ) (producerEnqueues producer))
      ]) cfg.producers)
      (
        let
          owners = mapAttrsToList (
            name: producer:
            if producer.kind == "pool-reachability" then
              {
                inherit name;
                pool = producer.probePool;
              }
            else
              null
          ) cfg.producers;
          activeOwners = builtins.filter (owner: owner != null) owners;
        in
        map (pool: {
          assertion = builtins.length (builtins.filter (owner: owner.pool == pool) activeOwners) <= 1;
          message = "more than one pool-reachability producer owns pool ${pool}";
        }) (unique (map (owner: owner.pool) activeOwners))
      )
    ];

  mkCheckedConfig =
    cfg:
    let
      rendered = pkgs.writeText "tally-config.json" (builtins.toJSON (mkRuntimeConfig cfg));
      expectedPriorityRanks = pkgs.writeText "tally-priority-ranks.json" (builtins.toJSON priorityRanks);
    in
    pkgs.runCommand "tally-checked-config.json"
      {
        nativeBuildInputs = [
          cfg.package
          pkgs.jq
        ];
      }
      ''
        contract="$(${lib.getExe cfg.package} --mode check-config --config ${rendered})"
        printf '%s\n' "$contract" | jq -e --slurpfile expected ${expectedPriorityRanks} \
          '.configuration == "valid" and .priorityRanks == $expected[0]' >/dev/null
        cp ${rendered} "$out"
      '';

  mkInstalledPackage =
    cfg:
    if cfg.installTallydSymlink then
      cfg.package
    else
      pkgs.runCommand "tally-cli-without-tallyd" { } ''
        mkdir -p "$out/bin"
        ln -s ${lib.getExe cfg.package} "$out/bin/tally"
      '';

  mkWitnessEmitter =
    cfg:
    import ../lib/witness-emitter.nix {
      inherit lib pkgs;
      tallyPackage = cfg.package;
    };

  meterEventPath =
    stateDir: pool: "${toString stateDir}/meters/${builtins.hashString "sha256" pool}.json";
in
{
  inherit
    adapterLibrary
    adapterDefaults
    meterEventPath
    mkAssertions
    mkCheckedConfig
    mkInstalledPackage
    mkOptions
    mkRuntimeConfig
    mkWitnessEmitter
    priorityRanks
    renderEnqueue
    ;
}
