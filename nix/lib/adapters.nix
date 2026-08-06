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
      # `pi --mode json` emits its session events as JSON lines on stdout --
      # its own `docs/json.md` says so ("Outputs all session events as JSON
      # lines to stdout"), and the capture in `test/fixtures/traces/pi.jsonl`
      # is a real run that did exactly that, 21 retained lines on stdout and
      # an empty stderr. Without this block a pi node produced no
      # `TraceGeneration` and no `TraceLane`, so `query trace` rendered no
      # lane for it.
      #
      # "JSON lines on stdout" is the framing, not an invariant pi holds on
      # every path: a resume whose cwd no longer matches the session's prints
      # the plain-text line `Session found in different project: <dir>` on
      # stdout and asks `Fork this session into current directory? [y/N]` on
      # stderr, then exits 0. Tally records a non-JSON line as a malformed
      # advisory observation, which is the honest handling, so the framing
      # declaration stands -- but see the resume argv below for what that
      # path costs.
      #
      # Volume, because it is a property of this stream and not of the
      # others: pi echoes the whole partial message on every
      # `message_update`, so stdout grows with the square of a turn's
      # length rather than linearly (the two-turn capture below wrote
      # 260 KB). A long pi campaign therefore reaches the 16 MiB trace
      # read bound far sooner than a codex or claude-code one. Truncation
      # is reported rather than hidden, so this costs trace depth, not
      # correctness -- it is a sizing note, not a defect.
      trace = {
        stream = "stdout";
        framing = "json-lines";
      };
      # No trailing `--`: pi has no end-of-options separator and rejects one
      # outright (`Error: Unknown option: --`, exit 1, zero bytes on stdout),
      # so the `--`-terminated argv this preset used to declare could never
      # produce the stream the trace block above describes. The cost of
      # dropping it is real and is not enforced anywhere: a workload argv
      # whose first element begins with `-` is parsed by pi as a flag. That
      # is a narrowing on leading-dash payloads; the alternative was an argv
      # that failed on every payload.
      argv = [
        "pi"
        "--mode"
        "json"
      ];
      # pi keys its session store by the directory it was launched in, and
      # `--session <id>` resolves against that key first. A resume from a
      # different cwd therefore does not fail: pi reports
      # `Session found in different project`, prompts on stderr, and exits 0
      # having done nothing -- a successful attempt that did no work. Pinning
      # `--session-dir` does not close this: with a custom session dir pi
      # still filters by the session's recorded cwd
      # (`sessionCwdMatches(session.cwd, resolvedCwd)`, exact path equality)
      # and falls through to the same cross-project branch. A pi node must be
      # resumed in the cwd it was launched in; nothing in this preset can
      # assert that, and pi offers no cwd flag for `launch.cwdArgv` to use.
      resume = [
        "pi"
        "--mode"
        "json"
        "--session"
        "%<sessionRef>%"
        "--model"
        "%<model>%"
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
        # Still no `fields` mapping, and now for a stated reason rather than
        # for want of evidence. A real `pi --mode json` capture is on hand
        # (`test/fixtures/traces/pi.jsonl`) and it settles the key names:
        # every assistant message carries
        # `usage = { input, output, cacheRead, cacheWrite, reasoning,
        # totalTokens, cost }`, with `input` exclusive of both cache halves
        # (the capture's second turn reports input 190, cacheRead 842,
        # output 46, totalTokens 1078 = 190 + 46 + 842 + 0).
        #
        # What the capture also settles is that pi states usage **per
        # assistant message and never per attempt**: there is no
        # `turn.completed`-style roll-up anywhere in the stream, so this
        # capture's last match is one turn's figures, not the attempt's
        # spend. Declaring `inputTokens = [ "input" ]` here would report a
        # single turn as an attempt's usage and understate every multi-turn
        # pi node -- the mirror image of the mistake `codex` declines when it
        # refuses to report a cumulative total as occupancy. The honest
        # reading of a per-turn figure is occupancy, which is declared below.
        usage = mkScrapeCapture {
          mode = "jsonPath";
          pattern = "$..usage";
        };
        # Occupancy is exactly what pi's per-message usage is: the tokens
        # resident in the context window as of one assistant turn. The
        # capture is scoped to assistant `message_end` events so its last
        # match is the last completed assistant turn rather than the
        # zero-filled `message_start` placeholder or a `toolResult` message,
        # and the field names are occupancy's own so a spend lookup can never
        # resolve against it -- see `crate::occupancy`'s module doc.
        #
        # The `stopReason` guards are what make "last assistant turn" mean
        # "last **valid** assistant turn", which is what `context_tokens`
        # documents itself as. pi zero-fills the usage object on a turn it
        # marks `aborted`, and `context_tokens` returns `None` only when all
        # three resident fields are absent -- three resolved zeroes are
        # `Some(0)`, a fabricated emptiness for a session that was thousands
        # of tokens full. `test/fixtures/traces/pi-aborted-turn.jsonl` is that
        # stream. `aborted` is proven from real pi data on this host;
        # `error` is guarded by analogy with SSSF's `calculateContextTokens`,
        # which skips both, and has not been observed here.
        occupancy = mkScrapeCapture {
          mode = "jsonPathLast";
          pattern = "$[?@.type == 'message_end' && @.message.role == 'assistant' && @.message.stopReason != 'aborted' && @.message.stopReason != 'error'].message.usage";
          fields = {
            residentInputTokens = [ "input" ];
            residentCacheReadTokens = [ "cacheRead" ];
            residentCacheWriteTokens = [ "cacheWrite" ];
          };
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
