# Eval findings — schema-valid, but nothing declared as expected

`bullets` and `files` are both empty and there is no `expected` block. This
manifest is schema-valid, but it must NOT be mistakable for one whose
declared surface was fully accounted for — neither by a reader nor by a
machine. This is round-1 HIGH-1's and round-2 HIGH-10's proof fixture:
`eval_manifest_check.py` must say plainly that coverage was not checked
(`coverage=unchecked`) and must exit `3`, not `0`.

<!-- eval-coverage-manifest:v1 -->
```json
{
  "version": 1,
  "bullets": [],
  "files": [],
  "run": {"status": "ok"}
}
```
