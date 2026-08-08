{ lib }:
let
  validArgv = argv: builtins.isList argv && builtins.all builtins.isString argv;
  mkScrapeCapture =
    {
      stream ? "stdout",
      mode ? "regex",
      pattern,
      counterScope ? null,
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
      counterScope == null
      || builtins.elem counterScope [
        "attempt"
        "session-cumulative"
      ]
    ) "tally adapter scrape counterScope must be null, attempt, or session-cumulative";
    assert lib.assertMsg (
      builtins.isAttrs fields
      && builtins.all (
        paths: builtins.isList paths && paths != [ ] && builtins.all builtins.isString paths
      ) (builtins.attrValues fields)
    ) "tally adapter scrape fields must map each declared name to a non-empty list of paths";
    {
      inherit stream mode pattern;
    }
    // lib.optionalAttrs (counterScope != null) { inherit counterScope; }
    // lib.optionalAttrs (fields != { }) { inherit fields; };

  mkAdapter =
    {
      argv ? [ ],
      resume ? null,
      resumeRequiresLaunchCwd ? false,
      usageCounterScope ? "attempt",
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
    assert lib.assertMsg (builtins.isBool resumeRequiresLaunchCwd)
      "tally adapter resumeRequiresLaunchCwd must be a boolean";
    assert lib.assertMsg (
      !resumeRequiresLaunchCwd || resume != null
    ) "tally adapter resumeRequiresLaunchCwd requires a resume template to constrain";
    assert lib.assertMsg (builtins.elem usageCounterScope [
      "attempt"
      "session-cumulative"
    ]) "tally adapter usageCounterScope must be attempt or session-cumulative";
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
    assert lib.assertMsg (builtins.isBool (
      launch.rejectOptionLikeWorkloadHead or false
    )) "tally adapter launch.rejectOptionLikeWorkloadHead must be a boolean";
    assert lib.assertMsg (
      let
        capture = launch.resumeOptionsBeforeCapture or null;
      in
      capture == null || (builtins.isString capture && capture != "")
    ) "tally adapter launch.resumeOptionsBeforeCapture must be null or a non-empty string";
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
        resumeRequiresLaunchCwd
        usageCounterScope
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
      # length rather than linearly (the full two-turn run the capture
      # below was excerpted from wrote 260 KB; the committed excerpt is
      # 10 KB). A long pi campaign therefore reaches the 16 MiB trace
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
      # produce the stream the trace block above describes. Without a
      # separator, a first workload element beginning with `-` is still parsed
      # by pi as a provider flag; a valid flag such as `-p` can make pi exit 0
      # without doing the requested work. The typed launch policy below makes
      # that limitation executable for both fresh and resumed invocations.
      argv = [
        "pi"
        "--mode"
        "json"
      ];
      launch.rejectOptionLikeWorkloadHead = true;
      # pi keys its session store by the directory it was launched in, and
      # `--session <id>` resolves against that key first. A resume from a
      # different cwd therefore does not fail: pi reports
      # `Session found in different project`, prompts on stderr, and exits 0
      # having done nothing -- a successful attempt that did no work. Pinning
      # `--session-dir` does not close this: with a custom session dir pi
      # still filters by the session's recorded cwd
      # (`sessionCwdMatches(session.cwd, resolvedCwd)`, exact path equality)
      # and falls through to the same cross-project branch. A pi node must be
      # resumed in the cwd it was launched in; nothing in this preset's argv
      # can assert that, and pi offers no cwd flag for `launch.cwdArgv` to use.
      #
      # `resumeRequiresLaunchCwd` below is where the invariant is asserted
      # instead. It is a declaration, not an argv: it tells tally to refuse a
      # continuation of a pi session from any directory other than the one the
      # session was launched in, naming both directories. That refusal is the
      # only enforcement available, and refusing loudly is strictly better than
      # the exit-0-having-done-nothing this preset otherwise falls into.
      resumeRequiresLaunchCwd = true;
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
        # Scoped to assistant `message_end` and guarded by the same two
        # `stopReason` clauses as `occupancy` and `finalMessage`, because a
        # bare `$..model` takes the last `model` anywhere in the stream and
        # so pins the model of a turn the other two captures exclude. On
        # `test/fixtures/traces/pi-aborted-turn.jsonl` that is
        # `qwen3-vl-8b-ocr`, a model no valid turn of that session ever used,
        # and it reaches an operator: the rendered resume argv carries it,
        # and `daemon/completion.rs` records it as the job's model.
        #
        # The scoping is what makes the guard work, and a descendant filter
        # does not, however it is clause-guarded. pi emits three records per
        # assistant message -- `message_start`, `message_update`,
        # `message_end` -- all carrying the same `AgentMessage`, so all
        # carrying `role: assistant` and the same `model`, with `stopReason`
        # `pending` until the message closes (pi's own `docs/json.md`
        # message lifecycle, and visible in `pi.jsonl`). An aborted turn
        # therefore contributes `pending` records *after* the last valid
        # turn, and a descendant filter that excludes only `aborted`/`error`
        # relocates the read to the same turn's `message_update` and
        # resolves the identical model. Excluding `pending` as a third
        # clause does not rescue it either: measured against a stream
        # truncated mid-turn, that variant resolves no model at all, exactly
        # like this one -- which is the point below.
        #
        # The cost, stated because it is a real narrowing: an attempt whose
        # stream never closed an assistant `message_end` now yields no
        # `model` capture, so a resume refuses loudly with
        # `resume capture "model" is absent` instead of rendering. That is
        # deliberate. Such a stream states only the model of a turn whose
        # outcome is unknown, and an aborted turn's mid-stream records are
        # indistinguishable from an open valid turn's until its
        # `message_end` arrives -- so there is no pattern that both excludes
        # the first and recovers from the second. Refusing beats pinning a
        # model no completed turn is known to have used.
        #
        # Say the rest of it plainly, because this preset offers no way out.
        # A pi-DERIVED adapter that declares `launch.model` can have a job
        # pin one; this preset declares `launch = {}`, so a job-supplied
        # model is refused before any template renders --
        # `model override is not authorized by this adapter`. So a pi
        # attempt whose stream never closed an assistant `message_end`
        # cannot be resumed by tally at all. The operator re-runs it from
        # scratch, or hand-authors a pi-derived adapter that declares
        # `launch.model`. Nothing here makes that cheaper; it is the cost of
        # not fabricating a model, and it is stated rather than discovered.
        model = mkScrapeCapture {
          mode = "jsonPathLast";
          pattern = "$[?@.type == 'message_end' && @.message.role == 'assistant' && @.message.stopReason != 'aborted' && @.message.stopReason != 'error'].message.model";
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
        # stream.
        #
        # Both clauses are guarded, and the evidence for them runs the
        # opposite way round from the order they are written in. `error` is
        # the branch a non-interactive `pi --mode json` can actually reach
        # in-stream: it is pi's own context-overflow signal, delivered on
        # this same `message_end` shape. An in-stream `aborted`
        # `message_end` could not be produced headlessly at all -- SIGINT
        # truncates the run before any assistant `message_end` is written,
        # exit 130 -- so the aborted turn in the fixture is real pi data
        # taken from pi's **session store**, where aborted turns are
        # recorded, not from a captured headless stream. `error` is
        # corroborated by SSSF's `calculateContextTokens`, which skips both.
        occupancy = mkScrapeCapture {
          mode = "jsonPathLast";
          pattern = "$[?@.type == 'message_end' && @.message.role == 'assistant' && @.message.stopReason != 'aborted' && @.message.stopReason != 'error'].message.usage";
          fields = {
            residentInputTokens = [ "input" ];
            residentCacheReadTokens = [ "cacheRead" ];
            residentCacheWriteTokens = [ "cacheWrite" ];
          };
        };
        # The same two `stopReason` clauses again, and this is the one where
        # the cost of omitting them is read by a human. An attempt that ends
        # on an aborted turn carrying partial text reported that truncated
        # text as the node's answer, unmarked -- occupancy correctly held at
        # the last valid turn while `finalMessage` moved to the aborted one.
        # `test/fixtures/traces/pi-aborted-turn.jsonl` observes exactly that:
        # unguarded it resolves to `The file notes.txt cont`, guarded to the
        # last valid turn's `The file notes.txt contains 42.`
        finalMessage = mkScrapeCapture {
          mode = "jsonPathLast";
          pattern = "$[?@.type == 'message_end' && @.message.role == 'assistant' && @.message.stopReason != 'aborted' && @.message.stopReason != 'error'].message.content[?@.type == 'text'].text";
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
      # `codex exec resume` rehydrates the thread's cumulative counters. The
      # executable delta accounting for this declaration arrives in #403.
      usageCounterScope = "session-cumulative";
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
        "%<sessionRef>%"
        "--"
      ];
      scrape = {
        sessionRef = mkScrapeCapture {
          mode = "jsonPath";
          pattern = "$..thread_id";
        };
        model = mkScrapeCapture {
          # Advisory and optional: a default-model `codex exec --json` stream
          # does not state the chosen model. An explicit per-job model is
          # rendered through launch.model on resume just as it is on launch;
          # the resume template must not turn this observation into a
          # requirement or infer a default that codex never reported.
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
          counterScope = "session-cumulative";
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
        # `codex exec resume` parses the thread id as a positional. Authorized
        # options therefore belong before that capture, not merely before the
        # final workload separator.
        resumeOptionsBeforeCapture = "sessionRef";
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
