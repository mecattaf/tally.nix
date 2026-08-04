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
strings. `usage` is the fourth declared capture and holds structured data; it
is what a normalized usage record is read from. Captures can enrich query rows,
enable resume, and populate an attestation, but they cannot claim a verdict,
artifact hash, evidence result, charge, or authorship.

## Usage is normalized at the adapter boundary

Harnesses disagree about token accounting. codex counts cached prompt tokens
inside the `input_tokens` figure it reports; claude-code reports
`cache_read_input_tokens` and `cache_creation_input_tokens` beside an
`input_tokens` figure that excludes both. tally does not learn either shape in
Rust. A capture may declare a `fields` map from a logical name to the ordered
candidate paths that carry it inside the captured value — `$` (or the empty
string) is the captured value itself, anything else is dot-separated object
keys with numeric segments indexing arrays — and the first candidate that
resolves to a non-null value wins. Adding a harness is an attrset in
`nix/lib/adapters.nix`.

The record reads `inputTokens` (input excluding cache), or
`inputTokensWithCacheRead` where the harness's own figure already contains the
cache read, plus `cacheReadTokens`, `cacheWriteTokens`, `outputTokens`,
`reasoningTokens` (nested within output, never added to it), `totalTokens`, and
`costUsd`. Cost is only ever what the harness reported; tally has no pricing
table and computes no dollar figure. A total the harness did not state is
derived from the components and labelled as derived.

`inputTokens` alone is not the cross-harness "fresh input" figure. claude-code's
cache-write tokens are fresh, uncached prompt tokens its `input_tokens`
excludes; codex has no cache-write category at all. A consumer comparing
harnesses adds `inputTokens + cacheWriteTokens`.

Three states are kept apart, and none of them is a zero:

- `not-declared` — the adapter declared no usage scrape;
- `not-reported` — a usage scrape was declared and the stream carried none;
- `reported` — the harness reported usage, including when it reported zero.

Only `reported` has a durable seat: it is persisted in the advisory attestation
ledger beside the raw capture, keyed by task, attempt, and lease epoch. The two
absences are recorded on the live row, so after a daemon restart they read back
as a missing field rather than as a stated absence — both are recomputable from
the adapter configuration, but a consumer counting coverage should treat a
missing record and a `not-declared` record as the same answer.

`tally query job` renders the record as a `SourcedValue` with
`advisory-provider-capture` authority and `adapter-scrape` provenance. The
built-in pool meter reads the same record and charges what it always charged: a
harness-stated total, else the harness's own input figure plus its output
figure. A capture with no declared `fields` keeps exactly that legacy reading.
Two shapes no harness emits — a key present with a JSON `null`, and a
whole-valued float — now charge a number where the old reader charged nothing;
both diverge upward, which for a windowed-consumption pool is the conservative
direction.

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
