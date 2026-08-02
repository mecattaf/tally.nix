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


class PublicationHarness(unittest.TestCase):
    """One published lane on a local forge, shared by the publication suites."""
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
        self.advances = 0
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

    def publish_brief(
        self,
        conflict_domains: object = MISSING,
        *,
        domains_required: bool = True,
        gates: object = MISSING,
    ) -> dict[str, Any]:
        brief = self.brief(conflict_domains, domains_required=domains_required)
        brief["gates"] = [] if gates is MISSING else gates
        return brief

    def ownership_brief(
        self, conflict_domains: object = MISSING, *, domains_required: bool = True
    ) -> dict[str, Any]:
        publication_brief = self.brief(
            conflict_domains, domains_required=domains_required
        )
        return {
            "task": publication_brief["task"],
            "domainsRequired": publication_brief["domainsRequired"],
            "repositoryConfig": publication_brief["repositoryConfig"],
            "workspace": publication_brief["workspace"],
        }

    def advance_base(self, path: str, content: str, message: str) -> str:
        """Land a mainline commit exactly the way a campaign merge does.

        `merge_local` integrates with `git merge --no-ff` and `merge_github`
        asks the forge for a merge commit, so the tip of a base branch that has
        integrated anything is a merge commit and every later lane inherits it
        by rebasing. A fixture that advanced the base linearly would agree with
        any claim about rebased lanes while production disagreed.
        """
        advancer = self.root / "advancer"
        if not advancer.exists():
            git("clone", str(self.remote), str(advancer))
            git("config", "user.name", "Base Advance", cwd=advancer)
            git("config", "user.email", "base-advance@example.invalid", cwd=advancer)
        else:
            git("fetch", "origin", cwd=advancer)
            git("switch", "main", cwd=advancer)
            git("reset", "--hard", "origin/main", cwd=advancer)
        self.advances += 1
        sibling = f"sibling-{self.advances}"
        git("switch", "-c", sibling, cwd=advancer)
        target = advancer / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
        git("add", "--all", cwd=advancer)
        git("commit", "-m", message, cwd=advancer)
        git("switch", "main", cwd=advancer)
        git("merge", "--no-ff", "--no-edit", sibling, cwd=advancer)
        git("push", "origin", "main", cwd=advancer)
        git("fetch", "origin", cwd=self.checkout)
        return git("rev-parse", "HEAD", cwd=advancer)

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



class PublicationConflictDomainTests(PublicationHarness):
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
        published = driver.action_publish(self.publish_brief(domains))

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
            driver.action_publish(self.publish_brief(["internal/contacts"]))
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
            driver.action_publish(self.publish_brief(["internal/contacts"]))
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
            driver.action_publish(self.publish_brief([]))
        self.assert_not_published()

    def test_deletion_outside_the_domain_is_rejected_before_push(self) -> None:
        (self.checkout / "internal/cli/root.go").unlink()
        self.commit("fixture: delete unowned registration file")

        with self.assertRaisesRegex(driver.DriverError, r'"internal/cli/root\.go"'):
            driver.action_publish(self.publish_brief(["internal/contacts"]))
        self.assert_not_published()

    def test_type_change_outside_the_domain_is_rejected_before_push(self) -> None:
        root_command = self.checkout / "internal/cli/root.go"
        root_command.unlink()
        root_command.symlink_to("../../README.md")
        self.commit("fixture: change unowned registration file type")

        with self.assertRaisesRegex(driver.DriverError, r'"internal/cli/root\.go"'):
            driver.action_publish(self.publish_brief(["internal/contacts"]))
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
            driver.action_publish(self.publish_brief(["internal/contacts"]))
        self.assert_not_published()

    def test_case_only_rename_witnesses_both_endpoints(self) -> None:
        destination = self.checkout / "internal/cli/ROOT.go"
        git("mv", "internal/cli/root.go", str(destination.relative_to(self.checkout)), cwd=self.checkout)
        head = self.commit("fixture: case-only rename")

        published = driver.action_publish(self.publish_brief(["internal/cli/root.go"]))

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
                    "narration": {
                        "source": "template",
                        "subject": "conflict-domain: Conflict domain",
                        "body": "",
                    },
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
        with self.assertRaisesRegex(
            driver.DriverError,
            rf'"internal/cli/root\.go".*exact published head {published_head} was abandoned',
        ):
            driver.action_rebase(brief)

        self.assertNotEqual(
            subprocess.run(
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
            ).returncode,
            0,
        )

    def test_rebase_fast_path_rechecks_and_witnesses_ownership(self) -> None:
        contact = self.checkout / "internal/contacts/model.go"
        contact.parent.mkdir(parents=True)
        contact.write_text("package contacts\n", encoding="utf-8")
        self.commit("fixture: fast-path task")
        published = driver.action_publish(self.publish_brief(["internal/contacts"]))
        brief = self.brief(["internal/contacts"])
        brief.update({"publication": published, "constraints": []})

        integrated = driver.action_rebase(brief)

        self.assertFalse(integrated["regate"])
        self.assertEqual(integrated["head"], published["head"])
        self.assertEqual(integrated["ownership"], published["ownership"])

    def test_a_lane_that_merged_the_base_is_rejected_by_its_real_cause(self) -> None:
        """A merge commit makes every mainline path look authored by the task.

        The union walks lane history with `git log -m`, so a lane that merged
        the base branch instead of rebasing onto it claims every path its
        siblings landed. The lane is named for what it actually did rather than
        failed on paths nobody in the task touched.
        """
        contact = self.checkout / "internal/contacts/model.go"
        contact.parent.mkdir(parents=True)
        contact.write_text("package contacts\n", encoding="utf-8")
        self.commit("fixture: owned work")

        # A merge-free lane on this exact fixture is admitted.
        self.assertEqual(
            driver.action_ownership(self.ownership_brief(["internal/contacts"]))[
                "ownedPaths"
            ],
            ["internal/contacts/model.go"],
        )

        self.advance_base(
            "internal/cli/root.go",
            "package cli\n// sibling lane merged\n",
            "fixture: sibling lane merges",
        )
        git("merge", "--no-edit", "origin/main", cwd=self.checkout)

        with self.assertRaisesRegex(
            driver.DriverError,
            "rebase instead of merging the base into your lane",
        ):
            driver.action_ownership(self.ownership_brief(["internal/contacts"]))
        with self.assertRaisesRegex(
            driver.DriverError,
            "rebase instead of merging the base into your lane",
        ):
            driver.action_publish(self.publish_brief(["internal/contacts"]))
        self.assert_not_published()

    def test_a_lane_rebased_onto_an_advanced_base_owns_only_its_own_commits(self) -> None:
        """Rebasing onto the current base is the documented remediation.

        Resolving the union against the prepared base turns that remediation
        into a spurious ownership failure on every mainline path a sibling lane
        landed while the task was live.
        """
        contact = self.checkout / "internal/contacts/model.go"
        contact.parent.mkdir(parents=True)
        contact.write_text("package contacts\n", encoding="utf-8")
        self.commit("fixture: owned work")
        advanced = self.advance_base(
            "internal/cli/root.go",
            "package cli\n// sibling lane merged\n",
            "fixture: sibling lane merges outside the domain",
        )
        git("rebase", "origin/main", cwd=self.checkout)
        head = git("rev-parse", "HEAD", cwd=self.checkout)

        ownership = driver.action_ownership(self.ownership_brief(["internal/contacts"]))

        self.assertEqual(ownership["ownedPaths"], ["internal/contacts/model.go"])
        # The receipt still names the base this lane was prepared and gated on;
        # only the union narrowed.
        self.assertEqual(ownership["baseRev"], self.base_rev)
        self.assertEqual(ownership["head"], head)
        self.assertNotEqual(advanced, self.base_rev)
        published = driver.action_publish(self.publish_brief(["internal/contacts"]))
        self.assertEqual(published["ownership"], ownership)

    def test_a_rebased_lane_survives_the_base_advancing_again_behind_it(self) -> None:
        """The union starts where the lane leaves the base branch.

        Resolving against the *current tip* only works until the tip moves
        again: the lane then no longer contains it, the resolution falls back
        to the stale prepared base, and the mainline merge commits the lane
        inherited when it rebased make it look like the lane merged the base.
        """
        contact = self.checkout / "internal/contacts/model.go"
        contact.parent.mkdir(parents=True)
        contact.write_text("package contacts\n", encoding="utf-8")
        self.commit("fixture: owned work")
        self.advance_base(
            "internal/cli/root.go",
            "package cli\n// sibling one\n",
            "fixture: sibling one",
        )
        git("rebase", "origin/main", cwd=self.checkout)
        rebased_head = git("rev-parse", "HEAD", cwd=self.checkout)
        # A second sibling lands while this lane is still being gated, so the
        # lane no longer contains the base branch tip.
        self.advance_base("docs/guide.md", "later\n", "fixture: sibling two")
        self.assertNotEqual(
            git("rev-parse", "origin/main", cwd=self.checkout), rebased_head
        )
        self.assertEqual(
            len(
                git(
                    "rev-list",
                    "--merges",
                    f"{self.base_rev}..{rebased_head}",
                    cwd=self.checkout,
                ).split()
            ),
            1,
            "the lane inherited a mainline merge commit by rebasing",
        )

        ownership = driver.action_ownership(self.ownership_brief(["internal/contacts"]))

        self.assertEqual(ownership["ownedPaths"], ["internal/contacts/model.go"])
        self.assertEqual(ownership["baseRev"], self.base_rev)

    def test_the_forbid_paths_gate_resolves_against_the_same_base_as_ownership(
        self,
    ) -> None:
        """#276 f2's second site: the gate node reads the lane's own history.

        A lane that rebased onto the advanced base used to pass ownership and
        then go red at the gate on a mainline path a sibling landed — the same
        spurious red, one node later.
        """
        contact = self.checkout / "internal/contacts/model.go"
        contact.parent.mkdir(parents=True)
        contact.write_text("package contacts\n", encoding="utf-8")
        self.commit("fixture: owned work")
        self.advance_base("build/late.db", "sibling artifact\n", "fixture: sibling db")
        git("rebase", "origin/main", cwd=self.checkout)
        head = git("rev-parse", "HEAD", cwd=self.checkout)
        gate = {
            "kind": "forbidPaths",
            "id": "no-db-artifacts",
            "forbidPaths": ["*.db", "*.db-wal", "*.db-shm", "*.sqlite*"],
            "runtimeMaxSec": 11,
        }

        receipt = driver.action_constraint(
            {
                "gate": gate,
                "repositoryConfig": self.brief()["repositoryConfig"],
                "workspace": {
                    "taskId": "conflict-domain",
                    "baseRev": self.base_rev,
                    "branch": self.local_branch,
                    "worktreePath": str(self.checkout),
                },
            }
        )

        self.assertEqual(receipt["gateId"], "no-db-artifacts")
        self.assertEqual(receipt["checkedPaths"], 1)
        self.assertEqual(receipt["baseRev"], self.base_rev)
        self.assertEqual(receipt["head"], head)

        published = driver.action_publish(
            {
                **self.publish_brief(["internal/contacts"], gates=[gate]),
                "constraints": [receipt],
            }
        )

        self.assertEqual(published["head"], head)
        self.assertEqual(published["ownership"]["ownedPaths"], ["internal/contacts/model.go"])

    def test_a_forged_remote_tracking_ref_cannot_empty_the_union(self) -> None:
        """The lane's ref store is shared with the checkout and agent-writable.

        `refs/remotes/<remote>/<base>` lives in the common Git directory, so a
        lane can point it at its own head. If the union base came from that
        ref, the union would collapse to nothing and every declared domain
        would be vacuously satisfied — in the one check that exists because the
        agent is not trusted to stay inside its lane.
        """
        worktree = self.root / "lanes/conflict-domain"
        worktree.parent.mkdir(parents=True)
        git(
            "worktree",
            "add",
            "--detach",
            str(worktree),
            self.base_rev,
            cwd=self.checkout,
        )
        git("switch", "-c", "tally-work/fixture/lane", cwd=worktree)
        contact = worktree / "internal/contacts/model.go"
        contact.parent.mkdir(parents=True)
        contact.write_text("package contacts\n", encoding="utf-8")
        (worktree / "internal/cli/root.go").write_text(
            "package cli\n// unowned\n", encoding="utf-8"
        )
        git("add", "--all", cwd=worktree)
        git("commit", "-m", "fixture: owned work and one unowned path", cwd=worktree)
        head = git("rev-parse", "HEAD", cwd=worktree)
        brief = self.ownership_brief(["internal/contacts"])
        brief["workspace"].update(
            {"branch": "tally-work/fixture/lane", "worktreePath": str(worktree)}
        )
        publish = self.publish_brief(["internal/contacts"])
        publish["workspace"] = dict(brief["workspace"])
        publish["workspace"]["publishBranch"] = self.publish_branch

        with self.assertRaisesRegex(driver.DriverError, r'"internal/cli/root\.go"'):
            driver.action_ownership(brief)

        # The agent, from inside its own lane, points the shared
        # remote-tracking ref at its head.
        git("update-ref", "refs/remotes/origin/main", head, cwd=worktree)
        self.assertEqual(git("rev-parse", "origin/main", cwd=self.checkout), head)

        with self.assertRaisesRegex(driver.DriverError, r'"internal/cli/root\.go"'):
            driver.action_ownership(brief)
        with self.assertRaisesRegex(driver.DriverError, r'"internal/cli/root\.go"'):
            driver.action_publish(publish)
        self.assert_not_published()

    def test_publication_answers_to_configured_gates_not_to_the_receipt(self) -> None:
        """A receipt re-run against itself proves nothing about today's gate."""
        contact = self.checkout / "internal/contacts/model.go"
        contact.parent.mkdir(parents=True)
        contact.write_text("package contacts\n", encoding="utf-8")
        head = self.commit("fixture: owned work")
        witnessed = {
            "gateId": "no-secrets",
            "kind": "forbidPaths",
            "patterns": ["secrets/**"],
            "checkedPaths": 1,
            "baseRev": self.base_rev,
            "head": head,
        }
        configured = {
            "kind": "forbidPaths",
            "id": "no-secrets",
            "forbidPaths": ["secrets/**"],
            "runtimeMaxSec": 60,
        }
        widened = {**configured, "forbidPaths": ["secrets/**", "vault/**"]}

        drifted = self.publish_brief(["internal/contacts"], gates=[widened])
        drifted["constraints"] = [witnessed]
        with self.assertRaisesRegex(
            driver.DriverError,
            r"forbidPaths gate 'no-secrets' was witnessed against patterns",
        ):
            driver.action_publish(drifted)
        self.assert_not_published()

        missing = self.publish_brief(["internal/contacts"], gates=[configured])
        missing["constraints"] = []
        with self.assertRaisesRegex(
            driver.DriverError,
            r"forbidPaths gate 'no-secrets' is configured .* no witnessed receipt",
        ):
            driver.action_publish(missing)
        self.assert_not_published()

        unconfigured = self.publish_brief(["internal/contacts"], gates=[])
        unconfigured["constraints"] = [witnessed]
        with self.assertRaisesRegex(
            driver.DriverError,
            r"forbidPaths gate 'no-secrets' presented a receipt",
        ):
            driver.action_publish(unconfigured)
        self.assert_not_published()

        matching = self.publish_brief(["internal/contacts"], gates=[configured])
        matching["constraints"] = [witnessed]
        published = driver.action_publish(matching)
        self.assertEqual(published["head"], head)

    def test_merge_refuses_an_ownership_receipt_that_disagrees_on_domains(self) -> None:
        """The merge action stops trusting the receipt's own domainsRequired."""
        contact = self.checkout / "internal/contacts/model.go"
        contact.parent.mkdir(parents=True)
        contact.write_text("package contacts\n", encoding="utf-8")
        self.commit("fixture: owned work")
        brief = self.publish_brief(["internal/contacts"])
        published = driver.action_publish(brief)
        rebase_brief = self.brief(["internal/contacts"])
        rebase_brief.update({"publication": published, "constraints": []})
        integrated = driver.action_rebase(rebase_brief)
        self.assertTrue(integrated["ownership"]["domainsRequired"])

        def merge_brief(domains_required: bool) -> dict[str, Any]:
            source = self.brief(["internal/contacts"])
            return {
                "campaign": source["campaign"],
                "repository": source["repository"],
                "repositoryConfig": source["repositoryConfig"],
                "issue": source["issue"],
                "runId": source["runId"],
                "workspaceRoot": source["workspaceRoot"],
                "task": source["task"],
                "domainsRequired": domains_required,
                "workspace": source["workspace"],
                "integration": integrated,
            }

        with self.assertRaisesRegex(
            driver.DriverError,
            "integration.ownership.domainsRequired does not match domainsRequired",
        ):
            driver.action_merge(merge_brief(False))
        self.assertEqual(
            git("--git-dir", str(self.remote), "rev-parse", "refs/heads/main"),
            self.base_rev,
        )

        merged = driver.action_merge(merge_brief(True))

        self.assertEqual(merged["taskId"], "conflict-domain")
        self.assertEqual(merged["ownership"], integrated["ownership"])
        self.assertNotEqual(
            git("--git-dir", str(self.remote), "rev-parse", "refs/heads/main"),
            self.base_rev,
        )

    def test_serial_task_without_domains_preserves_the_existing_contract(self) -> None:
        (self.checkout / "internal/cli/root.go").write_text(
            "package cli\n// serial campaign\n", encoding="utf-8"
        )
        head = self.commit("fixture: serial task")

        published = driver.action_publish(self.publish_brief(domains_required=False))

        self.assertEqual(published["head"], head)
        self.assertFalse(published["ownership"]["domainsRequired"])
        self.assertEqual(published["ownership"]["conflictDomains"], [])
        self.assertEqual(published["ownership"]["ownedPaths"], ["internal/cli/root.go"])


class PublicationNarrationTests(PublicationHarness):
    """The publish node is where the steward narrates, and where it cannot."""

    def shim(self, name: str, body: str) -> list[str]:
        import sys
        import textwrap

        path = self.root / name
        path.write_text(f"#!{sys.executable}\n" + textwrap.dedent(body), encoding="utf-8")
        path.chmod(0o755)
        return [sys.executable, str(path)]

    def deliver(self) -> str:
        (self.checkout / "README.md").write_text("base\nfeature\n", encoding="utf-8")
        return self.commit("wip: whatever the coder called it")

    def merge_squash(self, published: dict[str, Any], head: str) -> str:
        config = driver.repo_config(self.brief()["repositoryConfig"])
        return driver.merge_local(
            {
                "campaign": "fixture",
                "repository": "acme/spec",
                "issue": self.brief()["issue"],
                "workspaceRoot": str(self.root / "workspaces"),
                "task": task("conflict-domain", ["README.md"]),
            },
            config,
            {
                "taskId": "conflict-domain",
                "branch": self.publish_branch,
                "baseRev": self.base_rev,
                "head": head,
                "pullRequest": published["pullRequest"],
            },
            "squash",
            published["narration"],
        )

    def test_a_witnessed_publish_carries_steward_text_into_the_squash_commit(self) -> None:
        head = self.deliver()
        brief = self.publish_brief(["README.md"])
        brief["steward"] = {
            "adapter": "narrator",
            "argv": self.shim(
                "narrate",
                """
                import json
                import sys

                request = json.loads(sys.stdin.read())
                assert request["task"]["id"] == "conflict-domain", request
                assert "README.md" in request["diffStat"], request
                print("TALLY_FINAL_MESSAGE=" + json.dumps({
                    "type": "feat",
                    "scope": "readme",
                    "subject": "record the delivered behavior",
                    "body": "Narrated by the steward, executed by the node.",
                }))
                """,
            ),
            "runtimeMaxSec": 30,
        }

        published = driver.action_publish(brief)

        self.assertEqual(published["narration"]["source"], "steward")
        self.assertEqual(
            published["narration"]["subject"], "feat(readme): record the delivered behavior"
        )
        # The validator transcript is observable in the node's own result.
        self.assertEqual(
            published["narrationAttempts"],
            [{"attempt": 1, "status": "accepted", "reason": None}],
        )

        merge_commit = self.merge_squash(published, head)
        self.assertEqual(
            git("--git-dir", str(self.remote), "log", "-1", "--format=%B", merge_commit).strip(),
            "feat(readme): record the delivered behavior\n\n"
            "Narrated by the steward, executed by the node.",
        )
        self.assertEqual(
            len(
                git(
                    "--git-dir", str(self.remote), "log", "-1", "--format=%P", merge_commit
                ).split()
            ),
            1,
        )

    def test_a_dead_narrator_falls_back_to_the_template_and_the_lane_proceeds(self) -> None:
        head = self.deliver()
        brief = self.publish_brief(["README.md"])
        brief["steward"] = {
            "adapter": "narrator",
            "argv": self.shim(
                "narrate",
                """
                import sys

                sys.stdin.read()
                print("upstream refused the request", file=sys.stderr)
                raise SystemExit(1)
                """,
            ),
            "runtimeMaxSec": 30,
        }

        published = driver.action_publish(brief)

        self.assertEqual(published["narration"]["source"], "template")
        self.assertEqual(
            published["narration"]["subject"], "conflict-domain: Implement conflict-domain"
        )
        self.assertEqual(
            [entry["status"] for entry in published["narrationAttempts"]],
            ["failed", "failed"],
        )
        # The lane proceeds: the branch is published and the squash still lands.
        self.assertEqual(
            git("--git-dir", str(self.remote), "rev-parse", f"refs/heads/{self.publish_branch}"),
            head,
        )
        merge_commit = self.merge_squash(published, head)
        self.assertEqual(
            git("--git-dir", str(self.remote), "log", "-1", "--format=%s", merge_commit),
            "conflict-domain: Implement conflict-domain",
        )


if __name__ == "__main__":
    unittest.main()
