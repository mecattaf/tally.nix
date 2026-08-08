# Usage fixtures — provenance

These streams are not hand-authored. Each is an order-preserving excerpt of a
real harness capture produced by this project's own dispatch corpus, so the
normalizer is tested against topologies production actually emits rather than
against a shape someone imagined while writing the code that reads it
(`AUGUST-02-LEARNINGS.md` §3).

What is preserved verbatim: event types, key sets, nesting, ordering, and every
numeric `usage` object and `total_cost_usd` value. What is redacted, because
this repository is public: all free text, file paths, commands, command output,
and every session, thread, request, and item identifier, each replaced with a
deterministic fixture value. Environment inventory that is neither stream shape
nor usage — tool lists, skills, MCP servers, memory paths — is emptied rather
than reproduced.

| Fixture | Source shape | What it pins |
|---|---|---|
| `codex.jsonl` | one real `codex exec --json` run: `thread.started`, two `error` items, `turn.started`, agent/file-change/command items, `turn.completed` | all five keys real codex reports, including `cache_write_input_tokens: 0` (a measurement, not an absence) and `reasoning_output_tokens` |
| `codex-no-usage.jsonl` | a real complete codex run that hit a provider rate limit and ended in `turn.failed` | typed absence — a real stream that carries no usage anywhere |
| `codex-resume-fresh.jsonl` | reduced from `/home/tom/mecattaf/tally-codex-runs/probe-403/fresh-20260808T092702.jsonl`, captured 2026-08-08 | the fresh thread reading: 16,204 input-as-reported, 11,008 cache-read, 0 cache-write, 5 output, 0 reasoning; normalized total 16,209 |
| `codex-resume-cumulative.jsonl` | reduced from `/home/tom/mecattaf/tally-codex-runs/probe-403/resumed-20260808T092702.jsonl`, captured 2026-08-08 | the first resumed reading rehydrates the thread counters: 32,834 / 26,112 / 0 / 11 / 0; the exact attempt delta is 16,630 input-as-reported, 1,526 uncached input, 15,104 cache-read, 0 cache-write, 6 output, total 16,636. Fresh plus delta is 32,845; the forbidden raw-reading sum is 49,054 |
| `claude-code.jsonl` | one real `claude --print --output-format stream-json` run: `system/init`, assistant turns with `message.usage`, a `rate_limit_event`, a `user` tool-result turn with none, `result` | which `usage` object `$..usage` selects, the `iterations` array nested inside `result.usage`, and `total_cost_usd` beside a per-model `modelUsage.*.costUSD` that must not be read instead |
| `claude-code-no-usage.jsonl` | the same real claude run truncated before its first usage-bearing event | what the capture file holds when a job is preempted during its first turn |
| `n-minus-1-records.json` | hand-written durable records in the pre-#381 shapes | that a row and an `adapter-scrape` attestation written before the usage record existed read back unchanged |

Real codex emits exactly one `turn.completed` per `exec`, so each fixture is
one invocation. The paired resume fixtures share one redacted thread identity;
their cumulative relationship was measured across two real invocations, not
invented by joining them into a stream codex never emits.
