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
          example = "/worktrees/project";
          description = "Optional absolute working directory for the job.";
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
            subject = "local campaign task";
          };
          description = ''
            Optional structured JSON input. The producer materializes it in the
            daemon's content-addressed store outside argv; jobs receive its path
            and identity as TALLY_BRIEF and TALLY_BRIEF_HASH.
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

  producerKinds = [
    "calendar"
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
    (producerTypeFor "events-dir" eventsDirProducerType)
    invalidProducerType
  ];

  # `types.oneOf` deliberately does not merge the sub-options of its variants.
  # The runtime type stays discriminated and strict; the documentation builder
  # evaluates these same types separately so every supported producer field is
  # present in the generated reference.
  producerTypesForDocumentation = {
    calendar = calendarProducerType;
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
          assertion = !lib.hasPrefix "campaign/" name;
          message = "pool ${name} uses the reserved campaign/ namespace; repository campaign mutexes are minted on demand";
        }
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
              # given: operator retention horizon for the witness-liveness floor.
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
              # given: operator retention horizon — archives are replay material,
              # pruned once the witness record outlives them.
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
              # given: operator retention horizon — the ingress audit trail keeps
              # a longer window than ordinary evidence.
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
              # given: operator retention horizon — rejected events are
              # adversarially drivable, so they expire sooner than the audit trail.
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
              # given: operator count bound — whichever of this and the horizon is
              # exceeded first prunes, oldest file first.
              default = 10000;
              example = 1000;
              description = ''
                Maximum retained files under events/rejected. Whichever of this
                bound and eventsRejectedHorizon is exceeded first prunes, oldest
                file first.
              '';
            };
            lifecycleHorizon = mkOption {
              type = types.str;
              # given: operator retention floor kept across byte-triggered
              # lifecycle-log compaction.
              default = "30d";
              example = "90d";
              description = ''
                Minimum lifecycle-log history retained when byte-triggered
                online prefix compaction runs.
              '';
            };
            lifecycleMaxBytes = mkOption {
              type = types.ints.positive;
              # given: operator byte trigger for online lifecycle-log prefix
              # compaction (256 MiB).
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
          Registry of calendar and events-directory producers. Every entry
          requires an explicit kind.
          Only the Home Manager module renders their managed user units.
        '';
      };
      campaignPoll = mkOption {
        type = types.submodule {
          options = {
            enable = mkOption {
              type = types.bool;
              default = true;
              description = ''
                Install the timer that reconciles locally armed campaigns. A
                pass that advanced admits its own successor through the events
                directory, so this timer is the recovery path for a lost
                continuation event rather than the ordinary way a campaign
                reaches its next pass. Disabling it leaves a campaign that
                dropped its continuation with no automatic way back.
              '';
            };
            interval = mkOption {
              type = types.str;
              default = "60s";
              example = "5min";
              description = ''
                Systemd timespan between local registry scans. A scan that finds
                no dispatchable work returns without admitting a successor.
              '';
            };
            timeout = mkOption {
              type = types.str;
              default = "90s";
              example = "5min";
              description = ''
                Hard bound on one scan. The scan holds the registry lock while
                reading durable Git state, which blocks interactive `tally
                campaign arm`, `disarm`, and `list` until it returns.
              '';
            };
          };
        };
        default = { };
        description = ''
          Scheduling for the local campaign recovery poll rendered by both
          service modules.
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
          scrape.finalMessage = {
            mode = "jsonPathLast";
            pattern = "$[?@.type == 'item.completed' && @.item.type == 'agent_message'].item.text";
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
        else
          { }
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

  flowPoolDefaults = {
    resource = lib.mkDefault "cpu-slot";
    # given: operator pool capacity — the host declares how many flow slots it
    # offers; worklist maxParallel sits under this ceiling.
    capacity = lib.mkDefault 8;
    enforce = lib.mkDefault "cooperative";
    hardPreempt = lib.mkDefault false;
  };

  buildPoolDefaults = {
    resource = lib.mkDefault "build-slot";
    # given: operator pool capacity — builds serialize aggressively by
    # default; a host that wants parallel builds raises this explicitly.
    capacity = lib.mkDefault 2;
    enforce = lib.mkDefault "cooperative";
    hardPreempt = lib.mkDefault false;
  };

  # `tally campaign arm` reads policy from the committed worklist, but still
  # validates that the host has enough generic execution capacity. Keep these
  # defaults independent of the retired per-campaign module declarations.
  mkCampaignRuntimeConfig = cfg: {
    # given: operator envelope — one pass may fan out at most this many
    # enqueue admissions before the daemon pushes back.
    enqueue.fanoutCap = lib.mkDefault 64;
    pools = {
      flow = flowPoolDefaults;
      campaign-control = {
        resource = lib.mkDefault "cpu-slot";
        # given: operator pool capacity for campaign control lanes.
        capacity = lib.mkDefault 4;
        enforce = lib.mkDefault "cooperative";
        hardPreempt = lib.mkDefault false;
      };
      campaign-agent = {
        resource = lib.mkDefault "slot";
        # given: operator pool capacity for campaign agent lanes.
        capacity = lib.mkDefault 4;
        enforce = lib.mkDefault "cooperative";
        hardPreempt = lib.mkDefault false;
      };
    };
    adapters.spec-build-driver = {
      scrape.finalMessage = {
        stream = "stdout";
        mode = "regex";
        pattern = "^TALLY_FINAL_MESSAGE=(.*)$";
      };
      # Continuations and checkpoint snapshots are written by driver nodes.
      # Under hardened adapters these are the two state paths that must stay
      # writable; ordinary compatibility mode leaves this list inert.
      extraWritablePaths = [
        "${toString cfg.stateDir}/events"
        "${toString cfg.stateDir}/capture/archive"
      ];
    };
  };

  producerEnqueues = producer: if producer.kind == "calendar" then [ producer.enqueue ] else [ ];

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
      (mapAttrsToList (name: producer: [
        {
          assertion = validComponent name;
          message = "tally producer name ${name} is not a safe unit/file component";
        }
        producer._tallyAssertions
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
    mkCampaignRuntimeConfig
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
