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
from unittest import mock


DRIVER = Path(
    os.environ.get(
        "SPEC_BUILD_DRIVER",
        Path(__file__).parents[1] / "drivers/spec_build_driver.py",
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


def checkpoint_task(identifier: str, dependencies: list[str]) -> dict[str, Any]:
    return {
        "id": identifier,
        "kind": "checkpoint",
        "title": f"Validate {dependencies[0]}",
        "argv": ["true"],
        "runtimeMaxSec": 60,
        "dependencies": dependencies,
    }


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

        # A checkpoint task belongs in the split fixture: a spec-corpus
        # worklist phases itself with checkpoints, and the checkpoint node is
        # the one place that re-validates the reconciler's own `source` object.
        worklist = {
            "schemaVersion": 1,
            "tasks": [
                implementation_task("task-1", "one"),
                checkpoint_task("phase-checkpoint", ["task-1"]),
            ],
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
        self.workspaces = self.root / "workspaces"
        self.workspaces.mkdir()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def land_task_one(self) -> str:
        """Put a merged task-1 on the code repository's publish branch."""
        branch = driver.stable_publish_branch("fixture", "7", "task-1")
        landed = self.code.commit("src/task-1.txt", "done\n", "fixture: land task-1")
        git(
            "push", "--quiet", "origin", f"{landed}:refs/heads/{branch}",
            cwd=self.code.checkout,
        )
        return landed

    def record_checkpoint(self, reconciliation: dict[str, Any]) -> dict[str, Any]:
        """The checkpoint brief `spec-build.js` renders for a split campaign."""
        checkpoint = next(
            task for task in reconciliation["frontier"] if task["kind"] == "checkpoint"
        )
        prepared = self.prepared_lane(checkpoint, reconciliation)
        return driver.action_checkpoint(
            {
                "campaign": "fixture",
                "repository": self.code.name,
                "repositoryConfig": self.code.config,
                "issue": {"number": "7", "url": "local://acme/board/issues/7"},
                "task": checkpoint,
                "source": reconciliation["source"],
                "baseRevision": reconciliation["baseRevision"],
                "workspace": prepared,
                "specRepository": self.spec.coordinate,
                "issueRepository": self.issues.coordinate,
            }
        )

    def complete_campaign(self) -> dict[str, Any]:
        """Land the implementation task, pass the checkpoint, reconcile again."""
        self.land_task_one()
        self.record_checkpoint(driver.action_reconcile(self.brief()))
        return driver.action_reconcile(self.brief())

    def prepared_lane(self, task: dict[str, Any], reconciliation: dict[str, Any]) -> dict[str, Any]:
        """The prep brief `spec-build.js` renders -- which carries no seam."""
        return driver.action_prep(
            {
                "campaign": "fixture",
                "repository": self.code.name,
                "repositoryConfig": self.code.config,
                "issue": {"number": "7", "url": "local://acme/board/issues/7"},
                "runId": "run-1",
                "workspaceRoot": str(self.workspaces),
                "task": task,
                "sourceRevision": reconciliation["baseRevision"],
            }
        )

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
        self.assertEqual(
            [task["id"] for task in worklist["tasks"]], ["task-1", "phase-checkpoint"]
        )
        # The worklist file exists nowhere in the code repository.
        self.assertFalse((self.code.checkout / "specs").exists())

    def test_reconcile_splits_the_worklist_pin_from_the_code_anchor(self) -> None:
        reconciliation = driver.action_reconcile(self.brief())
        self.assertEqual(reconciliation["source"]["revision"], self.spec_rev)
        self.assertEqual(reconciliation["source"]["repository"], self.spec.name)
        self.assertEqual(reconciliation["baseRevision"], self.code_rev)
        self.assertNotEqual(self.spec_rev, self.code_rev)
        self.assertEqual(reconciliation["repository"], self.code.name)
        self.assertEqual(reconciliation["remaining"], ["task-1", "phase-checkpoint"])
        self.assertEqual([task["id"] for task in reconciliation["frontier"]], ["task-1"])

    def test_completion_is_read_from_the_code_repository(self) -> None:
        branch = driver.stable_publish_branch("fixture", "7", "task-1")
        landed = self.land_task_one()
        reconciliation = driver.action_reconcile(self.brief())
        self.assertEqual(
            [fact["taskId"] for fact in reconciliation["merged"]], ["task-1"]
        )
        self.assertEqual(reconciliation["merged"][0]["mergeCommit"], landed)
        self.assertEqual(
            reconciliation["merged"][0]["pullRequest"],
            f"local://{self.code.name}/{branch}",
        )
        # The checkpoint is now the frontier, and the campaign is not complete
        # until it holds a receipt of its own.
        self.assertFalse(reconciliation["complete"])
        self.assertEqual(reconciliation["remaining"], ["phase-checkpoint"])
        self.assertEqual(
            [task["id"] for task in reconciliation["frontier"]], ["phase-checkpoint"]
        )

    def test_a_checkpoint_task_completes_under_the_split_witness(self) -> None:
        """The reconciler's own `source` object must survive the checkpoint node.

        `action_worklist` adds `source.repository` whenever the campaign is
        split, `action_reconcile` forwards that object verbatim, and the flow
        hands it straight to the checkpoint node. A checkpoint node that
        re-validates it against a narrower key set fails every checkpoint task
        of every split campaign, permanently, on every pass.
        """
        self.land_task_one()
        reconciliation = driver.action_reconcile(self.brief())
        recorded = self.record_checkpoint(reconciliation)
        self.assertEqual(recorded["taskId"], "phase-checkpoint")
        self.assertEqual(recorded["revision"], reconciliation["baseRevision"])
        # The receipt is a fact about the code history, so it lives there.
        self.assertIn(recorded["ref"], self.code.refs())
        self.assertNotIn(recorded["ref"], self.spec.refs())
        self.assertNotIn(recorded["ref"], self.issues.refs())

        settled = driver.action_reconcile(self.brief())
        self.assertEqual(
            [fact["taskId"] for fact in settled["checkpoints"]], ["phase-checkpoint"]
        )
        self.assertTrue(settled["complete"])
        self.assertEqual(settled["remaining"], [])

    def test_the_publish_and_merge_chain_runs_under_a_split_brief(self) -> None:
        """prep -> publish -> merge, with the worklist on another repository."""
        reconciliation = driver.action_reconcile(self.brief())
        task = reconciliation["frontier"][0]
        prepared = self.prepared_lane(task, reconciliation)
        # The lane forks from the code history, never from the spec pin.
        self.assertEqual(prepared["baseRev"], self.code_rev)
        worktree = Path(prepared["worktreePath"])

        delivered = worktree / "one/task-1.txt"
        delivered.parent.mkdir(parents=True, exist_ok=True)
        delivered.write_text("delivered\n", encoding="utf-8")
        git("add", "--all", cwd=worktree)
        git("commit", "-m", "fixture: deliver task-1", cwd=worktree)
        head = git("rev-parse", "HEAD", cwd=worktree)

        publication = driver.action_publish(
            {
                "campaign": "fixture",
                "repository": self.code.name,
                "repositoryConfig": self.code.config,
                "issue": {"number": "7", "url": "local://acme/board/issues/7"},
                "runId": "run-1",
                "workspaceRoot": str(self.workspaces),
                "task": task,
                "domainsRequired": False,
                "gates": [],
                "steward": None,
                "workspace": prepared,
                "constraints": [],
                "specRepository": self.spec.coordinate,
                "issueRepository": self.issues.coordinate,
            }
        )
        self.assertEqual(publication["head"], head)
        self.assertEqual(
            publication["pullRequest"],
            f"local://{self.code.name}/{prepared['publishBranch']}",
        )
        self.assertIn(
            f"refs/heads/{prepared['publishBranch']}",
            self.code.refs(f"refs/heads/{prepared['publishBranch']}"),
        )

        merged = driver.action_merge(
            {
                "campaign": "fixture",
                "repository": self.code.name,
                "repositoryConfig": self.code.config,
                "issue": {"number": "7", "url": "local://acme/board/issues/7"},
                "runId": "run-1",
                "workspaceRoot": str(self.workspaces),
                "task": task,
                "domainsRequired": False,
                "mergeMethod": "squash",
                "gitAiBinding": "off",
                "gitAiAwaitSec": 60,
                "assistedBy": None,
                "workspace": prepared,
                "integration": {
                    "taskId": task["id"],
                    "baseRev": prepared["baseRev"],
                    "branch": prepared["publishBranch"],
                    "head": head,
                    "pullRequest": publication["pullRequest"],
                    "narration": publication["narration"],
                    "regate": False,
                    "ownership": publication["ownership"],
                },
            }
        )
        self.assertEqual(merged["taskId"], "task-1")
        # The merge landed on the code repository's base branch, and nothing
        # was written to the spec or issue repositories by the chain.
        git("fetch", "--quiet", "--prune", "origin", cwd=self.code.checkout)
        self.assertEqual(
            git("rev-parse", "origin/main", cwd=self.code.checkout),
            merged["mergeCommit"],
        )
        self.assertEqual(self.spec.refs(), {})
        self.assertEqual(self.issues.refs(), {})

        settled = driver.action_reconcile(self.brief())
        self.assertEqual(
            [fact["taskId"] for fact in settled["merged"]], ["task-1"]
        )
        self.assertEqual(settled["merged"][0]["mergeCommit"], merged["mergeCommit"])

    def pull_request_body(self, **seam: Any) -> str:
        """The body `github_pull_request` writes, with `gh` stubbed out."""
        data = {
            "campaign": "fixture",
            "repository": self.code.name,
            "issue": {"number": "7", "url": "https://example.invalid/acme/board/7"},
            "workspace": {
                "taskId": "task-1",
                "baseRev": self.code_rev,
                "publishBranch": driver.stable_publish_branch("fixture", "7", "task-1"),
            },
            "task": implementation_task("task-1", "one"),
            **seam,
        }
        created = subprocess.CompletedProcess(
            [], 0, "https://github.com/acme/code/pull/4\n", ""
        )
        with mock.patch.object(driver, "pull_requests_by_head", return_value=[]):
            with mock.patch.object(driver, "run", return_value=created) as run:
                driver.github_pull_request(
                    data,
                    driver.repo_config(self.code.config),
                    self.code.checkout,
                    self.code_rev,
                    {"source": "template", "subject": "task-1: Build", "body": ""},
                )
        command = run.call_args_list[-1].args[0]
        return command[command.index("--body") + 1]

    def test_the_pull_request_names_the_repository_its_campaign_issue_lives_on(
        self,
    ) -> None:
        """`owner/name#N` resolves in the repository it names, not in the reader's.

        Rendering the campaign back-reference against the code repository put a
        cross-reference on whatever object happened to be `#7` there -- an
        unrelated public thread, once per task.
        """
        body = self.pull_request_body(
            specRepository=self.spec.coordinate,
            issueRepository=self.issues.coordinate,
        )
        self.assertIn(f"campaign progress for {self.issues.name}#7", body)
        self.assertNotIn(f"{self.code.name}#7", body)

    def test_the_closing_summary_names_both_histories(self) -> None:
        settled = self.complete_campaign()
        self.assertTrue(settled["complete"])
        prefix = driver.local_state_prefix("fixture", "7")
        body = driver.read_local_blob(
            driver.repo_config(self.issues.config), f"{prefix}/summary/complete"
        )["body"]
        # The worklist revision resolves only in the spec repository; every
        # merge row below it names code repository artifacts. The summary says
        # which is which rather than printing a revision the reader cannot
        # check out.
        self.assertIn(f"`{self.spec_rev}` in `{self.spec.name}`", body)
        self.assertIn(
            f"code base `{settled['baseRevision']}` in `{self.code.name}`", body
        )
        self.assertNotEqual(settled["baseRevision"], self.spec_rev)

    def test_machine_receipts_land_on_the_issue_repository_only(self) -> None:
        steered = driver.action_steer(
            {
                "campaign": "fixture",
                "repository": self.code.name,
                "repositoryConfig": self.code.config,
                "issue": {"number": "7", "url": "local://acme/board/issues/7"},
                "taskId": "task-1",
                "attempt": 1,
                "diagnosis": "Diagnosed a lane that needs a narrower change.",
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
        reconciliation = self.complete_campaign()
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
        facts, restamps, warnings = driver.merged_github_tasks(
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
        self.assertEqual(restamps, [])
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
                "diagnosis": "Narrowed the change.",
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

    def test_the_pull_request_body_and_summary_keep_their_pre_seam_shape(self) -> None:
        data = {
            "campaign": "fixture",
            "repository": self.repository.name,
            "issue": {"number": "7", "url": "https://example.invalid/acme/solo/7"},
            "workspace": {
                "taskId": "task-1",
                "baseRev": self.base_rev,
                "publishBranch": driver.stable_publish_branch("fixture", "7", "task-1"),
            },
            "task": implementation_task("task-1", "one"),
        }
        created = subprocess.CompletedProcess(
            [], 0, "https://github.com/acme/solo/pull/4\n", ""
        )
        with mock.patch.object(driver, "pull_requests_by_head", return_value=[]):
            with mock.patch.object(driver, "run", return_value=created) as run:
                driver.github_pull_request(
                    data,
                    driver.repo_config(self.repository.config),
                    self.repository.checkout,
                    self.base_rev,
                    {"source": "template", "subject": "task-1: Build", "body": ""},
                )
        command = run.call_args_list[-1].args[0]
        body = command[command.index("--body") + 1]
        self.assertIn(f"campaign progress for {self.repository.name}#7", body)

        reconciliation = driver.action_reconcile(self.brief())
        summary = driver.render_campaign_summary(
            driver.campaign_digest(reconciliation, "quiescent")
        )
        # One history, so the worklist line keeps its unqualified one-line form.
        self.assertIn(
            f"Worklist `{reconciliation['source']['sha256']}` at `{self.base_rev}`.",
            summary,
        )
        self.assertNotIn("code base", summary)

    def test_receipts_stay_in_the_one_repository(self) -> None:
        steered = driver.action_steer(
            {
                "campaign": "fixture",
                "repository": self.repository.name,
                "repositoryConfig": self.repository.config,
                "issue": {"number": "7", "url": "local://acme/solo/issues/7"},
                "taskId": "task-1",
                "attempt": 1,
                "diagnosis": "Narrowed the change.",
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
