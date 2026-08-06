# Eval findings — a manifest that omits a reviewed file

`expected.files` names two files the eval was supposed to account for, but
`files` below only has an entry for one of them: `reader_state.rs` is missing
entirely, not merely marked `failed`. This is the proof fixture for "a
manifest that omits a reviewed file" — `eval_manifest_check.py` must reject
it as UNCOVERED SURFACE, not silently pass.

<!-- eval-coverage-manifest:v1 -->
```json
{
  "version": 1,
  "expected": {
    "bullets": ["388:schema"],
    "files": [
      "test/eval_manifest_check.py",
      "crates/tally-core/src/reader_state.rs"
    ]
  },
  "bullets": [
    {"issue": 388, "bullet": "schema", "status": "covered"}
  ],
  "files": [
    {"path": "test/eval_manifest_check.py", "status": "covered"}
  ],
  "run": {"status": "ok"}
}
```
