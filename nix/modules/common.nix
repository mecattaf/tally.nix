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

  ghLogin = import ../lib/gh-login.nix;

  specBuildFlow = ../../examples/flows/spec-build.js;
  specBuildDriver = import ../lib/spec-build-driver.nix { inherit pkgs; };
  briefSentinel = "Read the file whose path is in the TALLY_BRIEF environment variable and execute the mission it contains. That brief is your complete instruction set.";

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

  storePathWithContext =
    value:
    let
      path = toString value;
      # Derivation strings already carry build context. Adding opaque path context
      # would instead require their outputs to exist during module evaluation.
      context = builtins.getContext path;
      matched = builtins.match "(/nix/store/[0-9a-z]{32}-[^/]+)(/.*)?" path;
    in
    if matched == null || context != { } then
      path
    else
      builtins.appendContext path {
        ${builtins.head matched} = {
          path = true;
        };
      };

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
            "jsonPathLast"
          ];
          default = "regex";
          example = "jsonPathLast";
          description = ''
            How tally extracts this capture. "regex" applies pattern matching
            with line anchors enabled for ^ and $, "jsonPath" returns JSONPath
            matches, and "jsonPathLast" applies the same RFC 9535 expression but
            keeps the last whole-stream match.
          '';
        };
        pattern = mkOption {
          type = types.str;
          default = "";
          example = "$..session_id";
          description = ''
            A non-empty regular expression or RFC 9535 JSONPath expression,
            interpreted according to this capture's mode.
          '';
        };
        counterScope = mkOption {
          type = types.nullOr (
            types.enum [
              "attempt"
              "session-cumulative"
            ]
          );
          default = null;
          example = "session-cumulative";
          description = ''
            Optional lifetime declaration for counters carried by the usage
            capture. When present it must agree with the adapter's
            usageCounterScope; other named captures cannot declare it.
          '';
        };
        fields = mkOption {
          type = types.attrsOf (types.listOf types.str);
          default = { };
          example.inputTokens = [ "input_tokens" ];
          description = ''
            Per-harness key mapping for this capture: each declared name maps
            to the ordered candidate paths that carry it inside the captured
            value. "$" (or the empty string) is the captured value itself;
            anything else is dot-separated object keys, with numeric segments
            indexing arrays. The first candidate that resolves to a non-null
            value wins.

            Declaring a harness's shape here is what keeps tally from learning
            it in Rust. The usage record reads inputTokens,
            inputTokensWithCacheRead, cacheReadTokens, cacheWriteTokens,
            outputTokens, reasoningTokens, totalTokens, and costUsd; an adapter
            that declares none of them, on a capture named usage, keeps the
            legacy reading of total_tokens, input_tokens, and output_tokens.
          '';
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = [
        {
          assertion = config.pattern != "";
          message = "tally adapter scrape ${name} requires a non-empty pattern";
        }
        {
          assertion = builtins.all (paths: paths != [ ]) (builtins.attrValues config.fields);
          message = "tally adapter scrape ${name} declares a field with no candidate path";
        }
        {
          assertion = config.counterScope == null || name == "usage";
          message = "tally adapter scrape counterScope is only valid on the usage capture";
        }
      ];
    }
  );

  mkTraceType = types.submodule {
    options = {
      stream = mkOption {
        type = types.enum [
          "stdout"
          "stderr"
        ];
        default = "stdout";
        description = "Provider-emitted stream exposed as an advisory trace.";
      };
      framing = mkOption {
        type = types.enum [ "json-lines" ];
        default = "json-lines";
        description = "Record framing for the declared provider trace.";
      };
    };
  };

  mkAdapterValueOverrideType =
    field:
    types.submodule (
      { config, ... }: {
        options = {
          argv = mkOption {
            type = types.listOf types.str;
            default = [ ];
            example =
              if field == "model" then
                [
                  "--model"
                  "%<value>%"
                ]
              else
                [
                  "-c"
                  "model_reasoning_effort=%<value>%"
                ];
            description = "Direct argv template for an authorized per-job ${field} value.";
          };
          allowedValues = mkOption {
            type = types.listOf types.str;
            default = [ ];
            example = if field == "model" then [ "gpt-5-codex" ] else [ "high" ];
            description = "Closed set of per-job ${field} values accepted by this adapter.";
          };
          _tallyAssertions = internalAssertionsOption;
        };

        config._tallyAssertions = [
          {
            assertion = config.argv != [ ] && lib.any (argument: lib.hasInfix "%<value>%" argument) config.argv;
            message = "tally adapter launch.${field}.argv must be non-empty and reference %<value>%";
          }
          {
            assertion =
              config.allowedValues != [ ]
              && builtins.length config.allowedValues == builtins.length (unique config.allowedValues)
              && lib.all (value: value != "") config.allowedValues;
            message = "tally adapter launch.${field}.allowedValues must contain unique non-empty values";
          }
        ];
      }
    );

  mkAdapterLaunchType = types.submodule (
    { config, ... }: {
      options = {
        allowPrePromptArgv = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Authorize direct per-job argv insertion before the adapter's final -- delimiter.";
        };
        rejectOptionLikeWorkloadHead = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = ''
            Refuse a non-empty first workload argv element beginning with a
            dash before launch or resume composition. False preserves the
            workload as opaque positional data.
          '';
        };
        resumeOptionsBeforeCapture = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "sessionRef";
          description = ''
            On resume, insert authorized adapter options immediately before
            the argv element containing this capture placeholder. This keeps
            provider options ahead of a positional session identifier.
          '';
        };
        cwdArgv = mkOption {
          type = types.nullOr (types.listOf types.str);
          default = null;
          example = [
            "-C"
            "%<cwd>%"
          ];
          description = "Optional direct argv template used to pass a job cwd to the adapter.";
        };
        approvalPolicies = mkOption {
          type = types.attrsOf (types.listOf types.str);
          default = { };
          # The published option reference is where a consumer copies from, so
          # this example must be argv the target binary accepts. A codex-family
          # agent takes its approval policy as a config override: the top-level
          # --ask-for-approval flag is rejected by `codex exec`, which is the
          # subcommand every agent adapter here invokes.
          example.never = [
            "-c"
            "approval_policy=\"never\""
          ];
          description = "Named approval policies mapped to exact direct argv fragments.";
        };
        sandboxPolicies = mkOption {
          type = types.attrsOf (types.listOf types.str);
          default = { };
          example.danger-full-access = [
            "--sandbox"
            "danger-full-access"
          ];
          description = "Named sandbox policies mapped to exact direct argv fragments.";
        };
        commitCapableSandboxPolicies = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "danger-full-access" ];
          description = ''
            Subset of sandboxPolicies under which this adapter's agent can
            create a git commit. Leave empty to declare nothing; declare it to
            make a campaign whose implementation node cannot commit a refusal
            at evaluation time rather than a failure mid-run.
          '';
        };
        model = mkOption {
          type = types.nullOr (mkAdapterValueOverrideType "model");
          default = null;
          description = "Optional closed authorization for per-job model overrides.";
        };
        effort = mkOption {
          type = types.nullOr (mkAdapterValueOverrideType "effort");
          default = null;
          description = "Optional closed authorization for per-job effort overrides.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = flatten [
        {
          assertion =
            config.cwdArgv == null
            || (config.cwdArgv != [ ] && lib.any (argument: lib.hasInfix "%<cwd>%" argument) config.cwdArgv);
          message = "tally adapter launch.cwdArgv must be null or a non-empty argv referencing %<cwd>%";
        }
        (mapAttrsToList (policy: _: {
          assertion = policy != "";
          message = "tally adapter approvalPolicies contains an empty policy name";
        }) config.approvalPolicies)
        (mapAttrsToList (policy: _: {
          assertion = policy != "";
          message = "tally adapter sandboxPolicies contains an empty policy name";
        }) config.sandboxPolicies)
        (map (policy: {
          assertion = builtins.hasAttr policy config.sandboxPolicies;
          message = "tally adapter commitCapableSandboxPolicies names ${policy}, which sandboxPolicies does not declare";
        }) config.commitCapableSandboxPolicies)
        (if config.model == null then [ ] else config.model._tallyAssertions)
        (if config.effort == null then [ ] else config.effort._tallyAssertions)
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
        resumeRequiresLaunchCwd = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = ''
            Declare that this adapter's harness resolves a session by the
            directory the session was launched in, so a resume run anywhere
            else reaches no session. tally then refuses a continuation whose
            working directory differs from the recorded launch directory,
            naming both. Leave false to declare nothing: tally enforces
            nothing, which is not a claim that cross-directory resume is safe
            for that harness.
          '';
        };
        usageCounterScope = mkOption {
          type = types.enum [
            "attempt"
            "session-cumulative"
          ];
          default = "attempt";
          example = "session-cumulative";
          description = ''
            Lifetime of the harness usage counters. "attempt" means every
            invocation starts from zero. "session-cumulative" means a resume
            inherits the session counters, so tally accounts the exact delta
            from the bound predecessor attempt and treats a missing or
            incompatible predecessor as unavailable rather than fresh usage.
          '';
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
        trace = mkOption {
          type = types.nullOr mkTraceType;
          default = null;
          description = "Optional provider trace declaration; arbitrary shell output is never inferred as a trace.";
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
        launch = mkOption {
          type = mkAdapterLaunchType;
          default = { };
          description = "Closed per-job direct-argv, cwd, policy, model, and effort authorization.";
        };
        hardening = mkOption {
          type = types.nullOr (
            types.enum [
              "production"
              "strict"
              "workspace"
              "none"
            ]
          );
          default = null;
          example = "strict";
          description = ''
            Optional transient-unit hardening preset. Null preserves the
            compatibility behavior and renders no preset name; "none" is the
            explicit spelling of that behavior.
          '';
        };
        extraWritablePaths = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "/var/lib/tally-agent/.codex" ];
          description = ''
            Absolute paths added to the transient unit's ReadWritePaths. Keep
            this list minimal and provision each path for the daemon user.
          '';
        };
        skillBundle = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = lib.literalExpression "builtins.readFile ./agent-skill.md";
          description = ''
            Optional resolved skill or agent-definition content. tally hashes
            the exact UTF-8 bytes as sha256:<hex> for flow provenance. Resolve
            files at Nix evaluation time; tally never reads a skill file while
            replaying a flow.
          '';
        };
        skillRevision = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "review-agent-v3";
          description = ''
            Optional stable skill or agent-definition version/name identifier,
            copied verbatim into flow provenance when no bundle content is
            available.
          '';
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
          assertion = !config.resumeRequiresLaunchCwd || config.resume != null;
          message = "tally adapter ${name} resumeRequiresLaunchCwd requires a resume template to constrain";
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
        {
          assertion = config.skillBundle == null || config.skillRevision == null;
          message = "tally adapter ${name} skillBundle and skillRevision are mutually exclusive";
        }
        (map (path: {
          assertion = lib.hasPrefix "/" path && !(lib.hasInfix "%" path);
          message = "tally adapter ${name} extraWritablePaths entry ${path} must be absolute and contain no systemd specifier";
        }) config.extraWritablePaths)
        (mapAttrsToList (capture: value: value._tallyAssertions) config.scrape)
        (mapAttrsToList (environment: _: {
          assertion =
            validEnvironmentName environment
            && !(lib.hasPrefix "TALLY_" environment)
            && environment != "CREDENTIALS_DIRECTORY";
          message = "tally adapter ${name} environment name ${environment} is invalid or reserved";
        }) config.env)
        config.launch._tallyAssertions
      ];
    }
  );

  mkAdapterJobOptionsType = types.submodule (
    { config, name, ... }: {
      options = {
        prePromptArgv = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "--dangerously-bypass-approvals-and-sandbox" ];
          description = "Per-job direct argv inserted before the adapter's final -- delimiter.";
        };
        environment = mkOption {
          type = types.attrsOf types.str;
          default = { };
          example.NO_COLOR = "1";
          description = "Per-job non-reserved environment merged over the adapter environment.";
        };
        approvalPolicy = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "never";
          description = "Named approval policy authorized by the selected adapter.";
        };
        sandboxPolicy = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "danger-full-access";
          description = "Named sandbox policy authorized by the selected adapter.";
        };
        model = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "gpt-5-codex";
          description = "Per-job model override, accepted only from the adapter's closed allowlist.";
        };
        effort = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "high";
          description = "Per-job effort override, accepted only from the adapter's closed allowlist.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = flatten [
        (mapAttrsToList (environment: _: {
          assertion =
            validEnvironmentName environment
            && !(lib.hasPrefix "TALLY_" environment)
            && environment != "CREDENTIALS_DIRECTORY";
          message = "tally producer enqueue ${name} environment name ${environment} is invalid or reserved";
        }) config.environment)
        (map
          (field: {
            assertion = config.${field} == null || config.${field} != "";
            message = "tally producer enqueue ${name} ${field} must be null or non-empty";
          })
          [
            "approvalPolicy"
            "sandboxPolicy"
            "model"
            "effort"
          ]
        )
      ];
    }
  );

  mkWorkspaceMetadataType = types.submodule (
    { config, name, ... }: {
      options = {
        repo = mkOption {
          type = types.str;
          example = "mecattaf/tally.nix";
          description = "Stable repository identity.";
        };
        baseRev = mkOption {
          type = types.str;
          example = "origin/main";
          description = "Base revision from which this workspace was prepared.";
        };
        branch = mkOption {
          type = types.str;
          example = "wave-3-ergonomics";
          description = "Workspace branch identity.";
        };
        worktreePath = mkOption {
          type = types.str;
          example = "/worktrees/tally-wave-3";
          description = "Absolute worktree path recorded as job metadata.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = [
        {
          assertion = lib.all (value: value != "") [
            config.repo
            config.baseRev
            config.branch
            config.worktreePath
          ];
          message = "tally producer enqueue ${name} workspace fields must be non-empty";
        }
        {
          assertion = lib.hasPrefix "/" config.worktreePath && !(lib.hasInfix "%" config.worktreePath);
          message = "tally producer enqueue ${name} workspace.worktreePath must be absolute and contain no systemd specifier";
        }
      ];
    }
  );

  mkGateManifestType = types.submodule (
    { config, name, ... }: {
      options = {
        path = mkOption {
          type = types.str;
          example = "/worktrees/tally-wave-3/.tally/gates.json";
          description = "Absolute path to the versioned completion artifact written by the job.";
        };
        requiredGateIds = mkOption {
          type = types.listOf types.str;
          example = [
            "tests"
            "clippy"
          ];
          description = ''
            Non-empty, unique gate IDs that this explicitly declared manifest
            must contain exactly once. This field has no Nix default.
          '';
        };
        acceptancePolicy = mkOption {
          type = types.enum [
            "manual"
            "execution-and-gates"
          ];
          default = "manual";
          description = "Explicit policy that derives acceptance from execution and declared gates, or leaves it pending.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = [
        {
          assertion = lib.hasPrefix "/" config.path && !(lib.hasInfix "%" config.path);
          message = "tally producer enqueue ${name} gateManifest.path must be absolute and contain no systemd specifier";
        }
        {
          assertion =
            config.requiredGateIds != [ ]
            && builtins.length config.requiredGateIds == builtins.length (unique config.requiredGateIds)
            && lib.all (gate: gate != "") config.requiredGateIds;
          message = "tally producer enqueue ${name} gateManifest.requiredGateIds must contain unique non-empty IDs";
        }
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
        cwd = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "/worktrees/\${gh.repoName}";
          description = "Absolute job cwd; gh enqueue values may use documented origin placeholders.";
        };
        workspace = mkOption {
          type = types.nullOr mkWorkspaceMetadataType;
          default = null;
          description = "Optional durable repository/base/branch/worktree metadata.";
        };
        adapterOptions = mkOption {
          type = mkAdapterJobOptionsType;
          default = { };
          description = "Per-job adapter options constrained by the selected adapter.";
        };
        gateManifest = mkOption {
          type = types.nullOr mkGateManifestType;
          default = null;
          description = ''
            Optional versioned completion-artifact declaration and acceptance
            policy passed with this enqueue.
          '';
        };
        brief = mkOption {
          type = types.raw;
          default = null;
          example = {
            subject = "\${gh.url}";
          };
          description = ''
            Optional structured JSON input. The producer materializes it in the
            daemon's content-addressed store outside argv; jobs receive its path
            and identity as TALLY_BRIEF and TALLY_BRIEF_HASH. GitHub enqueue
            values may use documented origin placeholders.
          '';
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
        executor = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "worker";
          description = "Optional named daemonless execution target; null executes on the coordinator.";
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
          description = ''
            Admission priority for this job. "interrupt" ranks above "high",
            then "medium", then "low"; scheduling aging can promote a waiting
            row by one rank.
          '';
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
          description = ''
            Optional JSON value copied into durable evidence and witness
            records. tally does not interpret this application-defined value.
          '';
        };
        manifestHash = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "sha256:opaque-to-tally";
          description = ''
            Optional application-supplied manifest identity copied verbatim
            into durable evidence. tally neither computes nor verifies it.
          '';
        };
        consumptionEstimate = mkOption {
          type = types.nullOr types.ints.unsigned;
          default = null;
          example = 900;
          description = ''
            Authoritative non-negative charge supplied with this enqueue.
            Every request that names a windowed-consumption pool must provide
            an estimate; tally does not infer one from argv or adapter output.
          '';
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
          description = ''
            Refuse every child-enqueue attempt made by this job. Set this on
            leaf or advisory work that must not receive the normal one-hop
            enqueue capability.
          '';
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
          assertion = config.cwd == null || (lib.hasPrefix "/" config.cwd && !(lib.hasInfix "%" config.cwd));
          message = "tally producer enqueue ${name} cwd must be absolute and contain no systemd specifier";
        }
        {
          assertion = config.dedupKey == null || config.dedupKey != "";
          message = "tally producer enqueue ${name} dedupKey must be null or non-empty";
        }
        (mapAttrsToList (credential: _: {
          assertion = validCredentialName credential;
          message = "tally producer enqueue ${name} has invalid credential name ${credential}";
        }) config.credentials)
        (if config.workspace == null then [ ] else config.workspace._tallyAssertions)
        config.adapterOptions._tallyAssertions
        (if config.gateManifest == null then [ ] else config.gateManifest._tallyAssertions)
      ];
    }
  );

  mkExecutorType = types.submodule (
    { config, name, ... }: {
      options = {
        kind = mkOption {
          type = types.enum [ "ssh" ];
          default = "ssh";
          description = "Remote transport kind. SSH is the only supported fail-closed transport.";
        };
        host = mkOption {
          type = types.str;
          example = "worker.example.net";
          description = "Explicit OpenSSH destination host or IP literal.";
        };
        user = mkOption {
          type = types.str;
          example = "tally-worker";
          description = "Explicit remote login user.";
        };
        port = mkOption {
          type = types.port;
          default = 22;
          description = "OpenSSH destination port.";
        };
        sshProgram = mkOption {
          type = types.path;
          default = "${pkgs.openssh}/bin/ssh";
          defaultText = lib.literalExpression ''"${pkgs.openssh}/bin/ssh"'';
          description = "Absolute OpenSSH client path used with an empty config and explicit options.";
        };
        identityFile = mkOption {
          type = types.externalPath;
          example = "/run/credentials/tally-worker-key";
          description = "Absolute coordinator-side private-key path; no agent or ambient credential is used.";
        };
        knownHostsFile = mkOption {
          type = types.path;
          example = "/etc/ssh/tally-known-hosts";
          description = "Absolute pinned known-hosts path used with strict host-key checking.";
        };
        program = mkOption {
          type = types.str;
          example = "/run/current-system/sw/bin/tally";
          description = "Absolute tally executable path on the worker; only its fixed __remote-executor command is invoked.";
        };
        stateDir = mkOption {
          type = types.str;
          example = "/var/lib/tally-remote";
          description = "Absolute worker-side directory for transient-unit exit records and captures.";
        };
        connectTimeoutSec = mkOption {
          type = types.ints.positive;
          default = 10;
          description = "OpenSSH connection-establishment timeout in seconds.";
        };
        serverAliveIntervalSec = mkOption {
          type = types.ints.positive;
          default = 15;
          description = "OpenSSH server-alive interval used to detect transport loss.";
        };
        serverAliveCountMax = mkOption {
          type = types.ints.positive;
          default = 3;
          description = "Missed server-alive replies before reconnect and re-adoption.";
        };
        retryIntervalMs = mkOption {
          type = types.ints.positive;
          default = 1000;
          description = ''
            Delay in milliseconds between fail-closed transport retries. The
            module accepts values from 10 through 60000.
          '';
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = [
        {
          assertion =
            builtins.match "[A-Za-z0-9][A-Za-z0-9._:-]*" config.host != null
            && !(lib.hasPrefix "-" config.host);
          message = "tally executor ${name} host is not a safe explicit OpenSSH destination";
        }
        {
          assertion =
            builtins.match "[A-Za-z0-9][A-Za-z0-9_.-]*" config.user != null && !(lib.hasPrefix "-" config.user);
          message = "tally executor ${name} user is not a safe OpenSSH login";
        }
        {
          assertion = lib.hasPrefix "/" config.program && lib.hasPrefix "/" config.stateDir;
          message = "tally executor ${name} program and stateDir must be absolute worker paths";
        }
        {
          assertion = builtins.match "/[A-Za-z0-9/_+.,@=-]*" config.program != null;
          message = "tally executor ${name} program is unsafe for the fixed remote helper command";
        }
        {
          assertion = config.retryIntervalMs >= 10 && config.retryIntervalMs <= 60000;
          message = "tally executor ${name} retryIntervalMs must be in 10..60000";
        }
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
      selfDrain = mkOption {
        type = types.bool;
        default = true;
        example = false;
        description = ''
          Whether this producer renders its own drain unit and timer. The
          drain RPC always claims the whole events directory and only stamps
          the admission origin with the producer that called it, so a second
          drainer beside the shipped `tally-drain` timer buys no coverage: it
          costs one redundant unit and one redundant call per interval, and
          makes the durable admission origin depend on which timer won the
          race. Set this false for a registry entry that exists to declare the
          contract while `tally-drain` remains the single drainer.
        '';
      };
    } (_: _: [ ])
  );

  ghSourceConstraintsType = types.submodule (
    { config, name, ... }:
    {
      options = {
        repo = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "agency-agency/spec";
          description = "Optional single owner/repository identity constraint.";
        };
        repositories = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "agency-agency/spec" ];
          description = "Additional owner/repository identity constraints.";
        };
        owners = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "agency-agency" ];
          description = "Repository-owner identity constraints.";
        };
        labels = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "agency:codex-ready" ];
          description = "Labels all selected items must carry.";
        };
        state = mkOption {
          type = types.nullOr (
            types.enum [
              "open"
              "closed"
            ]
          );
          default = null;
          example = "open";
          description = "Optional exact item state.";
        };
        assignee = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "tally-bot";
          description = "Optional required assignee.";
        };
        kinds = mkOption {
          type = types.listOf (
            types.enum [
              "issue"
              "pull-request"
            ]
          );
          default = [ ];
          example = [ "pull-request" ];
          description = "Optional issue/pull-request kind filter.";
        };
        notificationReasons = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "mention" ];
          description = "Allowed notification reasons for a notifications source.";
        };
        query = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "draft:false";
          description = "Optional additional GitHub search query fragment.";
        };
        itemAllowlist = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "https://github.com/agency-agency/spec/issues/21" ];
          description = "Optional exact GitHub item URL allowlist for one-shot operation.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions =
        map
          (field: {
            assertion =
              builtins.length config.${field} == builtins.length (unique config.${field})
              && lib.all (value: value != "") config.${field};
            message = "GitHub source ${name} ${field} must contain unique non-empty values";
          })
          [
            "repositories"
            "owners"
            "labels"
            "kinds"
            "notificationReasons"
            "itemAllowlist"
          ];
    }
  );

  ghSourceType = types.submodule (
    { config, name, ... }:
    {
      options = {
        notifications = mkOption {
          type = types.nullOr ghSourceConstraintsType;
          default = null;
          example.repo = "agency-agency/spec";
          description = "Scoped GitHub notifications intake.";
        };
        search = mkOption {
          type = types.nullOr ghSourceConstraintsType;
          default = null;
          example = {
            repo = "agency-agency/spec";
            labels = [ "agency:codex-ready" ];
            state = "open";
          };
          description = "Scoped GitHub issue-search intake.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = flatten [
        {
          assertion = (config.notifications == null) != (config.search == null);
          message = "GitHub source ${name} requires exactly one of notifications or search";
        }
        {
          assertion = config.notifications == null || config.notifications.query == null;
          message = "GitHub notifications source ${name} cannot carry a search query";
        }
        {
          assertion = config.search == null || config.search.notificationReasons == [ ];
          message = "GitHub search source ${name} cannot carry notificationReasons";
        }
        (if config.notifications == null then [ ] else config.notifications._tallyAssertions)
        (if config.search == null then [ ] else config.search._tallyAssertions)
      ];
    }
  );

  ghTriggersType = types.submodule (
    { config, ... }:
    {
      options = {
        commandComments = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "/tally run" ];
          description = "Exact explicit slash-command comment grammar.";
        };
        mentions = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "@your-login run" ];
          description = ''
            Exact explicit mention-command grammar. Matched literally, but the
            comment that carries it at-mentions whoever it names on GitHub, so
            name an account that belongs to this deployment.
          '';
        };
        assignments = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "tally-bot" ];
          description = "Assignee values that trigger intake.";
        };
        labels = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "tally:run" ];
          description = "Label values that trigger intake.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions =
        map
          (field: {
            assertion =
              builtins.length config.${field} == builtins.length (unique config.${field})
              && lib.all (value: value != "") config.${field};
            message = "GitHub triggers.${field} must contain unique non-empty values";
          })
          [
            "commandComments"
            "mentions"
            "assignments"
            "labels"
          ];
    }
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
          type = types.listOf ghSourceType;
          default = [ ];
          example = [
            {
              search = {
                repo = "agency-agency/spec";
                labels = [ "agency:codex-ready" ];
                state = "open";
              };
            }
          ];
          description = "Explicit, identity-scoped GitHub intake sources.";
        };
        triggers = mkOption {
          type = ghTriggersType;
          default = { };
          example.commandComments = [ "/tally run" ];
          description = "Explicit GitHub comment, mention, assignment, and label triggers.";
        };
        actorExclude = mkOption {
          type = types.str;
          default = "self";
          example = "tally-bot";
          description = "Literal trigger actor refused by the GitHub intake narrower.";
        };
        allowSelfTriggered = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Explicitly allow a trigger whose actor is the authenticated GitHub identity.";
        };
        allowedActors = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "trusted-maintainer" ];
          description = "Optional external trigger-actor allowlist; authenticated self triggers are governed separately by allowSelfTriggered.";
        };
        pollIntervalSec = mkOption {
          type = types.ints.positive;
          default = 60;
          example = 120;
          description = "GitHub polling cadence.";
        };
        postReceipt = mkOption {
          type = types.bool;
          default = true;
          example = false;
          description = "Post one sticky acknowledgement for an accepted or filtered trigger; re-observing a recorded trigger stays producer-internal and is never published.";
        };
        postEvidence = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Post an idempotent evidence comment after a passing or reused verdict.";
        };
        postFailureEvidence = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Explicitly post one idempotent evidence comment for each failed attempt; disabled by default because failure metadata originates in private execution state.";
        };
        postFailureStderr = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Include a conservatively redacted, bounded stderr tail in explicitly enabled failure evidence; requires postFailureEvidence.";
        };
        postGateSummary = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Post the declared gate summary and derived acceptance fact.";
        };
        requestReview = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Request human review while semantic acceptance remains pending or rejected. Requires a non-empty reviewers list.";
        };
        reviewers = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "octocat" ];
          description = "GitHub logins to request review from. A pull request receives a real review request; an issue receives one fresh comment mentioning them.";
        };
        closeOnAcceptance = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Close only after the explicit acceptance policy derives accepted.";
        };
        neverMutate = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Absolute policy override that disables every GitHub acknowledgement, comment, review request, and close.";
        };
        closeOnPass = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Close the GitHub item after posting evidence for a passing or reused verdict.";
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
            assertion =
              builtins.length (map builtins.toJSON config.sources)
              == builtins.length (unique (map builtins.toJSON config.sources));
            message = "gh producer ${name} sources must not repeat an identical constraint set";
          }
          {
            assertion = config.actorExclude != "";
            message = "gh producer ${name} actorExclude must be non-empty";
          }
          {
            assertion =
              builtins.length config.allowedActors == builtins.length (unique config.allowedActors)
              && lib.all (actor: actor != "") config.allowedActors;
            message = "gh producer ${name} allowedActors must contain unique non-empty actors";
          }
          {
            assertion = !config.closeOnPass || config.postEvidence;
            message = "gh producer ${name} closeOnPass=true requires postEvidence=true";
          }
          {
            assertion = !config.requestReview || config.reviewers != [ ];
            message = "gh producer ${name} requestReview=true requires a non-empty reviewers list";
          }
          {
            # The same grammar the daemon enforces at config load, length bound
            # included. A login this accepted but the daemon rejected deployed
            # green and then killed the daemon it was deployed for.
            assertion =
              builtins.length config.reviewers == builtins.length (unique config.reviewers)
              && lib.all ghLogin.isValid config.reviewers;
            message = "gh producer ${name} reviewers must be unique GitHub logins";
          }
          {
            assertion = !config.postFailureStderr || config.postFailureEvidence;
            message = "gh producer ${name} postFailureStderr=true requires postFailureEvidence=true";
          }
          {
            assertion =
              !(config.postGateSummary || config.closeOnAcceptance) || config.enqueue.gateManifest != null;
            message = "gh producer ${name} postGateSummary/closeOnAcceptance requires enqueue.gateManifest";
          }
          (map (source: source._tallyAssertions) config.sources)
          config.triggers._tallyAssertions
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

  # `types.oneOf` deliberately does not merge the sub-options of its variants.
  # The runtime type stays discriminated and strict; the documentation builder
  # evaluates these same types separately so every supported producer field is
  # present in the generated reference.
  producerTypesForDocumentation = {
    calendar = calendarProducerType;
    "build-effect" = buildEffectProducerType;
    "pool-reachability" = poolReachabilityProducerType;
    gh = ghProducerType;
    "events-dir" = eventsDirProducerType;
  };

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

  # `null` is "the operator declared nothing", distinct from `"vram"` declared
  # explicitly. Every admission-relevant reading in this module — the shape
  # assertions below, and every other option that predates #382 — must keep
  # resolving an undeclared pool to `"vram"` exactly as it always has; this is
  # the Nix-side mirror of `PoolConfig::resource()` on the Rust side. Only the
  # rendered runtime config (`renderPool`) is allowed to see the raw `null`,
  # because that is the one place the distinction is meant to survive: the
  # daemon's own `gpuSeconds` gate reads "was `vram` declared", not "what does
  # this pool behave like".
  effectivePoolResource = pool: if pool.resource == null then "vram" else pool.resource;

  mkPoolType = types.submodule (
    { config, name, ... }: {
      options = {
        resource = mkOption {
          type = types.nullOr (
            types.enum [
              "vram"
              "build-slot"
              "cpu-slot"
              "slot"
              "budget"
              "mutex"
            ]
          );
          default = null;
          example = "build-slot";
          description = ''
            Resource accounted by this pool: memory co-residency ("vram"),
            counted build or CPU slots, a neutral counted concurrency lane
            for external or metered capacity ("slot"), rolling spend
            ("budget"), or a capacity-one exclusion lock ("mutex").

            Defaults to unset (`null`), meaning undeclared: every admission
            decision this module or the daemon makes still treats an
            undeclared pool as "vram", exactly as before this option gained a
            null state. The one exception is the daemon's witnessed
            `gpuSeconds` figure — that field is filled only for a pool that
            declares `resource = "vram"` explicitly, never for one that says
            nothing, so an operator who never intends a GPU pool never sees
            fabricated-looking GPU-seconds on a host with no GPU.
          '';
        };
        capacity = mkOption {
          type = types.ints.positive;
          default = 1;
          example = 2;
          description = ''
            Maximum concurrent holders for co-residency admission. Mutex pools
            must keep this at one; windowed-consumption admission uses its
            consumption cap instead.
          '';
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
                    description = ''
                      Authoritative spend cap in the resource's native unit. For a
                      windowed budget pool without usageMeter, the built-in adapter
                      usage feeder denominates this cap in tokens.
                    '';
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
              description = ''
                Admit only when the supplied consumption estimate fits beneath
                the configured cap over the rolling window. Requests without
                an estimate are rejected.
              '';
            };
          };
          default = {
            co-residency = { };
          };
          example.windowed-consumption = {
            windowSec = 604800;
            consumptionCap = 18000;
          };
          description = ''
            Exactly one admission algorithm. Use "co-residency" for counted
            holders, or "windowed-consumption" with resource = "budget" for a
            rolling spend limit.
          '';
        };
        enforce = mkOption {
          type = types.enum [ "cooperative" ];
          default = "cooperative";
          example = "cooperative";
          description = ''
            Enforcement implementation. "cooperative" is the only accepted
            value; dmem, serving-slice, and patched-systemd modes are not part
            of the shipped module.
          '';
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
          description = ''
            Optional supervised feeder for observed usage in a programmatic
            windowed budget. This is valid only on resource = "budget" with
            the windowed-consumption predicate, and only Home Manager renders
            the feeder unit.
          '';
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = flatten [
        {
          assertion =
            effectivePoolResource config != "mutex"
            || (config.capacity == 1 && config.predicate ? co-residency);
          message = "mutex pool ${name} must use co-residency with capacity 1";
        }
        {
          assertion =
            config.budgetGb == null
            || (
              effectivePoolResource config == "vram" && config.capacity > 1 && config.predicate ? co-residency
            );
          message = "pool ${name} budgetGb is valid only for a co-resident vram pool with capacity > 1";
        }
        {
          assertion = !(config.predicate ? windowed-consumption) || effectivePoolResource config == "budget";
          message = "pool ${name} windowed-consumption predicate requires resource = budget";
        }
        {
          assertion =
            config.usageMeter == null
            || (effectivePoolResource config == "budget" && config.predicate ? windowed-consumption);
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

  mkFlowType = types.submodule (
    {
      config,
      name,
      options,
      ...
    }:
    {
      options = {
        script = mkOption {
          type = types.path;
          example = lib.literalExpression "./flows/nightly.js";
          description = "Flow script store path; its content hash is the scriptHash identity.";
        };
        onCalendar = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "daily";
          description = "Optional systemd calendar expression; null registers without scheduling.";
        };
        args = mkOption {
          type = types.attrs;
          default = { };
          example.repository = "mecattaf/tally.nix";
          description = "JSON-serializable flow arguments validated against meta.argsSchema.";
        };
        priority = mkOption {
          type = types.enum [
            "interrupt"
            "high"
            "medium"
            "low"
          ];
          default = "low";
          example = "medium";
          description = "Priority of the runner job; flow nodes declare their own priorities.";
        };
        dedupKey = mkOption {
          type = types.str;
          default = "flow-${name}-%Y-%m-%d";
          example = "monthly-review-%Y-%m";
          description = "Strftime-expanded existence key for scheduled flow runs.";
        };
        runtimeMaxSec = mkOption {
          type = types.nullOr types.ints.positive;
          default = 43200;
          example = 7200;
          description = "Optional RuntimeMaxSec watchdog for the runner job.";
        };
        evidence = mkOption {
          type = types.listOf types.str;
          default = [ "exit:0" ];
          example = [
            "exit:0"
            "artifact:/var/lib/review/receipt.json"
            "hash:sha256"
          ];
          description = "Canonical evidence specifications for the flow runner.";
        };
        maxNodes = mkOption {
          type = types.ints.positive;
          default = 1000;
          example = 200;
          description = "Per-run node backstop; must cover meta.maxNodes when declared.";
        };
        catalog = mkOption {
          type = types.nullOr types.path;
          default = null;
          example = lib.literalExpression "./catalog.json";
          description = ''
            Optional selector catalog used by flow validation and execution.
            It is required when meta declares selectors and may otherwise stay
            null.
          '';
        };
        workloadMutex = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "monthly-review";
          description = ''
            Optional capacity-1 mutex pool co-leased with the mandatory
            "flow" pool for the lifetime of the runner process. Manual runs
            of a flow that declares this option must enter through an admitted
            parent job carrying the same pool set.
          '';
        };
        budgetPool = mkOption {
          visible = false;
          type = types.unspecified;
        };
        extraEnv = mkOption {
          type = types.attrsOf types.str;
          default = { };
          example.NO_COLOR = "1";
          description = "Non-reserved environment added to the flow runner invocation.";
        };
        credentials = mkOption {
          type = credentialType;
          default = { };
          example.API_TOKEN = "/run/credentials/flow-api-token";
          description = "Credential references passed to the flow runner through LoadCredential.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = flatten [
        {
          assertion = config.onCalendar == null || config.onCalendar != "";
          message = "tally flow ${name} onCalendar must be null or non-empty";
        }
        {
          assertion = !options.budgetPool.isDefined;
          message = "services.tally.flows.${name}.budgetPool has been removed: flows are excluded from windowed-consumption admission by design; use node priorities for contention, or workloadMutex for a process-scoped capacity-1 runner mutex";
        }
        {
          assertion = config.workloadMutex == null || config.workloadMutex != "";
          message = "tally flow ${name} workloadMutex must be null or non-empty";
        }
        {
          assertion = config.dedupKey != "";
          message = "tally flow ${name} dedupKey must be non-empty";
        }
        (mapAttrsToList (environment: _: {
          assertion =
            validEnvironmentName environment
            && !(lib.hasPrefix "TALLY_" environment)
            && environment != "CREDENTIALS_DIRECTORY";
          message = "tally flow ${name} environment name ${environment} is invalid or reserved";
        }) config.extraEnv)
        (mapAttrsToList (credential: _: {
          assertion = validCredentialName credential;
          message = "tally flow ${name} has invalid credential name ${credential}";
        }) config.credentials)
      ];
    }
  );

  mkCampaignRepositoryType = types.submodule (
    { config, name, ... }: {
      options = {
        checkout = mkOption {
          type = types.str;
          example = "/srv/spec-repositories/crm";
          description = ''
            Absolute writable Git checkout used to read the frozen corpus and
            create per-task worktrees. This is operational state, not a Nix
            store source path.
          '';
        };
        baseBranch = mkOption {
          type = types.str;
          default = "main";
          example = "main";
          description = "Remote branch fetched immediately before every task worktree is prepared.";
        };
        remote = mkOption {
          type = types.str;
          default = "origin";
          example = "origin";
          description = "Named Git remote used for fetch and push.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = [
        {
          assertion = builtins.match "[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+" name != null;
          message = "tally campaign repository ${name} must use a safe owner/name identity";
        }
        {
          assertion = lib.hasPrefix "/" config.checkout && !(lib.hasInfix "%" config.checkout);
          message = "tally campaign repository ${name} checkout must be absolute and contain no systemd specifier";
        }
        {
          assertion = config.baseBranch != "" && !(lib.hasInfix "\u0000" config.baseBranch);
          message = "tally campaign repository ${name} baseBranch must be non-empty and contain no NUL byte";
        }
        {
          assertion = builtins.match "[A-Za-z0-9._-]+" config.remote != null;
          message = "tally campaign repository ${name} remote must be a safe Git remote name";
        }
      ];
    }
  );

  mkCampaignGateType = types.submodule (
    { config, name, ... }: {
      options = {
        kind = mkOption {
          type = types.enum [
            "command"
            "forbidPaths"
          ];
          example = "command";
          description = "Explicit gate implementation discriminator.";
        };
        id = mkOption {
          type = types.str;
          example = "tests";
          description = "Stable gate identifier used in the witnessed node key.";
        };
        preflightArgv = mkOption {
          type = types.nullOr (types.listOf types.str);
          default = null;
          example = [
            "sh"
            "-euc"
            "command -v go >/dev/null; command -v gcc >/dev/null; go env CGO_ENABLED >/dev/null"
          ];
          description = ''
            Base-safe direct argv executed before the first agent dispatch for
            a command gate. Required when kind = "command" and unavailable
            when kind = "forbidPaths". Declare the actual host and toolchain probe; do not
            make the post-change merge criterion pretend that unbuilt output
            exists.

            Every command gate's probe runs before any gate's argv is witnessed
            on that lane, so a probe may assume the pristine fetched base and may
            assert that the output its own gate tests for is absent there.
          '';
        };
        argv = mkOption {
          type = types.nullOr (types.listOf types.str);
          default = null;
          example = [
            "go"
            "test"
            "./..."
          ];
          description = ''
            Direct argv executed in each task worktree after the agent exits
            successfully. Required when kind = "command" and unavailable when
            kind = "forbidPaths".

            It also runs once per pass on the isolated pristine preflight
            worktree, before any agent is dispatched, as a non-gating
            preflight-witness node: every command gate's base-safe preflightArgv
            probe runs first, and if all pass, every gate's argv then runs there
            in declaration order. Those runs decide nothing -- their verdicts are
            discarded and a red one never stops a pass -- but they do execute, so
            they are the reason the exact merge criterion is witnessed on the
            real host at t=0 instead of one agent cycle later. Preflight stops
            once the campaign's first pull request is merged. A merge criterion
            that must never run against an unbuilt base -- one that deploys,
            publishes, or mutates shared state rather than reading the tree --
            belongs in a checkpoint node, not in a command gate.
          '';
        };
        forbidPaths = mkOption {
          type = types.nullOr (types.listOf types.str);
          default = null;
          example = [
            "*.db"
            "*.db-wal"
            "*.db-shm"
            "*.sqlite*"
          ];
          description = ''
            Repository-relative path globs forbidden in the committed task
            branch history. A slashless glob matches a basename at any depth;
            slashful globs are rooted at the repository. Required when kind =
            "forbidPaths" and unavailable when kind = "command".
          '';
        };
        runtimeMaxSec = mkOption {
          type = types.ints.positive;
          default = 900;
          example = 300;
          description = ''
            Gate deadline. Command gates share it between the base preflight
            probe, the non-gating preflight witness of the same gate's argv,
            and each post-change invocation; constraint gates use it for the
            packaged driver invocation.
          '';
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = [
        {
          assertion = validComponent config.id;
          message = "tally campaign gate ${name} id ${config.id} is not a safe node-key component";
        }
        {
          assertion =
            (
              config.kind == "command"
              && config.argv != null
              && config.preflightArgv != null
              && config.forbidPaths == null
            )
            || (
              config.kind == "forbidPaths"
              && config.argv == null
              && config.preflightArgv == null
              && config.forbidPaths != null
            );
          message = "tally campaign gate ${name} fields must agree with kind: command requires preflightArgv and argv; forbidPaths requires only forbidPaths";
        }
        {
          assertion =
            config.preflightArgv == null
            || (config.preflightArgv != [ ] && builtins.head config.preflightArgv != "");
          message = "tally campaign gate ${name} preflightArgv must start with a non-empty executable";
        }
        {
          assertion =
            config.preflightArgv == null
            || lib.all (argument: !(lib.hasInfix "\u0000" argument)) config.preflightArgv;
          message = "tally campaign gate ${name} preflightArgv must not contain NUL bytes";
        }
        {
          assertion = config.argv == null || (config.argv != [ ] && builtins.head config.argv != "");
          message = "tally campaign gate ${name} argv must start with a non-empty executable";
        }
        {
          assertion =
            config.argv == null || lib.all (argument: !(lib.hasInfix "\u0000" argument)) config.argv;
          message = "tally campaign gate ${name} argv must not contain NUL bytes";
        }
        {
          assertion =
            config.forbidPaths == null
            || (
              config.forbidPaths != [ ]
              && builtins.length config.forbidPaths <= 128
              && builtins.length config.forbidPaths == builtins.length (unique config.forbidPaths)
              && lib.all (
                pattern:
                pattern != ""
                && builtins.stringLength pattern <= 1024
                && !(lib.hasPrefix "/" pattern)
                && !(builtins.elem ".." (lib.splitString "/" pattern))
                && !(lib.hasInfix "\u0000" pattern)
                && lib.all (component: !(lib.hasInfix "**" component) || component == "**") (
                  lib.splitString "/" pattern
                )
              ) config.forbidPaths
            );
          message = "tally campaign gate ${name} forbidPaths must contain 1-128 unique relative globs without '..' or NUL bytes and use '**' only as a complete path component";
        }
      ];
    }
  );

  mkCampaignType = types.submodule (
    { config, name, ... }: {
      options = {
        enable = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Render this spec-driven build campaign.";
        };
        repositories = mkOption {
          type = types.attrsOf mkCampaignRepositoryType;
          default = { };
          example."mecattaf/crm".checkout = "/srv/spec-repositories/crm";
          description = ''
            GitHub repository identities accepted by this campaign, mapped to
            the writable local checkouts that contain their frozen specs.
          '';
        };
        codeRepository = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "mecattaf/crm";
          description = ''
            Repository this campaign cuts lanes on, publishes branches to, and
            merges pull requests into. Names an entry of `repositories`. Null
            means the repository the campaign issue was read from, which is the
            single-repository shape.
          '';
        };
        specRepository = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "mecattaf/crm-spec";
          description = ''
            Repository the worklist artifact is read from, at the revision the
            pass pins. Names an entry of `repositories`. Null means the
            repository the campaign issue was read from.
          '';
        };
        issueRepository = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "mecattaf/crm-spec";
          description = ''
            Repository carrying the campaign issue thread, its task sub-issues,
            and every machine receipt. Names an entry of `repositories`. Null
            means `specRepository`, which in turn defaults to the repository
            the campaign issue was read from.
          '';
        };
        label = mkOption {
          type = types.str;
          default = "campaign";
          example = "spec-build";
          description = "Label required on an open campaign issue.";
        };
        mention = mkOption {
          type = types.str;
          default = "@tally build";
          example = "@your-login build";
          description = ''
            Exact mention comment that starts one bounded campaign reconcile
            pass. tally matches this string literally, but the comment carrying
            it is a real comment on a real issue, so GitHub resolves every
            `@name` in it. Name your own login — or the bot's, under a bot
            identity — and never a third party's. **Override the default**: it
            names an unrelated real GitHub account, which every trigger on a
            campaign that keeps it notifies. Nothing about the mechanism
            requires the mention form; a token with no `@` is a perfectly good
            trigger grammar.
          '';
        };
        allowSelfTriggered = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = ''
            Explicitly allow the operator-facing campaign mention when its
            actor is the authenticated GitHub identity. This governs the human
            trigger surface only: a campaign's own next-pass continuation is a
            local events-directory drop admitted by the shipped drain, not a
            comment this or any other gh producer polls back.
          '';
        };
        allowedActors = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "trusted-maintainer" ];
          description = "Optional external trigger-actor allowlist inherited by both rendered GitHub producers.";
        };
        pollIntervalSec = mkOption {
          type = types.ints.positive;
          default = 60;
          description = "GitHub campaign producer polling cadence.";
        };
        postFailureEvidence = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Explicitly post one public failure receipt for each failed campaign attempt; disabled by default.";
        };
        postFailureStderr = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Include the conservatively redacted 2 KiB stderr tail in public campaign failure receipts; requires postFailureEvidence.";
        };
        worklist = mkOption {
          type = types.str;
          default = "specs/*/tasks.json";
          example = "specs/001-crm/tasks.json";
          description = "Relative glob that must resolve to exactly one versioned JSON worklist.";
        };
        maxTasks = mkOption {
          type = types.ints.between 1 128;
          default = 64;
          example = 24;
          description = "Maximum implementation and checkpoint nodes accepted from the witnessed worklist.";
        };
        maxParallel = mkOption {
          type = types.ints.between 1 128;
          default = 1;
          example = 4;
          description = ''
            Maximum dependency-ready nodes dispatched by one stateless
            reconcile pass. Values above one require every implementation
            node to declare non-empty conflictDomains; checkpoints are
            non-mutating and therefore have no conflict domains.
          '';
        };
        gates = mkOption {
          type = types.listOf mkCampaignGateType;
          default = [ ];
          example = [
            {
              kind = "command";
              id = "tests";
              preflightArgv = [
                "sh"
                "-euc"
                "command -v go >/dev/null; command -v gcc >/dev/null; go env CGO_ENABLED >/dev/null"
              ];
              argv = [
                "go"
                "test"
                "./..."
              ];
            }
          ];
          description = ''
            Ordered command or built-in constraint gates run for every task.
            Command gates declare separate base-safe preflightArgv and
            post-change argv values with one deadline. Their preflights run on
            an isolated pristine fetched base before the first frontier agent,
            followed there by a non-gating witness of each gate's own argv.
            forbidPaths constraints begin after the agent creates a committed
            diff. Every post-change gate runs again after a base-changing
            rebase; a failure blocks that task lane without starving successful
            conflict-disjoint siblings.
          '';
        };
        mergeMethod = mkOption {
          type = types.enum [
            "merge"
            "squash"
          ];
          default = "squash";
          example = "merge";
          description = ''
            How the merge node integrates a completed task. The campaign
            default is squash: the exposed surface a campaign should leave
            behind is one conventional commit per task, authored at the publish
            boundary, not a merge commit carrying a template message. Under
            squash the merge node proves completion from the pull request's
            merge commit rather than from the task head, which a squash never
            makes an ancestor of the base branch.
          '';
        };
        gitAiBinding = mkOption {
          type = types.enum [
            "off"
            "advisory"
            "required"
          ];
          default = "off";
          example = "advisory";
          description = ''
            Whether the merge node binds Git AI authorship on the commit it
            just integrated. A squash mints a commit the forge authored, and
            authorship notes are minted where a commit is made, so a
            forge-side squash arrives with no note at all. Under `advisory`
            and `required` the merge node reconstructs that squash in the
            campaign checkout -- the one place that still holds the task
            branch's checkpoints -- proves the reconstruction is the same tree
            on the same parent, copies the minted note onto the integrated
            commit, and publishes refs/notes/ai to the campaign remote.

            Only the integrated commit's note is published. The campaign
            checkout's own refs/notes/ai accumulates a note for every commit
            the shared checkout ever made, so the node assembles a scratch ref
            from the remote's tip plus that one entry and pushes that. A
            remote already carrying a *different* note for the same commit is
            reported as a typed `conflict` and nothing is written: two git-ai
            authorship records cannot be merged without destroying both.

            `off` is the shipped state and binds nothing. `advisory` records
            every outcome as a merge receipt and never fails the node -- not
            as a promise but as an enforced property, because the merge has
            already landed irreversibly by the time the binding runs. It is
            the posture to arm first: an unprovisioned host and a squash that
            lost its attribution produce identical evidence, so only real
            squash merges can show that the binding works. `required` turns
            any outcome other than a published bound note into a failed merge
            node and couples every campaign merge to the externally
            provisioned binary's version, which tally.nix does not ship.

            This is the merge node's own posture and is independent of
            services.tally.gitAi, which governs the daemon's settlement
            barrier at code-result completion.
          '';
        };
        gitAiAwaitSec = mkOption {
          type = types.ints.positive;
          default = 60;
          example = 120;
          description = ''
            How long the merge node may wait on git-ai's settlement barrier
            before reporting the binding unsettled. The barrier runs inside
            the merge node, so this budget and driverRuntimeMaxSec are not
            independent: a campaign whose node deadline does not comfortably
            exceed this one is killed mid-wait on every task and reports a
            node timeout instead of a binding receipt. Whenever gitAiBinding
            is not `off`, evaluation refuses driverRuntimeMaxSec below twice
            this value.

            The measurement in doc/src/flows/git-ai-squash-fidelity.md is the
            scale to size it against: `git-ai await` costs roughly 18 seconds
            on a repository with nothing outstanding.
          '';
        };
        agent = mkOption {
          type = types.str;
          default = "codex";
          example = "codex";
          description = "Configured adapter used by implementation and diagnosis nodes.";
        };
        steward = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "narrator";
          description = ''
            Configured adapter bound as this campaign's steward role. The
            steward narrates at the publication boundary: it proposes the
            conventional-commit message and pull-request prose, a deterministic
            validator accepts or refuses that text, and the node executes git.
            Null leaves the seam empty: publication text stays the
            brief-derived template and no model is called.

            The adapter entry's argv, env, and scrape.finalMessage are what the
            seam reads: which model answers, at which endpoint, with which
            credentials, and how its proposal is captured are adapter changes,
            not changes here and never values in this campaign's options. What
            the seam does not read is the adapter's per-job launch policies,
            hardening preset, and extraWritablePaths, because the narrator runs
            as a direct-argv subprocess of the publish node rather than as a
            tally job -- that is what keeps the seam free of flow nodes. An
            adapter that declares any of those is refused here rather than
            silently narrated without them.
          '';
        };
        stewardArgv = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "--narrate" ];
          description = ''
            Direct argv appended to the steward adapter's own argv for the
            narration call. The narration request arrives on stdin as JSON and
            the proposal is read back from the line matching the adapter's
            declared scrape.finalMessage regex, defaulting to the same
            `TALLY_FINAL_MESSAGE=` contract the shipped spec-build-driver
            adapter scrapes. Credentials belong in the adapter's env, never
            here: this argv is rendered verbatim into the campaign brief.
          '';
        };
        stewardRuntimeMaxSec = mkOption {
          type = types.ints.positive;
          default = 120;
          example = 300;
          description = ''
            Deadline for one steward narration call. A narrator that does not
            answer inside it counts as a failed attempt; two failures spend the
            slot and publication falls back to the template.
          '';
        };
        agentArgv = mkOption {
          type = types.listOf types.str;
          default = [ briefSentinel ];
          defaultText = lib.literalExpression "the tally structured-brief sentinel";
          example = [ "/srv/campaign-fixtures/agent" ];
          description = ''
            Direct argv appended to the selected adapter for implementation
            and diagnosis. Agent adapters should keep the default
            structured-brief sentinel; a shell fixture may name its executable
            directly.
          '';
        };
        agentModel = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "provider/model-1";
          description = ''
            Model this campaign dispatches its implementation and diagnosis
            nodes with, rendered as the job's adapter model option. Null leaves
            the adapter's own resolution alone.

            It is also the only model the campaign can honestly name: the merge
            node's `Assisted-by:` trailer points at the canonical model the
            daemon recorded for the witnessed attempt, and with no model
            recorded the node writes no trailer rather than inventing one.
          '';
        };
        agentPriority = mkOption {
          type = types.enum [
            "interrupt"
            "high"
            "medium"
            "low"
          ];
          default = "low";
          description = "Priority of each campaign implementation or diagnosis node.";
        };
        agentApprovalPolicy = mkOption {
          type = types.nullOr types.str;
          default = "never";
          example = "on-failure";
          description = ''
            Named adapter approval policy for implementation and diagnosis
            nodes. A campaign node runs unattended, so there is nobody to grant
            an escalation and the default declines to ask for one. Set null only
            when the selected adapter declares no approval policies.
          '';
        };
        agentSandboxPolicy = mkOption {
          type = types.nullOr types.str;
          default = "danger-full-access";
          example = "read-only";
          description = ''
            Named adapter sandbox policy for implementation nodes. An
            implementation node's obligation is a commit, and the default is the
            weakest shipped policy under which the codex adapter can reach git
            metadata to make one; a merely writable sandbox lets the agent do
            all of its work and then fail at publication. Names outside the
            adapter's commitCapableSandboxPolicies are refused here rather than
            mid-run. Set null only when the selected adapter declares no sandbox
            policies.
          '';
        };
        agentDiagnosisSandboxPolicy = mkOption {
          type = types.nullOr types.str;
          default = "read-only";
          example = "workspace-write";
          description = ''
            Named adapter sandbox policy for diagnosis nodes. Diagnosis briefs
            prohibit mutation, so the default holds the node to that obligation
            rather than inheriting the implementation node's writable policy.
            Set null only when the selected adapter declares no sandbox
            policies, or name a writable policy when the adapter refuses to
            read a worktree without one.
          '';
        };
        agentRuntimeMaxSec = mkOption {
          type = types.nullOr types.ints.positive;
          default = 14400;
          example = 21600;
          description = "Optional process deadline for each implementation or diagnosis node.";
        };
        driverRuntimeMaxSec = mkOption {
          type = types.ints.positive;
          default = 900;
          description = "Process deadline for each deterministic spec-build driver node.";
        };
        runtimeMaxSec = mkOption {
          type = types.nullOr types.ints.positive;
          default = null;
          example = 82800;
          description = ''
            Optional deadline for one bounded reconcile-pass runner. Null
            leaves the fixed 24-hour evaluator budget as its safety boundary.
            The deadline bounds one pass, not the campaign: durable completion
            lives in marked pull requests and checkpoint refs, steering in
            marked issue comments, and the next pass is admitted from a
            continuation event this run writes before it exits.
          '';
        };
        pool.name = mkOption {
          type = types.str;
          default = "campaign";
          example = "campaign";
          description = "Capacity-1 mutex held for one bounded campaign reconcile pass.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = flatten [
        {
          assertion = !config.enable || config.repositories != { };
          message = "enabled tally campaign ${name} requires at least one repository";
        }
        # A role that names a repository the campaign never configured has no
        # checkout to read or write, and would only be discovered on the first
        # pass. Refuse it at evaluation instead.
        (map
          (role: {
            assertion = config.${role} == null || builtins.hasAttr config.${role} config.repositories;
            message = "tally campaign ${name} ${role} must name a configured repository";
          })
          [
            "codeRepository"
            "specRepository"
            "issueRepository"
          ]
        )
        {
          assertion = !config.enable || config.gates != [ ];
          message = "enabled tally campaign ${name} requires at least one deterministic gate";
        }
        {
          assertion = config.label != "";
          message = "tally campaign ${name} label must be non-empty";
        }
        {
          assertion = config.mention != "";
          message = "tally campaign ${name} mention must be non-empty";
        }
        {
          assertion = !config.postFailureStderr || config.postFailureEvidence;
          message = "tally campaign ${name} postFailureStderr=true requires postFailureEvidence=true";
        }
        {
          assertion = config.maxParallel <= config.maxTasks;
          message = "tally campaign ${name} maxParallel must not exceed maxTasks";
        }
        {
          assertion =
            config.worklist != ""
            && !(lib.hasPrefix "/" config.worklist)
            && !(builtins.elem ".." (lib.splitString "/" config.worklist));
          message = "tally campaign ${name} worklist must be a relative pattern without '..'";
        }
        {
          assertion = config.agent != "" && !(lib.hasInfix "\u0000" config.agent);
          message = "tally campaign ${name} agent must be non-empty and contain no NUL byte";
        }
        {
          assertion = config.agentArgv != [ ] && builtins.head config.agentArgv != "";
          message = "tally campaign ${name} agentArgv must start with a non-empty value";
        }
        {
          # The settlement barrier runs inside the merge node. Two numbers that
          # nothing relates produce a node killed mid-await on every task,
          # presenting as a timeout rather than as the binding receipt the
          # advisory posture exists to produce.
          assertion = config.gitAiBinding == "off" || config.driverRuntimeMaxSec >= 2 * config.gitAiAwaitSec;
          message = "tally campaign ${name} driverRuntimeMaxSec must be at least twice gitAiAwaitSec (${
            toString (2 * config.gitAiAwaitSec)
          }) while gitAiBinding is not off";
        }
        {
          assertion = config.steward == null || validComponent config.steward;
          message = "tally campaign ${name} steward must be null or a safe adapter name";
        }
        {
          assertion = config.steward != null || config.stewardArgv == [ ];
          message = "tally campaign ${name} stewardArgv requires a steward adapter";
        }
        {
          assertion = lib.all (item: item != "") config.stewardArgv;
          message = "tally campaign ${name} stewardArgv must contain non-empty values";
        }
        {
          assertion = config.agentApprovalPolicy == null || config.agentApprovalPolicy != "";
          message = "tally campaign ${name} agentApprovalPolicy must be null or non-empty";
        }
        {
          assertion = config.agentSandboxPolicy == null || config.agentSandboxPolicy != "";
          message = "tally campaign ${name} agentSandboxPolicy must be null or non-empty";
        }
        {
          assertion = config.agentDiagnosisSandboxPolicy == null || config.agentDiagnosisSandboxPolicy != "";
          message = "tally campaign ${name} agentDiagnosisSandboxPolicy must be null or non-empty";
        }
        {
          assertion = validComponent config.pool.name;
          message = "tally campaign ${name} pool.name ${config.pool.name} is not a safe component";
        }
        {
          assertion =
            !(builtins.elem config.pool.name [
              "flow"
              "build"
              "campaign-agent"
              "campaign-control"
            ]);
          message = "tally campaign ${name} pool.name must not use a reserved flow or campaign node pool";
        }
        {
          assertion =
            builtins.length config.allowedActors == builtins.length (unique config.allowedActors)
            && lib.all (actor: actor != "") config.allowedActors;
          message = "tally campaign ${name} allowedActors must contain unique non-empty actors";
        }
        {
          assertion =
            builtins.length (map (gate: gate.id) config.gates)
            == builtins.length (unique (map (gate: gate.id) config.gates));
          message = "tally campaign ${name} gate ids must be unique";
        }
        {
          assertion = builtins.length config.gates <= 16;
          message = "tally campaign ${name} supports at most 16 gates";
        }
        (mapAttrsToList (_: repository: repository._tallyAssertions) config.repositories)
        (map (gate: gate._tallyAssertions) config.gates)
      ];
    }
  );

  mkOptions =
    {
      defaultPackage,
      defaultDataDir,
      defaultStateDir,
      defaultDataDirText ? null,
      defaultStateDirText ? null,
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
      dataDir = mkOption (
        {
          type = types.path;
          default = defaultDataDir;
          example = "/var/lib/tally/data";
          description = "Durable witness, attestation, brief, and lifecycle data.";
        }
        // optionalAttrs (defaultDataDirText != null) {
          defaultText = defaultDataDirText;
        }
      );
      stateDir = mkOption (
        {
          type = types.path;
          default = defaultStateDir;
          example = "/var/lib/tally/state";
          description = "Mutable events, capture, exit-record, lease-epoch, and producer state.";
        }
        // optionalAttrs (defaultStateDirText != null) {
          defaultText = defaultStateDirText;
        }
      );
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
      transport.maxFrameBytes = mkOption {
        type = types.ints.positive;
        default = 16777216;
        example = 33554432;
        description = "Maximum local wire-frame size enforced on reads and writes.";
      };
      scheduling.agingThresholdSec = mkOption {
        type = types.ints.positive;
        default = 3600;
        example = 900;
        description = "Wait time before one-step priority aging applies.";
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
        description = "Coordinator lease and cooperative-yield timing guardrails.";
      };
      retention = mkOption {
        type = types.submodule {
          options = {
            enable = mkOption {
              type = types.bool;
              default = true;
              description = "Enable the age-based store-evidence retention timer.";
            };
            horizon = mkOption {
              type = types.str;
              default = "30d";
              example = "90d";
              description = "Systemd timespan for the witness-liveness retention floor.";
            };
            onCalendar = mkOption {
              type = types.str;
              default = "daily";
              example = "weekly";
              description = "Systemd calendar expression for store-evidence collection.";
            };
            captureArchiveHorizon = mkOption {
              type = types.str;
              default = "30d";
              example = "7d";
              description = ''
                Systemd timespan after which per-attempt capture archives under
                the state directory expire. Archives are replay material and are
                deliberately not pinned by the witness ledger: the witness record
                remains the durable evidence once an archive is pruned.
              '';
            };
            eventsDoneHorizon = mkOption {
              type = types.str;
              default = "180d";
              example = "1y";
              description = ''
                Systemd timespan after which consumed producer event files under
                events/done expire. This is the ingress audit trail, so it gets a
                longer horizon and no count bound.
              '';
            };
            eventsRejectedHorizon = mkOption {
              type = types.str;
              default = "30d";
              example = "7d";
              description = ''
                Systemd timespan after which rejected producer event files under
                events/rejected expire. This set is adversarially drivable, so it
                expires more aggressively than the audit trail.
              '';
            };
            eventsRejectedMaxCount = mkOption {
              type = types.ints.unsigned;
              default = 10000;
              example = 1000;
              description = ''
                Maximum retained files under events/rejected. Whichever of this
                bound and eventsRejectedHorizon is exceeded first prunes, oldest
                file first.
              '';
            };
            producerMarkerHorizon = mkOption {
              type = types.str;
              default = "180d";
              example = "90d";
              description = ''
                Systemd timespan after which the per-dispatch marker files under
                producers/gh-triggers, producers/gh-completed,
                producers/gh-comments, producers/gh-storage-warnings, and
                producers/gh-orphaned expire. Each of the first four makes one
                forge mutation idempotent; collecting one costs at most a
                re-publication that the marker scan on the thread already
                collapses, so the envelope matches the ingress audit trail
                rather than the shorter archive one. A gh-orphaned record
                guards nothing — it is the durable statement that one
                projection can never be applied, read only by the startup
                report and by "tally producer orphaned" — and it retires with
                the acknowledged event it describes, so keep this at or above
                eventsDoneHorizon unless a shorter report is worth losing the
                first-seen date.
              '';
            };
            lifecycleHorizon = mkOption {
              type = types.str;
              default = "30d";
              example = "90d";
              description = ''
                Minimum lifecycle-log history retained when byte-triggered
                online prefix compaction runs.
              '';
            };
            lifecycleMaxBytes = mkOption {
              type = types.ints.positive;
              default = 268435456;
              example = 67108864;
              description = ''
                Lifecycle-log size that triggers online prefix compaction.
                Recent records inside lifecycleHorizon remain even if the log
                stays above this size.
              '';
            };
          };
        };
        default = { };
        description = ''
          Age-based Nix GC-root retention with a live-witness floor, plus the
          content-addressed brief replay window and state-directory envelope for
          capture archives and ingress event files. One sweep and one timer;
          GC-root and brief-store locks close their respective admission races.
        '';
      };
      storage = mkOption {
        type = types.submodule {
          options = {
            pollIntervalSec = mkOption {
              type = types.ints.positive;
              default = 60;
              example = 15;
              description = "Daemon-owned storage measurement cadence in seconds.";
            };
            dataDir = mkOption {
              type = types.submodule {
                options = {
                  warningBytes = mkOption {
                    type = types.ints.positive;
                    default = 34359738368;
                    example = 8589934592;
                    description = "Allocated dataDir bytes that emit a durable warning.";
                  };
                  hardBytes = mkOption {
                    type = types.ints.positive;
                    default = 68719476736;
                    example = 17179869184;
                    description = "Allocated dataDir bytes that refuse new intake.";
                  };
                  warningFreeBytes = mkOption {
                    type = types.ints.positive;
                    default = 17179869184;
                    example = 34359738368;
                    description = "Available dataDir-filesystem bytes below which a durable early warning is emitted.";
                  };
                  minimumFreeBytes = mkOption {
                    type = types.ints.positive;
                    default = 8589934592;
                    example = 17179869184;
                    description = "Available bytes required on the dataDir filesystem before new intake is accepted.";
                  };
                };
              };
              default = { };
              description = "Budget for the witness, history, brief, and projection store.";
            };
            stateDir = mkOption {
              type = types.submodule {
                options = {
                  warningBytes = mkOption {
                    type = types.ints.positive;
                    default = 34359738368;
                    example = 8589934592;
                    description = "Allocated stateDir bytes that emit a durable warning.";
                  };
                  hardBytes = mkOption {
                    type = types.ints.positive;
                    default = 68719476736;
                    example = 17179869184;
                    description = "Allocated stateDir bytes that refuse new intake.";
                  };
                  warningFreeBytes = mkOption {
                    type = types.ints.positive;
                    default = 17179869184;
                    example = 34359738368;
                    description = "Available stateDir-filesystem bytes below which a durable early warning is emitted.";
                  };
                  minimumFreeBytes = mkOption {
                    type = types.ints.positive;
                    default = 8589934592;
                    example = 17179869184;
                    description = "Available bytes required on the stateDir filesystem before new intake is accepted.";
                  };
                };
              };
              default = { };
              description = "Budget for enqueue events, captures, and producer state.";
            };
          };
        };
        default = { };
        description = ''
          Allocated-byte budgets and filesystem free-space floors for both
          daemon-owned stores. Warning crossings are journaled and fsynced;
          hard crossings reject only new intake so admitted work and
          observability remain available.
        '';
      };
      attestations = mkOption {
        type = types.submodule {
          options.exec = mkOption {
            type = types.submodule {
              options.enable = mkOption {
                type = types.bool;
                default = true;
                description = "Wrap fresh and recovered child executions with per-host advisory attestations.";
              };
            };
            default = { };
            description = "Per-host execution attestation chain.";
          };
        };
        default = { };
        description = "Advisory attestation policy.";
      };
      gitAi = mkOption {
        type = types.submodule {
          options = {
            enable = mkOption {
              type = types.bool;
              default = false;
              description = "Bind code-result revisions to externally provisioned Git AI notes.";
            };
            mode = mkOption {
              type = types.enum [
                "advisory"
                "required"
              ];
              default = "advisory";
              description = "Whether a missing or invalid Git AI binding is advisory or fails the result.";
            };
            awaitTimeoutSec = mkOption {
              type = types.ints.positive;
              default = 60;
              description = "Maximum Git AI settlement-barrier duration in seconds.";
            };
            globalAwaitOk = mkOption {
              type = types.bool;
              default = false;
              description = "Permit git-ai's process-global await barrier on an isolated execution host.";
            };
          };
        };
        default = { };
        description = "External Git AI authorship binding policy; tally.nix does not provide the binary.";
      };
      pools = mkOption {
        type = types.attrsOf mkPoolType;
        default = { };
        example.worker-build = {
          resource = "build-slot";
          capacity = 1;
          enforce = "cooperative";
        };
        description = "Named logical resource gates owned by this coordinator.";
      };
      executors = mkOption {
        type = types.attrsOf mkExecutorType;
        default = { };
        example.worker = {
          host = "worker.example.net";
          user = "tally-worker";
          identityFile = "/run/credentials/tally-worker-key";
          knownHostsFile = "/etc/ssh/tally-known-hosts";
          program = "/run/current-system/sw/bin/tally";
          stateDir = "/var/lib/tally-remote";
        };
        description = "Named daemonless SSH execution targets owned by the central coordinator.";
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
        description = ''
          Registry of calendar, events-directory, GitHub, build-effect, and
          pool-reachability producers. Every entry requires an explicit kind.
          Only the Home Manager module renders their managed user units.
        '';
      };
      campaigns = mkOption {
        type = types.attrsOf mkCampaignType;
        default = { };
        example.crm = {
          enable = true;
          repositories."mecattaf/crm".checkout = "/srv/spec-repositories/crm";
          gates = [
            {
              kind = "forbidPaths";
              id = "no-db-artifacts";
              forbidPaths = [
                "*.db"
                "*.db-wal"
                "*.db-shm"
                "*.sqlite*"
              ];
            }
            {
              kind = "command";
              id = "tests";
              preflightArgv = [
                "sh"
                "-euc"
                "command -v go >/dev/null; command -v gcc >/dev/null; go env CGO_ENABLED >/dev/null"
              ];
              argv = [
                "go"
                "test"
                "./..."
              ];
            }
          ];
        };
        description = ''
          First-class spec-driven build campaigns. Each enabled entry expands
          to the shipped spec-build flow, a scoped GitHub mention producer, a
          capacity-1 runner mutex, and the campaign node pools and driver
          adapter. Home Manager renders campaigns; the NixOS module rejects
          them alongside producers and flows.
        '';
      };
      campaignPoll = mkOption {
        type = types.submodule {
          options = {
            enable = mkOption {
              type = types.bool;
              default = true;
              description = ''
                Install the timer that reconciles locally armed forge-native
                campaigns. A pass that advanced admits its own successor
                through the events directory, so this timer is the recovery
                path for a lost continuation event and the way an outside
                change to the issue graph is noticed — not the ordinary way a
                campaign reaches its next pass. Disabling it leaves a campaign
                that dropped its continuation with no automatic way back.
              '';
            };
            interval = mkOption {
              type = types.str;
              default = "60s";
              example = "5min";
              description = ''
                Systemd timespan between poll scans. A scan that finds nothing
                moved costs three REST reads per armed campaign — the
                authenticated actor, the master issue, and its sub-issue list —
                and no GraphQL at all: it compares the master and sub-issue
                timestamps that fetch already returned before deciding whether
                to run the bounded steering walk or the paginated master-comment
                read. Short intervals are affordable on that budget; raise it to
                reduce forge API traffic further.
              '';
            };
            timeout = mkOption {
              type = types.str;
              default = "90s";
              example = "5min";
              description = ''
                Hard bound on one scan. The scan holds the registry lock
                exclusively across its forge round-trips, which blocks
                interactive `tally campaign arm`, `disarm`, and `list`; this
                timeout caps how long a wedged forge call can hold it.
              '';
            };
          };
        };
        default = { };
        description = ''
          Scheduling for the forge-native campaign poll. Only the Home Manager
          module renders the unit.
        '';
      };
      flows = mkOption {
        type = types.attrsOf mkFlowType;
        default = { };
        example.nightly = {
          script = lib.literalExpression "./flows/nightly.js";
          onCalendar = "daily";
          args.repository = "mecattaf/tally.nix";
        };
        description = ''
          Declarative flow registrations. The shared schema validates every
          entry, but only the Home Manager module turns scheduled entries into
          producer units and auto-declares the reserved "flow" and "build"
          pools. The NixOS module does not deploy flow runners.
        '';
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

  renderAdapterJobOptions = options: {
    inherit (options)
      prePromptArgv
      environment
      approvalPolicy
      sandboxPolicy
      model
      effort
      ;
  };

  renderWorkspace =
    workspace:
    if workspace == null then
      null
    else
      {
        inherit (workspace)
          repo
          baseRev
          branch
          worktreePath
          ;
      };

  renderGateManifest =
    manifest:
    if manifest == null then
      null
    else
      {
        inherit (manifest)
          path
          requiredGateIds
          acceptancePolicy
          ;
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
        cwd
        priority
        dedupKey
        evidence
        evidenceClass
        manifestHash
        consumptionEstimate
        runtimeMaxSec
        noEnqueue
        executor
        ;
      workspace = renderWorkspace enqueue.workspace;
      adapterOptions = renderAdapterJobOptions enqueue.adapterOptions;
      gateManifest = renderGateManifest enqueue.gateManifest;
      inherit (enqueue) brief;
      pool = if builtins.length pools == 1 then builtins.head pools else pools;
      credentials = mapAttrs (_: toString) enqueue.credentials;
    };

  renderGhSourceConstraints = constraints: {
    inherit (constraints)
      repo
      repositories
      owners
      labels
      state
      assignee
      kinds
      notificationReasons
      query
      itemAllowlist
      ;
  };

  renderGhSource =
    source:
    if source.search != null then
      { search = renderGhSourceConstraints source.search; }
    else
      { notifications = renderGhSourceConstraints source.notifications; };

  renderGhTriggers = triggers: {
    inherit (triggers)
      commandComments
      mentions
      assignments
      labels
      ;
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
              actorExclude
              allowSelfTriggered
              allowedActors
              pollIntervalSec
              postReceipt
              postEvidence
              postFailureEvidence
              postFailureStderr
              postGateSummary
              requestReview
              reviewers
              closeOnAcceptance
              neverMutate
              closeOnPass
              ;
            sources = map renderGhSource producer.sources;
            triggers = renderGhTriggers producer.triggers;
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

  renderAdapterValueOverride =
    value:
    if value == null then
      null
    else
      {
        inherit (value) argv allowedValues;
      };

  renderAdapterLaunch =
    launch:
    {
      inherit (launch)
        allowPrePromptArgv
        rejectOptionLikeWorkloadHead
        cwdArgv
        approvalPolicies
        sandboxPolicies
        commitCapableSandboxPolicies
        ;
      model = renderAdapterValueOverride launch.model;
      effort = renderAdapterValueOverride launch.effort;
    }
    // optionalAttrs (launch.resumeOptionsBeforeCapture != null) {
      inherit (launch) resumeOptionsBeforeCapture;
    };

  renderAdapter =
    _: adapter:
    {
      inherit (adapter)
        argv
        resume
        usageCounterScope
        trace
        yieldHook
        env
        extraConfig
        extraWritablePaths
        ;
      launch = renderAdapterLaunch adapter.launch;
      scrape = mapAttrs (
        _: capture:
        {
          inherit (capture) stream mode pattern;
        }
        // optionalAttrs (capture.counterScope != null) { inherit (capture) counterScope; }
        // optionalAttrs (capture.fields != { }) { inherit (capture) fields; }
      ) adapter.scrape;
    }
    // optionalAttrs (adapter.hardening != null) {
      inherit (adapter) hardening;
    }
    // optionalAttrs (adapter.skillBundle != null) {
      inherit (adapter) skillBundle;
    }
    // optionalAttrs (adapter.skillRevision != null) {
      inherit (adapter) skillRevision;
    };

  renderPool =
    _: pool:
    {
      inherit (pool)
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
    }
    # `resource` is rendered only when the operator declared it. This is the
    # one place `null` must survive unresolved to `"vram"`: the daemon's own
    # `PoolConfig.resource: Option<ResourceKind>` (#382) reads an absent key
    # as "undeclared" and an emitted `"vram"` as "declared", and every other
    # admission decision on both sides of the wire keeps defaulting an
    # undeclared pool to `vram` regardless.
    // optionalAttrs (pool.resource != null) { inherit (pool) resource; };

  renderFlow = _: flow: {
    script = storePathWithContext flow.script;
    inherit (flow) workloadMutex;
  };

  renderExecutor = _: executor: {
    inherit (executor)
      kind
      host
      user
      port
      program
      stateDir
      connectTimeoutSec
      serverAliveIntervalSec
      serverAliveCountMax
      retryIntervalMs
      ;
    sshProgram = toString executor.sshProgram;
    identityFile = toString executor.identityFile;
    knownHostsFile = toString executor.knownHostsFile;
  };

  mkRuntimeConfig = cfg: {
    inherit (cfg.transport) maxFrameBytes;
    inherit (cfg.scheduling) agingThresholdSec;
    enqueue = {
      inherit (cfg.enqueue) depthCap fanoutCap requireDedupKey;
    };
    lease = {
      inherit (cfg.lease) graceSec yieldPollSec yieldGraceSec;
    };
    retention = {
      inherit (cfg.retention)
        enable
        horizon
        onCalendar
        captureArchiveHorizon
        eventsDoneHorizon
        eventsRejectedHorizon
        eventsRejectedMaxCount
        producerMarkerHorizon
        lifecycleHorizon
        lifecycleMaxBytes
        ;
    };
    storage = {
      inherit (cfg.storage) pollIntervalSec;
      dataDir = {
        inherit (cfg.storage.dataDir)
          warningBytes
          hardBytes
          warningFreeBytes
          minimumFreeBytes
          ;
      };
      stateDir = {
        inherit (cfg.storage.stateDir)
          warningBytes
          hardBytes
          warningFreeBytes
          minimumFreeBytes
          ;
      };
    };
    attestations.exec = {
      inherit (cfg.attestations.exec) enable;
    };
    gitAi = {
      inherit (cfg.gitAi)
        enable
        mode
        awaitTimeoutSec
        globalAwaitOk
        ;
    };
    pools = mapAttrs renderPool cfg.pools;
    flows = mapAttrs renderFlow cfg.flows;
    executors = mapAttrs renderExecutor cfg.executors;
    producers = mapAttrs renderProducer cfg.producers;
    adapters = mapAttrs renderAdapter cfg.adapters;
    journald = { inherit (cfg.journald) native; };
  };

  mkFlowProducer = cfg: name: flow: {
    kind = "calendar";
    inherit (flow) onCalendar;
    credentials = { };
    enqueue = {
      argv = [
        (lib.getExe cfg.package)
        "flow"
        "run"
        (storePathWithContext flow.script)
        "--args-from-brief"
        "--max-nodes"
        (toString flow.maxNodes)
      ]
      ++ lib.optionals (flow.catalog != null) [
        "--catalog"
        (storePathWithContext flow.catalog)
      ];
      adapter = "shell";
      brief = flow.args;
      adapterOptions.environment = flow.extraEnv;
      pool = [ "flow" ] ++ lib.optional (flow.workloadMutex != null) flow.workloadMutex;
      inherit (flow)
        priority
        dedupKey
        runtimeMaxSec
        credentials
        evidence
        ;
      noEnqueue = false;
    };
  };

  mkFlowProducers =
    cfg: flows:
    lib.mapAttrs' (name: flow: lib.nameValuePair "flow-${name}" (mkFlowProducer cfg name flow)) (
      filterAttrs (_: flow: flow.onCalendar != null) flows
    );

  renderCampaignRepositories = mapAttrs (
    _: repository: {
      inherit (repository) checkout baseBranch remote;
      forge = "github";
    }
  );

  renderCampaignGates = map (
    gate:
    {
      inherit (gate) kind id runtimeMaxSec;
    }
    // optionalAttrs (gate.kind == "command") {
      inherit (gate)
        preflightArgv
        argv
        ;
    }
    // optionalAttrs (gate.kind == "forbidPaths") { inherit (gate) forbidPaths; }
  );

  # The one final-message contract the campaign machinery reads: the shipped
  # spec-build-driver adapter's capture, and the fallback a steward adapter that
  # declares no capture of its own is narrated with.
  defaultFinalMessagePattern = "^TALLY_FINAL_MESSAGE=(.*)$";

  renderCampaignSteward =
    cfg: campaign:
    let
      adapter = cfg.adapters.${campaign.steward} or { };
      declared = (adapter.scrape or { }).finalMessage or null;
    in
    {
      adapter = campaign.steward;
      argv = (adapter.argv or [ ]) ++ campaign.stewardArgv;
      # The adapter's env is what carries a narrator's endpoint and credentials.
      # Dropping it was the difference between "the adapter table decides model,
      # endpoint, and credentials" being true and being prose.
      env = adapter.env or { };
      finalMessagePattern =
        if declared == null || declared.pattern == "" then defaultFinalMessagePattern else declared.pattern;
      runtimeMaxSec = campaign.stewardRuntimeMaxSec;
    };

  # Sweep, reconcile, one optional pass-level continuation, optional
  # pristine-base preflight prep/gates/cleanup, and one frontier's worst-case
  # implementation lanes: prep, steering re-check, agent, ownership check, initial gates,
  # publication, rebase, optional re-gates, merge, machinery retry,
  # diff/diagnosis/steering, and cleanup. A machinery fault past its retry
  # budget records the retry node and is then steered, so one lane can spend
  # both failure paths. Checkpoint lanes are smaller; quiescent escalation
  # needs only sweep, reconcile, and escalation. This is a pass bound, not the
  # complete worklist size. max_flow_nodes in crates/tally/src/cli/campaign.rs
  # computes the same bound independently and is pinned against this one.
  #
  # The preflight lane costs two nodes per command gate, not one: the gating
  # base-safe probe, plus the non-gating witness that runs the gate's real
  # merge-criterion argv on the same pristine base. The witness never changes a
  # verdict, but it is an admitted node and must be budgeted like one.
  campaignMaxNodes =
    campaign:
    let
      commandGateCount = builtins.length (builtins.filter (gate: gate.kind == "command") campaign.gates);
      preflightNodes = if commandGateCount == 0 then 0 else 2 + 2 * commandGateCount;
    in
    3 + preflightNodes + campaign.maxParallel * (12 + 2 * builtins.length campaign.gates);

  mkCampaignArgs =
    cfg: name: campaign: repository: issueNumber: issueUrl: runId:
    {
      campaign = name;
      inherit repository runId;
      issue = {
        number = issueNumber;
        url = issueUrl;
      };
      repositories = renderCampaignRepositories campaign.repositories;
      inherit (campaign) worklist maxTasks maxParallel;
    }
    # The two-repository seam. A role left null is absent from the args, so a
    # single-repository campaign renders exactly the args it rendered before.
    // optionalAttrs (campaign.codeRepository != null) {
      inherit (campaign) codeRepository;
    }
    // optionalAttrs (campaign.specRepository != null) {
      inherit (campaign) specRepository;
    }
    // optionalAttrs (campaign.issueRepository != null) {
      inherit (campaign) issueRepository;
    }
    // {
      # The machine's self-nudge is local: a pass that advanced writes this
      # payload into the shipped events directory instead of posting a public
      # `/tally reconcile` comment for a second GitHub producer to poll back.
      # The argv is the one the deleted reconcile producer built.
      continuation = {
        argv = [
          (lib.getExe cfg.package)
          "flow"
          "run"
          (storePathWithContext specBuildFlow)
          "--args-from-brief"
          "--max-nodes"
          (toString (campaignMaxNodes campaign))
        ];
        pool = [
          "flow"
          campaign.pool.name
        ];
        priority = "low";
        inherit (campaign) runtimeMaxSec;
        eventsDir = "${toString cfg.stateDir}/events";
      };
      workspaceRoot = "${toString cfg.stateDir}/campaigns/${name}";
      tally = lib.getExe cfg.package;
      driver = "${specBuildDriver}/bin/spec-build-driver";
      inherit (campaign) driverRuntimeMaxSec;
      inherit (campaign) mergeMethod;
      inherit (campaign) gitAiBinding;
      inherit (campaign) gitAiAwaitSec;
      agent = {
        adapter = campaign.agent;
        argv = campaign.agentArgv;
        model = campaign.agentModel;
        priority = campaign.agentPriority;
        runtimeMaxSec = campaign.agentRuntimeMaxSec;
        approvalPolicy = campaign.agentApprovalPolicy;
        sandboxPolicy = campaign.agentSandboxPolicy;
        diagnosisSandboxPolicy = campaign.agentDiagnosisSandboxPolicy;
      };
      # The narrate slot rides the open adapter map: the campaign names a catalog
      # role and the adapter entry supplies the argv and the environment that
      # reach the model, so swapping narrators is an adapter change and never a
      # driver change. The publish node runs this argv directly, which is what
      # keeps the seam free of flow nodes -- and is also why the adapter's
      # per-job launch policies, hardening, and writable paths cannot apply here.
      # Declaring any of those on a steward adapter is refused rather than
      # ignored; see the campaign assertions below.
      steward = if campaign.steward == null then null else renderCampaignSteward cfg campaign;
      gates = renderCampaignGates campaign.gates;
    };

  mkCampaignFlow =
    cfg: name: campaign:
    let
      repositories = builtins.attrNames campaign.repositories;
      repository = if repositories == [ ] then "invalid/invalid" else builtins.head repositories;
    in
    {
      script = specBuildFlow;
      args =
        mkCampaignArgs cfg name campaign repository "1" "https://example.invalid/campaign/1"
          "module-check";
      workloadMutex = campaign.pool.name;
      maxNodes = campaignMaxNodes campaign;
      runtimeMaxSec = campaign.runtimeMaxSec;
    };

  mkCampaignProducer =
    cfg: name: campaign:
    let
      runtimeArgs =
        mkCampaignArgs cfg name campaign "\${gh.repo}" "\${gh.number}" "\${gh.url}"
          "\${gh.eventId}";
    in
    {
      kind = "gh";
      enable = true;
      sources = [
        {
          search = {
            repositories = builtins.attrNames campaign.repositories;
            labels = [ campaign.label ];
            state = "open";
            kinds = [ "issue" ];
          };
        }
      ];
      triggers.mentions = [ campaign.mention ];
      inherit (campaign) allowSelfTriggered allowedActors pollIntervalSec;
      # The projection literals are defaults, not decisions. An estate tuning
      # one campaign's public surface -- a quiet profile, a louder receipt --
      # then does it with an ordinary override instead of forking this builder
      # or reaching for mkForce. The rendered defaults are unchanged.
      postReceipt = lib.mkDefault true;
      postEvidence = lib.mkDefault true;
      inherit (campaign) postFailureEvidence postFailureStderr;
      postGateSummary = lib.mkDefault false;
      requestReview = lib.mkDefault false;
      closeOnAcceptance = lib.mkDefault false;
      closeOnPass = lib.mkDefault false;
      neverMutate = lib.mkDefault false;
      enqueue = {
        argv = [
          (lib.getExe cfg.package)
          "flow"
          "run"
          (storePathWithContext specBuildFlow)
          "--args-from-brief"
          "--max-nodes"
          (toString (campaignMaxNodes campaign))
        ];
        adapter = "shell";
        brief = runtimeArgs;
        pool = [
          "flow"
          campaign.pool.name
        ];
        priority = "low";
        runtimeMaxSec = campaign.runtimeMaxSec;
        evidence = [ "exit:0" ];
        noEnqueue = false;
      };
    };

  mkCampaignConfig =
    cfg:
    let
      enabled = filterAttrs (_: campaign: campaign.enable) cfg.campaigns;
      requiredParallelism = lib.foldl' (capacity: campaign: lib.max capacity campaign.maxParallel) 4 (
        builtins.attrValues enabled
      );
      requiredFanout = lib.foldl' (capacity: campaign: lib.max capacity (campaignMaxNodes campaign)) 64 (
        builtins.attrValues enabled
      );
      mutexPools =
        lib.foldl'
          (
            pools: campaign:
            pools
            // {
              ${campaign.pool.name} = {
                resource = lib.mkDefault "mutex";
                capacity = lib.mkDefault 1;
                predicate.co-residency = { };
              };
            }
          )
          {
            # The forge-native ad-hoc lane is installed once. Arming a campaign
            # only registers an issue locator; it never mutates Nix state.
            campaign = {
              resource = lib.mkDefault "mutex";
              capacity = lib.mkDefault 1;
              predicate.co-residency = { };
            };
          }
          (builtins.attrValues enabled);
    in
    {
      enqueue.fanoutCap = lib.mkDefault requiredFanout;
      flows = mapAttrs (name: campaign: mkCampaignFlow cfg name campaign) enabled;
      producers =
        lib.foldl'
          (
            producers: name:
            producers
            // {
              "campaign-${name}" = mkCampaignProducer cfg name enabled.${name};
            }
          )
          {
            # One generic drain for every campaign's machine self-continuation,
            # installed once and unconditionally like the forge-native `campaign`
            # pool, so arming a campaign still needs no Nix change. Both campaign
            # classes write their next-pass payload here; the frozen enqueue
            # kernel collapses a duplicate against `tally-campaign-poll.timer`.
            #
            # It renders no unit of its own. `tally-drain.timer` already claimed
            # this directory unconditionally at the same 5 s cadence on every
            # tally home, and the drain RPC drains the whole directory whoever
            # calls it, so a second timer only raced the first for the right to
            # stamp `origin.producer`. This entry stays because it is the
            # declared contract the continuation payload is written against; the
            # drain itself is `tally-drain`'s, and the durable admission origin
            # of a continuation event is therefore stably producer-less.
            campaign-continuation = {
              kind = "events-dir";
              pollIntervalSec = lib.mkDefault 5;
              selfDrain = lib.mkDefault false;
            };
          }
          (builtins.attrNames enabled);
      pools = mutexPools // {
        flow = flowPoolDefaults;
        campaign-control = {
          resource = lib.mkDefault "cpu-slot";
          capacity = lib.mkDefault requiredParallelism;
          enforce = lib.mkDefault "cooperative";
          hardPreempt = lib.mkDefault false;
        };
        campaign-agent = {
          resource = lib.mkDefault "slot";
          capacity = lib.mkDefault requiredParallelism;
          enforce = lib.mkDefault "cooperative";
          hardPreempt = lib.mkDefault false;
        };
      };
      adapters = {
        spec-build-driver = {
          scrape.finalMessage = {
            stream = "stdout";
            mode = "regex";
            pattern = defaultFinalMessagePattern;
          };
          # The continue node writes the next pass's enqueue payload into the
          # daemon's events directory, so that directory is a hard write
          # dependency of the driver adapter. Under the compatibility default
          # (no hardening preset) nothing constrains the write and this list is
          # inert; under `strict` or `production` the state directory stops
          # being writable wholesale and only the paths named here survive.
          # Declaring it means hardening this adapter cannot silently break a
          # campaign's self-continuation. It is a plain definition rather than
          # an mkDefault so an estate adding its own paths extends the list
          # instead of replacing this one.
          extraWritablePaths = [ "${toString cfg.stateDir}/events" ];
        };
      };
    };

  flowPoolDefaults = {
    resource = lib.mkDefault "cpu-slot";
    capacity = lib.mkDefault 8;
    enforce = lib.mkDefault "cooperative";
    hardPreempt = lib.mkDefault false;
  };

  buildPoolDefaults = {
    resource = lib.mkDefault "build-slot";
    capacity = lib.mkDefault 2;
    enforce = lib.mkDefault "cooperative";
    hardPreempt = lib.mkDefault false;
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

  # One sweep entry point, so both module layers must spell the same argv.
  mkRetentionArgv = cfg: [
    "${cfg.package}/bin/tally"
    "gc"
    "--horizon"
    cfg.retention.horizon
    "--collect"
    "--data-dir"
    (toString cfg.dataDir)
    "--state-dir"
    (toString cfg.stateDir)
    "--capture-archive-horizon"
    cfg.retention.captureArchiveHorizon
    "--events-done-horizon"
    cfg.retention.eventsDoneHorizon
    "--events-rejected-horizon"
    cfg.retention.eventsRejectedHorizon
    "--events-rejected-max-count"
    (toString cfg.retention.eventsRejectedMaxCount)
    "--producer-marker-horizon"
    cfg.retention.producerMarkerHorizon
  ];

  mkAssertions =
    cfg:
    flatten [
      {
        assertion = cfg.retention.horizon != "";
        message = "tally retention horizon must be non-empty";
      }
      {
        assertion = cfg.retention.onCalendar != "";
        message = "tally retention onCalendar must be non-empty";
      }
      {
        assertion = cfg.retention.captureArchiveHorizon != "";
        message = "tally retention captureArchiveHorizon must be non-empty";
      }
      {
        assertion = cfg.retention.eventsDoneHorizon != "";
        message = "tally retention eventsDoneHorizon must be non-empty";
      }
      {
        assertion = cfg.retention.eventsRejectedHorizon != "";
        message = "tally retention eventsRejectedHorizon must be non-empty";
      }
      {
        assertion = cfg.retention.producerMarkerHorizon != "";
        message = "tally retention producerMarkerHorizon must be non-empty";
      }
      {
        assertion = cfg.retention.lifecycleHorizon != "";
        message = "tally retention lifecycleHorizon must be non-empty";
      }
      {
        assertion = cfg.storage.dataDir.warningBytes < cfg.storage.dataDir.hardBytes;
        message = "tally storage.dataDir.warningBytes must be less than hardBytes";
      }
      {
        assertion = cfg.storage.stateDir.warningBytes < cfg.storage.stateDir.hardBytes;
        message = "tally storage.stateDir.warningBytes must be less than hardBytes";
      }
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
      (mapAttrsToList (name: executor: [
        {
          assertion = validComponent name;
          message = "tally executor name ${name} is not a safe registry component";
        }
        executor._tallyAssertions
      ]) cfg.executors)
      (mapAttrsToList (name: campaign: [
        {
          assertion = validComponent name && builtins.stringLength name <= 80;
          message = "tally campaign name ${name} must be a safe unit/file component of at most 80 bytes";
        }
        {
          # Every declared campaign contributes a `campaign-<name>` gh producer
          # on top of the generic `campaign-continuation` events-dir entry the
          # campaign layer seeds. A campaign literally named `continuation`
          # therefore overwrites that entry with a gh producer, and the only
          # thing that noticed was a Home Manager assertion naming an internal
          # producer rather than the campaign that collided with it. Reject the
          # reserved name here, where the operator can read what to rename.
          assertion = name != "continuation";
          message = "tally campaign name continuation is reserved: it would replace the generic events-dir producer campaign-continuation with a gh producer; rename the campaign";
        }
        campaign._tallyAssertions
        {
          assertion = !campaign.enable || builtins.hasAttr campaign.agent cfg.adapters;
          message = "tally campaign ${name} references unknown agent adapter ${campaign.agent}";
        }
        {
          # The steward is a catalog role, so the catalog is what has to carry
          # it. A name with no adapter entry would render an empty narration
          # argv and silently degrade every publication to the template.
          assertion =
            !campaign.enable || campaign.steward == null || builtins.hasAttr campaign.steward cfg.adapters;
          message = "tally campaign ${name} references unknown steward adapter ${toString campaign.steward}";
        }
        {
          assertion =
            !campaign.enable
            || campaign.steward == null
            || !(builtins.hasAttr campaign.steward cfg.adapters)
            || (cfg.adapters.${campaign.steward}.argv or [ ]) != [ ]
            || campaign.stewardArgv != [ ];
          message = "tally campaign ${name} steward adapter ${toString campaign.steward} renders no narration argv; give the adapter an argv or set stewardArgv";
        }
        {
          # The narrate slot runs a direct argv from inside the publish node,
          # not a tally job, so nothing applies an adapter's per-job launch
          # policies, hardening preset, or writable paths to it. An estate that
          # declares them on a steward adapter has configured something that
          # cannot take effect; saying so here is the difference between a
          # loud refusal and a campaign that silently narrates from the
          # template forever.
          assertion =
            !campaign.enable
            || campaign.steward == null
            || !(builtins.hasAttr campaign.steward cfg.adapters)
            || (
              let
                adapter = cfg.adapters.${campaign.steward};
              in
              adapter.launch.model == null
              && adapter.launch.effort == null
              && adapter.launch.approvalPolicies == { }
              && adapter.launch.sandboxPolicies == { }
              && adapter.hardening == null
              && adapter.extraWritablePaths == [ ]
            );
          message = "tally campaign ${name} steward adapter ${toString campaign.steward} declares launch policies, hardening, or extraWritablePaths, which the narration seam cannot apply to a direct argv; give the steward its own adapter entry";
        }
        {
          # The narration proposal is read back from the adapter's declared
          # final-message capture. A capture on the wrong stream or in a JSON
          # mode is unreadable by the publish node, which would fall back to
          # the template on every attempt and never say why.
          assertion =
            !campaign.enable
            || campaign.steward == null
            || !(builtins.hasAttr campaign.steward cfg.adapters)
            || !(builtins.hasAttr "finalMessage" (cfg.adapters.${campaign.steward}.scrape or { }))
            || (
              let
                capture = cfg.adapters.${campaign.steward}.scrape.finalMessage;
              in
              capture.stream == "stdout" && capture.mode == "regex" && capture.pattern != ""
            );
          message = "tally campaign ${name} steward adapter ${toString campaign.steward} must declare scrape.finalMessage as a non-empty stdout regex; the narration seam reads the proposal from that capture";
        }
        {
          assertion =
            !campaign.enable
            || campaign.agentApprovalPolicy == null
            || (
              builtins.hasAttr campaign.agent cfg.adapters
              &&
                builtins.hasAttr campaign.agentApprovalPolicy
                  cfg.adapters.${campaign.agent}.launch.approvalPolicies
            );
          message = "tally campaign ${name} agentApprovalPolicy is not declared by adapter ${campaign.agent}";
        }
        {
          assertion =
            !campaign.enable
            || campaign.agentSandboxPolicy == null
            || (
              builtins.hasAttr campaign.agent cfg.adapters
              &&
                builtins.hasAttr campaign.agentSandboxPolicy
                  cfg.adapters.${campaign.agent}.launch.sandboxPolicies
            );
          message = "tally campaign ${name} agentSandboxPolicy is not declared by adapter ${campaign.agent}";
        }
        {
          # An implementation node that cannot commit fails at publication after
          # doing all of its work. When the adapter has declared which of its
          # sandbox policies reach git metadata, that is knowable here.
          assertion =
            !campaign.enable
            || !(builtins.hasAttr campaign.agent cfg.adapters)
            || cfg.adapters.${campaign.agent}.launch.commitCapableSandboxPolicies == [ ]
            || (
              campaign.agentSandboxPolicy != null
              &&
                builtins.elem campaign.agentSandboxPolicy
                  cfg.adapters.${campaign.agent}.launch.commitCapableSandboxPolicies
            );
          message = "tally campaign ${name} agentSandboxPolicy must be one of adapter ${campaign.agent} commitCapableSandboxPolicies (${
            lib.concatStringsSep ", " (
              if builtins.hasAttr campaign.agent cfg.adapters then
                cfg.adapters.${campaign.agent}.launch.commitCapableSandboxPolicies
              else
                [ ]
            )
          }); an implementation node must be able to commit";
        }
        {
          assertion =
            !campaign.enable
            || campaign.agentDiagnosisSandboxPolicy == null
            || (
              builtins.hasAttr campaign.agent cfg.adapters
              &&
                builtins.hasAttr campaign.agentDiagnosisSandboxPolicy
                  cfg.adapters.${campaign.agent}.launch.sandboxPolicies
            );
          message = "tally campaign ${name} agentDiagnosisSandboxPolicy is not declared by adapter ${campaign.agent}";
        }
        {
          assertion = !campaign.enable || cfg.enqueue.fanoutCap >= campaignMaxNodes campaign;
          message = "tally campaign ${name} requires services.tally.enqueue.fanoutCap >= ${toString (campaignMaxNodes campaign)}";
        }
        {
          assertion =
            !campaign.enable
            || (
              builtins.hasAttr "campaign-agent" cfg.pools
              && effectivePoolResource cfg.pools."campaign-agent" == "slot"
              && cfg.pools."campaign-agent".capacity >= campaign.maxParallel
            );
          message = "tally campaign ${name} requires campaign-agent slot capacity >= maxParallel ${toString campaign.maxParallel}";
        }
        {
          assertion =
            !campaign.enable
            || (
              builtins.hasAttr "campaign-control" cfg.pools
              && effectivePoolResource cfg.pools."campaign-control" == "cpu-slot"
              && cfg.pools."campaign-control".capacity >= campaign.maxParallel
            );
          message = "tally campaign ${name} requires campaign-control cpu-slot capacity >= maxParallel ${toString campaign.maxParallel}";
        }
        {
          assertion =
            !campaign.enable
            || (
              builtins.hasAttr campaign.pool.name cfg.pools
              && effectivePoolResource cfg.pools.${campaign.pool.name} == "mutex"
              && cfg.pools.${campaign.pool.name}.capacity == 1
            );
          message = "tally campaign ${name} pool ${campaign.pool.name} must remain a capacity-1 mutex";
        }
      ]) cfg.campaigns)
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
        (map (
          enqueue:
          (map (pool: {
            assertion = builtins.hasAttr pool cfg.pools;
            message = "tally producer ${name} references unknown pool ${pool}";
          }) enqueue.pool)
          ++ [
            {
              assertion = builtins.hasAttr enqueue.adapter cfg.adapters;
              message = "tally producer ${name} references unknown adapter ${enqueue.adapter}";
            }
            {
              assertion = enqueue.executor == null || builtins.hasAttr enqueue.executor cfg.executors;
              message = "tally producer ${name} references unknown executor ${toString enqueue.executor}";
            }
          ]
        ) (producerEnqueues producer))
      ]) cfg.producers)
      (mapAttrsToList (
        name: flow:
        let
          mutexPool =
            if flow.workloadMutex != null && builtins.hasAttr flow.workloadMutex cfg.pools then
              cfg.pools.${flow.workloadMutex}
            else
              null;
        in
        [
          {
            assertion = validComponent name;
            message = "tally flow name ${name} is not a safe unit/file component";
          }
          flow._tallyAssertions
          {
            assertion = flow.workloadMutex == null || builtins.hasAttr flow.workloadMutex cfg.pools;
            message = "tally flow ${name} references unknown workloadMutex ${toString flow.workloadMutex}";
          }
          {
            assertion =
              flow.workloadMutex == null
              || !(builtins.elem flow.workloadMutex [
                "flow"
                "build"
              ]);
            message = "tally flow ${name} workloadMutex must not be flow or build";
          }
          {
            assertion = mutexPool == null || effectivePoolResource mutexPool == "mutex";
            message = "tally flow ${name} workloadMutex must reference a resource = mutex pool";
          }
          {
            assertion = mutexPool == null || mutexPool.capacity == 1;
            message = "tally flow ${name} workloadMutex must reference a capacity-1 pool";
          }
          {
            assertion = mutexPool == null || mutexPool.predicate ? co-residency;
            message = "tally flow ${name} workloadMutex must reference a co-residency pool";
          }
          {
            assertion = mutexPool == null || !(mutexPool.predicate ? windowed-consumption);
            message = "tally flow ${name} workloadMutex must not reference a windowed-consumption pool";
          }
        ]
      ) cfg.flows)
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
      flowChecks = lib.concatStringsSep "\n" (
        mapAttrsToList (
          name: flow:
          let
            id = builtins.substring 0 16 (builtins.hashString "sha256" name);
            args = pkgs.writeText "tally-flow-${id}-args.json" (builtins.toJSON flow.args);
            script = lib.escapeShellArg (storePathWithContext flow.script);
            flowName = lib.escapeShellArg name;
            catalogCheck =
              if flow.catalog == null then
                ''
                  if [ "$selector_count" -ne 0 ]; then
                    printf 'tally flow %s declares selectors but has no catalog\n' ${flowName} >&2
                    exit 1
                  fi
                ''
              else
                ''
                  ${lib.getExe cfg.package} --config ${rendered} flow check ${script} \
                    --catalog ${lib.escapeShellArg (storePathWithContext flow.catalog)} >/dev/null
                '';
          in
          ''
            meta="$TMPDIR/flow-${id}-meta.json"
            ${lib.getExe cfg.package} --config ${rendered} flow check ${script} > "$meta"

            unknown_pool="$(
              jq -r --slurpfile config ${rendered} \
                '[.pools[] as $pool | select(($config[0].pools | has($pool)) | not) | $pool][0] // empty' \
                "$meta"
            )"
            if [ -n "$unknown_pool" ]; then
              printf 'tally flow %s references unknown pool %s\n' ${flowName} "$unknown_pool" >&2
              exit 1
            fi
            if jq -e '.pools | index("flow") != null' "$meta" >/dev/null; then
              printf 'tally flow %s script meta.pools must not include flow\n' ${flowName} >&2
              exit 1
            fi
            if jq -e '.pools | index("build") != null' "$meta" >/dev/null; then
              printf 'tally flow %s script meta.pools must not include build\n' ${flowName} >&2
              exit 1
            fi

            script_max_nodes="$(jq -r '.maxNodes // empty' "$meta")"
            if [ -n "$script_max_nodes" ] && [ "$script_max_nodes" -gt ${toString flow.maxNodes} ]; then
              printf 'tally flow %s maxNodes %s is less than script meta.maxNodes %s\n' \
                ${flowName} ${lib.escapeShellArg (toString flow.maxNodes)} "$script_max_nodes" >&2
              exit 1
            fi

            # A declaratively deployed runner is itself a job, so its nodes are
            # children bounded by enqueue.fanoutCap. A script that declares a
            # wider budget than this host will admit is a switch-time question,
            # not a 2am one. Only an explicit meta.maxNodes is checked: the
            # module's own backstop defaults far above any host cap.
            if [ -n "$script_max_nodes" ] && [ "$script_max_nodes" -gt ${toString cfg.enqueue.fanoutCap} ]; then
              printf 'tally flow %s script meta.maxNodes %s exceeds enqueue.fanoutCap %s; raise services.tally.enqueue.fanoutCap or lower meta.maxNodes\n' \
                ${flowName} "$script_max_nodes" ${lib.escapeShellArg (toString cfg.enqueue.fanoutCap)} >&2
              exit 1
            fi

            ${lib.getExe cfg.package} --config ${rendered} flow check ${script} \
              --args-path ${lib.escapeShellArg "${args}"} >/dev/null

            selector_count="$(jq -r '(.selectors // []) | length' "$meta")"
            ${catalogCheck}
          ''
        ) cfg.flows
      );
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
        ${flowChecks}
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
    buildPoolDefaults
    meterEventPath
    flowPoolDefaults
    mkAssertions
    mkCampaignConfig
    mkCheckedConfig
    mkFlowProducers
    mkInstalledPackage
    mkOptions
    mkRetentionArgv
    mkRuntimeConfig
    mkWitnessEmitter
    priorityRanks
    producerTypesForDocumentation
    renderEnqueue
    ;
}
