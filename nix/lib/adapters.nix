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
    ]) "tally adapter scrape mode must be regex or jsonPath";
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
      yieldHook ? null,
      env ? { },
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
      builtins.isAttrs env && builtins.all builtins.isString (builtins.attrValues env)
    ) "tally adapter env must be an attrset of strings";
    assert lib.assertMsg (builtins.isAttrs extraConfig) "tally adapter extraConfig must be an attrset";
    {
      inherit
        argv
        resume
        scrape
        yieldHook
        env
        extraConfig
        ;
    };

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
      };
      yieldHook = checkpointHook;
      extraConfig.modelFlag = "--model";
    };

    claude-code = mkAdapter {
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
      };
      yieldHook = checkpointHook;
      extraConfig.modelFlag = "--model";
    };

    shell = mkAdapter { };

    codex = mkAdapter {
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
      };
      yieldHook = checkpointHook;
      extraConfig.modelFlag = "--model";
    };
  };
in
{
  inherit mkAdapter mkScrapeCapture presets;
}
