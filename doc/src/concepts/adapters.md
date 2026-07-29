# Adapters

An adapter is a declarative argv harness for an already admitted job. The
registry is an open map: a Nix configuration can add a named adapter without
changing Rust or adding a new producer kind.

tally ships `shell`, `pi`, `claude-code`, and `codex` presets. They are ordinary
entries built with the same `mkAdapter` helper available to a deployment.

## Direct launch and resume

For a fresh launch, tally renders:

```text
adapter argv prefix + authorized job options + workload argv
```

Every component remains a string element. There is no shell interpolation.
Launch-time approval, sandbox, model, effort, working-directory, and
pre-prompt options are accepted only when that adapter declares how to render
and authorize them. An unrecognized policy or value fails admission rather
than becoming a free-form flag.

An adapter may also declare a resume argv template. Placeholders such as
`%<sessionRef>%` are filled from prior captures, and resume fails if a required
capture is absent. An adapter without a resume template cannot silently turn a
continuation into a fresh launch.

Adapter environment follows the same boundary: names beginning `TALLY_` and
`CREDENTIALS_DIRECTORY` are reserved, and NUL-containing names, values, or argv
are rejected.

## Scraping is advisory

After the canonical terminal acknowledgement, tally may read the selected
private stdout or stderr capture and apply one of three modes:

- `regex` enables line anchors for `^` and `$` and retains the last match,
  using the first capture group when present (`\A` and `\z` remain available
  for whole-capture anchors);
- `jsonPath` evaluates the expression against each JSON value in the stream
  and retains the last non-null result;
- `jsonPathLast` parses the complete JSON-lines stream as one array, evaluates
  the expression once, and retains the last non-null match.

The third mode is necessary for event-shape predicates such as “the last Codex
`item.completed` whose item is an agent message.” Older prose listed only the
first two modes; the shipped enum is exactly `regex`, `jsonPath`, and
`jsonPathLast`.

Reserved captures `sessionRef`, `model`, and `finalMessage` must be JSON
strings. `usage` may be structured data for the built-in token meter. Captures
can enrich query rows, enable resume, and populate an attestation, but they
cannot claim a verdict, artifact hash, evidence result, charge, or authorship.

An adapter may additionally declare a JSON-lines trace stream. Trace queries
preserve event order and malformed/unknown payloads as advisory observations;
a configured running local or remote trace is not silently reported as an
empty successful trace.

## Configuration and proof

`nix/lib/adapters.nix` defines the helper and presets.
`crates/tally-core/src/adapters.rs` validates the open registry, renders direct
argv, and implements scraping. `crates/tally-core/src/trace.rs` owns the
advisory trace projection. The `module-layer` flake check proves Nix-to-Rust
rendering, while tests
`open_adapter_dispatches_direct_argv_and_multi_capture_resume`,
`json_path_last_selects_only_the_normative_final_agent_events`,
`unconfigured_pre_prompt_and_unauthorized_values_fail_closed`, and
`acceptance_24_3_claude_and_codex_jsonl_are_lossless_ordered_and_advisory` pin
the runtime contract.

Find jobs that used one adapter and inspect its sourced captures:

```console
$ tally query jobs --adapter codex | jq '.items[] | {taskUuid, adapter, sessionRef, model, finalMessage}'
```
