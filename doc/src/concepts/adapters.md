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

The stock `codex` template requires the captured session and recorded working
directory, but not a captured model. Real default-model `codex exec --json`
streams do not state which model Codex chose, so recovery leaves the model
absent and lets Codex select its default again. A job that explicitly requested
an authorized model keeps that request in `adapterOptions`; the same declared
`launch.model` override inserts it exactly once on both launch and resume.
Codex's `launch.resumeOptionsBeforeCapture = "sessionRef"` declaration keeps
that option before the positional thread ID required by its resume grammar.

Adapter environment follows the same boundary: names beginning `TALLY_` and
`CREDENTIALS_DIRECTORY` are reserved, and NUL-containing names, values, or argv
are rejected.

The workload head is a typed part of that boundary.
`launch.rejectOptionLikeWorkloadHead` defaults to false, preserving
option-looking strings as opaque workload for adapters whose harness has an
end-of-options separator. The `pi` preset declares it true instead: pi rejects
`--` and keeps parsing a leading-dash first workload element as its own flag,
so tally returns a typed `pre-launch-refusal` before admitting either a launch
or resume. Its structured reason is `option-like-workload-head`, and it names
workload index 0 plus the offending argument. Authorized pre-prompt adapter
options remain part of the prefix and are not workload.

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

The provider-facing `scrape.usage.counterScope` repeats the adapter's
`usageCounterScope` declaration beside the capture whose values it describes;
when present, adapter validation requires the two declarations to agree.

`inputTokens` alone is not the cross-harness "fresh input" figure. claude-code's
cache-write tokens are fresh, uncached prompt tokens its `input_tokens`
excludes; codex declares its own cache-write category
(`cache_write_input_tokens`), observed at 0 on every real capture so far but
not structurally absent. A consumer comparing harnesses adds
`inputTokens + cacheWriteTokens` for both.

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
empty successful trace. All three structured presets declare one: `pi`,
`claude-code`, and `codex` each emit their event stream as JSON lines on
stdout. Framing is what the declaration states, not a promise that every
byte on that stream parses — pi, for one, prints a plain-text line when a
resume lands in the wrong directory, and tally records such a line as a
malformed advisory observation rather than dropping it. An adapter that
declares no trace produces no `TraceGeneration` and no lane in
`tally query trace` — silence that reads as "nothing happened" rather than as
"nothing was declared", which is why the presets declare it.

## Context occupancy is a narrower read than usage

Context is occupancy, not spend, and it is **not** the same value `usage`
normalizes under `totalTokens`. `usage`'s capture keeps the last `usage`
object anywhere in the stream, which for claude-code is the `result` event's
session-lifetime roll-up and for codex is the final `turn.completed`'s
cumulative total — both grow without bound across a session and would render
as many multiples of a fixed context window if read as occupancy.

`contextTokens` instead reads the tokens resident in the context window as of
the attempt's **last valid assistant turn**: input plus both cache halves,
excluding that turn's own output (which has not yet been folded back into
history at the moment this is measured). The `claude-code` preset declares a
dedicated `occupancy` capture, scoped to only `type == "assistant"` events
(`$[?@.type == 'assistant'].message.usage`) rather than `usage`'s
stream-wide `$..usage`, under logical field names of its own
(`residentInputTokens`, `residentCacheReadTokens`, `residentCacheWriteTokens`)
so a lookup for one concern can never resolve against the other's declared
capture. `codex exec --json` states no comparable per-turn figure — it emits
exactly one `turn.completed` per exec, carrying only the cumulative shape —
so the `codex` preset declares no `occupancy` capture, and `contextTokens`
reads `None` for codex rather than restating the cumulative total under
occupancy's name.

`pi` sits at the opposite end of the same argument. Its stream states usage
per assistant message and never per attempt, so the figures it does carry are
occupancy figures. The `pi` preset declares an `occupancy` capture scoped to
assistant `message_end` events reading `input`, `cacheRead`, and `cacheWrite`,
and declares no `usage` key mapping at all: reporting one turn as an
attempt's spend would be the same error codex declines in the other
direction.

That capture additionally excludes turns whose `stopReason` is `aborted` or
`error`, which is what makes it a read of the last **valid** turn. pi
zero-fills every token field on an aborted turn, and `context_tokens` returns
`None` only when all three resident fields are *absent* — three resolved
zeroes are `Some(0)`. Without the guard a session carrying a full context
would report as empty, which is the one reading occupancy exists to prevent.

`contextWindow` is the ceiling that total is measured against, and it has two
independent, distinguishable provenances. A harness that states its own
window inside the captured stream declares it the same way a usage field is —
a capture's `fields` map, resolved through the exact mapping usage reads,
never a parallel mechanism. The `claude-code` preset declares a `contextWindow`
capture beside `usage`, `usageCost`, and `occupancy`, resolved at
`modelUsage.<model>.contextWindow`, a field real captures carry. An operator
may alternatively declare a ceiling in the adapter's `extraConfig.contextWindow`;
a stream-stated window wins when both are present, because it is what the
harness actually applied. `codex` and `pi` declare no `contextWindow` scrape:
no real capture from either has ever stated one, and declaring a key nobody
has observed is a guess wearing a declaration's clothes.

Both fields are independently optional. A scraped `contextTokens` with no
known `contextWindow` is a legitimate state and does not blank the first, and
a `null` draws no bar rather than reading as zero. `contextTokens` and
`contextWindow` are recorded everywhere `sessionRef` is: journal lifecycle
events, `tally query trace` lanes, and `tally query job` /
`tally query jobs --session`, the last two rendering `contextWindow` as a
`SourcedValue` with `advisory-provider-capture` authority for a scraped window
and `advisory-config` authority for a configured one — never
`durable-admission-fact`, because a config ceiling is read from live adapter
configuration and does not survive a daemon restart the way a durable row
field does. Recording only — no admission or scheduling decision reads these
fields.

## Configuration and proof

`nix/lib/adapters.nix` defines the helper and presets.
`crates/tally-core/src/adapters.rs` validates the open registry, enforces the
workload-head policy, renders direct argv, and implements scraping.
`crates/tally-core/src/trace.rs` owns the
advisory trace projection. The `module-layer` flake check proves Nix-to-Rust
rendering, while tests
`open_adapter_dispatches_direct_argv_and_multi_capture_resume`,
`json_path_last_selects_only_the_normative_final_agent_events`,
`unconfigured_pre_prompt_and_unauthorized_values_fail_closed`, and
`acceptance_24_3_harness_jsonl_captures_are_lossless_ordered_and_advisory` pin
the runtime contract.

Find jobs that used one adapter and inspect its sourced captures:

```console
$ tally query jobs --adapter codex | jq '.items[] | {taskUuid, adapter, sessionRef, model, finalMessage}'
```
