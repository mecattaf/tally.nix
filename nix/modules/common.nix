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
          example.never = [
            "--ask-for-approval"
            "never"
          ];
          description = "Named approval policies mapped to exact direct argv fragments.";
        };
        sandboxPolicies = mkOption {
          type = types.attrsOf (types.listOf types.str);
          default = { };
          example.workspace-write = [
            "--sandbox"
            "workspace-write"
          ];
          description = "Named sandbox policies mapped to exact direct argv fragments.";
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
            Optional structured JSON input. The daemon content-addresses it,
            materializes it outside argv, and exposes its path as TALLY_BRIEF.
            GitHub enqueue values may use documented origin placeholders.
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
          example = [ "@tally run" ];
          description = "Exact explicit mention-command grammar.";
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
          description = "Optional trigger-actor allowlist; an empty list preserves unrestricted external actors.";
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
          description = "Post an idempotent acknowledgement for accepted, filtered, and duplicate triggers.";
        };
        postEvidence = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = "Post an idempotent evidence comment after a passing or reused verdict.";
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
          description = "Request human review while semantic acceptance remains pending or rejected.";
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

  mkPoolType = types.submodule (
    { config, name, ... }: {
      options = {
        resource = mkOption {
          type = types.enum [
            "vram"
            "build-slot"
            "cpu-slot"
            "slot"
            "budget"
            "mutex"
          ];
          default = "vram";
          example = "build-slot";
          description = ''
            Resource accounted by this pool: memory co-residency ("vram"),
            counted build or CPU slots, a neutral counted concurrency lane
            for external or metered capacity ("slot"), rolling spend
            ("budget"), or a capacity-one exclusion lock ("mutex").
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
        id = mkOption {
          type = types.str;
          example = "tests";
          description = "Stable gate identifier used in the witnessed node key.";
        };
        preflightArgv = mkOption {
          type = types.listOf types.str;
          example = [
            "sh"
            "-euc"
            "command -v go >/dev/null; command -v gcc >/dev/null; go env CGO_ENABLED >/dev/null"
          ];
          description = ''
            Base-safe direct argv executed before the first agent dispatch.
            Declare the actual host and toolchain probe; do not make the
            post-change merge criterion pretend that unbuilt output exists.
          '';
        };
        argv = mkOption {
          type = types.listOf types.str;
          example = [
            "go"
            "test"
            "./..."
          ];
          description = "Direct argv executed in each task worktree after the agent exits successfully.";
        };
        runtimeMaxSec = mkOption {
          type = types.ints.positive;
          default = 900;
          example = 300;
          description = "Process deadline shared by the base preflight and each post-change gate invocation.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = [
        {
          assertion = validComponent config.id;
          message = "tally campaign gate ${name} id ${config.id} is not a safe node-key component";
        }
        {
          assertion = config.preflightArgv != [ ] && builtins.head config.preflightArgv != "";
          message = "tally campaign gate ${name} preflightArgv must start with a non-empty executable";
        }
        {
          assertion = lib.all (argument: !(lib.hasInfix "\u0000" argument)) config.preflightArgv;
          message = "tally campaign gate ${name} preflightArgv must not contain NUL bytes";
        }
        {
          assertion = config.argv != [ ] && builtins.head config.argv != "";
          message = "tally campaign gate ${name} argv must start with a non-empty executable";
        }
        {
          assertion = lib.all (argument: !(lib.hasInfix "\u0000" argument)) config.argv;
          message = "tally campaign gate ${name} argv must not contain NUL bytes";
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
        label = mkOption {
          type = types.str;
          default = "campaign";
          example = "spec-build";
          description = "Label required on an open campaign issue.";
        };
        mention = mkOption {
          type = types.str;
          default = "@tally build";
          example = "@tally build";
          description = "Exact mention comment that starts one campaign run.";
        };
        allowSelfTriggered = mkOption {
          type = types.bool;
          default = false;
          example = true;
          description = ''
            Explicitly allow a campaign mention whose actor is the
            authenticated GitHub identity. Leave this disabled when the
            campaign runs under a bot identity so self-posted output cannot
            start another run.
          '';
        };
        allowedActors = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "trusted-maintainer" ];
          description = "Optional trigger-actor allowlist inherited by the rendered GitHub producer.";
        };
        pollIntervalSec = mkOption {
          type = types.ints.positive;
          default = 60;
          description = "GitHub campaign producer polling cadence.";
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
          description = "Maximum tasks accepted from the witnessed worklist.";
        };
        gates = mkOption {
          type = types.listOf mkCampaignGateType;
          default = [ ];
          example = [
            {
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
            Ordered gates with an explicit base-safe preflight argv and a
            post-change merge-criterion argv. Both run with the same task
            environment and deadline; the first red invocation stops the
            campaign.
          '';
        };
        agent = mkOption {
          type = types.str;
          default = "codex";
          example = "codex";
          description = "Configured adapter used by implementation nodes.";
        };
        agentArgv = mkOption {
          type = types.listOf types.str;
          default = [ briefSentinel ];
          defaultText = lib.literalExpression "the tally structured-brief sentinel";
          example = [ "/srv/campaign-fixtures/agent" ];
          description = ''
            Direct argv appended to the selected adapter. Agent adapters should
            keep the default structured-brief sentinel; a shell fixture may
            name its executable directly.
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
          description = "Priority of each campaign implementation node.";
        };
        agentApprovalPolicy = mkOption {
          type = types.nullOr types.str;
          default = "on-request";
          example = "never";
          description = ''
            Named adapter approval policy for implementation nodes. The
            default pairs with workspace-write so an agent may request an
            adapter-supported escalation. Set null only when the selected
            adapter declares no approval policies.
          '';
        };
        agentSandboxPolicy = mkOption {
          type = types.nullOr types.str;
          default = "workspace-write";
          example = "read-only";
          description = ''
            Named adapter sandbox policy for implementation nodes. The
            writable default matches the node's obligation to create commits;
            set read-only explicitly for a non-writing agent, or null only
            when the selected adapter declares no sandbox policies.
          '';
        };
        agentRuntimeMaxSec = mkOption {
          type = types.nullOr types.ints.positive;
          default = 14400;
          example = 21600;
          description = "Optional process deadline for each implementation node.";
        };
        driverRuntimeMaxSec = mkOption {
          type = types.ints.positive;
          default = 900;
          description = "Process deadline for each deterministic worklist, prep, publish, or merge node.";
        };
        runtimeMaxSec = mkOption {
          type = types.nullOr types.ints.positive;
          default = null;
          example = 82800;
          description = ''
            Optional runner deadline. Null leaves the fixed 24-hour evaluator
            budget as the campaign continuation boundary.
          '';
        };
        pool.name = mkOption {
          type = types.str;
          default = "campaign";
          example = "campaign";
          description = "Capacity-1 mutex held by the runner for the whole campaign.";
        };
        _tallyAssertions = internalAssertionsOption;
      };

      config._tallyAssertions = flatten [
        {
          assertion = !config.enable || config.repositories != { };
          message = "enabled tally campaign ${name} requires at least one repository";
        }
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
          assertion = config.agentApprovalPolicy == null || config.agentApprovalPolicy != "";
          message = "tally campaign ${name} agentApprovalPolicy must be null or non-empty";
        }
        {
          assertion = config.agentSandboxPolicy == null || config.agentSandboxPolicy != "";
          message = "tally campaign ${name} agentSandboxPolicy must be null or non-empty";
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
          description = "Durable witness, attestation, and rebuildable TaskChampion data.";
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
          };
        };
        default = { };
        description = ''
          Age-based Nix GC-root retention with a live-witness floor, plus the
          state-directory envelope for capture archives and ingress event files.
          One sweep, one timer, one lock.
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
              postGateSummary
              requestReview
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

  renderAdapterLaunch = launch: {
    inherit (launch)
      allowPrePromptArgv
      cwdArgv
      approvalPolicies
      sandboxPolicies
      ;
    model = renderAdapterValueOverride launch.model;
    effort = renderAdapterValueOverride launch.effort;
  };

  renderAdapter =
    _: adapter:
    {
      inherit (adapter)
        argv
        resume
        trace
        yieldHook
        env
        extraConfig
        extraWritablePaths
        ;
      launch = renderAdapterLaunch adapter.launch;
      scrape = mapAttrs (_: capture: {
        inherit (capture) stream mode pattern;
      }) adapter.scrape;
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
        ;
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

  mkFlowProducer = name: flow: {
    kind = "calendar";
    inherit (flow) onCalendar;
    credentials = { };
    enqueue = {
      argv = [
        "tally"
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
    flows:
    lib.mapAttrs' (name: flow: lib.nameValuePair "flow-${name}" (mkFlowProducer name flow)) (
      filterAttrs (_: flow: flow.onCalendar != null) flows
    );

  renderCampaignRepositories = mapAttrs (
    _: repository: {
      inherit (repository) checkout baseBranch remote;
      forge = "github";
    }
  );

  renderCampaignGates = map (gate: {
    inherit (gate)
      id
      preflightArgv
      argv
      runtimeMaxSec
      ;
  });

  campaignMaxNodes =
    campaign:
    1 + builtins.length campaign.gates + campaign.maxTasks * (4 + builtins.length campaign.gates);

  mkCampaignArgs = cfg: name: campaign: repository: issueNumber: issueUrl: runId: {
    campaign = name;
    inherit repository runId;
    issue = {
      number = issueNumber;
      url = issueUrl;
    };
    repositories = renderCampaignRepositories campaign.repositories;
    inherit (campaign) worklist maxTasks;
    workspaceRoot = "${toString cfg.stateDir}/campaigns/${name}";
    driver = "${specBuildDriver}/bin/spec-build-driver";
    inherit (campaign) driverRuntimeMaxSec;
    agent = {
      adapter = campaign.agent;
      argv = campaign.agentArgv;
      priority = campaign.agentPriority;
      runtimeMaxSec = campaign.agentRuntimeMaxSec;
      approvalPolicy = campaign.agentApprovalPolicy;
      sandboxPolicy = campaign.agentSandboxPolicy;
    };
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
      postReceipt = true;
      postEvidence = true;
      postGateSummary = false;
      requestReview = false;
      closeOnAcceptance = false;
      closeOnPass = false;
      neverMutate = false;
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
      requiredFanout = lib.foldl' (capacity: campaign: lib.max capacity (campaignMaxNodes campaign)) 64 (
        builtins.attrValues enabled
      );
      mutexPools = lib.foldl' (
        pools: campaign:
        pools
        // {
          ${campaign.pool.name} = {
            resource = lib.mkDefault "mutex";
            capacity = lib.mkDefault 1;
            predicate.co-residency = { };
          };
        }
      ) { } (builtins.attrValues enabled);
    in
    {
      enqueue.fanoutCap = lib.mkDefault requiredFanout;
      flows = mapAttrs (name: campaign: mkCampaignFlow cfg name campaign) enabled;
      producers = lib.mapAttrs' (
        name: campaign: lib.nameValuePair "campaign-${name}" (mkCampaignProducer cfg name campaign)
      ) enabled;
      pools =
        mutexPools
        // optionalAttrs (enabled != { }) {
          campaign-control = {
            resource = lib.mkDefault "cpu-slot";
            capacity = lib.mkDefault 4;
            enforce = lib.mkDefault "cooperative";
            hardPreempt = lib.mkDefault false;
          };
          campaign-agent = {
            resource = lib.mkDefault "slot";
            capacity = lib.mkDefault 1;
            enforce = lib.mkDefault "cooperative";
            hardPreempt = lib.mkDefault false;
          };
        };
      adapters = optionalAttrs (enabled != { }) {
        spec-build-driver = {
          scrape.finalMessage = {
            stream = "stdout";
            mode = "regex";
            pattern = "^TALLY_FINAL_MESSAGE=(.*)$";
          };
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
          assertion = validComponent name;
          message = "tally campaign name ${name} is not a safe unit/file component";
        }
        campaign._tallyAssertions
        {
          assertion = !campaign.enable || builtins.hasAttr campaign.agent cfg.adapters;
          message = "tally campaign ${name} references unknown agent adapter ${campaign.agent}";
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
          assertion = !campaign.enable || cfg.enqueue.fanoutCap >= campaignMaxNodes campaign;
          message = "tally campaign ${name} requires services.tally.enqueue.fanoutCap >= ${toString (campaignMaxNodes campaign)}";
        }
        {
          assertion =
            !campaign.enable
            || (
              builtins.hasAttr campaign.pool.name cfg.pools
              && cfg.pools.${campaign.pool.name}.resource == "mutex"
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
            assertion = mutexPool == null || mutexPool.resource == "mutex";
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
