# Eval findings — a manifest with an untyped failure class

The `389:reader-state-store` bullet below is marked `failed` with
`failureClass: "flaky"` — not one of the typed classes (`timeout`, `budget`,
`input`, `unknown`). This is the proof fixture for "one with an unknown
failure class" — `eval_manifest_check.py` must reject it, not accept an
ad hoc string in place of the mandatory `unknown` catch-all.

<!-- eval-coverage-manifest:v1 -->
```json
{
  "version": 1,
  "bullets": [
    {
      "issue": 389,
      "bullet": "reader-state-store",
      "status": "failed",
      "failureClass": "flaky"
    }
  ],
  "files": [],
  "run": {"status": "ok"}
}
```
