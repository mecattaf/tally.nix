# Eval findings — two marked blocks in one file

This is HIGH-2's proof fixture: a findings file that quotes the schema
example (a natural thing to do when an eval prompt is built from the
checker's own docstring) and then carries its own real manifest below. Before
the repair, `find_manifest` used a first-match search and silently graded the
quoted example — `covered=2` — while the real manifest, which declares an
expected file it never covered and a failed run, was never read at all. The
checker must now refuse this file outright rather than pick either block.

Quoted schema example (not the real manifest):

<!-- eval-coverage-manifest:v1 -->
```json
{"version": 1, "bullets": [{"issue": 388, "bullet": "schema", "status": "covered"}], "files": [{"path": "x.rs", "status": "covered"}], "run": {"status": "ok"}}
```

## My actual manifest

<!-- eval-coverage-manifest:v1 -->
```json
{"version": 1, "expected": {"files": ["crates/tally-core/src/reader_state.rs"]}, "bullets": [], "files": [], "run": {"status": "failed", "failureClass": "budget"}}
```
