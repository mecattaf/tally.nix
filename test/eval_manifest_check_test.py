#!/usr/bin/env python3
"""Unit and fixture tests for `eval_manifest_check.py`.

Not wired into `nix flake check` or `test/fleet-gate.sh` — issue #388 caps
this lane at "no gate-side change." Run directly:

    python3 test/eval_manifest_check_test.py
"""

from __future__ import annotations

import importlib.util
import io
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

CHECKER = Path(__file__).with_name("eval_manifest_check.py")
SPEC = importlib.util.spec_from_file_location("eval_manifest_check", CHECKER)
assert SPEC is not None and SPEC.loader is not None
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)

FIXTURES = Path(__file__).with_name("fixtures") / "eval-manifest"


def run_main(*paths: Path) -> tuple[int, str, str]:
    out, err = io.StringIO(), io.StringIO()
    with redirect_stdout(out), redirect_stderr(err):
        code = checker.main([str(path) for path in paths])
    return code, out.getvalue(), err.getvalue()


def valid_manifest() -> dict:
    return {
        "version": 1,
        "bullets": [
            {"issue": 388, "bullet": "schema", "status": "covered"},
        ],
        "files": [
            {"path": "test/eval_manifest_check.py", "status": "covered"},
        ],
        "run": {"status": "ok"},
    }


class FixtureTests(unittest.TestCase):
    def test_valid_fixture_passes_with_the_strong_coverage_claim(self):
        code, out, err = run_main(FIXTURES / "valid.md")
        self.assertEqual(code, 0, err)
        self.assertIn(
            "ok (schema-valid; 3/3 bullets covered; 2/2 files covered; "
            "covered=3 reused=1 failed=1)",
            out,
        )

    def test_missing_file_fixture_is_rejected(self):
        """Proof scenario 1: a manifest that omits a reviewed file."""
        code, _out, err = run_main(FIXTURES / "missing-file.md")
        self.assertEqual(code, 1)
        self.assertIn("UNCOVERED", err)
        self.assertIn("reader_state.rs", err)

    def test_unknown_failure_class_fixture_is_rejected(self):
        """Proof scenario 2: a manifest with an untyped failure class."""
        code, _out, err = run_main(FIXTURES / "unknown-class.md")
        self.assertEqual(code, 1)
        self.assertIn("failureClass", err)
        self.assertIn("'flaky'", err)

    def test_no_manifest_fixture_is_rejected(self):
        code, _out, err = run_main(FIXTURES / "no-manifest.md")
        self.assertEqual(code, 1)
        self.assertIn("no ", err)
        self.assertIn("coverage-manifest block found", err)

    def test_multiple_files_all_checked_in_one_invocation(self):
        code, _out, err = run_main(
            FIXTURES / "valid.md",
            FIXTURES / "missing-file.md",
        )
        self.assertEqual(code, 1)
        self.assertIn("valid.md: ok", _out)
        self.assertIn("missing-file.md", err)

    def test_usage_with_no_arguments_is_exit_2(self):
        code, _out, err = run_main()
        self.assertEqual(code, 2)
        self.assertIn("usage:", err)

    def test_no_expected_surface_fixture_prints_the_weak_claim_not_ok(self):
        """HIGH-1 (round-1 eval): a manifest that reviewed nothing must not
        print the same bare `ok` a fully-covered manifest gets."""
        code, out, err = run_main(FIXTURES / "no-expected-surface.md")
        self.assertEqual(code, 0, err)
        self.assertIn("coverage NOT checked", out)
        # And the weak line must be textually distinct from the strong one.
        strong_code, strong_out, _ = run_main(FIXTURES / "valid.md")
        self.assertEqual(strong_code, 0)
        self.assertNotEqual(
            out.split(": ", 1)[1],
            strong_out.split(": ", 1)[1],
            "a reviewed-nothing manifest must not print byte-identically to a "
            "fully-covered one",
        )

    def test_two_blocks_fixture_is_refused_not_silently_graded(self):
        """HIGH-2 (round-1 eval): a findings file that quotes the schema
        example is refused outright, not graded against the quoted decoy."""
        code, out, err = run_main(FIXTURES / "two-blocks.md")
        self.assertEqual(code, 1)
        self.assertIn("2 coverage-manifest blocks found", err)
        # The old defect: the decoy's `covered=2` must never appear as a
        # success line for this file.
        self.assertNotIn("ok (", out)


class SchemaUnitTests(unittest.TestCase):
    def test_valid_manifest_is_ok(self):
        report = checker.check_manifest(valid_manifest())
        self.assertTrue(report.ok, report.errors)
        self.assertEqual(report.covered, 2)

    def test_wrong_version_is_rejected(self):
        manifest = valid_manifest()
        manifest["version"] = 2
        report = checker.check_manifest(manifest)
        self.assertFalse(report.ok)
        self.assertTrue(any("version" in error for error in report.errors))

    def test_failed_item_without_failure_class_is_rejected(self):
        manifest = valid_manifest()
        manifest["bullets"][0]["status"] = "failed"
        report = checker.check_manifest(manifest)
        self.assertFalse(report.ok)
        self.assertTrue(any("failureClass" in error for error in report.errors))

    def test_covered_item_with_failure_class_is_rejected(self):
        """`failureClass` is only meaningful on a `failed` item."""
        manifest = valid_manifest()
        manifest["bullets"][0]["failureClass"] = "unknown"
        report = checker.check_manifest(manifest)
        self.assertFalse(report.ok)

    def test_run_failed_requires_typed_class(self):
        manifest = valid_manifest()
        manifest["run"] = {"status": "failed"}
        report = checker.check_manifest(manifest)
        self.assertFalse(report.ok)
        manifest["run"] = {"status": "failed", "failureClass": "unknown"}
        report = checker.check_manifest(manifest)
        self.assertTrue(report.ok, report.errors)

    def test_run_failure_class_is_a_separate_taxonomy_from_item(self):
        """`crash` is a valid RUN failure class but not a valid ITEM one."""
        manifest = valid_manifest()
        manifest["run"] = {"status": "failed", "failureClass": "crash"}
        self.assertTrue(checker.check_manifest(manifest).ok)
        manifest["bullets"][0]["status"] = "failed"
        manifest["bullets"][0]["failureClass"] = "crash"
        self.assertFalse(checker.check_manifest(manifest).ok)

    def test_duplicate_bullet_key_is_rejected(self):
        manifest = valid_manifest()
        manifest["bullets"].append(dict(manifest["bullets"][0]))
        report = checker.check_manifest(manifest)
        self.assertFalse(report.ok)
        self.assertTrue(any("duplicates" in error for error in report.errors))

    def test_expected_bullet_missing_entirely_is_uncovered(self):
        manifest = valid_manifest()
        manifest["expected"] = {"bullets": ["388:schema", "389:never-checked"]}
        report = checker.check_manifest(manifest)
        self.assertFalse(report.ok)
        self.assertTrue(any("389:never-checked" in gap for gap in report.uncovered))

    def test_expected_bullet_present_as_failed_is_not_uncovered(self):
        """A typed `failed` entry satisfies `expected`; only a missing entry doesn't."""
        manifest = valid_manifest()
        manifest["bullets"][0]["status"] = "failed"
        manifest["bullets"][0]["failureClass"] = "timeout"
        manifest["expected"] = {"bullets": ["388:schema"]}
        report = checker.check_manifest(manifest)
        self.assertTrue(report.ok, report.errors)
        self.assertEqual(report.failed, 1)

    def test_unknown_top_level_field_is_rejected(self):
        manifest = valid_manifest()
        manifest["extra"] = True
        report = checker.check_manifest(manifest)
        self.assertFalse(report.ok)

    def test_manifest_missing_run_is_rejected(self):
        manifest = valid_manifest()
        del manifest["run"]
        report = checker.check_manifest(manifest)
        self.assertFalse(report.ok)
        self.assertTrue(any("run" in error for error in report.errors))


class CoverageClauseTests(unittest.TestCase):
    """HIGH-1: the success line must say what it actually verified."""

    def test_absent_expected_is_not_checked(self):
        self.assertIn("NOT checked", checker._coverage_clause("bullets", None))

    def test_empty_expected_list_is_not_checked(self):
        """Declaring `expected.bullets: []` must read the same as omitting
        it entirely -- both are "nothing named," not "0/0 covered."""
        self.assertIn("NOT checked", checker._coverage_clause("bullets", []))

    def test_non_empty_expected_is_the_strong_claim(self):
        clause = checker._coverage_clause("files", ["a.rs", "b.rs"])
        self.assertEqual(clause, "2/2 files covered")
        self.assertNotIn("NOT checked", clause)

    def test_degenerate_manifest_is_distinguishable_from_full_manifest(self):
        """The exact HIGH-1 reproduction: an eval that reviewed nothing
        must not be byte-indistinguishable from one that reviewed
        everything."""
        empty = {"version": 1, "bullets": [], "files": [], "run": {"status": "ok"}}
        full = valid_manifest()
        full["expected"] = {"bullets": ["388:schema"], "files": ["test/eval_manifest_check.py"]}

        def render(manifest: dict) -> str:
            report = checker.check_manifest(manifest)
            assert report.ok, report.errors
            expected = manifest.get("expected") or {}
            return "; ".join(
                [
                    checker._coverage_clause("bullets", expected.get("bullets")),
                    checker._coverage_clause("files", expected.get("files")),
                ]
            )

        self.assertNotEqual(render(empty), render(full))


class MultipleManifestsTests(unittest.TestCase):
    """HIGH-2: a second marked block must refuse, never silently pick one."""

    def test_single_block_parses_normally(self):
        text = (
            "prose\n\n"
            + checker.MARKER
            + '\n```json\n{"version": 1}\n```\n'
        )
        self.assertEqual(checker.find_manifest(text), {"version": 1})

    def test_two_blocks_raise_instead_of_picking_the_first(self):
        one = checker.MARKER + '\n```json\n{"version": 1, "decoy": true}\n```\n'
        two = checker.MARKER + '\n```json\n{"version": 1, "real": true}\n```\n'
        with self.assertRaises(checker.MultipleManifestsError) as raised:
            checker.find_manifest(f"{one}\n{two}")
        self.assertEqual(raised.exception.count, 2)

    def test_three_blocks_report_the_true_count(self):
        block = checker.MARKER + '\n```json\n{"version": 1}\n```\n'
        with self.assertRaises(checker.MultipleManifestsError) as raised:
            checker.find_manifest("\n".join([block] * 3))
        self.assertEqual(raised.exception.count, 3)


if __name__ == "__main__":
    unittest.main()
