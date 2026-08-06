# Eval findings — a declared key repeated three times

Round-2 HIGH-9's secondary shape. `expected.bullets` names one surface three
times. That is one surface, not three: the declared-surface count is
deduplicated, so this prints `1/1 bullets accounted for`, not `3/3`.

Before the repair a manifest could inflate its own denominator simply by
repeating itself, and one entry read as "3/3".

<!-- eval-coverage-manifest:v1 -->
```json
{
  "version": 1,
  "expected": {
    "bullets": ["389:reader-state-store", "389:reader-state-store", "389:reader-state-store"],
    "files": ["crates/tally-core/src/reader_state.rs"]
  },
  "bullets": [
    {"issue": 389, "bullet": "reader-state-store", "status": "covered"}
  ],
  "files": [
    {"path": "crates/tally-core/src/reader_state.rs", "status": "covered"}
  ],
  "run": {"status": "ok"}
}
```
