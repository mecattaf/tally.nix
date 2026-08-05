{ lib }:
let
  validArgv = argv: builtins.isList argv && builtins.all builtins.isString argv;

  mkScrapeCapture =
    {
      stream ? "stdout",
      mode ? "regex",
      pattern,
      fields ? { },
    }:
    assert lib.assertMsg (builtins.elem stream [
      "stdout"
      "stderr"
    ]) "tally adapter scrape stream must be stdout or stderr";
    assert lib.assertMsg (builtins.elem mode [
      "regex"
      "jsonPath"
      "jsonPathLast"
    ]) "tally adapter scrape mode must be regex, jsonPath, or jsonPathLast";
    assert lib.assertMsg (
      builtins.isString pattern && pattern != ""
    ) "tally adapter scrape pattern must be a non-empty string";
    assert lib.assertMsg (
      builtins.isAttrs fields
      && builtins.all (
        paths: builtins.isList paths && paths != [ ] && builtins.all builtins.isString paths
      ) (builtins.attrValues fields)
    ) "tally adapter scrape fields must map each declared name to a non-empty list of paths";
    {
      inherit stream mode pattern;
    }
    // lib.optionalAttrs (fields != { }) { inherit fields; };

  mkAdapter =
    {
      argv ? [ ],
      resume ? null,
      scrape ? { },
      trace ? null,
      yieldHook ? null,
      env ? { },
      launch ? { },
      hardening ? null,
      extraWritablePaths ? [ ],
      skillBundle ? null,
      skillRevision ? null,
      extraConfig ? { },
    }:
    assert lib.assertMsg (validArgv argv) "tally adapter argv must be a list of strings";
    assert lib.assertMsg (
      resume == null || validArgv resume
    ) "tally adapter resume must be null or a list of strings";
    assert lib.assertMsg (
      yieldHook == null || validArgv yieldHook
    ) "tally adapter yieldHook must be null or a list of strings";
    assert lib.assertMsg (builtins.isAttrs scrape) "tally adapter scrape must be an attrset";
    assert lib.assertMsg (
      trace == null
      || (
        builtins.isAttrs trace
        && builtins.elem (trace.stream or "stdout") [
          "stdout"
          "stderr"
        ]
        && (trace.framing or "json-lines") == "json-lines"
      )
    ) "tally adapter trace must declare stdout/stderr with json-lines framing";
    assert lib.assertMsg (
      builtins.isAttrs env && builtins.all builtins.isString (builtins.attrValues env)
    ) "tally adapter env must be an attrset of strings";
    assert lib.assertMsg (builtins.isAttrs launch) "tally adapter launch must be an attrset";
    assert lib.assertMsg (
      hardening == null
      || builtins.elem hardening [
        "production"
        "strict"
        "workspace"
        "none"
      ]
    ) "tally adapter hardening must be null, production, strict, workspace, or none";
    assert lib.assertMsg (
      builtins.isList extraWritablePaths
      && builtins.all (
        path: builtins.isString path && lib.hasPrefix "/" path && !(lib.hasInfix "%" path)
      ) extraWritablePaths
    ) "tally adapter extraWritablePaths must contain absolute strings without systemd specifiers";
    assert lib.assertMsg (
      skillBundle == null || builtins.isString skillBundle
    ) "tally adapter skillBundle must be null or a string";
    assert lib.assertMsg (
      skillRevision == null || builtins.isString skillRevision
    ) "tally adapter skillRevision must be null or a string";
    assert lib.assertMsg (
      skillBundle == null || skillRevision == null
    ) "tally adapter skillBundle and skillRevision are mutually exclusive";
    assert lib.assertMsg (builtins.isAttrs extraConfig) "tally adapter extraConfig must be an attrset";
    {
      inherit
        argv
        resume
        scrape
        trace
        yieldHook
        env
        launch
        extraConfig
        extraWritablePaths
        ;
    }
    // lib.optionalAttrs (hardening != null) { inherit hardening; }
    // lib.optionalAttrs (skillBundle != null) { inherit skillBundle; }
    // lib.optionalAttrs (skillRevision != null) { inherit skillRevision; };

  checkpointHook = [
    "tally"
    "lease"
    "status"
  ];

  presets = {
    pi = mkAdapter {
      argv = [
        "pi"
        "--mode"
        "json"
        "--"
      ];
      resume = [
        "pi"
        "--mode"
        "json"
        "--session"
        "%<sessionRef>%"
        "--model"
        "%<model>%"
        "--"
      ];
      scrape = {
        sessionRef = mkScrapeCapture {
          mode = "jsonPath";
          pattern = "$.id";
        };
        model = mkScrapeCapture {
          mode = "jsonPath";
          pattern = "$..model";
        };
        # No `fields` mapping yet: pi has never run a campaign here, and
        # inventing its key names would be a fixture wrong in the same
        # direction as the code. Until a real `pi --mode json` capture is on
        # hand this capture keeps the legacy reading (total_tokens, else
        # input_tokens plus output_tokens), which is what it had before the
        # mapping existed. Declaring the real keys is an attrset here, not a
        # Rust change.
        usage = mkScrapeCapture {
          mode = "jsonPath";
          pattern = "$..usage";
        };
        finalMessage = mkScrapeCapture {
          mode = "jsonPathLast";
          pattern = "$[?@.type == 'message_end' && @.message.role == 'assistant'].message.content[?@.type == 'text'].text";
        };
      };
      yieldHook = checkpointHook;
      extraConfig.modelFlag = "--model";
    };

    claude-code = mkAdapter {
      trace = {
        stream = "stdout";
        framing = "json-lines";
      };
      argv = [
        "claude"
        "--print"
        "--verbose"
        "--output-format"
        "stream-json"
        "--"
      ];
      resume = [
        "claude"
        "--resume"
        "%<sessionRef>%"
        "--model"
        "%<model>%"
        "--print"
        "--verbose"
        "--output-format"
        "stream-json"
        "--"
      ];
      scrape = {
        sessionRef = mkScrapeCapture {
          mode = "jsonPath";
          pattern = "$..session_id";
        };
        model = mkScrapeCapture {
          mode = "jsonPath";
          pattern = "$..model";
        };
        # claude-code reports the cached halves of the prompt beside an
        # `input_tokens` figure that excludes both, so the exclusive spelling
        # is the honest one here.
        usage = mkScrapeCapture {
          mode = "jsonPath";
          pattern = "$..usage";
          fields = {
            inputTokens = [ "input_tokens" ];
            cacheReadTokens = [ "cache_read_input_tokens" ];
            cacheWriteTokens = [ "cache_creation_input_tokens" ];
            outputTokens = [ "output_tokens" ];
          };
        };
        # Cost sits on the result event rather than inside `usage`, so it is
        # its own capture feeding the same record. Tally never computes a
        # dollar figure; this is only what the harness said.
        usageCost = mkScrapeCapture {
          mode = "jsonPathLast";
          pattern = "$[?@.type == 'result'].total_cost_usd";
          fields.costUsd = [ "$" ];
        };
        # Occupancy needs a narrower capture than `usage`: `usage` keeps the
        # last `usage` object anywhere in the stream, which is the `result`
        # event's session-lifetime roll-up, not a turn. This capture is
        # scoped to only `type == "assistant"` events, so its last match is
        # genuinely the last assistant turn. The field names are spelled
        # differently from `usage`'s own (`residentInputTokens` rather than
        # `inputTokens`) so a lookup for one can never resolve against the
        # other's declared capture -- see `crate::occupancy`'s module doc.
        occupancy = mkScrapeCapture {
          mode = "jsonPathLast";
          pattern = "$[?@.type == 'assistant'].message.usage";
          fields = {
            residentInputTokens = [ "input_tokens" ];
            residentCacheReadTokens = [ "cache_read_input_tokens" ];
            residentCacheWriteTokens = [ "cache_creation_input_tokens" ];
          };
        };
        # The result event's per-model usage breakdown carries the harness's
        # own context window beside its cost, so this is a stated fact, not
        # a guess: real captures put it at `modelUsage.<model>.contextWindow`.
        contextWindow = mkScrapeCapture {
          mode = "jsonPathLast";
          pattern = "$[?@.type == 'result'].modelUsage.*.contextWindow";
          fields.contextWindow = [ "$" ];
        };
        finalMessage = mkScrapeCapture {
          mode = "jsonPathLast";
          pattern = "$[?@.type == 'result'].result";
        };
      };
      yieldHook = checkpointHook;
      extraConfig.modelFlag = "--model";
    };

    shell = mkAdapter { };

    codex = mkAdapter {
      trace = {
        stream = "stdout";
        framing = "json-lines";
      };
      argv = [
        "codex"
        "exec"
        "--json"
        "--"
      ];
      resume = [
        "codex"
        "-C"
        "%<cwd>%"
        "exec"
        "resume"
        "--json"
        "--model"
        "%<model>%"
        "%<sessionRef>%"
        "--"
      ];
      scrape = {
        sessionRef = mkScrapeCapture {
          mode = "jsonPath";
          pattern = "$..thread_id";
        };
        model = mkScrapeCapture {
          mode = "jsonPath";
          pattern = "$..model";
        };
        # codex counts cached prompt tokens inside its own `input_tokens`
        # figure, so the inclusive spelling is declared: the record subtracts
        # the cache read to reach the same meaning claude-code's exclusive
        # figure already has.
        #
        # All five keys real `codex exec --json` emits are declared. Every
        # `turn.completed` in this project's own dispatch corpus carries the
        # same five, `cache_write_input_tokens` among them with the value 0 —
        # a measurement, which the record must state rather than leave absent.
        usage = mkScrapeCapture {
          mode = "jsonPath";
          pattern = "$..usage";
          fields = {
            inputTokensWithCacheRead = [ "input_tokens" ];
            cacheReadTokens = [ "cached_input_tokens" ];
            cacheWriteTokens = [ "cache_write_input_tokens" ];
            outputTokens = [ "output_tokens" ];
            reasoningTokens = [ "reasoning_output_tokens" ];
          };
        };
        finalMessage = mkScrapeCapture {
          mode = "jsonPathLast";
          pattern = "$[?@.type == 'item.completed' && @.item.type == 'agent_message'].item.text";
        };
        # No `contextWindow` capture: no `turn.completed` in this project's
        # corpus has ever stated one, and declaring a key nobody has observed
        # is a guess wearing a declaration's clothes. An operator who knows
        # the model's ceiling can still assert it via `extraConfig.contextWindow`.
        #
        # No `occupancy` capture either: `codex exec --json` emits exactly
        # one `turn.completed` per exec, carrying only the cumulative
        # `total_token_usage` shape codex's own rollout journal keeps beside
        # a true per-turn `last_token_usage` -- a shape the exec stream never
        # exposes. Declaring occupancy from the cumulative total would state
        # a number that grows without bound against a fixed window, so it is
        # left undeclared, matching `pi`'s usage mapping precedent.
      };
      yieldHook = checkpointHook;
      launch = {
        allowPrePromptArgv = true;
        cwdArgv = [
          "-C"
          "%<cwd>%"
        ];
        # `--ask-for-approval` is a top-level codex flag; `codex exec` rejects
        # it outright, and this adapter's argv puts every policy fragment after
        # the `exec` subcommand. The config override is the exec-local spelling
        # of the same setting and is what the real binary accepts.
        approvalPolicies = {
          untrusted = [
            "-c"
            "approval_policy=\"untrusted\""
          ];
          on-failure = [
            "-c"
            "approval_policy=\"on-failure\""
          ];
          on-request = [
            "-c"
            "approval_policy=\"on-request\""
          ];
          never = [
            "-c"
            "approval_policy=\"never\""
          ];
        };
        sandboxPolicies = {
          read-only = [
            "--sandbox"
            "read-only"
          ];
          workspace-write = [
            "--sandbox"
            "workspace-write"
          ];
          danger-full-access = [
            "--sandbox"
            "danger-full-access"
          ];
          dangerously-bypass = [ "--dangerously-bypass-approvals-and-sandbox" ];
        };
        # Under workspace-write codex mounts the repository's git metadata
        # read-only: the agent writes files and then fails at .git/index.lock,
        # which is the one outcome where a spec-build implementation node does
        # all of its work and still cannot publish it.
        commitCapableSandboxPolicies = [
          "danger-full-access"
          "dangerously-bypass"
        ];
      };
      extraConfig.modelFlag = "--model";
    };
  };
in
{
  inherit mkAdapter mkScrapeCapture presets;
}
