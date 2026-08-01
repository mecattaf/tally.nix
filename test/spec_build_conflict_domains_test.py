#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
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

MISSING = object()


def git(*argv: str, cwd: Path | None = None, check: bool = True) -> str:
    return subprocess.run(
        ["git", *argv],
        cwd=cwd,
        check=check,
        capture_output=True,
        text=True,
    ).stdout.strip()


def task(task_id: str, conflict_domains: object = MISSING) -> dict[str, Any]:
    value: dict[str, Any] = {
        "id": task_id,
        "title": f"Implement {task_id}",
        "goal": f"Deliver the bounded behavior for {task_id}.",
        "deliveredBehaviors": [f"{task_id} is present"],
        "readFirst": {
            "specSections": [f"spec.md#{task_id}"],
            "styleReferences": [],
        },
        "acceptanceCriteria": [
            {
                "id": f"{task_id}-focused",
                "description": "The focused check passes.",
                "argv": ["true"],
            }
        ],
        "dependencies": [],
    }
    if conflict_domains is not MISSING:
        value["conflictDomains"] = conflict_domains
    return value


class ConflictDomainSemanticsTests(unittest.TestCase):
    def test_equal_and_ancestor_domains_overlap_at_path_component_boundaries(self) -> None:
        self.assertTrue(driver.domains_overlap("src/domain", "src/domain"))
        self.assertTrue(driver.domains_overlap("src/domain", "src/domain/customer.rs"))
        self.assertTrue(driver.domains_overlap("src/domain/customer.rs", "src/domain"))
        self.assertFalse(driver.domains_overlap("src/domain", "src/domains"))
        self.assertFalse(driver.domains_overlap("src/domain", "tests/domain"))


class PublicationConflictDomainTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.remote = self.root / "remote.git"
        self.checkout = self.root / "checkout"
        git("init", "--bare", "--initial-branch=main", str(self.remote))
        git("init", "--initial-branch=main", str(self.checkout))
        git("config", "user.name", "Conflict Domain Test", cwd=self.checkout)
        git("config", "user.email", "conflict-domain@example.invalid", cwd=self.checkout)

        root_command = self.checkout / "internal/cli/root.go"
        root_command.parent.mkdir(parents=True)
        root_command.write_text("package cli\n", encoding="utf-8")
        (self.checkout / "README.md").write_text("base\n", encoding="utf-8")
        git("add", "--all", cwd=self.checkout)
        git("commit", "-m", "fixture: base", cwd=self.checkout)
        git("remote", "add", "origin", str(self.remote), cwd=self.checkout)
        git("push", "--set-upstream", "origin", "main", cwd=self.checkout)
        self.base_rev = git("rev-parse", "HEAD", cwd=self.checkout)
        self.local_branch = "tally-work/fixture/conflict-domain"
        self.publish_branch = "tally/fixture-issue-7/conflict-domain"
        git("switch", "-c", self.local_branch, cwd=self.checkout)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def commit(self, message: str) -> str:
        git("add", "--all", cwd=self.checkout)
        git("commit", "-m", message, cwd=self.checkout)
        return git("rev-parse", "HEAD", cwd=self.checkout)

    def brief(self, conflict_domains: object = MISSING) -> dict[str, Any]:
        return {
            "campaign": "fixture",
            "repository": "acme/spec",
            "repositoryConfig": {
                "checkout": str(self.checkout),
                "baseBranch": "main",
                "remote": "origin",
                "forge": "local",
            },
            "issue": {
                "number": "7",
                "url": "https://example.invalid/acme/spec/issues/7",
            },
            "runId": "conflict-domain-test",
            "workspaceRoot": str(self.root / "workspaces"),
            "task": task("conflict-domain", conflict_domains),
            "workspace": {
                "taskId": "conflict-domain",
                "baseRev": self.base_rev,
                "branch": self.local_branch,
                "publishBranch": self.publish_branch,
                "worktreePath": str(self.checkout),
            },
            "constraints": [],
        }

    def assert_not_published(self) -> None:
        result = subprocess.run(
            [
                "git",
                "--git-dir",
                str(self.remote),
                "show-ref",
                "--verify",
                "--quiet",
                f"refs/heads/{self.publish_branch}",
            ],
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)

    def test_directory_and_exact_file_domains_admit_owned_changes(self) -> None:
        contact = self.checkout / "internal/contacts/model.go"
        contact.parent.mkdir(parents=True)
        contact.write_text("package contacts\n", encoding="utf-8")
        (self.checkout / "internal/cli/root.go").write_text(
            "package cli\n// register contacts\n", encoding="utf-8"
        )
        head = self.commit("fixture: implement contacts")

        published = driver.action_publish(
            self.brief(["internal/contacts", "internal/cli/root.go"])
        )

        self.assertEqual(published["head"], head)
        self.assertEqual(
            git(
                "--git-dir",
                str(self.remote),
                "rev-parse",
                f"refs/heads/{self.publish_branch}",
            ),
            head,
        )

    def test_shared_registration_file_is_rejected_before_push_when_undeclared(self) -> None:
        contact = self.checkout / "internal/contacts/model.go"
        contact.parent.mkdir(parents=True)
        contact.write_text("package contacts\n", encoding="utf-8")
        (self.checkout / "internal/cli/root.go").write_text(
            "package cli\n// register contacts\n", encoding="utf-8"
        )
        self.commit("fixture: under-declare contacts")

        with self.assertRaisesRegex(
            driver.DriverError,
            r'outside its declared conflictDomains: "internal/cli/root\.go"',
        ):
            driver.action_publish(self.brief(["internal/contacts"]))
        self.assert_not_published()

    def test_deletion_outside_the_domain_is_rejected_before_push(self) -> None:
        (self.checkout / "internal/cli/root.go").unlink()
        self.commit("fixture: delete unowned registration file")

        with self.assertRaisesRegex(driver.DriverError, r'"internal/cli/root\.go"'):
            driver.action_publish(self.brief(["internal/contacts"]))
        self.assert_not_published()

    def test_rename_requires_ownership_of_both_source_and_destination(self) -> None:
        destination = self.checkout / "internal/contacts/root.go"
        destination.parent.mkdir(parents=True)
        git(
            "mv",
            "internal/cli/root.go",
            "internal/contacts/root.go",
            cwd=self.checkout,
        )
        self.commit("fixture: move unowned registration file")

        with self.assertRaisesRegex(driver.DriverError, r'"internal/cli/root\.go"'):
            driver.action_publish(self.brief(["internal/contacts"]))
        self.assert_not_published()

    def test_rebase_rechecks_domains_before_force_push(self) -> None:
        (self.checkout / "internal/cli/root.go").write_text(
            "package cli\n// undeclared task change\n", encoding="utf-8"
        )
        published_head = self.commit("fixture: under-declare rebased task")
        git(
            "push",
            "origin",
            f"HEAD:refs/heads/{self.publish_branch}",
            cwd=self.checkout,
        )

        git("switch", "main", cwd=self.checkout)
        (self.checkout / "README.md").write_text("advanced base\n", encoding="utf-8")
        self.commit("fixture: advance base")
        git("push", "origin", "main", cwd=self.checkout)
        git("switch", self.local_branch, cwd=self.checkout)

        brief = self.brief(["internal/contacts"])
        brief.update(
            {
                "publication": {
                    "taskId": "conflict-domain",
                    "branch": self.publish_branch,
                    "head": published_head,
                    "pullRequest": f"local://acme/spec/{self.publish_branch}",
                },
                "constraints": [],
            }
        )
        with self.assertRaisesRegex(driver.DriverError, r'"internal/cli/root\.go"'):
            driver.action_rebase(brief)

        self.assertEqual(
            git(
                "--git-dir",
                str(self.remote),
                "rev-parse",
                f"refs/heads/{self.publish_branch}",
            ),
            published_head,
        )

    def test_serial_task_without_domains_preserves_the_existing_contract(self) -> None:
        (self.checkout / "internal/cli/root.go").write_text(
            "package cli\n// serial campaign\n", encoding="utf-8"
        )
        head = self.commit("fixture: serial task")

        published = driver.action_publish(self.brief())

        self.assertEqual(published["head"], head)


if __name__ == "__main__":
    unittest.main()
