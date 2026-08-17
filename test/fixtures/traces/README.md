# Trace fixtures — provenance

`pi.jsonl`, `pi-aborted-turn.jsonl` and the three `*-quota.jsonl` files
are the fixtures in this directory this file makes a claim about.
`claude-code.jsonl` and `codex.jsonl` predate it and are described by the
tests that read them, not here; nothing below should be read as a statement
about their origin.

## `pi.jsonl`

**Real capture.** This is an order-preserving excerpt of a single real
`pi --mode json` run (pi 0.83.0), captured to prove the `pi` preset's trace
framing, occupancy capture, and scrape patterns against bytes pi actually
emitted rather than against a stream authored to agree with the preset. The
run used a local provider, two turns, and one `read` tool call, and it wrote
21 of these lines plus 167 further `message_update` deltas to **stdout** and
zero bytes to stderr — which is what `stream = "stdout"` and
`framing = "json-lines"` on the preset assert, and what pi's own
`docs/json.md` documents ("Outputs all session events as JSON lines to
stdout").

What is preserved verbatim: event types, key sets, nesting, ordering, and
**every number in every `usage` object**, including the second turn's
`cacheRead: 842` beside `input: 190` — the two figures that show pi's `input`
excludes its cache halves (`input + output + cacheRead + cacheWrite ==
totalTokens`, 190 + 46 + 842 + 0 == 1078).

What is an excerpt: 170 of the run's 188 lines were `message_update` deltas,
and three representative ones are kept (`thinking_start`, `toolcall_end`,
`text_end`) rather than all of them. No line was reordered, and no line was
synthesised — there is no invented event type, no invented malformed line,
and no invented trailing garbage in this file.

What is redacted, because this repository is public: the session UUID, the
response ids, the response model (a store path in the original), the working
directory, the tool-call id, and every free-text `thinking`/`text` body, each
replaced with a deterministic fixture value. The redacted `thinking` text is
one constant string wherever it appears, so it is obviously a fixture value
and not a transcript.

Read by the `adapter-presets` flake check, which renders the `pi` preset
against this file and asserts the resolved `sessionRef`, `model`, `usage`,
`occupancy`, and `finalMessage` captures.

## `pi-aborted-turn.jsonl`

**Composed from two real sources, and not a single capture.** Saying so
exactly is the point of this section. It is `pi.jsonl` above with its final
`agent_settled` removed, then two appended lines:

1. One aborted assistant **turn**, in the three-record shape pi emits for
   every message (`message_start`, `message_update`, `message_end`), preceded
   by a bare `{"type":"turn_start"}`.

   The `message_end` is the real one: one assistant message with
   `stopReason: "aborted"`, lifted
   verbatim from a different real pi session on the machine this fixture was
   built on (a session tally did not produce), and reframed as a
   `message_end` stream event — the framing `--mode json` uses for the same
   message object. Its `usage` is verbatim: pi zero-fills every token field
   on an aborted turn, `{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,
   "totalTokens":0,...}`, with no `reasoning` key. Its `errorMessage`
   (`"Operation aborted"`), `api`, `provider`, `model`, `stopReason` and
   `timestamp` are verbatim too. Its `thinking` body is redacted to the same
   constant fixture string used above. The `model` differs from the rest of
   the stream (`qwen3-vl-8b-ocr`) because it genuinely came from another
   session; that is left as it is rather than rewritten to look seamless.

   **One block of this message is synthetic and it is the only synthesised
   content in either pi fixture:** the `{"type":"text","text":"The file
   notes.txt cont"}` block in its `content` array. The real message carries
   no `text` block, because the turn it came from was aborted *during model
   load* — before any text was generated. That made the fixture structurally
   unable to observe the thing it exists for: with no text to find,
   `finalMessage` fell through to the last valid turn whether or not it was
   guarded, so an unguarded `finalMessage` looked correct here. The added
   block is a deliberately truncated prefix of the valid turn's answer
   (`The file notes.txt contains 42.`), which is what a mid-generation abort
   actually leaves behind, and it is short enough that a reader who ever
   sees it rendered can tell it is a fragment.

   **The `message_start` and `message_update` are derived, not captured**,
   and they matter more than they look. Each is that same real message with
   the fields pi sets mid-stream: `stopReason: "pending"`, no
   `errorMessage`, `content: []` on the `message_start` with its `usage`
   zeroed, and the `message_update` carrying a `text_end`
   `assistantMessageEvent` for the partial text above. No value in either is
   invented — every one is copied from the real `message_end` or is the
   literal pi writes at that point in the lifecycle, with one qualification:
   the `message_update`'s `stopReason` is carried over as the
   `message_start` states it (`pending`). In the one real capture on hand
   every `*_end` assistant event carries the message's *final* stopReason,
   so the `text_end`-with-`pending` pairing is not attestable here and is
   not claimed as literal capture provenance. It costs nothing: all three
   captures are `message_end`-scoped, so no capture reads this field.

   They are here because **a bare spliced `message_end` is not a shape
   `pi --mode json` can produce.** pi's own `docs/json.md` message lifecycle
   emits `message_start` → `message_update`* → `message_end` for every
   message, each carrying the same `AgentMessage` — so the same `role` and
   the same `model`, under `stopReason: "pending"` until the message closes.
   A fixture without them cannot see a guard that excludes an invalid turn's
   `message_end` and then reads that turn's model straight back out of its
   `pending` records; that is exactly the defect that shipped in this
   fixture's first version and was caught in review. It is doubly required
   now that the message carries partial text, which under pi's streaming
   model exists *because* `message_update` deltas produced it.
2. `{"type":"agent_settled"}`, to close the stream.

What is synthetic in this file is therefore the **splice** — these real
fragments did not occur in one run — that one `text` block, and the two
derived lifecycle records described above. No usage number is synthesised
and no aborted turn is hand-written.

It exists because a `jsonPathLast` scrape cannot say "last *valid* turn" on
its own. Without a `stopReason` guard the `pi` preset's occupancy capture
lands on the aborted turn's zeroes and `occupancy::context_tokens` returns
`Some(0)` — a fabricated empty context for a session carrying 1032 resident
tokens — rather than `None`. The same holds for the other two captures an
operator reads: unguarded, `finalMessage` resolves to `The file notes.txt
cont` and `model` to `qwen3-vl-8b-ocr`, a model no valid turn of this
session used, which the rendered resume argv then carries. The
`adapter-presets` flake check asserts that all three guarded patterns
resolve this stream to the last valid turn instead, and asserts the argv
whole because the argv is where the wrong model became operator-visible.

For all three, the guard is the `stopReason` clauses **and** the scoping to
assistant `message_end` together; neither half is sufficient. A
clause-guarded filter that also matches `message_start` / `message_update`
resolves the excluded turn's model out of its `pending` records — which is
what makes the lifecycle records above load-bearing rather than cosmetic.
One consequence is worth stating here too: an attempt whose stream never
closed an assistant `message_end` yields no `model` capture at all, so its
resume refuses rather than rendering. No fixture in this directory has that
shape.

### What these fixtures still cannot see

Stated because the question is worth answering once rather than
rediscovering it per finding.

* Neither fixture exercises the `error` half of the `stopReason` guard. pi's
  in-stream context-overflow signal is the reachable invalid-turn branch for
  a non-interactive run, and no capture of one has been taken here; the
  clause is guarded on that description plus SSSF's precedent, and the
  `adapters.rs` unit mirror is the only place both halves are exercised.
* `pi.jsonl` is two turns, so nothing here shows the `message_update` echo's
  super-linear growth at the scale where it matters (the full run these 21
  lines were excerpted from wrote 260 KB); the 16 MiB trace read bound is
  not reached by any committed fixture.
* Neither fixture can say anything about resume behaviour. pi keys its
  session store by launch cwd and states that cwd in the session header, but
  a captured stream is one process's output — a cross-cwd resume is a
  property of the *next* launch, which no fixture observes.
* Neither fixture has a stream that ends with an assistant turn still open —
  a `message_start` with no matching `message_end`, which is what a
  SIGINT-truncated run leaves. That is the shape where the `message_end`
  scoping costs something (no `model` capture, so the resume refuses), and
  no committed fixture exercises it.
* `pi.jsonl`'s redactions mean no fixture here carries a real session UUID,
  response id, working directory or free-text body, so nothing in this
  directory can be used to check tally's redaction paths; those have their
  own fixtures.

## The three `*-quota.jsonl` fixtures

`claude-code-quota.jsonl`, `codex-quota.jsonl` and `pi-quota.jsonl` exist for
the `terminal` scrape capture each of those presets declares — the
adapter-terminal outcome class of vestige-sweep V-16. Each ends on the event
genre its harness uses to state a terminal condition, and each of those
events carries a provider usage-limit message naming the time the wall lifts,
because the reset time surviving into the outcome envelope is the whole point
of the class.

**None of the three is a recorded capture of a quota-terminated run, and
saying so exactly is the point of this section.** No such stream was captured
on the machine these were built on. What is real about them is the *shape*, and the shapes come from
different places, which is why they are described separately below rather
than as one family. Nothing here should be read as evidence about what any
provider's refusal text actually says: the message bodies are fixture prose
written to carry a reset time, not transcribed refusals. The claim these
fixtures support is narrow and structural — that the declared capture selects
the terminal event and reads its text out of the path the preset names — and
that claim does not depend on the wording.

* `claude-code-quota.jsonl`. The terminating line is
  `{"type":"error","message":"..."}`, the genre and key the vestige ledger
  records from the incident that commissioned this work
  (`specs/substrate/evidence/vestige-sweep.md`, V-16 — *"a quota-terminated
  stream ends `{"type":"error","message":"You've hit your usage limit... try
  again at Aug 20th"}` and emits NO result"*). The ledger is the only source
  for it; the preceding `system`/`assistant`/`user` lines are modelled on the
  committed `claude-code.jsonl`, with `usage` objects added so the same
  fixture can observe token spend surviving a walled lane. The absence
  matters as much as the presence: there is **no `result` event**, which is
  exactly why `finalMessage` cannot scrape such a stream and why the lane
  used to exit with no envelope at all.
* `codex-quota.jsonl`. The terminating line is
  `{"type":"turn.failed","error":{"message":"..."}}`. codex states a terminal
  condition in two genres and the preset declares both; a single stream can
  only carry one, so this file carries `turn.failed` and the stream-level
  `{"type":"error","message":"..."}` genre is exercised from an inline
  synthetic line in the driver's own test. Neither genre is attested by a
  recorded capture here — both are declared on the same footing as the rest
  of the codex preset's flags, and the fixture proves only that the declared
  candidate paths resolve the text where each genre puts it. There is no
  `turn.completed` in this file, so it also fixes the honest reading of a
  refused turn's spend: `not-reported`, never a zero.
* `pi-quota.jsonl`. **Part real.** Lines 1–19 are `pi.jsonl` verbatim — the
  real two-turn capture described above, truncated before its `agent_end` and
  `agent_settled`, which a refused turn never reaches. Appended are a
  `turn_start` and one derived `message_start`/`message_end` pair for the
  refused turn. Every field in that pair is copied from the real capture's own
  records or is the literal pi writes at that point in the lifecycle: the
  `usage` object is pi's zero-fill for an invalid turn, exactly as
  `pi-aborted-turn.jsonl` documents it, and `errorMessage` is the field that
  fixture shows carrying `Operation aborted` on the `aborted` sibling of this
  same `stopReason` guard. What is synthesised is the splice, the
  `stopReason: "error"` value, and the `errorMessage` body.

  The real prefix is load-bearing rather than decorative. The refused turn's
  usage is zero-filled, so a fixture without a preceding valid turn could not
  see that `tokenSpend` and `occupancy` still report the work that happened
  (input 190, output 46, cacheRead 842) while `terminal` reports the wall —
  which is the property those two captures' `stopReason` guards exist for,
  read from both sides at once. The `message_start` is present for the reason
  the aborted fixture states: a bare spliced `message_end` is not a shape
  `pi --mode json` can produce.

### What these three still cannot see

* Nothing here observes a provider's real refusal text, so no fixture in this
  directory can be used to check any parsing of a reset time out of a
  message. Tally deliberately does none: the message travels whole.
* Nothing here observes what a harness does *after* it states a terminal
  event — whether the process exits non-zero, whether more lines follow — so
  these fixtures say nothing about exit-code handling, only about the scrape.
* `claude-code-quota.jsonl` and `codex-quota.jsonl` carry no real session
  identifiers, working directories or free-text bodies, for the same reason
  `pi.jsonl` is redacted: this repository is public.

### What reads them

`crates/spec-build-driver/src/adapter_outcome.rs`, against the preset
declarations in `test/fixtures/spec-build/adapter-terminal-catalog.json`.
That file is a snapshot of `nix/lib/adapters.nix`'s preset catalog, generated
by evaluating it (`nix eval` over `presets`, filtered to the adapters these
cases exercise) rather than hand-typed, and embedded in the driver rather
than read from the repository root so the packaged build carries it. The nix
expression stays the authority: regenerate the snapshot when a preset's
declarations change.
