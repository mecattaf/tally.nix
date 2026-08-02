#!/usr/bin/env python3

"""The two-repository seam, exercised end to end against a local forge.

A spec-corpus campaign reads its worklist from a spec repository, cuts lanes
and publishes branches on a code repository, and keeps its campaign thread --
and therefore every machine receipt -- on an issue repository. The three
coordinates default inward, so the last class in this file is the control: a
campaign that configures none of them takes the same code path it took before
the seam existed, and nothing it emits moves.
"""

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


def implementation_task(identifier: str, domain: str) -> dict[str, Any]:
    return {
        "id": identifier,
        "kind": "implementation",
        "title": f"Build {identifier}",
        "goal": f"Materialize {identifier}.",
        "deliveredBehaviors": [f"{identifier} exists"],
        "readFirst": {"specSections": ["spec.md#one"], "styleReferences": []},
        "acceptanceCriteria": [
            {"id": f"{identifier}-ok", "description": "It passes.", "argv": ["true"]}
        ],
        "dependencies": [],
        "conflictDomains": [domain],
    }


class Repository:
    """A bare remote plus one writable checkout, the local-forge shape."""

    def __init__(self, root: Path, name: str, owner: str = "acme") -> None:
        self.name = f"{owner}/{name}"
        self.remote = root / f"{name}.git"
        self.checkout = root / name
        git("init", "--bare", "--initial-branch=main", str(self.remote))
        git("init", "--initial-branch=main", str(self.checkout))
        git("config", "user.name", "Two Repo Test", cwd=self.checkout)
        git("config", "user.email", "two-repo@example.invalid", cwd=self.checkout)
        git("remote", "add", "origin", str(self.remote), cwd=self.checkout)

    def commit(self, path: str, content: str, message: str) -> str:
        target = self.checkout / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
        git("add", "--all", cwd=self.checkout)
        git("commit", "-m", message, cwd=self.checkout)
        git("push", "--quiet", "origin", "main", cwd=self.checkout)
        return git("rev-parse", "HEAD", cwd=self.checkout)

    @property
    def config(self) -> dict[str, str]:
        return {
            "checkout": str(self.checkout),
            "baseBranch": "main",
            "remote": "origin",
            "forge": "local",
        }

    @property
    def coordinate(self) -> dict[str, Any]:
        return {"repository": self.name, "repositoryConfig": self.config}

    def refs(self, pattern: str = "refs/tally/*") -> dict[str, str]:
        listed = git("ls-remote", str(self.remote), pattern)
        return {
            line.split("\t", 1)[1]: line.split("\t", 1)[0]
            for line in listed.splitlines()
            if "\t" in line
        }


class TwoRepositoryCampaign(unittest.TestCase):
    """Spec, code and issue on three distinct repositories."""

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.spec = Repository(self.root, "spec")
        self.code = Repository(self.root, "code")
        self.issues = Repository(self.root, "board")

        worklist = {
            "schemaVersion": 1,
            "tasks": [implementation_task("task-1", "one")],
        }
        self.spec.commit("README.md", "spec corpus\n", "fixture: spec base")
        self.spec_rev = self.spec.commit(
            "specs/001-fixture/tasks.json",
            json.dumps(worklist) + "\n",
            "fixture: worklist",
        )
        # Three unrelated histories. Nothing the code repository publishes can
        # be an ancestor of the spec revision, so any node that confuses the
        # two fails loudly rather than reading the wrong history.
        self.code_rev = self.code.commit("src/main.txt", "code\n", "fixture: code base")
        self.issues.commit("README.md", "board\n", "fixture: board base")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def brief(self, **overrides: Any) -> dict[str, Any]:
        brief = {
            "campaign": "fixture",
            "repository": self.code.name,
            "repositoryConfig": self.code.config,
            "issue": {"number": "7", "url": "local://acme/board/issues/7"},
            "worklist": "specs/*/tasks.json",
            "maxTasks": 4,
            "maxParallel": 1,
            "specRepository": self.spec.coordinate,
            "issueRepository": self.issues.coordinate,
        }
        brief.update(overrides)
        return brief

    def test_the_worklist_is_witnessed_from_the_spec_repository(self) -> None:
        worklist = driver.action_worklist(
            {
                "repository": self.code.name,
                "repositoryConfig": self.code.config,
                "worklist": "specs/*/tasks.json",
                "maxTasks": 4,
                "maxParallel": 1,
                "specRepository": self.spec.coordinate,
            }
        )
        self.assertEqual(worklist["source"]["revision"], self.spec_rev)
        self.assertEqual(worklist["source"]["repository"], self.spec.name)
        self.assertEqual(worklist["source"]["path"], "specs/001-fixture/tasks.json")
        self.assertEqual([task["id"] for task in worklist["tasks"]], ["task-1"])
        # The worklist file exists nowhere in the code repository.
        self.assertFalse((self.code.checkout / "specs").exists())

    def test_reconcile_splits_the_worklist_pin_from_the_code_anchor(self) -> None:
        reconciliation = driver.action_reconcile(self.brief())
        self.assertEqual(reconciliation["source"]["revision"], self.spec_rev)
        self.assertEqual(reconciliation["source"]["repository"], self.spec.name)
        self.assertEqual(reconciliation["baseRevision"], self.code_rev)
        self.assertNotEqual(self.spec_rev, self.code_rev)
        self.assertEqual(reconciliation["repository"], self.code.name)
        self.assertEqual(reconciliation["remaining"], ["task-1"])
        self.assertEqual([task["id"] for task in reconciliation["frontier"]], ["task-1"])

    def test_completion_is_read_from_the_code_repository(self) -> None:
        branch = driver.stable_publish_branch("fixture", "7", "task-1")
        landed = self.code.commit("src/task-1.txt", "done\n", "fixture: land task-1")
        git("push", "--quiet", "origin", f"{landed}:refs/heads/{branch}", cwd=self.code.checkout)
        reconciliation = driver.action_reconcile(self.brief())
        self.assertEqual(
            [fact["taskId"] for fact in reconciliation["merged"]], ["task-1"]
        )
        self.assertEqual(reconciliation["merged"][0]["mergeCommit"], landed)
        self.assertEqual(
            reconciliation["merged"][0]["pullRequest"],
            f"local://{self.code.name}/{branch}",
        )
        self.assertTrue(reconciliation["complete"])
        self.assertEqual(reconciliation["remaining"], [])

    def test_machine_receipts_land_on_the_issue_repository_only(self) -> None:
        steered = driver.action_steer(
            {
                "campaign": "fixture",
                "repository": self.code.name,
                "repositoryConfig": self.code.config,
                "issue": {"number": "7", "url": "local://acme/board/issues/7"},
                "taskId": "task-1",
                "attempt": 1,
                "diagnosis": "The lane needs a narrower change.",
                "specRepository": self.spec.coordinate,
                "issueRepository": self.issues.coordinate,
            }
        )
        self.assertTrue(steered["posted"])
        prefix = driver.local_state_prefix("fixture", "7")
        self.assertEqual(
            steered["comment"],
            f"local://{self.issues.name}/{prefix}/diagnosis/task-1/1",
        )
        self.assertIn(f"{prefix}/diagnosis/task-1/1", self.issues.refs())
        self.assertEqual(self.spec.refs(), {})
        self.assertEqual(self.code.refs(), {})
        # And the reconciler reads them back from the same coordinate.
        reconciliation = driver.action_reconcile(self.brief())
        self.assertEqual(
            [(item["taskId"], item["attempt"]) for item in reconciliation["diagnoses"]],
            [("task-1", 1)],
        )

    def test_a_retry_receipt_follows_the_campaign_thread(self) -> None:
        recorded = driver.action_retry(
            {
                "campaign": "fixture",
                "repository": self.code.name,
                "repositoryConfig": self.code.config,
                "issue": {"number": "7", "url": "local://acme/board/issues/7"},
                "taskId": "task-1",
                "stage": "prep",
                "detail": "The lane could not be cut.",
                "specRepository": self.spec.coordinate,
                "issueRepository": self.issues.coordinate,
            }
        )
        self.assertTrue(recorded["posted"])
        prefix = driver.local_state_prefix("fixture", "7")
        self.assertIn(f"{prefix}/retry/task-1/1", self.issues.refs())
        self.assertEqual(self.code.refs(), {})
        reconciliation = driver.action_reconcile(self.brief())
        self.assertEqual(
            [(item["taskId"], item["attempt"]) for item in reconciliation["retries"]],
            [("task-1", 1)],
        )

    def test_the_closing_summary_is_written_where_the_campaign_thread_lives(self) -> None:
        branch = driver.stable_publish_branch("fixture", "7", "task-1")
        landed = self.code.commit("src/task-1.txt", "done\n", "fixture: land task-1")
        git("push", "--quiet", "origin", f"{landed}:refs/heads/{branch}", cwd=self.code.checkout)
        reconciliation = driver.action_reconcile(self.brief())
        self.assertTrue(reconciliation["complete"])
        prefix = driver.local_state_prefix("fixture", "7")
        self.assertEqual(
            reconciliation["closingSummary"],
            f"local://{self.issues.name}/{prefix}/summary/complete",
        )
        self.assertIn(f"{prefix}/summary/complete", self.issues.refs())
        self.assertNotIn(f"{prefix}/summary/complete", self.code.refs())

    def test_the_continuation_receipt_lands_with_the_campaign_thread(self) -> None:
        events = self.root / "events"
        events.mkdir()
        continued = driver.action_continue(
            {
                "campaign": "fixture",
                "repository": self.code.name,
                "repositoryConfig": self.code.config,
                "issue": {"number": "7", "url": "local://acme/board/issues/7"},
                "runId": "run-1",
                "continuation": {
                    "argv": ["/bin/true"],
                    "pool": ["flow"],
                    "priority": "low",
                    "eventsDir": str(events),
                },
                "brief": None,
                "specRepository": self.spec.coordinate,
                "issueRepository": self.issues.coordinate,
            }
        )
        self.assertTrue(continued["created"])
        # The dedup key stays anchored to the code coordinate: campaign
        # identity is not something the seam is allowed to move.
        self.assertTrue(continued["dedupKey"].startswith(f"campaign-continuation:{self.code.name}:7:"))
        self.assertTrue(continued["receipt"].startswith(f"local://{self.issues.name}/"))
        self.assertEqual(self.code.refs(), {})

    def test_an_unconfigured_coordinate_is_refused(self) -> None:
        with self.assertRaises(driver.DriverError) as raised:
            driver.action_reconcile(
                self.brief(specRepository={"repository": "acme/spec", "repositoryConfig": {}})
            )
        self.assertIn("repositoryConfig", str(raised.exception))

    def test_a_pull_request_on_a_foreign_repository_never_completes_a_task(self) -> None:
        """The walk's cross-repository reference must be checked, not trusted."""
        marker = driver.pull_request_marker("fixture", "7", "task-1")
        branch = driver.stable_publish_branch("fixture", "7", "task-1")
        landed = self.code.commit("src/task-1.txt", "done\n", "fixture: land task-1")
        foreign = {
            "url": "https://github.com/acme/elsewhere/pull/9",
            "body": marker,
            "merged": True,
            "baseRefName": "main",
            "headRefName": branch,
            "mergeCommit": {"oid": landed},
            "repository": {"nameWithOwner": "acme/elsewhere"},
        }
        walk = {
            11: {
                "number": 11,
                "state": "open",
                "url": "https://github.com/acme/board/issues/11",
                "pullRequests": [foreign],
                "comments": [],
            }
        }
        task = dict(implementation_task("task-1", "one"))
        task["brief"] = {"issue": {"number": "11", "url": "https://example.invalid/11"}}
        facts, warnings = driver.merged_github_tasks(
            self.code.name,
            driver.repo_config(self.code.config),
            "fixture",
            "7",
            "main",
            None,
            [task],
            walk,
        )
        self.assertEqual(facts, [])
        self.assertEqual(len(warnings), 1)
        self.assertIn("acme/elsewhere", warnings[0])
        self.assertIn("not campaign code repository", warnings[0])


class IssueDefaultsToTheSpecRepository(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.spec = Repository(self.root, "spec")
        self.code = Repository(self.root, "code")
        self.spec.commit("README.md", "spec corpus\n", "fixture: spec base")
        self.spec_rev = self.spec.commit(
            "specs/001-fixture/tasks.json",
            json.dumps(
                {"schemaVersion": 1, "tasks": [implementation_task("task-1", "one")]}
            )
            + "\n",
            "fixture: worklist",
        )
        self.code_rev = self.code.commit("src/main.txt", "code\n", "fixture: code base")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_a_two_repository_campaign_keeps_its_receipts_with_the_spec(self) -> None:
        steered = driver.action_steer(
            {
                "campaign": "fixture",
                "repository": self.code.name,
                "repositoryConfig": self.code.config,
                "issue": {"number": "7", "url": "local://acme/spec/issues/7"},
                "taskId": "task-1",
                "attempt": 1,
                "diagnosis": "Narrow the change.",
                "specRepository": self.spec.coordinate,
            }
        )
        prefix = driver.local_state_prefix("fixture", "7")
        self.assertEqual(
            steered["comment"], f"local://{self.spec.name}/{prefix}/diagnosis/task-1/1"
        )
        self.assertIn(f"{prefix}/diagnosis/task-1/1", self.spec.refs())
        self.assertEqual(self.code.refs(), {})


class SingleRepositoryControl(unittest.TestCase):
    """No seam configured: the pre-seam path, unmoved."""

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.repository = Repository(self.root, "solo")
        self.repository.commit("README.md", "solo\n", "fixture: base")
        self.base_rev = self.repository.commit(
            "specs/001-fixture/tasks.json",
            json.dumps(
                {"schemaVersion": 1, "tasks": [implementation_task("task-1", "one")]}
            )
            + "\n",
            "fixture: worklist",
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def brief(self) -> dict[str, Any]:
        return {
            "campaign": "fixture",
            "repository": self.repository.name,
            "repositoryConfig": self.repository.config,
            "issue": {"number": "7", "url": "local://acme/solo/issues/7"},
            "worklist": "specs/*/tasks.json",
            "maxTasks": 4,
            "maxParallel": 1,
        }

    def test_the_witness_carries_no_seam_and_the_anchors_coincide(self) -> None:
        reconciliation = driver.action_reconcile(self.brief())
        self.assertEqual(
            set(reconciliation["source"]), {"path", "sha256", "revision"}
        )
        self.assertEqual(reconciliation["source"]["revision"], self.base_rev)
        self.assertEqual(reconciliation["baseRevision"], self.base_rev)

    def test_receipts_stay_in_the_one_repository(self) -> None:
        steered = driver.action_steer(
            {
                "campaign": "fixture",
                "repository": self.repository.name,
                "repositoryConfig": self.repository.config,
                "issue": {"number": "7", "url": "local://acme/solo/issues/7"},
                "taskId": "task-1",
                "attempt": 1,
                "diagnosis": "Narrow the change.",
            }
        )
        prefix = driver.local_state_prefix("fixture", "7")
        self.assertEqual(
            steered["comment"],
            f"local://{self.repository.name}/{prefix}/diagnosis/task-1/1",
        )
        self.assertIn(f"{prefix}/diagnosis/task-1/1", self.repository.refs())

    def test_an_unknown_brief_field_is_still_refused(self) -> None:
        brief = self.brief()
        brief["codeRepository"] = {"repository": "acme/solo"}
        with self.assertRaises(driver.DriverError) as raised:
            driver.action_reconcile(brief)
        self.assertIn("reconcile brief", str(raised.exception))

    def test_a_forge_native_campaign_refuses_the_seam(self) -> None:
        with self.assertRaises(driver.DriverError) as raised:
            driver.action_reconcile(
                {
                    "repository": self.repository.name,
                    "issue": {"number": "7", "url": "local://acme/solo/issues/7"},
                    "worklist": {"kind": "github-issue", "graphDigest": "sha256:" + "0" * 64},
                    "specRepository": {
                        "repository": "acme/other",
                        "repositoryConfig": self.repository.config,
                    },
                }
            )
        self.assertIn("forge-native campaign cannot carry specRepository", str(raised.exception))


if __name__ == "__main__":
    unittest.main(verbosity=1)
