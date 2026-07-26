{ lib }:
let
  validArgv = argv: builtins.isList argv && builtins.all builtins.isString argv;

  mkScrapeCapture =
    {
      stream ? "stdout",
      mode ? "regex",
      pattern,
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
    {
      inherit stream mode pattern;
    };

  mkAdapter =
    {
      argv ? [ ],
      resume ? null,
      scrape ? { },
      trace ? null,
      yieldHook ? null,
      env ? { },
      launch ? { },
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
        ;
    }
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
        usage = mkScrapeCapture {
          mode = "jsonPath";
          pattern = "$..usage";
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
        usage = mkScrapeCapture {
          mode = "jsonPath";
          pattern = "$..usage";
        };
        finalMessage = mkScrapeCapture {
          mode = "jsonPathLast";
          pattern = "$[?@.type == 'item.completed' && @.item.type == 'agent_message'].item.text";
        };
      };
      yieldHook = checkpointHook;
      launch = {
        allowPrePromptArgv = true;
        cwdArgv = [
          "-C"
          "%<cwd>%"
        ];
        approvalPolicies = {
          untrusted = [
            "--ask-for-approval"
            "untrusted"
          ];
          on-failure = [
            "--ask-for-approval"
            "on-failure"
          ];
          on-request = [
            "--ask-for-approval"
            "on-request"
          ];
          never = [
            "--ask-for-approval"
            "never"
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
      };
      extraConfig.modelFlag = "--model";
    };
  };
in
{
  inherit mkAdapter mkScrapeCapture presets;
}
