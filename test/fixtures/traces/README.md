# Trace fixtures — provenance

`pi.jsonl` and `pi-aborted-turn.jsonl` are the only fixtures in this
directory this file makes a claim about. `claude-code.jsonl` and
`codex.jsonl` predate it and are described by the tests that read them, not
here; nothing below should be read as a statement about their origin.

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

1. One assistant message with `stopReason: "aborted"`, lifted
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
2. `{"type":"agent_settled"}`, to close the stream.

What is synthetic in this file is therefore the **splice** — these real
fragments did not occur in one run — and that one `text` block. No usage
number is synthesised and no aborted turn is hand-written.

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
* `pi.jsonl`'s redactions mean no fixture here carries a real session UUID,
  response id, working directory or free-text body, so nothing in this
  directory can be used to check tally's redaction paths; those have their
  own fixtures.
