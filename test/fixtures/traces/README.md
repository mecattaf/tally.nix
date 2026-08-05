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

1. One **real** assistant message with `stopReason: "aborted"`, lifted
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
2. `{"type":"agent_settled"}`, to close the stream.

Nothing here is invented: no synthesised usage numbers, no hand-written
aborted turn. What is synthetic is the **splice** — these two real fragments
did not occur in one run.

It exists because a `jsonPathLast` scrape cannot say "last *valid* turn" on
its own. Without a `stopReason` guard the `pi` preset's occupancy capture
lands on the aborted turn's zeroes and `occupancy::context_tokens` returns
`Some(0)` — a fabricated empty context for a session carrying 1032 resident
tokens — rather than `None`. The `adapter-presets` flake check asserts that
the guarded pattern resolves this stream to the last valid turn instead.
