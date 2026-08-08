# Eval findings — at least one declared item directly covered

<!-- eval-coverage-manifest:v1 -->
```json
{
  "version": 1,
  "expected": {
    "bullets": ["426:exit-code", "426:precedence"],
    "files": ["test/eval_manifest_check.py"]
  },
  "bullets": [
    {"issue": 426, "bullet": "exit-code", "status": "covered"},
    {"issue": 426, "bullet": "precedence", "status": "failed", "failureClass": "input"}
  ],
  "files": [
    {"path": "test/eval_manifest_check.py", "status": "reused"}
  ],
  "run": {"status": "ok"}
}
```
