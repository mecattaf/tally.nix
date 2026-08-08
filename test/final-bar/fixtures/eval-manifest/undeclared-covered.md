# Eval findings — only an undeclared extra item is covered

<!-- eval-coverage-manifest:v1 -->
```json
{
  "version": 1,
  "expected": {
    "bullets": ["426:declared-only"],
    "files": ["test/eval_manifest_check.py"]
  },
  "bullets": [
    {"issue": 426, "bullet": "declared-only", "status": "failed", "failureClass": "input"},
    {"issue": 999, "bullet": "not-declared", "status": "covered"}
  ],
  "files": [
    {"path": "test/eval_manifest_check.py", "status": "reused"},
    {"path": "undeclared-extra.txt", "status": "covered"}
  ],
  "run": {"status": "ok"}
}
```
