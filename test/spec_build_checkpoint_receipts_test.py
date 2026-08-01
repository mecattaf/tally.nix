#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import Any


DRIVER = Path(
    os.environ.get(
        "SPEC_BUILD_DRIVER",
        Path(__file__).parents[1] / "examples/flows/spec_build_driver.py",
    )
)
SPEC = importlib.util.spec_from_file_location("spec_build_driver", DRIVER)
assert SPEC is not None and SPEC.loader is not None
driver = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(driver)


def git(*argv: str, cwd: Path | None = None, check: bool = True) -> str:
    return subprocess.run(
        ["git", *argv],
        cwd=cwd,
        check=check,
        capture_output=True,
        text=True,
    ).stdout.strip()


def implementation_task() -> dict[str, Any]:
    return {
        "id": "task-1",
        "kind": "implementation",
        "title": "Build phase one",
        "goal": "Materialize phase one.",
        "deliveredBehaviors": ["phase one exists"],
        "readFirst": {"specSections": ["spec.md#phase-one"], "styleReferences": []},
        "acceptanceCriteria": [
            {
                "id": "phase-one",
                "description": "Phase one passes.",
                "argv": ["true"],
            }
        ],
        "dependencies": [],
        "conflictDomains": ["phase-one"],
    }


def checkpoint_task() -> dict[str, Any]:
    return {
        "id": "phase-checkpoint",
        "kind": "checkpoint",
        "title": "Validate phase one",
        "argv": ["true"],
        "runtimeMaxSec": 60,
        "dependencies": ["task-1"],
    }


class CheckpointReceiptTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.remote = self.root / "remote.git"
        self.checkout = self.root / "checkout"
        git("init", "--bare", "--initial-branch=main", str(self.remote))
        git("init", "--initial-branch=main", str(self.checkout))
        self.configure_identity(self.checkout)
        worklist = self.checkout / "specs/001-fixture/tasks.json"
        worklist.parent.mkdir(parents=True)
        worklist.write_text(
            json.dumps(
                {
                    "schemaVersion": 1,
                    "tasks": [implementation_task(), checkpoint_task()],
                }
            )
            + "\n",
            encoding="utf-8",
        )
        (self.checkout / "README.md").write_text("base\n", encoding="utf-8")
        git("add", "--all", cwd=self.checkout)
        git("commit", "-m", "fixture: base", cwd=self.checkout)
        git("remote", "add", "origin", str(self.remote), cwd=self.checkout)
        git("push", "--set-upstream", "origin", "main", cwd=self.checkout)
        self.base_rev = git("rev-parse", "HEAD", cwd=self.checkout)
        self.config = {
            "checkout": str(self.checkout),
            "baseBranch": "main",
            "remote": "origin",
            "forge": "local",
        }
        self.worklist_brief = {
            "repository": "acme/spec",
            "repositoryConfig": self.config,
            "worklist": "specs/*/tasks.json",
            "maxTasks": 2,
            "maxParallel": 1,
        }
        self.worklist = driver.action_worklist(self.worklist_brief)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    @staticmethod
    def configure_identity(checkout: Path) -> None:
        git("config", "user.name", "Checkpoint Receipt Test", cwd=checkout)
        git("config", "user.email", "checkpoint@example.invalid", cwd=checkout)

    def merged_fact(self, revision: str | None = None) -> dict[str, str]:
        return {
            "taskId": "task-1",
            "pullRequest": "local://acme/spec/task-1",
            "mergeCommit": revision or self.base_rev,
        }

    def reference(self, source: dict[str, str] | None = None) -> str:
        selected = source or self.worklist["source"]
        return driver.checkpoint_ref(
            "fixture",
            "7",
            "phase-checkpoint",
            selected["sha256"],
            selected["revision"],
        )

    def completed(
        self,
        *,
        source: dict[str, str] | None = None,
        merged_revision: str | None = None,
    ) -> list[dict[str, str]]:
        return driver.completed_checkpoint_tasks(
            driver.repo_config(self.config),
            "fixture",
            "7",
            self.worklist["tasks"],
            source or self.worklist["source"],
            [self.merged_fact(merged_revision)],
        )

    def push_receipt(self, revision: str, reference: str | None = None) -> None:
        git(
            "push",
            "origin",
            f"{revision}:{reference or self.reference()}",
            cwd=self.checkout,
        )

    def orphan_commit(self) -> str:
        git("switch", "--orphan", "forged", cwd=self.checkout)
        git("rm", "-r", "--quiet", "--ignore-unmatch", ".", cwd=self.checkout)
        (self.checkout / "forged.txt").write_text("forged\n", encoding="utf-8")
        git("add", "forged.txt", cwd=self.checkout)
        git("commit", "-m", "fixture: forged lineage", cwd=self.checkout)
        revision = git("rev-parse", "HEAD", cwd=self.checkout)
        git("switch", "main", cwd=self.checkout)
        return revision

    def clone_advancer(self) -> Path:
        advancer = self.root / "advancer"
        git("clone", str(self.remote), str(advancer))
        self.configure_identity(advancer)
        return advancer

    def checkpoint_brief(self) -> dict[str, Any]:
        return {
            "campaign": "fixture",
            "repository": "acme/spec",
            "repositoryConfig": self.config,
            "issue": {
                "number": "7",
                "url": "https://example.invalid/acme/spec/issues/7",
            },
            "task": self.worklist["tasks"][1],
            "source": self.worklist["source"],
            "workspace": {
                "taskId": "phase-checkpoint",
                "baseRev": self.base_rev,
                "branch": "main",
                "publishBranch": "tally/fixture-issue-7/phase-checkpoint",
                "worktreePath": str(self.checkout),
            },
        }

    def test_worklist_reads_remote_base_and_pushed_edit_invalidates_receipt(self) -> None:
        self.push_receipt(self.base_rev)
        original_source = self.worklist["source"]
        path = self.checkout / original_source["path"]
        local_document = json.loads(path.read_text(encoding="utf-8"))
        local_document["tasks"][1]["title"] = "uncommitted local title"
        path.write_text(json.dumps(local_document) + "\n", encoding="utf-8")

        still_remote = driver.action_worklist(self.worklist_brief)
        self.assertEqual(still_remote["source"], original_source)
        self.assertEqual(still_remote["tasks"][1]["title"], "Validate phase one")

        git("restore", str(path.relative_to(self.checkout)), cwd=self.checkout)
        advancer = self.clone_advancer()
        pushed_path = advancer / original_source["path"]
        pushed_document = json.loads(pushed_path.read_text(encoding="utf-8"))
        pushed_document["tasks"][1]["title"] = "pushed remote title"
        pushed_path.write_text(json.dumps(pushed_document) + "\n", encoding="utf-8")
        git("add", "--all", cwd=advancer)
        git("commit", "-m", "fixture: edit checkpoint", cwd=advancer)
        git("push", "origin", "main", cwd=advancer)
        pushed_revision = git("rev-parse", "HEAD", cwd=advancer)
        self.assertEqual(git("rev-parse", "HEAD", cwd=self.checkout), self.base_rev)

        updated = driver.action_worklist(self.worklist_brief)
        self.assertNotEqual(updated["source"], original_source)
        self.assertEqual(updated["source"]["revision"], pushed_revision)
        self.assertEqual(updated["tasks"][1]["title"], "pushed remote title")
        self.assertEqual(self.completed(source=updated["source"]), [])

    def test_receipt_is_invalid_after_unrelated_base_advance(self) -> None:
        self.push_receipt(self.base_rev)
        self.assertEqual([fact["taskId"] for fact in self.completed()], ["phase-checkpoint"])
        advancer = self.clone_advancer()
        (advancer / "unrelated.txt").write_text("later\n", encoding="utf-8")
        git("add", "unrelated.txt", cwd=advancer)
        git("commit", "-m", "fixture: unrelated base advance", cwd=advancer)
        git("push", "origin", "main", cwd=advancer)

        advanced = driver.action_worklist(self.worklist_brief)
        self.assertEqual(advanced["source"]["sha256"], self.worklist["source"]["sha256"])
        self.assertNotEqual(advanced["source"]["revision"], self.base_rev)
        self.assertEqual(self.completed(source=advanced["source"]), [])

    def test_forged_ref_pointing_outside_named_base_is_rejected(self) -> None:
        forged = self.orphan_commit()
        self.push_receipt(forged)
        with self.assertRaisesRegex(driver.DriverError, "does not point to its named base revision"):
            self.completed()

    def test_receipt_missing_dependency_ancestry_is_rejected(self) -> None:
        forged_dependency = self.orphan_commit()
        self.push_receipt(self.base_rev)
        with self.assertRaisesRegex(driver.DriverError, "does not contain dependency"):
            self.completed(merged_revision=forged_dependency)

    def test_annotated_receipt_tag_is_rejected(self) -> None:
        git("tag", "-a", "annotated-receipt", "-m", "not a direct commit", cwd=self.checkout)
        git(
            "push",
            "origin",
            f"annotated-receipt:{self.reference()}",
            cwd=self.checkout,
        )
        with self.assertRaisesRegex(driver.DriverError, "must point directly to a commit"):
            self.completed()

    def test_checkpoint_rejects_dirty_worktree(self) -> None:
        (self.checkout / "README.md").write_text("dirty\n", encoding="utf-8")
        with self.assertRaisesRegex(driver.DriverError, "changed tracked files"):
            driver.action_checkpoint(self.checkpoint_brief())

    def test_checkpoint_rejects_moved_head(self) -> None:
        (self.checkout / "local.txt").write_text("commit\n", encoding="utf-8")
        git("add", "local.txt", cwd=self.checkout)
        git("commit", "-m", "fixture: move local head", cwd=self.checkout)
        with self.assertRaisesRegex(driver.DriverError, "changed HEAD"):
            driver.action_checkpoint(self.checkpoint_brief())

    def test_checkpoint_rejects_changed_branch(self) -> None:
        git("switch", "-c", "other", cwd=self.checkout)
        with self.assertRaisesRegex(driver.DriverError, "changed branches"):
            driver.action_checkpoint(self.checkpoint_brief())

    def test_checkpoint_records_tested_point_when_remote_base_moves_forward(self) -> None:
        advancer = self.clone_advancer()
        (advancer / "later.txt").write_text("later\n", encoding="utf-8")
        git("add", "later.txt", cwd=advancer)
        git("commit", "-m", "fixture: advance while checkpoint runs", cwd=advancer)
        git("push", "origin", "main", cwd=advancer)

        recorded = driver.action_checkpoint(self.checkpoint_brief())
        self.assertEqual(recorded["revision"], self.base_rev)
        self.assertEqual(recorded["ref"], self.reference())
        self.assertEqual(
            git("ls-remote", "origin", recorded["ref"], cwd=self.checkout).split()[0],
            self.base_rev,
        )
        current = driver.action_worklist(self.worklist_brief)
        self.assertEqual(self.completed(source=current["source"]), [])

    def test_issue_checkpoint_binds_graph_digest_task_revision_and_base(self) -> None:
        brief = self.checkpoint_brief()
        brief["source"] = {
            "kind": "github-issue",
            "url": "https://github.com/acme/spec/issues/7",
            "sha256": "sha256:" + "a" * 64,
            "revision": self.base_rev,
        }
        brief["task"] = {
            **checkpoint_task(),
            "brief": {
                "issue": {
                    "number": "9",
                    "url": "https://github.com/acme/spec/issues/9",
                },
                "body": "Run the admitted checkpoint.",
            },
            "revision": "sha256:" + "b" * 64,
        }

        recorded = driver.action_checkpoint(brief)
        self.assertEqual(recorded["revision"], self.base_rev)
        self.assertIn("-" + "a" * 64 + "/" + self.base_rev, recorded["ref"])

        del brief["task"]["revision"]
        with self.assertRaisesRegex(driver.DriverError, "admitted revision"):
            driver.action_checkpoint(brief)

    def test_checkpoint_rejects_diverged_remote_base(self) -> None:
        advancer = self.clone_advancer()
        git("switch", "--orphan", "replacement", cwd=advancer)
        git("rm", "-r", "--quiet", "--ignore-unmatch", ".", cwd=advancer)
        (advancer / "replacement.txt").write_text("replacement\n", encoding="utf-8")
        git("add", "replacement.txt", cwd=advancer)
        git("commit", "-m", "fixture: replace base lineage", cwd=advancer)
        git("push", "--force", "origin", "HEAD:main", cwd=advancer)

        with self.assertRaisesRegex(driver.DriverError, "remote base diverged"):
            driver.action_checkpoint(self.checkpoint_brief())

    def test_checkpoint_will_not_move_an_existing_receipt_ref(self) -> None:
        forged = self.orphan_commit()
        self.push_receipt(forged)
        with self.assertRaisesRegex(driver.DriverError, "immutable checkpoint ref"):
            driver.action_checkpoint(self.checkpoint_brief())


if __name__ == "__main__":
    unittest.main()
