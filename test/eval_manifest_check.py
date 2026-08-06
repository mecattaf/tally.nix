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

Declining `expected` (or declaring it with empty lists) is a legitimate,
self-contained choice -- this checker never shells out to `git`/`gh` to
discover the surface a findings file was supposed to cover, and that design
is intentional. But it means the checker CANNOT independently know an eval
reviewed anything at all: a manifest with empty `bullets`, empty `files`,
and no `expected` is schema-valid on exactly the same terms as one that
accounted for everything. Both the success line and the EXIT CODE say so
(see "Exit codes" below) -- "schema-valid" is a real but much weaker claim,
and the two cases must be distinguishable without reading English, because
the consumer this checker exists for is an orchestrator close-out reading a
status, not a person reading a sentence.

Note what the declared-surface count does and does not say. A declared key
is *accounted for* when it has an entry; that entry may be `covered`,
`reused`, or `failed`. So the line reads "N/N bullets accounted for
(M covered, K reused, F failed)" and never "N/N covered" -- the number of
declared keys is a count of surfaces the eval WROTE DOWN, not of surfaces it
verified, and conflating the two is precisely the defect this whole file
exists to make unrepeatable.

The exact wire shape is designed to make a SECOND embedded copy of this
schema example a hazard: an eval prompt built by quoting this docstring
verbatim (a natural thing to do, and the intended adoption path) reproduces
the same marker line inside a findings file that also carries the eval's
real manifest. Rather than silently picking one, this checker refuses a
findings file with more than one marked block -- do not paste this example
into a findings document verbatim; describe the shape in prose instead, or
make sure only the real manifest carries the literal marker line.

Usage: eval_manifest_check.py <findings-file.md>...

Exit codes (the contract for a mechanical consumer; the worst outcome across
all files given wins):

    0  every manifest is schema-valid, every declared surface is accounted
       for, AND both categories declared a surface -- i.e. coverage was
       actually checked. This is the only code that licenses "the eval
       covered what it said it would."
    1  at least one file was refused: schema-invalid, unparsable, missing a
       manifest, carrying more than one, or declaring a surface with no
       matching entry.
    2  usage error (no paths given).
    3  every manifest is schema-valid, but at least one declared no surface
       in one or both categories, so its coverage was NOT checked. Schema
       validity is real; a coverage claim is not available. A close-out that
       treats 3 as success is asserting something this tool did not verify.

Each success line also carries a stable `coverage=checked` / `coverage=unchecked`
token for consumers that grep rather than branch on status. A findings file
with no manifest section at all, or with more than one, is reported, not
silently resolved -- "no manifest," "an invalid manifest," and "an ambiguous
manifest" are three different failures, and all three are failures for a
checker whose whole job is proving the eval made this claim at all,
unambiguously.
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

# Exit codes. These are the contract for a mechanical consumer (an
# orchestrator close-out that "names the checker" per #388's second
# acceptance bullet); see this module's docstring. The distinction between
# EXIT_OK and EXIT_COVERAGE_UNCHECKED exists because a close-out reads a
# status, not English: without it, a manifest that declared no surface to be
# held to is indistinguishable from one that declared and accounted for
# everything, which was round-2 HIGH-10.
EXIT_OK = 0
EXIT_INVALID = 1
EXIT_USAGE = 2
EXIT_COVERAGE_UNCHECKED = 3


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


def _declared_keys(kind: str, expected: Any) -> list[str]:
    """The DISTINCT keys `expected.<kind>` names, in first-seen order.

    Deduplicated on purpose. `["389:store", "389:store", "389:store"]` names
    one surface three times, not three surfaces; counting the raw list length
    would let a manifest inflate its own denominator by repeating itself
    (round-2 HIGH-9's secondary shape).
    """
    if expected is None:
        return []
    _require(isinstance(expected, list), f"expected.{kind} must be a list")
    for index, key in enumerate(expected):
        _require(
            isinstance(key, str) and key.strip() != "",
            f"expected.{kind}[{index}] must be a non-empty string",
        )
    return list(dict.fromkeys(expected))


class Report:
    """The result of checking one manifest: valid or not, plus coverage counts.

    `covered` / `reused` / `failed` tally EVERY entry in `bullets` + `files`.
    `declared` and `status_by_key` describe only the subset an `expected`
    block named, which is the only subset whose completeness this checker can
    speak to at all.
    """

    def __init__(self) -> None:
        self.errors: list[str] = []
        self.uncovered: list[str] = []
        self.covered = 0
        self.reused = 0
        self.failed = 0
        # kind -> distinct declared keys, and kind -> key -> that entry's status.
        self.declared: dict[str, list[str]] = {"bullets": [], "files": []}
        self.status_by_key: dict[str, dict[str, str]] = {"bullets": {}, "files": {}}

    @property
    def ok(self) -> bool:
        return not self.errors and not self.uncovered

    def declared_statuses(self, kind: str) -> dict[str, int]:
        """How the entries satisfying `expected.<kind>` are actually typed.

        This is the honest basis for any sentence about declared surface:
        a declared key is *accounted for* when it has an entry, but that
        entry may say `covered`, `reused`, or `failed`, and only the first
        of those means what the English word "covered" means.
        """
        statuses = self.status_by_key[kind]
        counts = {"covered": 0, "reused": 0, "failed": 0}
        for key in self.declared[kind]:
            status = statuses.get(key)
            if status in counts:
                counts[status] += 1
        return counts


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

        for kind in ("bullets", "files"):
            seen: set[str] = set()
            for index, entry in enumerate(manifest[kind]):
                key = _check_item(kind, entry, index)
                _require(key not in seen, f"{kind}[{index}] duplicates {key!r}")
                seen.add(key)
                report.status_by_key[kind][key] = entry.get("status")
                _tally(report, entry.get("status"))

        _require("run" in manifest, "manifest is missing 'run'")
        _check_run(manifest["run"])

        expected = manifest.get("expected")
        if expected is not None:
            _require(isinstance(expected, dict), "expected must be an object")
            extra = set(expected) - {"bullets", "files"}
            _require(not extra, f"expected has unknown field(s) {sorted(extra)}")
            for kind, noun in (("bullets", "bullet"), ("files", "file")):
                declared = _declared_keys(kind, expected.get(kind))
                report.declared[kind] = declared
                report.uncovered.extend(
                    f"{noun} {key!r} is in expected.{kind} but has no {kind}[] entry"
                    for key in declared
                    if key not in report.status_by_key[kind]
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


class MultipleManifestsError(Exception):
    """More than one marked block was found; picking one would be a guess."""

    def __init__(self, count: int) -> None:
        self.count = count
        super().__init__(
            f"{count} coverage-manifest blocks found; exactly one is allowed "
            "(did this findings file quote the schema example verbatim?)"
        )


def find_manifest(text: str) -> Any | None:
    """The parsed manifest block, or None if the marker is absent.

    A marker present with unparsable JSON after it is not "absent" -- that is
    a malformed manifest, reported by the caller as a JSON decode failure,
    not silently treated as "no manifest here." More than one marked block
    is not "the first one wins" or "the last one wins" either -- both are a
    guess about which block is the real manifest, and a findings file that
    quotes this module's own docstring example ends up with a decoy block
    that a first-match search would silently grade instead of the real one.
    Raises [`MultipleManifestsError`] rather than choosing.
    """
    matches = list(BLOCK.finditer(text))
    if not matches:
        return None
    if len(matches) > 1:
        raise MultipleManifestsError(len(matches))
    return json.loads(matches[0].group(1))


def _coverage_clause(kind: str, report: Report) -> str:
    """What the success line says about ONE `expected` category.

    Never uses the word "covered" for the declared-surface count. A declared
    key is *accounted for* when it has an entry at all, and `_check_expected`
    deliberately accepts an entry of any status -- so the number of declared
    keys says how many surfaces the eval WROTE DOWN, not how many it
    verified. Rendering that number as "N/N covered" was round-2 HIGH-9: the
    headline word asserted from presence-of-a-key while `covered` is one of
    three status terms three tokens later on the same line. The statuses of
    the entries satisfying the declared keys are printed alongside, so the
    sentence and the tally can never disagree.

    An `expected.<kind>` that is absent, or present but empty, means this
    manifest made no claim the checker could hold it to for that category.
    Say plainly that coverage was not checked -- "0/0" would print exactly
    like "everything named was accounted for," which is the
    reassuring-direction lie round-1 HIGH-1 was about.
    """
    declared = report.declared[kind]
    if not declared:
        return f"no expected {kind} declared -- {kind} coverage NOT checked"
    statuses = report.declared_statuses(kind)
    total = len(declared)
    return (
        f"{total}/{total} {kind} accounted for "
        f"({statuses['covered']} covered, {statuses['reused']} reused, "
        f"{statuses['failed']} failed)"
    )


def main(paths: list[str]) -> int:
    if not paths:
        print("usage: eval_manifest_check.py <findings-file.md>...", file=sys.stderr)
        return EXIT_USAGE
    failures = 0
    unchecked = 0
    for raw in paths:
        path = Path(raw)
        text = path.read_text(encoding="utf-8")
        try:
            manifest = find_manifest(text)
        except MultipleManifestsError as error:
            print(f"{path}: {error}", file=sys.stderr)
            failures += 1
            continue
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
        if not report.ok:
            failures += 1
            continue
        checked = bool(report.declared["bullets"]) and bool(report.declared["files"])
        if not checked:
            unchecked += 1
        # `coverage=` is the documented machine token: a consumer greps for
        # `coverage=checked` rather than for the absence of an English
        # phrase. The exit code carries the same fact for consumers that
        # read only the status.
        print(
            f"{path}: ok (schema-valid; "
            f"coverage={'checked' if checked else 'unchecked'}; "
            f"{_coverage_clause('bullets', report)}; "
            f"{_coverage_clause('files', report)}; "
            f"covered={report.covered} reused={report.reused} failed={report.failed})"
        )
    if failures:
        return EXIT_INVALID
    if unchecked:
        return EXIT_COVERAGE_UNCHECKED
    return EXIT_OK


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
