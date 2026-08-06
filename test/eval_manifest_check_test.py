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
    def test_valid_fixture_passes(self):
        code, out, err = run_main(FIXTURES / "valid.md")
        self.assertEqual(code, 0, err)
        self.assertIn("ok (covered=3 reused=1 failed=1)", out)

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


if __name__ == "__main__":
    unittest.main()
