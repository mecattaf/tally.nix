# Eval findings — schema-valid, but nothing declared as expected

`bullets` and `files` are both empty and there is no `expected` block. This
manifest is schema-valid — the checker exits 0 — but it must NOT print the
same unqualified `ok` a fully-covered manifest gets. This is HIGH-1's proof
fixture: `eval_manifest_check.py` must say plainly that coverage was not
checked, not merely that the JSON parsed.

<!-- eval-coverage-manifest:v1 -->
```json
{
  "version": 1,
  "bullets": [],
  "files": [],
  "run": {"status": "ok"}
}
```
