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

    def test_domains_overlap_case_insensitively_for_portable_scheduling(self) -> None:
        self.assertTrue(driver.domains_overlap("Docs", "docs/guide.md"))
        self.assertTrue(driver.domains_overlap("SRC/Domain", "src/domain"))

    def test_case_only_duplicate_domains_are_rejected(self) -> None:
        with self.assertRaisesRegex(driver.DriverError, "contains duplicates"):
            driver.normalize_conflict_domains(
                ["Docs", "docs"], "task.conflictDomains", required=True
            )

    def test_underfilled_ready_frontier_names_the_domain_collisions(self) -> None:
        checkpoint = {"id": "checkpoint", "kind": "checkpoint"}
        implementations = [
            {"id": "one", "conflictDomains": ["CHANGELOG.md"]},
            {"id": "two", "conflictDomains": ["changelog.md"]},
            {"id": "three", "conflictDomains": ["CHANGELOG.md"]},
        ]
        ready = [checkpoint, *implementations]

        warnings = driver.parallelism_warnings(
            ready, [checkpoint, implementations[0]], 4
        )

        self.assertEqual(len(warnings), 1)
        self.assertIn("limited this ready frontier to 2 of requested maxParallel 4", warnings[0])
        self.assertIn("two:\"changelog.md\" overlaps one:\"CHANGELOG.md\"", warnings[0])


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

    def brief(
        self, conflict_domains: object = MISSING, *, domains_required: bool = True
    ) -> dict[str, Any]:
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
            "domainsRequired": domains_required,
            "workspace": {
                "taskId": "conflict-domain",
                "baseRev": self.base_rev,
                "branch": self.local_branch,
                "publishBranch": self.publish_branch,
                "worktreePath": str(self.checkout),
            },
            "constraints": [],
        }

    def ownership_brief(
        self, conflict_domains: object = MISSING, *, domains_required: bool = True
    ) -> dict[str, Any]:
        publication_brief = self.brief(
            conflict_domains, domains_required=domains_required
        )
        return {
            "task": publication_brief["task"],
            "domainsRequired": publication_brief["domainsRequired"],
            "workspace": publication_brief["workspace"],
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

        domains = ["internal/contacts", "internal/cli/root.go"]
        prechecked = driver.action_ownership(self.ownership_brief(domains))
        published = driver.action_publish(self.brief(domains))

        self.assertEqual(published["head"], head)
        self.assertEqual(published["ownership"], prechecked)
        self.assertEqual(published["ownership"]["conflictDomains"], domains)
        self.assertEqual(
            published["ownership"]["ownedPaths"],
            ["internal/cli/root.go", "internal/contacts/model.go"],
        )
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

    def test_transient_unowned_path_in_commit_history_is_rejected(self) -> None:
        transient = self.checkout / "other/x.txt"
        transient.parent.mkdir(parents=True)
        transient.write_text("transient\n", encoding="utf-8")
        self.commit("fixture: add transient unowned path")
        transient.unlink()
        contact = self.checkout / "internal/contacts/model.go"
        contact.parent.mkdir(parents=True)
        contact.write_text("package contacts\n", encoding="utf-8")
        self.commit("fixture: delete transient path and keep owned work")

        with self.assertRaisesRegex(driver.DriverError, r'"other/x\.txt"'):
            driver.action_publish(self.brief(["internal/contacts"]))
        self.assert_not_published()

    def test_option_shaped_base_revision_fails_closed_without_git_side_effects(self) -> None:
        contact = self.checkout / "internal/contacts/model.go"
        contact.parent.mkdir(parents=True)
        contact.write_text("package contacts\n", encoding="utf-8")
        self.commit("fixture: owned work")
        sink = self.root / "git-option-sink"
        brief = self.ownership_brief(["internal/contacts"])
        brief["workspace"]["baseRev"] = f"--output={sink}"

        with self.assertRaisesRegex(driver.DriverError, "must be a full Git object ID"):
            driver.action_ownership(brief)

        self.assertFalse(sink.exists())

    def test_required_domains_reject_an_explicit_empty_declaration(self) -> None:
        (self.checkout / "internal/cli/root.go").write_text(
            "package cli\n// changed\n", encoding="utf-8"
        )
        self.commit("fixture: empty declaration")

        with self.assertRaisesRegex(
            driver.DriverError, r"task\.conflictDomains must be a non-empty array"
        ):
            driver.action_publish(self.brief([]))
        self.assert_not_published()

    def test_deletion_outside_the_domain_is_rejected_before_push(self) -> None:
        (self.checkout / "internal/cli/root.go").unlink()
        self.commit("fixture: delete unowned registration file")

        with self.assertRaisesRegex(driver.DriverError, r'"internal/cli/root\.go"'):
            driver.action_publish(self.brief(["internal/contacts"]))
        self.assert_not_published()

    def test_type_change_outside_the_domain_is_rejected_before_push(self) -> None:
        root_command = self.checkout / "internal/cli/root.go"
        root_command.unlink()
        root_command.symlink_to("../../README.md")
        self.commit("fixture: change unowned registration file type")

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

    def test_case_only_rename_witnesses_both_endpoints(self) -> None:
        destination = self.checkout / "internal/cli/ROOT.go"
        git("mv", "internal/cli/root.go", str(destination.relative_to(self.checkout)), cwd=self.checkout)
        head = self.commit("fixture: case-only rename")

        published = driver.action_publish(self.brief(["internal/cli/root.go"]))

        self.assertEqual(published["head"], head)
        self.assertEqual(
            published["ownership"]["ownedPaths"],
            ["internal/cli/ROOT.go", "internal/cli/root.go"],
        )

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
                    "ownership": {
                        "taskId": "conflict-domain",
                        "domainsRequired": True,
                        "conflictDomains": ["internal/contacts"],
                        "ownedPaths": [],
                        "baseRev": self.base_rev,
                        "head": published_head,
                    },
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

    def test_rebase_fast_path_rechecks_and_witnesses_ownership(self) -> None:
        contact = self.checkout / "internal/contacts/model.go"
        contact.parent.mkdir(parents=True)
        contact.write_text("package contacts\n", encoding="utf-8")
        self.commit("fixture: fast-path task")
        brief = self.brief(["internal/contacts"])
        published = driver.action_publish(brief)
        brief.update({"publication": published, "constraints": []})

        integrated = driver.action_rebase(brief)

        self.assertFalse(integrated["regate"])
        self.assertEqual(integrated["head"], published["head"])
        self.assertEqual(integrated["ownership"], published["ownership"])

    def test_serial_task_without_domains_preserves_the_existing_contract(self) -> None:
        (self.checkout / "internal/cli/root.go").write_text(
            "package cli\n// serial campaign\n", encoding="utf-8"
        )
        head = self.commit("fixture: serial task")

        published = driver.action_publish(self.brief(domains_required=False))

        self.assertEqual(published["head"], head)
        self.assertFalse(published["ownership"]["domainsRequired"])
        self.assertEqual(published["ownership"]["conflictDomains"], [])
        self.assertEqual(published["ownership"]["ownedPaths"], ["internal/cli/root.go"])


if __name__ == "__main__":
    unittest.main()
