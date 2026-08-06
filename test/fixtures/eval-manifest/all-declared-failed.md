# Eval findings — every declared surface has a FAILED entry

Round-2 HIGH-9's proof fixture. Both declared bullets and the one declared
file are accounted for — each has an entry — but every one of those entries
is `status: "failed"`. The success line must therefore say the surfaces were
*accounted for*, and break the statuses out, rather than calling them
"covered": nothing here was covered.

Before the repair this printed `2/2 bullets covered; 1/1 files covered`
beside `covered=0 reused=0 failed=3` — a headline computed from
presence-of-a-key while contradicting its own tally three tokens later.

<!-- eval-coverage-manifest:v1 -->
```json
{
  "version": 1,
  "expected": {
    "bullets": ["388:schema", "388:checker"],
    "files": ["test/eval_manifest_check.py"]
  },
  "bullets": [
    {"issue": 388, "bullet": "schema", "status": "failed", "failureClass": "timeout"},
    {"issue": 388, "bullet": "checker", "status": "failed", "failureClass": "budget"}
  ],
  "files": [
    {"path": "test/eval_manifest_check.py", "status": "failed", "failureClass": "input"}
  ],
  "run": {"status": "ok"}
}
```
