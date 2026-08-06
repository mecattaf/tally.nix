# Eval findings — lane L7 (#388, #389)

Some prose an eval would normally write here: what was verified, what broke,
mutation-test notes, and so on. None of that is read by the checker; only the
marked block below is.

<!-- eval-coverage-manifest:v1 -->
```json
{
  "version": 1,
  "expected": {
    "bullets": ["388:schema", "388:checker", "389:reader-state-store"],
    "files": [
      "test/eval_manifest_check.py",
      "crates/tally-core/src/reader_state.rs"
    ]
  },
  "bullets": [
    {"issue": 388, "bullet": "schema", "status": "covered"},
    {"issue": 388, "bullet": "checker", "status": "covered"},
    {
      "issue": 389,
      "bullet": "reader-state-store",
      "status": "failed",
      "failureClass": "input"
    }
  ],
  "files": [
    {"path": "test/eval_manifest_check.py", "status": "covered"},
    {"path": "crates/tally-core/src/reader_state.rs", "status": "reused"}
  ],
  "run": {"status": "ok"}
}
```
