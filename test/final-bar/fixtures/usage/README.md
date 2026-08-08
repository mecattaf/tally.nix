# Final-bar usage evidence

`codex-fresh-20260808.jsonl` and `codex-resumed-20260808.jsonl` are the raw
Phase 0 probe streams captured by `probe-403/probe.sh` on 2026-08-08 with
Codex CLI 0.145.0.  They are copied byte-for-byte from the probe run directory.
The resumed checkpoint includes the fresh checkpoint; `corpus.json` records
the component-wise delta and the desired normalized two-attempt sum.

The remaining corpus values are declarative wire fixtures.  They intentionally
carry `declaredFields`, `counterScope`, and lineage evidence not present on the
frozen target: the bar describes the resolved contract, not the defect.
