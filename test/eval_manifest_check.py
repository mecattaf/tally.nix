#!/usr/bin/env python3
"""Validate the coverage-manifest section of an eval findings file.

An adversarial eval writes a findings file (a plain Markdown document) at the
end of its run. "The eval ran" and "the eval covered every acceptance bullet
and every reviewed file" are both currently a vibe in that document -- prose a
reader has to trust. This gives the eval a place to make that claim in a
checkable shape, mirroring what `GateManifestSpec.required_gate_ids`
(`crates/tally-core/src/completion.rs`) already does for the gate side.

Schema
------

A findings file embeds ONE coverage manifest as a fenced JSON code block,
directly preceded by the exact marker line:

    <!-- eval-coverage-manifest:v1 -->
    ```json
    { ... }
    ```

The marker exists so this checker never mistakes some other JSON block in the
findings prose (a fixture excerpt, a captured RPC response) for the manifest.
The JSON object has this shape:

    {
      "version": 1,
      "expected": {                 # optional; enables "uncovered surface"
        "bullets": ["388:schema", "389:reader-state-store"],
        "files": ["crates/tally-core/src/reader_state.rs"]
      },
      "bullets": [
        {"issue": 388, "bullet": "schema", "status": "covered"},
        {"issue": 389, "bullet": "reader-state-store", "status": "failed",
         "failureClass": "input"}
      ],
      "files": [
        {"path": "crates/tally-core/src/reader_state.rs", "status": "covered"}
      ],
      "run": {"status": "ok"}
    }

`bullets` carries one entry per acceptance bullet the eval actually judged,
keyed by `(issue, bullet)`; `bullet` is a short slug the eval author picks,
not the bullet's full prose. `files` carries one entry per file the eval
actually read. Both are ITEM-level: `status` is `covered` (verified true),
`reused` (accepted on evidence carried over from an earlier round, not
independently reverified this round), or `failed` (the eval tried and could
not verify it) -- OCR's `covered / reused / failed-with-class` triad. A
`failed` item MUST carry `failureClass`, one of: `timeout`, `budget`,
`input`, `unknown`. `unknown` is the mandatory catch-all: an eval that cannot
name why an item failed still must not leave it untyped.

`run` is a SEPARATE, run-level taxonomy from the item-level one above -- one
failed item does not mean the eval run itself failed, and a run that died
outright (crashed, hit its own wall-clock budget) is a different claim than
"this one bullet timed out." `run.status` is `ok` or `failed`; a `failed` run
MUST carry `failureClass`, one of: `timeout`, `budget`, `crash`, `unknown`.

`expected` is optional and enables the checker's one structural check beyond
schema validity: every key in `expected.bullets` / `expected.files` (each a
canonical `"issue:bullet"` string or a plain path, matching the keys the
`bullets` / `files` entries above compute to) must have a matching entry
somewhere in `bullets` / `files` -- not merely a `covered` one; `failed` is a
legitimate, typed outcome, but an item silently missing an entry at all is
the omission this checker exists to catch. An `expected` key with no matching
entry is reported as UNCOVERED SURFACE and fails the check.

Usage: eval_manifest_check.py <findings-file.md>...

Exit 0 and a summary line per file when every manifest present is schema-valid
and has no uncovered surface. Exit 1 and one line per problem otherwise. A
findings file with no manifest section at all is reported, not silently
skipped -- "no manifest" and "an invalid manifest" are different failures,
but both are failures for a checker whose whole job is proving the eval
made this claim at all.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Any

MARKER = "<!-- eval-coverage-manifest:v1 -->"
BLOCK = re.compile(
    re.escape(MARKER) + r"\s*\n```json\s*\n(.*?\n)```",
    re.DOTALL,
)

ITEM_STATUSES = {"covered", "reused", "failed"}
ITEM_FAILURE_CLASSES = {"timeout", "budget", "input", "unknown"}
RUN_STATUSES = {"ok", "failed"}
RUN_FAILURE_CLASSES = {"timeout", "budget", "crash", "unknown"}


class ManifestError(Exception):
    """One schema violation, carrying enough context to report on its own."""


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise ManifestError(message)


def _item_key(kind: str, entry: dict[str, Any], index: int) -> str:
    if kind == "bullets":
        issue = entry.get("issue")
        bullet = entry.get("bullet")
        return f"{issue}:{bullet}"
    path = entry.get("path")
    return str(path) if path is not None else f"<files[{index}] with no path>"


def _check_item(kind: str, entry: Any, index: int) -> str:
    """Validate one `bullets` / `files` entry; return its canonical key."""
    _require(isinstance(entry, dict), f"{kind}[{index}] is not an object")
    if kind == "bullets":
        issue = entry.get("issue")
        bullet = entry.get("bullet")
        _require(
            isinstance(issue, int) and not isinstance(issue, bool) and issue > 0,
            f"{kind}[{index}].issue must be a positive integer",
        )
        _require(
            isinstance(bullet, str) and bullet.strip() != "",
            f"{kind}[{index}].bullet must be a non-empty string",
        )
    else:
        path = entry.get("path")
        _require(
            isinstance(path, str) and path.strip() != "",
            f"{kind}[{index}].path must be a non-empty string",
        )
    status = entry.get("status")
    _require(
        status in ITEM_STATUSES,
        f"{kind}[{index}].status {status!r} must be one of {sorted(ITEM_STATUSES)}",
    )
    failure_class = entry.get("failureClass")
    if status == "failed":
        _require(
            failure_class in ITEM_FAILURE_CLASSES,
            f"{kind}[{index}].failureClass {failure_class!r} must be one of "
            f"{sorted(ITEM_FAILURE_CLASSES)} when status is 'failed'",
        )
    else:
        _require(
            failure_class is None,
            f"{kind}[{index}].failureClass must be absent when status is {status!r}",
        )
    extra = set(entry) - {"issue", "bullet", "path", "status", "failureClass"}
    _require(not extra, f"{kind}[{index}] has unknown field(s) {sorted(extra)}")
    return _item_key(kind, entry, index)


def _check_run(run: Any) -> None:
    _require(isinstance(run, dict), "run must be an object")
    status = run.get("status")
    _require(
        status in RUN_STATUSES, f"run.status {status!r} must be one of {sorted(RUN_STATUSES)}"
    )
    failure_class = run.get("failureClass")
    if status == "failed":
        _require(
            failure_class in RUN_FAILURE_CLASSES,
            f"run.failureClass {failure_class!r} must be one of "
            f"{sorted(RUN_FAILURE_CLASSES)} when status is 'failed'",
        )
    else:
        _require(failure_class is None, "run.failureClass must be absent when status is 'ok'")
    extra = set(run) - {"status", "failureClass"}
    _require(not extra, f"run has unknown field(s) {sorted(extra)}")


def _check_expected(kind: str, expected: Any, actual_keys: set[str]) -> list[str]:
    if expected is None:
        return []
    _require(isinstance(expected, list), f"expected.{kind} must be a list")
    for index, key in enumerate(expected):
        _require(isinstance(key, str) and key.strip() != "", f"expected.{kind}[{index}] must be a non-empty string")
    return [key for key in expected if key not in actual_keys]


class Report:
    """The result of checking one manifest: valid or not, plus coverage counts."""

    def __init__(self) -> None:
        self.errors: list[str] = []
        self.uncovered: list[str] = []
        self.covered = 0
        self.reused = 0
        self.failed = 0

    @property
    def ok(self) -> bool:
        return not self.errors and not self.uncovered


def check_manifest(manifest: Any) -> Report:
    report = Report()
    try:
        _require(isinstance(manifest, dict), "manifest is not a JSON object")
        _require(manifest.get("version") == 1, "version must be the integer 1")
        extra = set(manifest) - {"version", "expected", "bullets", "files", "run"}
        _require(not extra, f"manifest has unknown top-level field(s) {sorted(extra)}")

        for kind in ("bullets", "files"):
            _require(kind in manifest, f"manifest is missing '{kind}'")
            _require(isinstance(manifest[kind], list), f"'{kind}' must be a list")

        bullet_keys: set[str] = set()
        seen_bullets: set[str] = set()
        for index, entry in enumerate(manifest["bullets"]):
            key = _check_item("bullets", entry, index)
            _require(key not in seen_bullets, f"bullets[{index}] duplicates {key!r}")
            seen_bullets.add(key)
            bullet_keys.add(key)
            _tally(report, entry.get("status"))

        file_keys: set[str] = set()
        seen_files: set[str] = set()
        for index, entry in enumerate(manifest["files"]):
            key = _check_item("files", entry, index)
            _require(key not in seen_files, f"files[{index}] duplicates {key!r}")
            seen_files.add(key)
            file_keys.add(key)
            _tally(report, entry.get("status"))

        _require("run" in manifest, "manifest is missing 'run'")
        _check_run(manifest["run"])

        expected = manifest.get("expected")
        if expected is not None:
            _require(isinstance(expected, dict), "expected must be an object")
            extra = set(expected) - {"bullets", "files"}
            _require(not extra, f"expected has unknown field(s) {sorted(extra)}")
            report.uncovered.extend(
                f"bullet {key!r} is in expected.bullets but has no bullets[] entry"
                for key in _check_expected("bullets", expected.get("bullets"), bullet_keys)
            )
            report.uncovered.extend(
                f"file {key!r} is in expected.files but has no files[] entry"
                for key in _check_expected("files", expected.get("files"), file_keys)
            )
    except ManifestError as error:
        report.errors.append(str(error))
    return report


def _tally(report: Report, status: object) -> None:
    if status == "covered":
        report.covered += 1
    elif status == "reused":
        report.reused += 1
    elif status == "failed":
        report.failed += 1


def find_manifest(text: str) -> Any | None:
    """The parsed manifest block, or None if the marker is absent.

    A marker present with unparsable JSON after it is not "absent" -- that is
    a malformed manifest, reported by the caller as a JSON decode failure,
    not silently treated as "no manifest here."
    """
    match = BLOCK.search(text)
    if match is None:
        return None
    return json.loads(match.group(1))


def main(paths: list[str]) -> int:
    if not paths:
        print("usage: eval_manifest_check.py <findings-file.md>...", file=sys.stderr)
        return 2
    failures = 0
    for raw in paths:
        path = Path(raw)
        text = path.read_text(encoding="utf-8")
        try:
            manifest = find_manifest(text)
        except json.JSONDecodeError as error:
            print(f"{path}: manifest block is not valid JSON: {error}", file=sys.stderr)
            failures += 1
            continue
        if manifest is None:
            print(f"{path}: no {MARKER!r} coverage-manifest block found", file=sys.stderr)
            failures += 1
            continue
        report = check_manifest(manifest)
        for error in report.errors:
            print(f"{path}: {error}", file=sys.stderr)
        for gap in report.uncovered:
            print(f"{path}: UNCOVERED: {gap}", file=sys.stderr)
        if report.ok:
            print(
                f"{path}: ok "
                f"(covered={report.covered} reused={report.reused} failed={report.failed})"
            )
        else:
            failures += 1
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
