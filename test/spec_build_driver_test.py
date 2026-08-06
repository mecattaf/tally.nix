#!/usr/bin/env python3
"""Focused forge and lifecycle regressions for the spec-build policy driver."""

from __future__ import annotations

import importlib.util
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from typing import Any
from unittest import mock


SOURCE = Path(
    os.environ.get(
        "SPEC_BUILD_DRIVER_SOURCE",
        Path(__file__).resolve().parents[1] / "examples/flows/spec_build_driver.py",
    )
)
SPEC = importlib.util.spec_from_file_location("spec_build_driver", SOURCE)
assert SPEC is not None and SPEC.loader is not None
DRIVER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DRIVER)
# The shared worktree manager the driver resolves as a sibling module. Reading
# lane identity back through it is what proves the round-trip.
WORKTREES = DRIVER.worktrees


def command(*arguments: str, cwd: Path | None = None, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        list(arguments),
        cwd=cwd,
        check=check,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def git(checkout: Path, *arguments: str, check: bool = True) -> str:
    return command("git", "-C", str(checkout), *arguments, check=check).stdout.strip()


def initialize_repository(root: Path, *, remote: bool = False) -> tuple[Path, Path | None]:
    checkout = root / "checkout"
    checkout.mkdir()
    command("git", "init", "--quiet", "--initial-branch=main", str(checkout))
    git(checkout, "config", "user.name", "Tally Test")
    git(checkout, "config", "user.email", "tally-test@invalid")
    (checkout / "root.go").write_text("base\n", encoding="utf-8")
    git(checkout, "add", "root.go")
    git(checkout, "commit", "--quiet", "-m", "initial")
    if not remote:
        return checkout, None
    remote_path = root / "remote.git"
    command("git", "init", "--bare", "--quiet", "--initial-branch=main", str(remote_path))
    git(checkout, "remote", "add", "origin", str(remote_path))
    git(checkout, "push", "--quiet", "--set-upstream", "origin", "main")
    return checkout, remote_path


def repository_config(checkout: Path, forge: str = "local") -> dict[str, str]:
    return {
        "checkout": str(checkout),
        "baseBranch": "main",
        "remote": "origin",
        "forge": forge,
    }


def issue() -> dict[str, str]:
    return {"number": "7", "url": "https://github.com/acme/spec/issues/7"}


def continuation_spec(events: Path) -> dict[str, object]:
    """The module-declared continuation the Nix campaign module renders."""
    return {
        "argv": [
            "/nix/store/tally/bin/tally",
            "flow",
            "run",
            "/nix/store/spec-build.js",
            "--args-from-brief",
            "--max-nodes",
            "51",
        ],
        "pool": ["flow", "fixture-campaign"],
        "priority": "low",
        "runtimeMaxSec": 600,
        "eventsDir": str(events),
    }


def task(identifier: str, dependencies: list[str] | None = None) -> dict[str, object]:
    return {
        "kind": "implementation",
        "id": identifier,
        "title": f"Task {identifier}",
        "goal": "Deliver the task.",
        "deliveredBehaviors": ["The task is delivered."],
        "readFirst": {"specSections": ["spec.md"], "styleReferences": []},
        "acceptanceCriteria": [
            {"id": "test", "description": "The test passes.", "argv": ["true"]}
        ],
        "dependencies": dependencies or [],
        "conflictDomains": [identifier],
    }


def prep_brief(
    checkout: Path,
    workspace_root: Path,
    run_id: str,
    *,
    forge: str = "local",
    source_revision: str | None = None,
) -> dict[str, object]:
    return {
        "campaign": "fixture",
        "repository": "acme/spec",
        "repositoryConfig": repository_config(checkout, forge),
        "issue": issue(),
        "runId": run_id,
        "workspaceRoot": str(workspace_root),
        "task": task("task-1"),
        # The reconciler witnesses the worklist at the remote base head and
        # carries that revision into the prep brief.
        "sourceRevision": source_revision
        or git(checkout, "rev-parse", "--verify", "origin/main^{commit}"),
    }


def preflight_brief(
    checkout: Path,
    workspace_root: Path,
    run_id: str,
) -> dict[str, object]:
    return {
        "campaign": "fixture",
        "repository": "acme/spec",
        "repositoryConfig": repository_config(checkout),
        "issue": issue(),
        "runId": run_id,
        "workspaceRoot": str(workspace_root),
    }


def sweep_brief(
    checkout: Path,
    workspace_root: Path,
    run_id: str,
    tally: Path,
    *,
    campaign: str = "fixture",
    campaign_identity: str | None = None,
) -> dict[str, object]:
    brief: dict[str, object] = {
        "campaign": campaign,
        "repository": "acme/spec",
        "repositoryConfig": repository_config(checkout),
        "runId": run_id,
        "workspaceRoot": str(workspace_root),
        "tally": str(tally),
    }
    if campaign_identity is not None:
        brief["campaignIdentity"] = campaign_identity
    return brief


class FakeGitHub:
    def __init__(self, root: Path, state: dict[str, object]) -> None:
        self.root = root
        self.state_path = root / "fake-gh-state.json"
        self.bin = root / "bin"
        self.bin.mkdir()
        self.program = self.bin / "gh"
        self.state_path.write_text(json.dumps(state), encoding="utf-8")
        self.program.write_text(
            f"#!{sys.executable}\n"
            + textwrap.dedent(
                """\
                import json
                import os
                from pathlib import Path
                import sys

                state_path = Path(os.environ["FAKE_GH_STATE"])
                state = json.loads(state_path.read_text(encoding="utf-8"))
                args = sys.argv[1:]
                state.setdefault("calls", []).append(args)

                def rest_pull(item):
                    merged = item.get("state", "MERGED") == "MERGED"
                    return {
                        "html_url": item.get("url"),
                        "body": item.get("body"),
                        "base": {"ref": item.get("baseRefName")},
                        "head": {
                            "ref": item.get("headRefName"),
                            "sha": item.get("headRefOid"),
                        },
                        "merge_commit_sha": (item.get("mergeCommit") or {}).get("oid"),
                        "merged_at": "2026-08-02T00:00:00Z" if merged else None,
                        "state": "open" if item.get("state") == "OPEN" else "closed",
                    }

                def thread(number):
                    if number == "7":
                        return state.setdefault("issueComments", [])
                    return state.setdefault("threadComments", {}).setdefault(number, [])

                if args[:2] == ["pr", "merge"]:
                    # The real command both advances the base branch and flips
                    # the pull request; onMerge stands in for the first half so
                    # the driver's post-merge ancestry proof reads real git.
                    import subprocess

                    if state.get("onMerge"):
                        subprocess.run(state["onMerge"], check=True)
                    view = state.setdefault("prView", {})
                    view["state"] = "MERGED"
                    view["mergeCommit"] = {"oid": state.get("mergeCommitOid", "e" * 40)}
                elif args[:2] == ["pr", "reopen"]:
                    url = args[2]
                    for candidate in state.get("pulls", []):
                        if candidate.get("url") == url:
                            candidate["state"] = "OPEN"
                    print(url)
                elif args[:2] == ["pr", "view"]:
                    print(json.dumps(state.get("prView", {})))
                elif args[:2] == ["api", "user"]:
                    print(state.get("actor", "tally-test"))
                elif args[:2] == ["api", "graphql"]:
                    if state.get("walkFails"):
                        print("sub-issue walk unavailable", file=sys.stderr)
                        state_path.write_text(json.dumps(state), encoding="utf-8")
                        raise SystemExit(1)
                    print(json.dumps({
                        "data": {
                            "repository": {
                                "issue": {
                                    "subIssues": {
                                        "pageInfo": {
                                            "hasNextPage": False,
                                            "endCursor": None,
                                        },
                                        "nodes": state.get("walk", []),
                                    }
                                }
                            }
                        }
                    }))
                elif args and args[0] == "api":
                    endpoint = next(
                        (item for item in args[1:] if item.startswith("repos/")), ""
                    )
                    if endpoint.endswith("/sub_issues?per_page=100"):
                        print(json.dumps(state.get("subIssues", [])))
                    elif "/pulls?head=" in endpoint:
                        query = endpoint.split("/pulls?", 1)[1]
                        fields = dict(
                            pair.split("=", 1) for pair in query.split("&") if "=" in pair
                        )
                        branch = fields.get("head", "").split(":", 1)[-1]
                        if fields.get("state") == "all":
                            source = state.get("pulls", [])
                        else:
                            source = list(state.get("merged", []))
                            source += state.get("byHead", {}).get(branch, [])
                        print(json.dumps([
                            rest_pull(item)
                            for item in source
                            if item.get("headRefName") == branch
                        ]))
                    elif "/comments" in endpoint:
                        number = endpoint.split("/issues/", 1)[1].split("/", 1)[0]
                        if "--slurp" in args:
                            print(json.dumps([thread(number)]))
                        else:
                            for comment in thread(number):
                                print(comment.get("body", ""))
                    elif "/issues/" in endpoint:
                        number = endpoint.rsplit("/issues/", 1)[1]
                        if number == "7":
                            print(json.dumps(state.get("master", {})))
                        else:
                            found = next(
                                (
                                    candidate
                                    for candidate in state.get("subIssues", [])
                                    if str(candidate.get("number")) == number
                                ),
                                {"number": int(number), "state": "closed"},
                            )
                            print(json.dumps(found))
                    else:
                        print(f"unexpected fake gh api endpoint: {endpoint!r}", file=sys.stderr)
                        state_path.write_text(json.dumps(state), encoding="utf-8")
                        raise SystemExit(92)
                elif args[:2] == ["issue", "comment"]:
                    failures = state.get("commentFailures", 0)
                    if failures:
                        state["commentFailures"] = failures - 1
                        state_path.write_text(json.dumps(state), encoding="utf-8")
                        print("injected comment failure", file=sys.stderr)
                        raise SystemExit(93)
                    number = args[2]
                    body = args[args.index("--body") + 1]
                    state.setdefault("comments", []).append(body)
                    comment_number = len(state["comments"])
                    url = (
                        f"https://github.com/acme/spec/issues/{number}"
                        f"#issuecomment-test-{comment_number}"
                    )
                    thread(number).append({
                        "body": body,
                        "html_url": url,
                        "user": {"login": state.get("actor", "tally-test")},
                    })
                    print(url)
                elif args[:2] == ["issue", "edit"]:
                    body = sys.stdin.read()
                    state.setdefault("master", {})["body"] = body
                    print("https://github.com/acme/spec/issues/7")
                else:
                    print(f"unexpected fake gh argv: {args!r}", file=sys.stderr)
                    state_path.write_text(json.dumps(state), encoding="utf-8")
                    raise SystemExit(91)

                state_path.write_text(json.dumps(state), encoding="utf-8")
                """
            ),
            encoding="utf-8",
        )
        self.program.chmod(0o755)
        self.old_path: str | None = None
        self.old_state: str | None = None

    def __enter__(self) -> "FakeGitHub":
        self.old_path = os.environ.get("PATH")
        self.old_state = os.environ.get("FAKE_GH_STATE")
        os.environ["PATH"] = str(self.bin) + os.pathsep + (self.old_path or "")
        os.environ["FAKE_GH_STATE"] = str(self.state_path)
        return self

    def __exit__(self, *_: object) -> None:
        if self.old_path is None:
            os.environ.pop("PATH", None)
        else:
            os.environ["PATH"] = self.old_path
        if self.old_state is None:
            os.environ.pop("FAKE_GH_STATE", None)
        else:
            os.environ["FAKE_GH_STATE"] = self.old_state

    def state(self) -> dict[str, object]:
        return json.loads(self.state_path.read_text(encoding="utf-8"))


class ForgeNativeReconcileTests(unittest.TestCase):
    def test_issue_worklist_binds_completion_to_the_observed_base_revision(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root)
            base_revision = git(checkout, "rev-parse", "HEAD")
            issue_task = {
                "id": "task-1",
                "kind": "implementation",
                "title": "Task one",
                "brief": {
                    "issue": {
                        "number": "8",
                        "url": "https://github.com/acme/spec/issues/8",
                    },
                    "body": "Implement task one.",
                },
                "dependencies": [],
                "conflictDomains": ["task-1"],
                "revision": "sha256:" + "b" * 64,
            }
            worklist = {
                "schemaVersion": 1,
                "repository": "acme/spec",
                "source": {
                    "kind": "github-issue",
                    "url": "https://github.com/acme/spec/issues/7",
                    "sha256": "sha256:" + "a" * 64,
                    "revision": base_revision,
                },
                "tasks": [issue_task],
                "config": {
                    "campaign": "fixture-native",
                    "repositoryConfig": repository_config(checkout, "github"),
                    "maxParallel": 1,
                },
                "masterBody": "fixture",
            }
            with (
                mock.patch.object(DRIVER, "issue_graph_worklist", return_value=worklist),
                mock.patch.object(DRIVER, "merged_github_tasks", return_value=([], [])) as merged,
                mock.patch.object(DRIVER, "forge_campaign_state", return_value=([], [], None, [])),
                mock.patch.object(DRIVER, "sync_issue_checkboxes"),
            ):
                result = DRIVER.action_reconcile(
                    {
                        "repository": "acme/spec",
                        "issue": issue(),
                        "worklist": {"kind": "github-issue"},
                    }
                )

            self.assertEqual(result["source"], worklist["source"])
            self.assertEqual(result["source"]["revision"], base_revision)
            self.assertEqual(merged.call_args.args[5], base_revision)
            self.assertEqual([item["id"] for item in result["frontier"]], ["task-1"])


class FakeTally:
    TASK_UUID = "00000000-0000-4000-8000-000000000901"

    def __init__(self, root: Path, current_flow_run_id: str) -> None:
        self.root = root
        self.state_path = root / "fake-tally-state.json"
        self.program = root / "fake-tally"
        self.state_path.write_text(
            json.dumps(
                {
                    "currentFlowRunId": current_flow_run_id,
                    "flows": {},
                    "calls": [],
                    "failQueries": False,
                }
            ),
            encoding="utf-8",
        )
        self.program.write_text(
            f"#!{sys.executable}\n"
            + textwrap.dedent(
                """\
                import json
                import os
                from pathlib import Path
                import sys

                state_path = Path(os.environ["FAKE_TALLY_STATE"])
                state = json.loads(state_path.read_text(encoding="utf-8"))
                args = sys.argv[1:]
                state.setdefault("calls", []).append(args)
                state_path.write_text(json.dumps(state), encoding="utf-8")
                if state.get("failQueries"):
                    print("injected tally query failure", file=sys.stderr)
                    raise SystemExit(92)
                if args[:2] == ["query", "job"]:
                    print(json.dumps({
                        "job": {
                            "orchestration": {
                                "flowRunId": state["currentFlowRunId"]
                            }
                        }
                    }))
                elif args[:2] == ["query", "jobs"]:
                    if "--flow-run" in args:
                        flow_run_id = args[args.index("--flow-run") + 1]
                        items = state.get("flows", {}).get(flow_run_id, [])
                    else:
                        live_state = args[args.index("--state") + 1]
                        configured = state.get("liveJobs")
                        if configured is None:
                            configured = [
                                item
                                for flow_items in state.get("flows", {}).values()
                                for item in flow_items
                            ]
                        items = [
                            item for item in configured
                            if item.get("liveState") == live_state
                        ]
                    print(json.dumps({
                        "items": items,
                        "nextCursor": None
                    }))
                else:
                    print(f"unexpected fake tally argv: {args!r}", file=sys.stderr)
                    raise SystemExit(91)
                """
            ),
            encoding="utf-8",
        )
        self.program.chmod(0o755)

    def __enter__(self) -> "FakeTally":
        self.environment = mock.patch.dict(
            os.environ,
            {
                "FAKE_TALLY_STATE": str(self.state_path),
                "TALLY_TASK_UUID": self.TASK_UUID,
            },
        )
        self.environment.start()
        return self

    def __exit__(self, *_: object) -> None:
        self.environment.stop()

    def state(self) -> dict[str, object]:
        return json.loads(self.state_path.read_text(encoding="utf-8"))

    def update(self, **values: object) -> None:
        state = self.state()
        state.update(values)
        self.state_path.write_text(json.dumps(state), encoding="utf-8")

REDACTION_VECTORS = Path(
    os.environ.get(
        "SPEC_BUILD_REDACTION_VECTORS",
        Path(__file__).resolve().parents[1] / "test/fixtures/redaction/vectors.json",
    )
)


class RedactionVectorTests(unittest.TestCase):
    def test_shared_vector_holds_for_public_steering(self) -> None:
        corpus = json.loads(REDACTION_VECTORS.read_text(encoding="utf-8"))
        cases = corpus["cases"]
        self.assertTrue(cases)
        for case in cases:
            with self.subTest(case=case["name"]):
                text, redacted = DRIVER.redact_public_text(case["input"])
                self.assertEqual(
                    text,
                    case["output"].replace(
                        "%LINE%", "[redacted sensitive diagnosis line]"
                    ),
                )
                self.assertEqual(redacted, case["redacted"])

    def test_a_diagnosis_naming_tasks_and_revisions_survives_intact(self) -> None:
        steering = (
            "task-1 and subtask-2 both failed after disk-1 filled.\n"
            "Rebase onto 6347cbb9f4a2b1c0d5e6f70819a2b3c4d5e6f708 and retry the gate.\n"
            "The auth token bug is unrelated."
        )
        text, redacted = DRIVER.redact_public_text(steering)
        self.assertEqual(text, steering)
        self.assertFalse(redacted)

    def test_receipts_written_by_a_superseded_redactor_stay_readable(self) -> None:
        self.assertIn("conservative-v1", DRIVER.PUBLIC_REDACTIONS)
        self.assertIn(DRIVER.PUBLIC_REDACTION, DRIVER.PUBLIC_REDACTIONS)


class GitHubForgeTests(unittest.TestCase):
    def test_reconcile_requires_exact_marker_and_degrades_unusable_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            worklist = checkout / "specs/campaign/tasks.json"
            worklist.parent.mkdir(parents=True)
            third_task = task("task-3", ["task-1"])
            third_task["conflictDomains"] = ["task-2"]
            worklist.write_text(
                json.dumps(
                    {
                        "schemaVersion": 1,
                        "tasks": [
                            task("task-1"),
                            task("task-2", ["task-1"]),
                            third_task,
                        ],
                    }
                ),
                encoding="utf-8",
            )
            git(checkout, "add", str(worklist.relative_to(checkout)))
            git(checkout, "commit", "--quiet", "-m", "add worklist")
            git(checkout, "push", "--quiet", "origin", "main")
            base_rev = git(checkout, "rev-parse", "HEAD")
            task_1_marker = DRIVER.pull_request_marker("fixture", "7", "task-1")
            task_2_marker = DRIVER.pull_request_marker("fixture", "7", "task-2")
            state = {
                "merged": [
                    {
                        "url": "https://github.com/acme/spec/pull/1",
                        "body": task_1_marker,
                        "baseRefName": "main",
                        "headRefName": "tally/fixture-issue-7/task-1",
                        "mergeCommit": {"oid": base_rev},
                    },
                    {
                        "url": "https://github.com/acme/spec/pull/2",
                        "body": (
                            "Spec-build campaign progress for acme/spec#7.\n"
                            "Task `task-2`: quoted text without an identity marker"
                        ),
                        "baseRefName": "main",
                        "headRefName": "tally/fixture-issue-7/task-2",
                        "mergeCommit": {"oid": "b" * 40},
                    },
                    {
                        "url": "https://github.com/acme/spec/pull/3",
                        "body": task_2_marker,
                        "baseRefName": "release",
                        "headRefName": "tally/fixture-issue-7/task-2",
                        "mergeCommit": {"oid": "c" * 40},
                    },
                    {
                        "url": "https://github.com/acme/spec/pull/4",
                        "body": DRIVER.pull_request_marker("fixture", "7", "unknown-task"),
                        "baseRefName": "main",
                        "headRefName": "tally/fixture-issue-7/task-2",
                        "mergeCommit": {"oid": "d" * 40},
                    },
                ],
                "byHead": {},
                "comments": [],
                "calls": [],
            }
            with FakeGitHub(root, state) as github:
                result = DRIVER.action_reconcile(
                    {
                        "campaign": "fixture",
                        "repository": "acme/spec",
                        "repositoryConfig": repository_config(checkout, "github"),
                        "issue": issue(),
                        "worklist": "specs/*/tasks.json",
                        "maxTasks": 3,
                        "maxParallel": 2,
                    }
                )
                self.assertEqual([fact["taskId"] for fact in result["merged"]], ["task-1"])
                self.assertEqual(result["remaining"], ["task-2", "task-3"])
                self.assertEqual([item["id"] for item in result["frontier"]], ["task-2"])
                self.assertTrue(any("pull/3" in warning for warning in result["warnings"]))
                self.assertTrue(any("no task" in warning for warning in result["warnings"]))
                self.assertTrue(
                    any(
                        "conflictDomains limited this ready frontier"
                        in warning
                        for warning in result["warnings"]
                    )
                )

                ambiguous = github.state()
                ambiguous["merged"].append(
                    {
                        "url": "https://github.com/acme/spec/pull/5",
                        "body": task_1_marker,
                        "baseRefName": "main",
                        "headRefName": "tally/fixture-issue-7/task-1",
                        "mergeCommit": {"oid": base_rev},
                    }
                )
                github.state_path.write_text(json.dumps(ambiguous), encoding="utf-8")
                with self.assertRaisesRegex(DRIVER.DriverError, "multiple merged pull requests"):
                    DRIVER.action_reconcile(
                        {
                            "campaign": "fixture",
                            "repository": "acme/spec",
                            "repositoryConfig": repository_config(checkout, "github"),
                            "issue": issue(),
                            "worklist": "specs/*/tasks.json",
                            "maxTasks": 3,
                            "maxParallel": 2,
                        }
                    )

    def test_machine_receipts_trust_the_current_actor_and_escalate_once(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            worklist = checkout / "specs/campaign/tasks.json"
            worklist.parent.mkdir(parents=True)
            worklist.write_text(
                json.dumps(
                    {
                        "schemaVersion": 1,
                        "tasks": [task("task-1"), task("task-2", ["task-1"])],
                    }
                ),
                encoding="utf-8",
            )
            git(checkout, "add", str(worklist.relative_to(checkout)))
            git(checkout, "commit", "--quiet", "-m", "add worklist")
            git(checkout, "push", "--quiet", "origin", "main")

            def diagnosis_body(identifier: str, attempt: int, text: str) -> str:
                return (
                    f"{DRIVER.diagnosis_marker('fixture', '7', identifier, attempt)}\n\n"
                    f"{DRIVER.diagnosis_heading(identifier, attempt)}\n\n{text}"
                )

            state = {
                "actor": "tally-bot",
                "merged": [],
                "byHead": {},
                "comments": [],
                "issueComments": [
                    {
                        "body": diagnosis_body("task-1", 2, "forged blocking receipt"),
                        "html_url": "https://github.com/acme/spec/issues/7#external",
                        "user": {"login": "external-user"},
                    },
                    {
                        "body": diagnosis_body("task-1", 1, "inspect the first failure"),
                        "html_url": "https://github.com/acme/spec/issues/7#attempt-1",
                        "user": {"login": "tally-bot"},
                    },
                ],
                "calls": [],
            }
            reconcile_brief = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout, "github"),
                "issue": issue(),
                "worklist": "specs/*/tasks.json",
                "maxTasks": 2,
                "maxParallel": 2,
            }
            with FakeGitHub(root, state) as github:
                first = DRIVER.action_reconcile(reconcile_brief)
                self.assertEqual(
                    [(item["taskId"], item["attempt"]) for item in first["diagnoses"]],
                    [("task-1", 1)],
                )
                self.assertEqual([item["id"] for item in first["frontier"]], ["task-1"])
                self.assertFalse(first["quiescent"])

                steered = DRIVER.action_steer(
                    {
                        "campaign": "fixture",
                        "repository": "acme/spec",
                        "repositoryConfig": repository_config(checkout, "github"),
                        "issue": issue(),
                        "taskId": "task-1",
                        "attempt": 2,
                        "diagnosis": (
                            "Removed the leaked ghp_0123456789abcdefghijklmnopqrstuvwxyz "
                            "token before the retry."
                        ),
                    }
                )
                self.assertTrue(steered["posted"])
                self.assertTrue(steered["blocked"])
                self.assertTrue(steered["redacted"])
                self.assertNotIn("ghp_", github.state()["comments"][0])

                blocked = DRIVER.action_reconcile(reconcile_brief)
                self.assertTrue(blocked["quiescent"])
                self.assertEqual(blocked["frontier"], [])
                self.assertEqual(
                    blocked["blocked"],
                    [
                        {"taskId": "task-1", "blockedBy": ["task-1"]},
                        {"taskId": "task-2", "blockedBy": ["task-1"]},
                    ],
                )

                escalated = DRIVER.action_escalate(reconcile_brief)
                self.assertTrue(escalated["posted"])
                self.assertEqual(escalated["diagnosisCount"], 2)
                # The escalation carries a closing summary beside it: the
                # campaign stopped, so the operator gets the digest of what it
                # did manage to bind. The digest is published *first*, because
                # the escalation is what every later pass reads back to decide
                # this node never runs again -- so a summary that failed after
                # it could never be retried.
                self.assertIsNotNone(escalated["summary"])
                summary_body, escalation_body = github.state()["comments"][-2:]
                self.assertIn("Campaign closed at frontier quiescence", summary_body)
                self.assertIn("`task-1`", summary_body)
                self.assertNotIn("ghp_", summary_body)
                self.assertIn("Spec-build escalation", escalation_body)
                repeated = DRIVER.action_escalate(reconcile_brief)
                self.assertFalse(repeated["posted"])
                self.assertIsNone(repeated["summary"])
                self.assertEqual(repeated["comment"], escalated["comment"])
                # One steering note, one escalation, one closing summary, and
                # the repeat pass adds nothing.
                self.assertEqual(len(github.state()["comments"]), 3)

    def test_a_completed_file_worklist_campaign_summarises_and_leaves_the_issue_open(
        self,
    ) -> None:
        """The spec-corpus class gets its digest; tally still never closes that issue.

        A file-worklist campaign's master issue is a projection tally does not
        own the lifecycle of -- only the forge-native issue graph is closed by
        the reconciler. Publishing the closing summary there and stopping is
        the deliberate shape: the operator learns the campaign finished, and
        the issue stays theirs to close.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            worklist = checkout / "specs/campaign/tasks.json"
            worklist.parent.mkdir(parents=True)
            worklist.write_text(
                json.dumps({"schemaVersion": 1, "tasks": [task("task-1")]}),
                encoding="utf-8",
            )
            git(checkout, "add", str(worklist.relative_to(checkout)))
            git(checkout, "commit", "--quiet", "-m", "add worklist")
            git(checkout, "push", "--quiet", "origin", "main")
            base_rev = git(checkout, "rev-parse", "HEAD")
            state = {
                "actor": "tally-bot",
                "merged": [
                    {
                        "url": "https://github.com/acme/spec/pull/1",
                        "body": DRIVER.pull_request_marker("fixture", "7", "task-1"),
                        "baseRefName": "main",
                        "headRefName": "tally/fixture-issue-7/task-1",
                        "mergeCommit": {"oid": base_rev},
                    }
                ],
                "byHead": {},
                "comments": [],
                "issueComments": [],
                "calls": [],
            }
            with FakeGitHub(root, state) as github:
                result = DRIVER.action_reconcile(
                    {
                        "campaign": "fixture",
                        "repository": "acme/spec",
                        "repositoryConfig": repository_config(checkout, "github"),
                        "issue": issue(),
                        "worklist": "specs/*/tasks.json",
                        "maxTasks": 1,
                        "maxParallel": 1,
                    }
                )
            self.assertTrue(result["complete"])
            self.assertIsNotNone(result["closingSummary"])
            published = github.state()
            self.assertEqual(len(published["comments"]), 1)
            self.assertIn("### Campaign complete", published["comments"][0])
            self.assertIn("tally:campaign-complete:v1", published["comments"][0])
            self.assertFalse(
                any(call[:2] == ["issue", "close"] for call in published["calls"]),
                published["calls"],
            )

    def test_a_failed_quiescent_summary_leaves_the_whole_act_retryable(self) -> None:
        """The escalation is the thing every later pass reads back to stop.

        So it must be the *last* thing published on the quiescent path. If the
        summary went second and failed once -- a rate limit, a transient
        network error -- the escalation would already be on the issue, every
        later pass would return early, and the campaign would silently lose the
        only artifact that says what it managed to bind.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            worklist = checkout / "specs/campaign/tasks.json"
            worklist.parent.mkdir(parents=True)
            worklist.write_text(
                json.dumps({"schemaVersion": 1, "tasks": [task("task-1")]}),
                encoding="utf-8",
            )
            git(checkout, "add", str(worklist.relative_to(checkout)))
            git(checkout, "commit", "--quiet", "-m", "add worklist")
            git(checkout, "push", "--quiet", "origin", "main")

            def diagnosis_body(attempt: int) -> str:
                return (
                    f"{DRIVER.diagnosis_marker('fixture', '7', 'task-1', attempt)}\n\n"
                    f"{DRIVER.diagnosis_heading('task-1', attempt)}\n\nsteering {attempt}"
                )

            state = {
                "actor": "tally-bot",
                "merged": [],
                "byHead": {},
                "comments": [],
                "issueComments": [
                    {
                        "body": diagnosis_body(attempt),
                        "html_url": f"https://github.com/acme/spec/issues/7#a{attempt}",
                        "user": {"login": "tally-bot"},
                    }
                    for attempt in (1, 2)
                ],
                "calls": [],
            }
            reconcile_brief = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout, "github"),
                "issue": issue(),
                "worklist": "specs/*/tasks.json",
                "maxTasks": 1,
                "maxParallel": 1,
            }
            with FakeGitHub(root, state) as github:
                published = DRIVER.publish_closing_summary
                attempts: list[int] = []

                def flaky(*arguments: object, **keywords: object) -> str:
                    attempts.append(1)
                    if len(attempts) == 1:
                        raise DRIVER.DriverError("gh: API rate limit exceeded")
                    return published(*arguments, **keywords)  # type: ignore[arg-type]

                with mock.patch.object(DRIVER, "publish_closing_summary", flaky):
                    with self.assertRaisesRegex(DRIVER.DriverError, "rate limit"):
                        DRIVER.action_escalate(reconcile_brief)
                    # Nothing was published, so nothing tells a later pass to
                    # stop: the terminal act is retried whole.
                    self.assertEqual(github.state()["comments"], [])

                    escalated = DRIVER.action_escalate(reconcile_brief)
                self.assertEqual(len(attempts), 2)
                self.assertTrue(escalated["posted"])
                self.assertIsNotNone(escalated["summary"])
                summary_body, escalation_body = github.state()["comments"]
                self.assertIn("Campaign closed at frontier quiescence", summary_body)
                self.assertIn("Spec-build escalation", escalation_body)

    def test_steering_escalation_and_the_closing_summary_are_always_fresh(self) -> None:
        """§9.1.3 holds a line: receipts upsert silently, these three never do.

        A sticky comment is invisible to whoever is watching the thread, so the
        three surfaces that exist to notify the operator must keep creating new
        comments. None of them may acquire an edit-in-place path.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            worklist = checkout / "specs/campaign/tasks.json"
            worklist.parent.mkdir(parents=True)
            worklist.write_text(
                json.dumps({"schemaVersion": 1, "tasks": [task("task-1")]}),
                encoding="utf-8",
            )
            git(checkout, "add", "specs/campaign/tasks.json")
            git(checkout, "commit", "--quiet", "-m", "add worklist")
            git(checkout, "push", "--quiet", "origin", "main")
            state = {
                "actor": "tally-bot",
                "merged": [],
                "byHead": {},
                "comments": [],
                "issueComments": [],
                "calls": [],
            }
            reconcile_brief = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout, "github"),
                "issue": issue(),
                "worklist": "specs/*/tasks.json",
                "maxTasks": 1,
                "maxParallel": 1,
            }
            with FakeGitHub(root, state) as github:
                for attempt in (1, 2):
                    steered = DRIVER.action_steer(
                        {
                            "campaign": "fixture",
                            "repository": "acme/spec",
                            "repositoryConfig": repository_config(checkout, "github"),
                            "issue": issue(),
                            "taskId": "task-1",
                            "attempt": attempt,
                            "diagnosis": f"Steered attempt {attempt}.",
                        }
                    )
                    self.assertTrue(steered["posted"])
                escalated = DRIVER.action_escalate(reconcile_brief)
                self.assertTrue(escalated["posted"])

                published = github.state()
                # Two steering notes, one escalation, and the closing summary
                # that rides with it: four distinct comments, each created
                # rather than edited.
                self.assertEqual(len(published["comments"]), 4)
                self.assertEqual(
                    len({comment["html_url"] for comment in published["issueComments"]}),
                    4,
                )
                creates = [
                    call for call in published["calls"] if call[:2] == ["issue", "comment"]
                ]
                self.assertEqual(len(creates), 4)
                self.assertFalse(
                    any("--edit-last" in call for call in published["calls"])
                )
                self.assertFalse(
                    any(
                        "updateIssueComment" in argument
                        for call in published["calls"]
                        for argument in call
                    )
                )

            # The completion summary publishes the same way: create, never
            # edit, and only once for a given worklist digest.
            digest = "sha256:" + "a" * 64
            reconciliation = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "source": {"sha256": digest, "revision": "b" * 40},
                "baseRevision": "b" * 40,
                "tasks": [{"id": "task-1", "title": "Task 1"}],
                "merged": [
                    {
                        "taskId": "task-1",
                        "pullRequest": "https://github.com/acme/spec/pull/4",
                        "mergeCommit": "c" * 40,
                    }
                ],
                "checkpoints": [],
                "remaining": [],
                "diagnoses": [],
                "retries": [],
                "deferrals": [],
                "blocked": [],
                "anomalies": [],
                "warnings": [],
            }
            posted = subprocess.CompletedProcess(
                [], 0, "https://github.com/acme/spec/issues/7#issuecomment-1\n", ""
            )
            empty = subprocess.CompletedProcess([], 0, "", "")
            with mock.patch.object(
                DRIVER, "run", side_effect=[empty, posted]
            ) as run:
                DRIVER.publish_closing_summary(
                    "acme/spec",
                    repository_config(checkout, "github"),
                    "fixture",
                    "7",
                    DRIVER.campaign_digest(reconciliation, "complete"),
                )
            commands = [call.args[0] for call in run.call_args_list]
            summary = next(
                command for command in commands if command[:3] == ["gh", "issue", "comment"]
            )
            self.assertIn("Campaign complete", summary[-1])
            self.assertIn(f"tally:campaign-complete:v1 source={digest}", summary[-1])
            self.assertIn("https://github.com/acme/spec/pull/4", summary[-1])
            self.assertFalse(
                any("--edit-last" in command or "updateIssueComment" in command for command in commands)
            )
            # A repeated terminal pass finds its own marker and stays quiet.
            seen = subprocess.CompletedProcess([], 0, summary[-1], "")
            with mock.patch.object(DRIVER, "run", side_effect=[seen]) as run:
                DRIVER.publish_closing_summary(
                    "acme/spec",
                    repository_config(checkout, "github"),
                    "fixture",
                    "7",
                    DRIVER.campaign_digest(reconciliation, "complete"),
                )
            self.assertFalse(
                any(
                    call.args[0][:3] == ["gh", "issue", "comment"]
                    for call in run.call_args_list
                )
            )

    def test_worklist_edits_degrade_receipts_instead_of_bricking_the_campaign(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            worklist = checkout / "specs/campaign/tasks.json"
            worklist.parent.mkdir(parents=True)

            def write_worklist(*identifiers: str) -> None:
                worklist.write_text(
                    json.dumps(
                        {
                            "schemaVersion": 1,
                            "tasks": [task(identifier) for identifier in identifiers],
                        }
                    ),
                    encoding="utf-8",
                )
                git(checkout, "add", "specs/campaign/tasks.json")
                git(checkout, "commit", "--quiet", "-m", "operator: edit the worklist")
                git(checkout, "push", "--quiet", "origin", "main")

            def diagnosis_body(identifier: str, attempt: int, text: str) -> str:
                return (
                    f"{DRIVER.diagnosis_marker('fixture', '7', identifier, attempt)}\n\n"
                    f"{DRIVER.diagnosis_heading(identifier, attempt)}\n\n{text}"
                )

            write_worklist("task-1", "task-2")
            state = {
                "actor": "tally-bot",
                "merged": [],
                "byHead": {},
                "comments": [],
                "issueComments": [
                    {
                        "body": diagnosis_body("task-2", 1, "first failure"),
                        "html_url": "https://github.com/acme/spec/issues/7#one",
                        "user": {"login": "tally-bot"},
                    },
                    {
                        "body": diagnosis_body("task-2", 2, "second failure"),
                        "html_url": "https://github.com/acme/spec/issues/7#two",
                        "user": {"login": "tally-bot"},
                    },
                ],
                "calls": [],
            }
            reconcile_brief = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout, "github"),
                "issue": issue(),
                "worklist": "specs/*/tasks.json",
                "maxTasks": 2,
                "maxParallel": 2,
            }
            with FakeGitHub(root, state) as github:
                blocked = DRIVER.action_reconcile(reconcile_brief)
                self.assertEqual(
                    blocked["blocked"], [{"taskId": "task-2", "blockedBy": ["task-2"]}]
                )

                # The operator renames the diagnosed task between passes.
                write_worklist("task-1", "task-2-renamed")
                renamed = DRIVER.action_reconcile(reconcile_brief)
                self.assertEqual(renamed["diagnoses"], [])
                self.assertEqual(renamed["blocked"], [])
                self.assertEqual(
                    [
                        warning
                        for warning in renamed["warnings"]
                        if "no longer names that task" in warning
                    ],
                    [
                        "dropped machine diagnosis for 'task-2': "
                        "the worklist no longer names that task",
                        "dropped machine diagnosis for 'task-2': "
                        "the worklist no longer names that task",
                    ],
                )
                self.assertEqual(
                    [item["id"] for item in renamed["frontier"]],
                    ["task-1", "task-2-renamed"],
                )

                # An operator who deletes the first receipt leaves a gap, not a halt.
                write_worklist("task-1", "task-2")
                gapped_state = github.state()
                gapped_state["issueComments"] = [gapped_state["issueComments"][1]]
                github.state_path.write_text(
                    json.dumps(gapped_state), encoding="utf-8"
                )
                gapped = DRIVER.action_reconcile(reconcile_brief)
                self.assertEqual(gapped["diagnoses"], [])
                self.assertEqual(gapped["blocked"], [])
                self.assertIn(
                    "dropped machine diagnosis for 'task-2' attempt 2: "
                    "no attempt 1 receipt precedes it",
                    gapped["warnings"],
                )

                # Escalation reconciles the same edited worklist. It reaches its
                # own quiescence check instead of dying on the dropped receipts.
                with self.assertRaisesRegex(
                    DRIVER.DriverError, "incomplete empty frontier"
                ):
                    DRIVER.action_escalate(reconcile_brief)

    def test_machinery_retries_are_bounded_and_spend_no_steering_attempt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            worklist = checkout / "specs/campaign/tasks.json"
            worklist.parent.mkdir(parents=True)
            worklist.write_text(
                json.dumps({"schemaVersion": 1, "tasks": [task("task-1")]}),
                encoding="utf-8",
            )
            git(checkout, "add", "specs/campaign/tasks.json")
            git(checkout, "commit", "--quiet", "-m", "fixture: worklist")
            git(checkout, "push", "--quiet", "origin", "main")
            reconcile_brief = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout),
                "issue": issue(),
                "worklist": "specs/*/tasks.json",
                "maxTasks": 1,
                "maxParallel": 1,
            }

            def retry(stage: str) -> dict[str, object]:
                return DRIVER.action_retry(
                    {
                        "campaign": "fixture",
                        "repository": "acme/spec",
                        "repositoryConfig": repository_config(checkout),
                        "issue": issue(),
                        "taskId": "task-1",
                        "stage": stage,
                        "detail": "the integration checkout could not be staged",
                    }
                )

            first = retry("merge")
            self.assertTrue(first["posted"])
            self.assertEqual(first["attempt"], 1)
            self.assertFalse(first["exhausted"])
            second = retry("rebase")
            self.assertTrue(second["posted"])
            self.assertEqual(second["attempt"], 2)
            self.assertTrue(second["exhausted"])

            # The budget is spent: the caller must steer the next fault instead.
            third = retry("merge")
            self.assertFalse(third["posted"])
            self.assertTrue(third["exhausted"])

            reconciled = DRIVER.action_reconcile(reconcile_brief)
            self.assertEqual(
                [(item["taskId"], item["attempt"]) for item in reconciled["retries"]],
                [("task-1", 1), ("task-1", 2)],
            )
            self.assertEqual(reconciled["diagnoses"], [])
            self.assertEqual(reconciled["blocked"], [])
            self.assertEqual([item["id"] for item in reconciled["frontier"]], ["task-1"])

    def test_a_checkpoint_defers_only_while_unrelated_work_can_still_change_it(self) -> None:
        tasks = [
            task("task-1"),
            {
                "kind": "checkpoint",
                "id": "phase-one",
                "title": "Validate phase one",
                "argv": ["true"],
                "runtimeMaxSec": 10,
                "dependencies": ["task-1"],
            },
            task("task-2", ["phase-one"]),
            task("task-3"),
        ]
        remaining = [candidate for candidate in tasks if candidate["id"] != "task-1"]
        deferrals = DRIVER.checkpoint_deferrals(tasks, remaining, {"task-1"}, set())
        self.assertEqual(
            deferrals, [{"taskId": "phase-one", "waitingOn": ["task-3"]}]
        )

        # task-2 sits below the checkpoint and task-3 is blocked, so neither can
        # change its verdict: the checkpoint runs for real and reaches quiescence.
        self.assertEqual(
            DRIVER.checkpoint_deferrals(tasks, remaining, {"task-1"}, {"task-3"}),
            [],
        )

    def test_forge_native_issue_graph_derives_blocking_and_escalation_config(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            manifest = {
                "schemaVersion": 1,
                "name": "fixture",
                "repository": repository_config(checkout, "github"),
                "maxTasks": 2,
                "maxParallel": 2,
                "driverRuntimeMaxSec": 900,
                "runtimeMaxSec": 3600,
                "pool": "campaign",
                "agent": {
                    "adapter": "codex",
                    "argv": ["read the admitted brief"],
                    "priority": "low",
                    "runtimeMaxSec": 900,
                    "approvalPolicy": "never",
                    "sandboxPolicy": "danger-full-access",
                },
                "gates": [
                    {
                        "kind": "command",
                        "id": "tests",
                        "preflightArgv": ["true"],
                        "argv": ["true"],
                        "runtimeMaxSec": 60,
                    }
                ],
                "tasks": [
                    {
                        "id": "task-1",
                        "kind": "implementation",
                        "issue": 8,
                        "dependencies": [],
                        "conflictDomains": ["src/one"],
                    },
                    {
                        "id": "task-2",
                        "kind": "implementation",
                        "issue": 9,
                        "dependencies": ["task-1"],
                        "conflictDomains": ["src/two"],
                    },
                ],
            }
            master_body = (
                f"{DRIVER.CAMPAIGN_BEGIN}\n```json\n"
                f"{json.dumps(manifest)}\n```\n{DRIVER.CAMPAIGN_END}\n\n"
                f"{DRIVER.WORKLIST_BEGIN}\n\n{DRIVER.WORKLIST_END}\n"
            )

            def diagnosis_body(attempt: int) -> str:
                marker = DRIVER.diagnosis_marker("fixture", "7", "task-1", attempt)
                heading = DRIVER.diagnosis_heading("task-1", attempt)
                return f"{marker}\n\n{heading}\n\nsteering attempt {attempt}"

            state = {
                "actor": "tally-bot",
                "master": {
                    "number": 7,
                    "state": "open",
                    "html_url": "https://github.com/acme/spec/issues/7",
                    "body": master_body,
                    "updated_at": "2026-08-01T12:00:00Z",
                },
                "subIssues": [
                    {
                        "number": 8,
                        "title": "First task",
                        "body": "Implement the first task.",
                        "state": "open",
                        "html_url": "https://github.com/acme/spec/issues/8",
                        "updated_at": "2026-08-01T12:00:00Z",
                    },
                    {
                        "number": 9,
                        "title": "Dependent task",
                        "body": "Implement the dependent task.",
                        "state": "open",
                        "html_url": "https://github.com/acme/spec/issues/9",
                        "updated_at": "2026-08-01T12:00:00Z",
                    },
                ],
                "merged": [],
                "byHead": {},
                "comments": [],
                "issueComments": [
                    {
                        "body": diagnosis_body(1),
                        "html_url": "https://github.com/acme/spec/issues/7#attempt-1",
                        "user": {"login": "tally-bot"},
                    },
                    {
                        "body": diagnosis_body(2),
                        "html_url": "https://github.com/acme/spec/issues/7#attempt-2",
                        "user": {"login": "tally-bot"},
                    },
                ],
                "calls": [],
            }
            _, references, normalized_manifest = DRIVER.forge_manifest(manifest)
            graph_digest = DRIVER.canonical_sha256(
                {
                    "manifest": normalized_manifest,
                    "tasks": [
                        {
                            "number": reference["issue"],
                            "title": state["subIssues"][index]["title"],
                            "body": state["subIssues"][index]["body"],
                        }
                        for index, reference in enumerate(references)
                    ],
                }
            )
            reconcile_brief = {
                "repository": "acme/spec",
                "issue": issue(),
                "worklist": {
                    "kind": "github-issue",
                    "graphDigest": graph_digest,
                },
            }
            with FakeGitHub(root, state) as github:
                result = DRIVER.action_reconcile(reconcile_brief)
                self.assertEqual(result["campaign"], "fixture")
                self.assertTrue(result["quiescent"])
                self.assertEqual(result["frontier"], [])
                self.assertEqual(
                    result["blocked"],
                    [
                        {"taskId": "task-1", "blockedBy": ["task-1"]},
                        {"taskId": "task-2", "blockedBy": ["task-1"]},
                    ],
                )
                self.assertNotIn("reconcileCommand", result["config"])
                self.assertIn("#8 — First task", github.state()["master"]["body"])

                escalated = DRIVER.action_escalate(reconcile_brief)
                self.assertTrue(escalated["posted"])
                self.assertEqual(escalated["diagnosisCount"], 2)
                # Closing summary first, escalation second.
                summary_body, escalation_body = github.state()["comments"]
                self.assertIn("Campaign closed at frontier quiescence", summary_body)
                self.assertIn("frontier quiescent", escalation_body)

    def test_pr_reopen_progress_and_one_pass_continuation_use_fake_gh(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root)
            marker = DRIVER.pull_request_marker("fixture", "7", "task-1")
            head = "a" * 40
            state = {
                "pulls": [
                    {
                        "url": "https://github.com/acme/spec/pull/7",
                        "body": marker,
                        "baseRefName": "main",
                        "headRefName": "tally/fixture-issue-7/task-1",
                        "headRefOid": head,
                        "state": "CLOSED",
                    }
                ],
                "comments": [],
                "calls": [],
            }
            data = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "issue": issue(),
                "task": task("task-1"),
                "workspace": {"publishBranch": "tally/fixture-issue-7/task-1"},
            }
            with FakeGitHub(root, state) as github:
                url = DRIVER.github_pull_request(
                    data,
                    {"baseBranch": "main"},
                    checkout,
                    head,
                    DRIVER.template_narration(data["task"]),
                )
                self.assertEqual(url, "https://github.com/acme/spec/pull/7")
                self.assertEqual(github.state()["pulls"][0]["state"], "OPEN")

                DRIVER.github_merge_checkbox_repair(data)
                DRIVER.github_merge_checkbox_repair(data)
                events = root / "events"
                continued = DRIVER.action_continue(
                    {
                        "campaign": "fixture",
                        "repository": "acme/spec",
                        "repositoryConfig": repository_config(checkout, "github"),
                        "issue": issue(),
                        "runId": "pass-1",
                        "continuation": continuation_spec(events),
                        "brief": {"campaign": "fixture", "runId": "pass-1"},
                    }
                )
                self.assertTrue(continued["created"])
                self.assertIsNone(continued["receipt"])
                self.assertEqual(
                    continued["dedupKey"],
                    f"campaign-continuation:acme/spec:7:{continued['runId']}",
                )
                payload = json.loads(Path(continued["event"]).read_text(encoding="utf-8"))
                self.assertEqual(payload["source"], "events-dir")
                self.assertEqual(payload["adapter"], "shell")
                self.assertEqual(payload["pool"], ["flow", "fixture-campaign"])
                self.assertEqual(payload["priority"], "low")
                self.assertEqual(payload["evidence"], ["exit:0"])
                self.assertEqual(payload["submission"], {"mode": "full"})
                self.assertEqual(payload["runtimeMaxSec"], 600)
                self.assertFalse(payload["noEnqueue"])
                self.assertEqual(payload["dedupKey"], continued["dedupKey"])
                self.assertEqual(payload["brief"]["runId"], continued["runId"])
                self.assertEqual(payload["brief"]["campaign"], "fixture")

                final_state = github.state()
                # The machine's note-to-self is process, and process belongs in
                # the journal. A merging pass now leaves no public comment at
                # all: the per-merge progress comment is gone, and a task with
                # no sub-issue has no checkbox to repair either.
                self.assertEqual(final_state["comments"], [])
                self.assertEqual(
                    sum(call[:2] == ["issue", "comment"] for call in final_state["calls"]),
                    0,
                )
                self.assertEqual(
                    sum(call[:2] == ["pr", "reopen"] for call in final_state["calls"]),
                    1,
                )

    def test_continuation_event_is_derived_bounded_and_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root)
            events = root / "events"
            brief = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout, "github"),
                "issue": issue(),
                "runId": "pass-2",
                "continuation": continuation_spec(events),
                "brief": None,
            }
            with FakeGitHub(root, {"comments": [], "calls": []}) as github:
                first = DRIVER.action_continue(dict(brief))
                second = DRIVER.action_continue(dict(brief))
                self.assertEqual(github.state()["comments"], [])
                self.assertEqual(github.state()["calls"], [])
            # One pass derives exactly one successor identity, and re-running
            # the node cannot mint a second event while the first is undrained.
            self.assertEqual(first["runId"], second["runId"])
            self.assertEqual(first["event"], second["event"])
            self.assertTrue(first["created"])
            self.assertFalse(second["created"])
            self.assertEqual(
                [entry.name for entry in sorted(events.iterdir())],
                [Path(first["event"]).name],
            )
            payload = json.loads(Path(first["event"]).read_text(encoding="utf-8"))
            self.assertNotIn("brief", payload)
            self.assertTrue(Path(first["event"]).name.endswith(".json"))
            self.assertFalse(Path(first["event"]).name.endswith(".enqueue.json"))
            self.assertLess(
                len(Path(first["event"]).read_bytes()),
                DRIVER.MAX_CONTINUATION_EVENT_BYTES,
            )
            # The successor identity chains, so consecutive passes never
            # collide on one dedup key.
            third = DRIVER.action_continue(dict(brief, runId=first["runId"]))
            self.assertNotEqual(third["runId"], first["runId"])
            self.assertNotEqual(third["dedupKey"], first["dedupKey"])

    def test_local_forge_continuation_keeps_its_durable_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            events = root / "events"
            continued = DRIVER.action_continue(
                {
                    "campaign": "fixture",
                    "repository": "acme/spec",
                    "repositoryConfig": repository_config(checkout),
                    "issue": issue(),
                    "runId": "pass-3",
                    "continuation": continuation_spec(events),
                    "brief": None,
                }
            )
            self.assertTrue(continued["created"])
            reference = continued["receipt"].split("acme/spec/", 1)[1]
            self.assertEqual(
                reference,
                f"refs/tally/spec-build/v1/{DRIVER.state_scope('fixture', '7')}"
                f"/continuation/{continued['runId']}",
            )
            listed = git(checkout, "ls-remote", "origin", reference)
            self.assertTrue(listed, "the local forge kept no continuation receipt")
            blob = git(checkout, "cat-file", "blob", listed.split()[0])
            self.assertEqual(
                json.loads(blob),
                {
                    "schemaVersion": 1,
                    "kind": "continuation",
                    "campaign": "fixture",
                    "issueNumber": "7",
                    "runId": "pass-3",
                    "dedupKey": continued["dedupKey"],
                },
            )

    def test_continuation_rejects_an_unbounded_or_relative_target(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root)
            base = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout, "github"),
                "issue": issue(),
                "runId": "pass-4",
                "brief": None,
            }
            relative = continuation_spec(root / "events")
            relative["eventsDir"] = "events"
            with self.assertRaises(DRIVER.DriverError):
                DRIVER.action_continue(dict(base, continuation=relative))
            oversized = continuation_spec(root / "events")
            with self.assertRaises(DRIVER.DriverError):
                DRIVER.action_continue(
                    dict(
                        base,
                        continuation=oversized,
                        brief={"pad": "x" * (DRIVER.MAX_CONTINUATION_EVENT_BYTES + 1)},
                    )
                )
            self.assertFalse((root / "events").exists())

    def test_issue_graph_digest_is_admitted_and_preserves_checkpoint_kind(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout, _ = initialize_repository(Path(temporary), remote=True)
            manifest = {
                "schemaVersion": 1,
                "name": "fixture",
                "repository": repository_config(checkout, "github"),
                "maxParallel": 1,
                "agent": {},
                "gates": [
                    {
                        "kind": "command",
                        "id": "tests",
                        "preflightArgv": ["true"],
                        "argv": ["true"],
                    }
                ],
                "tasks": [
                    {
                        "id": "build",
                        "kind": "implementation",
                        "issue": 8,
                        "dependencies": [],
                        "conflictDomains": [],
                    },
                    {
                        "id": "verify",
                        "kind": "checkpoint",
                        "issue": 9,
                        "dependencies": ["build"],
                        "argv": ["true"],
                        "runtimeMaxSec": 30,
                    },
                ],
            }
            _, references, normalized = DRIVER.forge_manifest(manifest)
            issues = [
                {
                    "number": 8,
                    "title": "Build",
                    "body": "Implement the admitted change.",
                    "state": "open",
                    "html_url": "https://github.com/acme/spec/issues/8",
                },
                {
                    "number": 9,
                    "title": "Verify",
                    "body": "Run the admitted automated barrier.",
                    "state": "open",
                    "html_url": "https://github.com/acme/spec/issues/9",
                },
            ]
            source = {
                "manifest": normalized,
                "tasks": [
                    {
                        "number": reference["issue"],
                        "title": issues[index]["title"],
                        "body": issues[index]["body"],
                    }
                    for index, reference in enumerate(references)
                ],
            }
            digest = DRIVER.canonical_sha256(source)
            master = {
                "number": 7,
                "state": "open",
                "html_url": "https://github.com/acme/spec/issues/7",
                "body": (
                    f"{DRIVER.CAMPAIGN_BEGIN}\n```json\n"
                    f"{json.dumps(manifest)}\n```\n{DRIVER.CAMPAIGN_END}\n"
                ),
            }
            brief = {
                "repository": "acme/spec",
                "issue": issue(),
                "worklist": {"kind": "github-issue", "graphDigest": digest},
            }
            with mock.patch.object(DRIVER, "github_json", side_effect=[master, issues]):
                worklist = DRIVER.issue_graph_worklist(brief)
            self.assertEqual([task["kind"] for task in worklist["tasks"]], ["implementation", "checkpoint"])
            self.assertEqual(worklist["tasks"][1]["argv"], ["true"])
            self.assertRegex(worklist["tasks"][0]["revision"], r"^sha256:[0-9a-f]{64}$")
            self.assertEqual(
                worklist["source"]["revision"],
                git(checkout, "rev-parse", "origin/main"),
            )

            changed = [dict(candidate) for candidate in issues]
            changed[0]["body"] = "Edited after arm."
            with mock.patch.object(DRIVER, "github_json", side_effect=[master, changed]):
                with self.assertRaisesRegex(DRIVER.DriverError, "explicitly re-arm"):
                    DRIVER.issue_graph_worklist(brief)

    def test_merged_pr_completion_is_bound_to_task_revision(self) -> None:
        revision = "sha256:" + "1" * 64
        task_value = {"id": "task-1", "kind": "implementation", "revision": revision}
        branch = DRIVER.stable_publish_branch("fixture", "7", "task-1", revision)
        candidate = {
            "html_url": "https://github.com/acme/spec/pull/8",
            "body": DRIVER.pull_request_marker("fixture", "7", "task-1", revision),
            "base": {"ref": "main"},
            "head": {"ref": branch, "sha": "c" * 40},
            "merge_commit_sha": "a" * 40,
            "merged_at": "2026-08-02T00:00:00Z",
            "state": "closed",
        }

        def listed(*_args: object, **_kwargs: object) -> subprocess.CompletedProcess[str]:
            return subprocess.CompletedProcess([], 0, json.dumps([candidate]), "")

        with mock.patch.object(DRIVER, "run", side_effect=listed):
            facts, _ = DRIVER.merged_github_tasks(
                "acme/spec", {}, "fixture", "7", "main", None, [task_value]
            )
            self.assertEqual(facts[0]["revision"], revision)
            task_value["revision"] = "sha256:" + "2" * 64
            stale, warnings = DRIVER.merged_github_tasks(
                "acme/spec", {}, "fixture", "7", "main", None, [task_value]
            )
            self.assertEqual(stale, [])
            self.assertTrue(any("pull/8" in warning for warning in warnings))

    def test_the_walk_binds_completion_to_the_admitted_task_revision(self) -> None:
        """A stale-revision pull request reached through the walk proves nothing.

        The walk narrows where candidates come from; it never widens what
        counts. A pull request linked to the task's own sub-issue, merged, and
        on the right base and head still fails to complete the task when its
        marker names a pre-edit revision.
        """
        revision = "sha256:" + "1" * 64
        task_value = {
            "id": "task-1",
            "kind": "implementation",
            "revision": revision,
            "brief": {
                "issue": {
                    "number": "8",
                    "url": "https://github.com/acme/spec/issues/8",
                }
            },
        }
        branch = DRIVER.stable_publish_branch("fixture", "7", "task-1", revision)
        walk = {
            8: {
                "number": 8,
                # A task pull request carries `Closes #<sub-issue>`, so the
                # forge state after a merge is closed, not open.
                "state": "closed",
                "url": "https://github.com/acme/spec/issues/8",
                "comments": [],
                "pullRequests": [
                    {
                        "url": "https://github.com/acme/spec/pull/8",
                        "body": DRIVER.pull_request_marker(
                            "fixture", "7", "task-1", revision
                        ),
                        "merged": True,
                        "baseRefName": "main",
                        "headRefName": branch,
                        "mergeCommit": {"oid": "a" * 40},
                    },
                    {
                        "url": "https://github.com/acme/spec/pull/9",
                        "body": DRIVER.pull_request_marker("fixture", "7", "task-1"),
                        "merged": False,
                        "baseRefName": "main",
                        "headRefName": branch,
                        "mergeCommit": {"oid": "b" * 40},
                    },
                ],
            }
        }
        facts, warnings = DRIVER.merged_github_tasks(
            "acme/spec", {}, "fixture", "7", "main", None, [task_value], walk
        )
        self.assertEqual([fact["taskId"] for fact in facts], ["task-1"])
        self.assertEqual(facts[0]["revision"], revision)
        self.assertEqual(warnings, [])

        # The graph was edited after that pull request merged: the task now
        # carries a different revision, so its stable branch and marker moved.
        stale_task = dict(task_value, revision="sha256:" + "2" * 64)
        stale, warnings = DRIVER.merged_github_tasks(
            "acme/spec", {}, "fixture", "7", "main", None, [stale_task], walk
        )
        self.assertEqual(stale, [])
        self.assertEqual(
            warnings,
            [
                "ignored https://github.com/acme/spec/pull/8: its campaign marker "
                "names no task in the witnessed worklist"
            ],
        )

    def test_issue_campaign_checkbox_repair_and_closeout_are_separate(self) -> None:
        tasks = [
            {
                "id": "first",
                "title": "First task",
                "brief": {
                    "issue": {
                        "number": "2",
                        "url": "https://github.com/acme/spec/issues/2",
                    }
                },
            },
            {
                "id": "second",
                "title": "Second task",
                "brief": {
                    "issue": {
                        "number": "3",
                        "url": "https://github.com/acme/spec/issues/3",
                    }
                },
            },
        ]
        body = (
            "operator prose\n"
            f"{DRIVER.WORKLIST_BEGIN}\n\nold projection\n\n"
            f"{DRIVER.WORKLIST_END}\n"
        )
        completed = subprocess.CompletedProcess([], 0, "", "")
        with mock.patch.object(DRIVER, "run", return_value=completed) as run:
            DRIVER.sync_issue_checkboxes("acme/spec", "1", body, tasks, {"first"})
        edited = run.call_args.kwargs["input_text"]
        self.assertIn("- [x] <!-- tally:campaign-task:v1 id=first -->", edited)
        self.assertIn("- [ ] <!-- tally:campaign-task:v1 id=second -->", edited)
        self.assertTrue(edited.startswith("operator prose\n"))

        with mock.patch.object(
            DRIVER,
            "github_json",
            side_effect=[{"state": "open"}, {"state": "closed"}],
        ), mock.patch.object(DRIVER, "run", return_value=completed) as run:
            DRIVER.close_completed_issue_campaign("acme/spec", "1", tasks)
        commands = [call.args[0] for call in run.call_args_list]
        self.assertIn(["gh", "issue", "close", "2", "--repo", "acme/spec"], commands)
        self.assertNotIn(["gh", "issue", "close", "3", "--repo", "acme/spec"], commands)
        self.assertIn(["gh", "issue", "close", "1", "--repo", "acme/spec"], commands)
        # Closing is closing. The summary comment is the closing summary's job,
        # published before this runs.
        self.assertFalse(
            any(command[:3] == ["gh", "issue", "comment"] for command in commands)
        )


class NativeSubIssueTests(unittest.TestCase):
    """The native read path: one walk, per-task threads, no written projection."""

    MANIFEST_TASKS = [
        {
            "id": "task-1",
            "kind": "implementation",
            "issue": 8,
            "dependencies": [],
            "conflictDomains": ["src/one"],
        },
        {
            "id": "task-2",
            "kind": "implementation",
            "issue": 9,
            "dependencies": [],
            "conflictDomains": ["src/two"],
        },
    ]

    def manifest(self, checkout: Path) -> dict[str, object]:
        return {
            "schemaVersion": 1,
            "name": "fixture",
            "repository": repository_config(checkout, "github"),
            "maxTasks": 2,
            "maxParallel": 2,
            "driverRuntimeMaxSec": 900,
            "runtimeMaxSec": 3600,
            "pool": "campaign",
            "agent": {
                "adapter": "codex",
                "argv": ["read the admitted brief"],
                "priority": "low",
                "runtimeMaxSec": 900,
                "approvalPolicy": "never",
                "sandboxPolicy": "danger-full-access",
            },
            "gates": [
                {
                    "kind": "command",
                    "id": "tests",
                    "preflightArgv": ["true"],
                    "argv": ["true"],
                    "runtimeMaxSec": 60,
                }
            ],
            "tasks": self.MANIFEST_TASKS,
        }

    def subissues(self) -> list[dict[str, object]]:
        return [
            {
                "number": number,
                "title": f"Task {index + 1}",
                "body": f"Implement task {index + 1}.",
                "state": "open",
                "html_url": f"https://github.com/acme/spec/issues/{number}",
                "updated_at": "2026-08-01T12:00:00Z",
            }
            for index, number in enumerate((8, 9))
        ]

    def fixture(self, checkout: Path) -> tuple[dict[str, object], dict[str, object]]:
        manifest = self.manifest(checkout)
        subissues = self.subissues()
        master_body = (
            f"{DRIVER.CAMPAIGN_BEGIN}\n```json\n"
            f"{json.dumps(manifest)}\n```\n{DRIVER.CAMPAIGN_END}\n\n"
            f"{DRIVER.WORKLIST_BEGIN}\n\n{DRIVER.WORKLIST_END}\n"
        )
        _, references, normalized = DRIVER.forge_manifest(manifest)
        digest = DRIVER.canonical_sha256(
            {
                "manifest": normalized,
                "tasks": [
                    {
                        "number": reference["issue"],
                        "title": subissues[index]["title"],
                        "body": subissues[index]["body"],
                    }
                    for index, reference in enumerate(references)
                ],
            }
        )
        state = {
            "actor": "tally-bot",
            "master": {
                "number": 7,
                "state": "open",
                "html_url": "https://github.com/acme/spec/issues/7",
                "body": master_body,
                "updated_at": "2026-08-01T12:00:00Z",
            },
            "subIssues": subissues,
            "walk": [self.walk_node(8), self.walk_node(9)],
            "comments": [],
            "issueComments": [],
            "calls": [],
        }
        brief = {
            "repository": "acme/spec",
            "issue": issue(),
            "worklist": {"kind": "github-issue", "graphDigest": digest},
            "capabilities": {"subIssueWalk": True},
        }
        return state, brief

    @staticmethod
    def walk_node(
        number: int,
        *,
        state: str = "OPEN",
        pulls: list[dict[str, object]] | None = None,
        comments: list[dict[str, object]] | None = None,
        truncated: bool = False,
        comments_truncated: bool = False,
    ) -> dict[str, object]:
        return {
            "number": number,
            "state": state,
            "url": f"https://github.com/acme/spec/issues/{number}",
            "closedByPullRequestsReferences": {
                "pageInfo": {"hasNextPage": truncated},
                "nodes": pulls or [],
            },
            # `last:` returns the newest comments, so an exhausted window drops
            # the oldest: `hasPreviousPage`, not `hasNextPage`.
            "comments": {
                "pageInfo": {"hasPreviousPage": comments_truncated},
                "nodes": comments or [],
            },
        }

    @staticmethod
    def merged_pull(url: str, body: str, branch: str, oid: str) -> dict[str, object]:
        return {
            "url": url,
            "body": body,
            "merged": True,
            "baseRefName": "main",
            "headRefName": branch,
            "mergeCommit": {"oid": oid},
        }

    @staticmethod
    def machine_comment(number: int, body: str) -> dict[str, object]:
        return {
            "url": f"https://github.com/acme/spec/issues/{number}#machine",
            "body": body,
            "author": {"login": "tally-bot"},
        }

    def test_a_hand_closed_sub_issue_is_an_anomaly_not_completion(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            state, brief = self.fixture(checkout)
            state["walk"] = [self.walk_node(8, state="CLOSED"), self.walk_node(9)]
            with FakeGitHub(root, state) as github:
                result = DRIVER.action_reconcile(brief)
            self.assertEqual(result["merged"], [])
            self.assertIn("task-1", result["remaining"])
            self.assertIn("task-1", [task["id"] for task in result["frontier"]])
            self.assertEqual(
                result["anomalies"],
                [
                    {
                        "kind": "closed-without-merged-proof",
                        "taskId": "task-1",
                        "issue": "8",
                        "url": "https://github.com/acme/spec/issues/8",
                        "detail": (
                            "sub-issue #8 is closed but task 'task-1' holds no "
                            "revision-valid merged pull request; closing a "
                            "sub-issue by hand does not complete a task"
                        ),
                    }
                ],
            )
            # Native mode writes no projection at all: no comment, and the
            # master's checkbox list is left exactly as authored.
            final = github.state()
            self.assertEqual(final["comments"], [])
            self.assertEqual(final["master"]["body"], state["master"]["body"])

    def test_the_campaigns_own_closure_is_a_warning_not_an_anomaly(self) -> None:
        """A graph edit must not turn every merged task into operator error.

        A task pull request carries `Closes #<sub-issue>`, so the campaign
        closes its own sub-issues as it merges. Editing one task brief and
        re-arming rotates every task's revision, so every already-merged task
        loses its proof while keeping a sub-issue the campaign closed. Those
        are already named in the ignored-marker warnings; reporting them as
        hand closures would fire the status verb's loudest surface once per
        merged task on the campaign's own documented workflow.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            state, brief = self.fixture(checkout)
            pre_edit = DRIVER.pull_request_marker(
                "fixture", "7", "task-1", "sha256:" + "1" * 64
            )
            state["walk"] = [
                self.walk_node(
                    8,
                    state="CLOSED",
                    pulls=[
                        self.merged_pull(
                            "https://github.com/acme/spec/pull/8",
                            pre_edit,
                            "tally/fixture-issue-7/task-1-1111111111111111",
                            "a" * 40,
                        )
                    ],
                ),
                # Task 2's sub-issue was closed by a human: no pull request of
                # this campaign's ever touched it.
                self.walk_node(9, state="CLOSED"),
            ]
            with FakeGitHub(root, state):
                result = DRIVER.action_reconcile(brief)
            self.assertEqual(result["merged"], [])
            self.assertEqual(sorted(result["remaining"]), ["task-1", "task-2"])
            self.assertEqual(
                [anomaly["taskId"] for anomaly in result["anomalies"]], ["task-2"]
            )
            self.assertIn(
                "ignored https://github.com/acme/spec/pull/8: its campaign marker "
                "names no task in the witnessed worklist",
                result["warnings"],
            )

    def test_a_truncated_reference_page_fails_the_pass(self) -> None:
        """`first:` returns the oldest references, so truncation drops the newest.

        Reading past that would let the walk narrow what counts as proof — the
        one thing the issue says it must never do — and the task would then be
        re-dispatched into a publish node that hits its own merged pull request.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            state, brief = self.fixture(checkout)
            state["walk"] = [self.walk_node(8, truncated=True), self.walk_node(9)]
            with FakeGitHub(root, state):
                with self.assertRaisesRegex(
                    DRIVER.DriverError, "truncated reference page"
                ):
                    DRIVER.action_reconcile(brief)

    def test_machine_receipts_follow_the_task_sub_issue_thread(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            state, brief = self.fixture(checkout)

            def diagnosis(task_id: str, attempt: int) -> str:
                marker = DRIVER.diagnosis_marker("fixture", "7", task_id, attempt)
                heading = DRIVER.diagnosis_heading(task_id, attempt)
                return f"{marker}\n\n{heading}\n\nsteering for {task_id}"

            state["walk"] = [
                self.walk_node(
                    8, comments=[self.machine_comment(8, diagnosis("task-1", 1))]
                ),
                self.walk_node(9),
            ]
            # A pre-#307 receipt for the same task still sits on the master.
            state["issueComments"] = [
                {
                    "body": diagnosis("task-1", 1),
                    "html_url": "https://github.com/acme/spec/issues/7#legacy",
                    "user": {"login": "tally-bot"},
                }
            ]
            with FakeGitHub(root, state):
                result = DRIVER.action_reconcile(brief)
            # The thread copy is the one the fact points at, and the master copy
            # of the same attempt is counted once, not twice.
            self.assertEqual(
                [(item["taskId"], item["comment"]) for item in result["diagnoses"]],
                [("task-1", "https://github.com/acme/spec/issues/8#machine")],
            )
            self.assertTrue(
                any(
                    "duplicate machine diagnosis for 'task-1' attempt 1 on the "
                    "master thread" in warning
                    for warning in result["warnings"]
                ),
                result["warnings"],
            )

    def test_a_master_receipt_recorded_before_the_thread_still_counts(self) -> None:
        """#334 item 6: an upgraded campaign keeps its diagnosis/retry ledger.

        A campaign armed before the sub-issue walk capability records its
        receipts on the master. Re-arming into the native projection moves
        where new receipts are posted; discarding the old ones reset each
        task's attempt counters, which bought one extra agent attempt and
        re-posted a public comment that had already been made.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            state, brief = self.fixture(checkout)

            def diagnosis(task_id: str, attempt: int) -> str:
                marker = DRIVER.diagnosis_marker("fixture", "7", task_id, attempt)
                heading = DRIVER.diagnosis_heading(task_id, attempt)
                return f"{marker}\n\n{heading}\n\nsteering for {task_id}"

            state["walk"] = [self.walk_node(8), self.walk_node(9)]
            state["issueComments"] = [
                {
                    "body": diagnosis("task-1", 1),
                    "html_url": "https://github.com/acme/spec/issues/7#legacy",
                    "user": {"login": "tally-bot"},
                }
            ]
            with FakeGitHub(root, state):
                result = DRIVER.action_reconcile(brief)
            self.assertEqual(
                [(item["taskId"], item["attempt"], item["comment"]) for item in result["diagnoses"]],
                [("task-1", 1, "https://github.com/acme/spec/issues/7#legacy")],
            )
            self.assertTrue(
                any(
                    "counted a master-thread machine diagnosis for 'task-1' "
                    "attempt 1" in warning
                    for warning in result["warnings"]
                ),
                result["warnings"],
            )

    def test_a_truncated_comment_window_is_reported_not_refused(self) -> None:
        """#334 item 1: `comments(last: 100)` silently dropped the oldest.

        These comments are machine-authored-filtered and feed the diagnosis and
        retry ledger, not steering -- the steering read is the CLI's own walk.
        So the warning has to name the consequence that actually follows here: a
        task's oldest receipts fall out of the ledger and its attempt budget can
        reset, which is #334 item 6's harm arriving through a second door. A
        long thread must not halt the campaign, so it is reported, not refused.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            state, brief = self.fixture(checkout)
            state["walk"] = [
                self.walk_node(8, comments_truncated=True),
                self.walk_node(9),
            ]
            with FakeGitHub(root, state):
                result = DRIVER.action_reconcile(brief)
            self.assertTrue(
                any(
                    "campaign sub-issue #8 carries more than 100 comments" in warning
                    and "receipt ledger" in warning
                    and "attempt budget" in warning
                    for warning in result["warnings"]
                ),
                result["warnings"],
            )
            # It must not send an operator looking for a lost steering comment:
            # steering does not come from this walk.
            self.assertTrue(
                all(
                    "steering read" not in warning for warning in result["warnings"]
                ),
                result["warnings"],
            )
            self.assertNotIn(
                True,
                [
                    "campaign sub-issue #9 carries more than" in warning
                    for warning in result["warnings"]
                ],
            )

    def test_steering_and_retry_receipts_post_on_the_task_thread(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            state = {"actor": "tally-bot", "comments": [], "calls": []}
            base = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout, "github"),
                "issue": issue(),
                "taskId": "task-1",
                "capabilities": {"subIssueWalk": True},
                "taskIssue": {
                    "number": "8",
                    "url": "https://github.com/acme/spec/issues/8",
                },
            }
            with FakeGitHub(root, state) as github:
                steered = DRIVER.action_steer(
                    {**base, "attempt": 1, "diagnosis": "Narrowed the failing gate."}
                )
                retried = DRIVER.action_retry(
                    {**base, "stage": "prep", "detail": "the lane vanished"}
                )
                # Reading the same thread back returns exactly what was posted,
                # so a retry brief sees its own task's steering history.
                replayed = DRIVER.action_steer(
                    {**base, "attempt": 1, "diagnosis": "Narrowed the failing gate."}
                )
            self.assertTrue(steered["posted"])
            self.assertTrue(retried["posted"])
            self.assertFalse(replayed["posted"])
            self.assertEqual(replayed["comment"], steered["comment"])
            final = github.state()
            self.assertEqual(
                [call[2] for call in final["calls"] if call[:2] == ["issue", "comment"]],
                ["8", "8"],
            )
            self.assertEqual(final.get("issueComments", []), [])
            self.assertEqual(len(final["threadComments"]["8"]), 2)

    def test_a_merging_pass_posts_no_progress_comment_in_native_mode(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            git(checkout, "switch", "--quiet", "-c", "feature")
            (checkout / "feature.txt").write_text("feature\n", encoding="utf-8")
            git(checkout, "add", "feature.txt")
            git(checkout, "commit", "--quiet", "-m", "feature")
            head = git(checkout, "rev-parse", "HEAD")
            git(checkout, "push", "--quiet", "origin", "feature")
            git(checkout, "switch", "--quiet", "main")
            git(checkout, "merge", "--quiet", "--no-ff", "--no-edit", "feature")
            merge_commit = git(checkout, "rev-parse", "HEAD")
            git(checkout, "push", "--quiet", "origin", "main")

            master_body = (
                f"{DRIVER.WORKLIST_BEGIN}\n"
                f"- [ ] {DRIVER.TASK_MARKER_PREFIX}task-1 --> #8 — Task 1\n"
                f"{DRIVER.WORKLIST_END}\n"
            )
            state = {
                "actor": "tally-bot",
                "master": {"number": 7, "state": "open", "body": master_body},
                "prView": {
                    "state": "MERGED",
                    "mergeCommit": {"oid": merge_commit},
                    "baseRefName": "main",
                    "headRefName": "feature",
                    "headRefOid": head,
                },
                "comments": [],
                "calls": [],
            }
            data = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "issue": issue(),
                "task": {
                    **task("task-1"),
                    "title": "Task 1",
                    "brief": {
                        "issue": {
                            "number": "8",
                            "url": "https://github.com/acme/spec/issues/8",
                        },
                        "body": "Implement task 1.",
                    },
                },
            }
            config = DRIVER.repo_config(repository_config(checkout, "github"))
            integration = {
                "pullRequest": "https://github.com/acme/spec/pull/1",
                "branch": "feature",
                "baseRev": git(checkout, "rev-parse", "origin/main"),
                "head": head,
            }
            with FakeGitHub(root, state) as github:
                DRIVER.merge_github(
                    data,
                    config,
                    integration,
                    {"subIssueWalk": True},
                    "merge",
                    DRIVER.template_narration(data["task"]),
                )
                native = github.state()
                self.assertEqual(native["comments"], [])
                self.assertIn("- [ ] ", native["master"]["body"])

                DRIVER.merge_github(
                    data,
                    config,
                    integration,
                    {"subIssueWalk": False},
                    "merge",
                    DRIVER.template_narration(data["task"]),
                )
                degraded = github.state()
                self.assertEqual(degraded["comments"], [])
                self.assertIn(
                    f"- [x] {DRIVER.TASK_MARKER_PREFIX}task-1 -->",
                    degraded["master"]["body"],
                )


class LaneLifecycleTests(unittest.TestCase):
    def test_a_lane_base_that_left_the_witnessed_worklist_history_fails_closed(
        self,
    ) -> None:
        """Worklist and worktrees must come from one history.

        The reconciler witnesses the worklist at a revision; prep fetches later
        and cuts the lane from whatever the remote base points at then. A
        rewound or force-replaced remote silently produced lanes from a history
        the witnessed worklist never described. Checkpoint lanes already
        refused exactly this; implementation lanes now do too, and the ordinary
        fast-forward case is unchanged.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            witnessed = git(checkout, "rev-parse", "HEAD")

            # The ordinary case: base moved forward from the witnessed
            # revision, so the lane descends from the worklist's history.
            (checkout / "moved-on.txt").write_text("main moved\n", encoding="utf-8")
            git(checkout, "add", "moved-on.txt")
            git(checkout, "commit", "--quiet", "-m", "main: independent change")
            git(checkout, "push", "--quiet", "origin", "main")
            prepared = DRIVER.action_prep(
                prep_brief(
                    checkout,
                    workspace_root,
                    "coherent-pass",
                    source_revision=witnessed,
                )
            )
            self.assertTrue(Path(prepared["worktreePath"]).is_dir())

            # The remote is force-replaced with an unrelated history. The
            # witnessed revision is no longer an ancestor of the base, so no
            # lane may be cut from it.
            replacement = root / "replacement"
            replacement.mkdir()
            command("git", "init", "--quiet", "--initial-branch=main", str(replacement))
            git(replacement, "config", "user.name", "Tally Test")
            git(replacement, "config", "user.email", "tally-test@invalid")
            (replacement / "other.go").write_text("other\n", encoding="utf-8")
            git(replacement, "add", "other.go")
            git(replacement, "commit", "--quiet", "-m", "unrelated root")
            git(replacement, "remote", "add", "origin", git(checkout, "remote", "get-url", "origin"))
            git(replacement, "push", "--quiet", "--force", "origin", "main")

            with self.assertRaises(DRIVER.DriverError) as raised:
                DRIVER.action_prep(
                    prep_brief(
                        checkout,
                        workspace_root,
                        "rewound-pass",
                        source_revision=witnessed,
                    )
                )
            self.assertIn(
                "does not descend from the witnessed worklist revision",
                str(raised.exception),
            )

    def test_the_resume_door_refuses_a_lane_the_fresh_cut_door_would_refuse(self) -> None:
        """A prep retry inside one flow run took the resume path and skipped the check.

        The already-prepared early return sat before the fetch and before the
        worklist/worktree coherence check, so a prep node re-run that straddled
        a remote force-replacement handed back the stale lane and its stale
        baseRev with no error at all: the resume door bypassed the fail-closed
        guard the fresh-cut door has.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            witnessed = git(checkout, "rev-parse", "HEAD")

            prepared = DRIVER.action_prep(
                prep_brief(
                    checkout,
                    workspace_root,
                    "resumed-pass",
                    source_revision=witnessed,
                )
            )
            self.assertTrue(Path(prepared["worktreePath"]).is_dir())
            # A second call inside the same run resumes the lane it just cut.
            resumed = DRIVER.action_prep(
                prep_brief(
                    checkout,
                    workspace_root,
                    "resumed-pass",
                    source_revision=witnessed,
                )
            )
            self.assertEqual(resumed, prepared)

            replacement = root / "replacement"
            replacement.mkdir()
            command("git", "init", "--quiet", "--initial-branch=main", str(replacement))
            git(replacement, "config", "user.name", "Tally Test")
            git(replacement, "config", "user.email", "tally-test@invalid")
            (replacement / "other.go").write_text("other\n", encoding="utf-8")
            git(replacement, "add", "other.go")
            git(replacement, "commit", "--quiet", "-m", "unrelated root")
            git(
                replacement,
                "remote",
                "add",
                "origin",
                git(checkout, "remote", "get-url", "origin"),
            )
            git(replacement, "push", "--quiet", "--force", "origin", "main")

            with self.assertRaises(DRIVER.DriverError) as raised:
                DRIVER.action_prep(
                    prep_brief(
                        checkout,
                        workspace_root,
                        "resumed-pass",
                        source_revision=witnessed,
                    )
                )
            self.assertIn(
                "does not descend from the witnessed worklist revision",
                str(raised.exception),
            )

    def test_a_prep_brief_without_a_source_revision_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            brief = prep_brief(checkout, root / "workspaces", "no-revision")
            del brief["sourceRevision"]
            with self.assertRaises(DRIVER.DriverError) as raised:
                DRIVER.action_prep(brief)
            self.assertIn("sourceRevision", str(raised.exception))

            malformed = prep_brief(
                checkout, root / "workspaces", "bad-revision", source_revision="main"
            )
            with self.assertRaises(DRIVER.DriverError) as raised:
                DRIVER.action_prep(malformed)
            self.assertIn("full Git object ID", str(raised.exception))

    def test_a_killed_lane_resumes_the_same_branch_and_worktree(self) -> None:
        """The resume invariant both managers promised, now proven once.

        A lane killed mid-task keeps its branch and its committed work, and the
        next prep in the same pass adopts it rather than refusing it or
        starting a second one. That holds whether the worktree survived the
        kill or only its branch did.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            brief = prep_brief(checkout, workspace_root, "killed-pass")

            prepared = DRIVER.action_prep(brief)
            worktree = Path(prepared["worktreePath"])
            (worktree / "lane-work.txt").write_text("in flight\n", encoding="utf-8")
            git(worktree, "add", "lane-work.txt")
            git(worktree, "commit", "--quiet", "-m", "task-1: in flight")
            in_flight = git(worktree, "rev-parse", "HEAD")

            # The lane is still there: resume adopts it, work and all.
            resumed = DRIVER.action_prep(brief)
            self.assertEqual(resumed, prepared)
            self.assertEqual(git(worktree, "rev-parse", "HEAD"), in_flight)

            # The runner died hard enough to lose the directory, and the base
            # branch moved on meanwhile. The branch is the lane's durable half,
            # so the rebuilt lane is the same lane -- and its prepared base is
            # where *its own history* forks from base, never wherever base
            # happens to point now. A base that is not an ancestor of the lane
            # head makes ownership fail and feeds the diagnosing agent a patch
            # that reverses commits the task never touched.
            command("git", "-C", str(checkout), "worktree", "remove", "--force", str(worktree))
            self.assertFalse(worktree.exists())
            (checkout / "moved-on.txt").write_text("main moved\n", encoding="utf-8")
            git(checkout, "add", "moved-on.txt")
            git(checkout, "commit", "--quiet", "-m", "main: independent change")
            git(checkout, "push", "--quiet", "origin", "main")

            rebuilt = DRIVER.action_prep(brief)
            self.assertEqual(rebuilt["branch"], prepared["branch"])
            self.assertEqual(rebuilt["worktreePath"], prepared["worktreePath"])
            self.assertEqual(git(worktree, "rev-parse", "HEAD"), in_flight)
            self.assertEqual(WORKTREES.read_identity(worktree)["runid"], "killed-pass")
            self.assertEqual(
                command(
                    "git",
                    "-C",
                    str(worktree),
                    "merge-base",
                    "--is-ancestor",
                    rebuilt["baseRev"],
                    in_flight,
                    check=False,
                ).returncode,
                0,
                "the adopted lane's base must be an ancestor of its own head",
            )
            self.assertEqual(rebuilt["baseRev"], prepared["baseRev"])
            self.assertEqual(
                command(
                    "git",
                    "-C",
                    str(worktree),
                    "diff",
                    "--name-only",
                    f"{rebuilt['baseRev']}..{in_flight}",
                ).stdout.split(),
                ["lane-work.txt"],
                "an adopted lane must not diff as though it deleted base's own files",
            )
            self.assertEqual(
                WORKTREES.read_identity(worktree)["baserev"], rebuilt["baseRev"]
            )

            # The other way a lane directory disappears: removed underneath
            # git, so the lane is still registered and has to be pruned first.
            shutil.rmtree(worktree)
            pruned = DRIVER.action_prep(brief)
            self.assertEqual(pruned, rebuilt)
            self.assertEqual(git(worktree, "rev-parse", "HEAD"), in_flight)

    def test_a_lane_that_lost_its_identity_is_healed_not_cemented(self) -> None:
        """A lane whose identity write was interrupted must still resume.

        Identity is written in one atomic act now, so this state can only be
        reached by upgrading a tally across #312 over a live lane -- but that
        is exactly the upgrade path, and the lane must recover rather than
        acquire a complete-looking identity it can never answer `baserev` for.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            brief = prep_brief(checkout, workspace_root, "crash-pass")

            prepared = DRIVER.action_prep(brief)
            worktree = Path(prepared["worktreePath"])
            (worktree / "lane-work.txt").write_text("in flight\n", encoding="utf-8")
            git(worktree, "add", "lane-work.txt")
            git(worktree, "commit", "--quiet", "-m", "task-1: in flight")
            in_flight = git(worktree, "rev-parse", "HEAD")

            # The pre-#312 lane: registered by git, carrying no tally identity.
            command(
                "git",
                "-C",
                str(worktree),
                "config",
                "--worktree",
                "--remove-section",
                "tally",
            )
            self.assertEqual(WORKTREES.read_identity(worktree), {})

            healed = DRIVER.action_prep(brief)
            self.assertEqual(healed["branch"], prepared["branch"])
            self.assertEqual(healed["worktreePath"], prepared["worktreePath"])
            self.assertEqual(healed["baseRev"], prepared["baseRev"])
            self.assertEqual(git(worktree, "rev-parse", "HEAD"), in_flight)
            recorded = WORKTREES.read_identity(worktree)
            self.assertEqual(recorded["baserev"], prepared["baseRev"])
            self.assertEqual(recorded["runid"], "crash-pass")
            # And the healed lane resumes normally from here on.
            self.assertEqual(DRIVER.action_prep(brief), healed)

    def test_lane_identity_is_written_in_one_atomic_act(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            prepared = DRIVER.action_prep(
                prep_brief(checkout, workspace_root, "atomic-pass")
            )
            worktree = Path(prepared["worktreePath"])
            config_path = WORKTREES.worktree_config_path(worktree)
            self.assertTrue(config_path.is_file())

            # A per-worktree key this driver does not own survives a rewrite,
            # and the rewrite replaces the whole tally section rather than
            # accumulating stale keys.
            command(
                "git", "-C", str(worktree), "config", "--worktree", "other.key", "keep me"
            )
            WORKTREES.write_identity(worktree, {"campaign": "fixture", "taskid": "task-1"})
            self.assertEqual(
                WORKTREES.read_identity(worktree),
                {"campaign": "fixture", "taskid": "task-1"},
            )
            self.assertEqual(
                command(
                    "git", "-C", str(worktree), "config", "--worktree", "--get", "other.key"
                ).stdout.strip(),
                "keep me",
            )
            # Lane identity is never recorded on the main worktree, where
            # `git config --worktree` means the shared config instead.
            with self.assertRaises(DRIVER.worktrees.WorktreeError) as raised:
                WORKTREES.write_identity(checkout, {"campaign": "fixture"})
            self.assertIn("main worktree", str(raised.exception))

    def test_a_foreign_lane_at_the_same_path_is_a_conflict(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            prepared = DRIVER.action_prep(
                prep_brief(checkout, workspace_root, "first-pass")
            )
            worktree = Path(prepared["worktreePath"])
            # Another campaign's identity on this campaign's path is not
            # something to clobber.
            WORKTREES.write_identity(worktree, {"campaign": "other"})
            with self.assertRaises(DRIVER.DriverError) as raised:
                DRIVER.action_prep(prep_brief(checkout, workspace_root, "first-pass"))
            self.assertIn("different lane identity", str(raised.exception))
            self.assertIn("campaign", str(raised.exception))

    def test_local_forge_closing_summary_is_a_durable_blob(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            config = DRIVER.repo_config(repository_config(checkout))
            reconciliation = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "source": {"sha256": "sha256:" + "a" * 64, "revision": "b" * 40},
                "baseRevision": "b" * 40,
                "tasks": [
                    {"id": "task-1", "title": "Task 1"},
                    {"id": "task-2", "title": "Task 2"},
                ],
                "merged": [
                    {
                        "taskId": "task-1",
                        "pullRequest": "local://acme/spec/task-1",
                        "mergeCommit": "c" * 40,
                    }
                ],
                "checkpoints": [],
                "remaining": ["task-2"],
                "diagnoses": [
                    {"taskId": "task-2", "attempt": 1, "diagnosis": "the gate stayed red"},
                    {"taskId": "task-2", "attempt": 2, "diagnosis": "still red"},
                ],
                "retries": [],
                "deferrals": [],
                "blocked": [{"taskId": "task-2", "blockedBy": ["task-2"]}],
                "anomalies": [],
                "warnings": [],
            }
            digest = DRIVER.campaign_digest(reconciliation, "quiescent")
            self.assertEqual(digest["blocked"][0]["attempts"], 2)
            self.assertEqual(digest["outstanding"], [])
            receipt = DRIVER.publish_closing_summary(
                "acme/spec", config, "fixture", "7", digest
            )
            self.assertTrue(receipt.endswith("/summary/quiescent"))
            ref = receipt.split("acme/spec/", 1)[1]
            stored = DRIVER.read_local_blob(config, ref)
            self.assertEqual(stored["kind"], "closing-summary")
            self.assertIn("Campaign closed at frontier quiescence", stored["body"])
            self.assertIn("1 of 2 task(s)", stored["body"])
            self.assertIn("local://acme/spec/task-1", stored["body"])
            self.assertIn("the gate stayed red", stored["body"])
            # A second terminal pass writes no second summary.
            self.assertEqual(
                DRIVER.publish_closing_summary("acme/spec", config, "fixture", "7", digest),
                receipt,
            )
            self.assertEqual(DRIVER.read_local_blob(config, ref), stored)

    def test_conflicting_published_head_is_aborted_abandoned_and_rebuilt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            stable_branch = "tally/fixture-issue-7/task-1"

            git(checkout, "switch", "--quiet", "-c", "published")
            (checkout / "root.go").write_text("task\n", encoding="utf-8")
            git(checkout, "commit", "--quiet", "-am", "task change")
            published_head = git(checkout, "rev-parse", "HEAD")
            git(checkout, "push", "--quiet", "origin", f"HEAD:refs/heads/{stable_branch}")
            git(checkout, "switch", "--quiet", "main")
            (checkout / "root.go").write_text("main\n", encoding="utf-8")
            git(checkout, "commit", "--quiet", "-am", "base change")
            current_main = git(checkout, "rev-parse", "HEAD")
            git(checkout, "push", "--quiet", "origin", "main")

            old_brief = prep_brief(checkout, workspace_root, "old-pass")
            old_brief["task"]["conflictDomains"] = ["root.go"]
            prepared = DRIVER.action_prep(old_brief)
            self.assertEqual(
                git(Path(prepared["worktreePath"]), "rev-parse", "HEAD"),
                published_head,
            )
            rebase_brief = {
                # The rebase brief is built independently of the prep brief in
                # the flow; it does not carry the prep-only sourceRevision.
                **{key: value for key, value in old_brief.items() if key != "sourceRevision"},
                "domainsRequired": True,
                "workspace": prepared,
                "publication": {
                    "taskId": "task-1",
                    "branch": stable_branch,
                    "head": published_head,
                    "pullRequest": "local://acme/spec/task-1",
                    "narration": {
                        "source": "template",
                        "subject": "task-1: Task 1",
                        "body": "",
                    },
                    "ownership": {
                        "taskId": "task-1",
                        "domainsRequired": True,
                        "conflictDomains": ["root.go"],
                        "ownedPaths": ["root.go"],
                        "baseRev": prepared["baseRev"],
                        "head": published_head,
                    },
                },
                "constraints": [],
            }
            with self.assertRaises(DRIVER.DriverError) as raised:
                DRIVER.action_rebase(rebase_brief)
            self.assertIn("was abandoned", str(raised.exception))
            self.assertIn(published_head, str(raised.exception))
            rebase_head = git(
                Path(prepared["worktreePath"]),
                "rev-parse",
                "--verify",
                "REBASE_HEAD",
                check=False,
            )
            self.assertEqual(rebase_head, "")
            self.assertNotEqual(
                command(
                    "git",
                    "-C",
                    str(checkout),
                    "ls-remote",
                    "--exit-code",
                    "origin",
                    stable_branch,
                    check=False,
                ).returncode,
                0,
            )
            DRIVER.action_cleanup(
                {
                    "campaign": "fixture",
                    "repository": "acme/spec",
                    "repositoryConfig": repository_config(checkout),
                    "runId": "old-pass",
                    "taskId": "task-1",
                    "workspaceRoot": str(workspace_root),
                    "workspace": prepared,
                }
            )

            new_brief = prep_brief(checkout, workspace_root, "new-pass")
            rebuilt = DRIVER.action_prep(new_brief)
            self.assertEqual(rebuilt["baseRev"], current_main)
            self.assertEqual(git(Path(rebuilt["worktreePath"]), "rev-parse", "HEAD"), current_main)
            DRIVER.action_cleanup(
                {
                    "campaign": "fixture",
                    "repository": "acme/spec",
                    "repositoryConfig": repository_config(checkout),
                    "runId": "new-pass",
                    "taskId": "task-1",
                    "workspaceRoot": str(workspace_root),
                }
            )
            self.assertFalse(Path(rebuilt["worktreePath"]).exists())

    def test_post_rebase_domain_failure_abandons_and_names_the_published_head(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            stable_branch = "tally/fixture-issue-7/task-1"

            git(checkout, "switch", "--quiet", "-c", "published")
            (checkout / "root.go").write_text("task\n", encoding="utf-8")
            git(checkout, "commit", "--quiet", "-am", "task change")
            published_head = git(checkout, "rev-parse", "HEAD")
            git(checkout, "push", "--quiet", "origin", f"HEAD:refs/heads/{stable_branch}")
            git(checkout, "switch", "--quiet", "main")
            (checkout / "main-only.txt").write_text("main\n", encoding="utf-8")
            git(checkout, "add", "main-only.txt")
            git(checkout, "commit", "--quiet", "-m", "independent base change")
            git(checkout, "push", "--quiet", "origin", "main")

            rebase_task = task("task-1")
            rebase_task["conflictDomains"] = ["owned-only.txt"]
            old_brief = {
                **prep_brief(checkout, workspace_root, "old-pass"),
                "task": rebase_task,
            }
            prepared = DRIVER.action_prep(old_brief)
            with self.assertRaises(DRIVER.DriverError) as raised:
                DRIVER.action_rebase(
                    {
                        **{
                            key: value
                            for key, value in old_brief.items()
                            if key != "sourceRevision"
                        },
                        "domainsRequired": True,
                        "workspace": prepared,
                        "publication": {
                            "taskId": "task-1",
                            "branch": stable_branch,
                            "head": published_head,
                            "pullRequest": "local://acme/spec/task-1",
                            "narration": {
                                "source": "template",
                                "subject": "task-1: Task 1",
                                "body": "",
                            },
                            "ownership": {
                                "taskId": "task-1",
                                "domainsRequired": True,
                                "conflictDomains": ["owned-only.txt"],
                                "ownedPaths": ["owned-only.txt"],
                                "baseRev": prepared["baseRev"],
                                "head": published_head,
                            },
                        },
                        "constraints": [],
                    }
                )
            message = str(raised.exception)
            self.assertIn("failed integration policy", message)
            self.assertIn(published_head, message)
            self.assertIn("was abandoned", message)
            self.assertNotEqual(
                command(
                    "git",
                    "-C",
                    str(checkout),
                    "ls-remote",
                    "--exit-code",
                    "origin",
                    stable_branch,
                    check=False,
                ).returncode,
                0,
            )

    def test_next_pass_sweeps_old_worktree_and_its_branch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            dead_flow = "00000000-0000-4000-8000-000000000911"
            live_flow = "00000000-0000-4000-8000-000000000912"
            with FakeTally(root, dead_flow) as tally:
                DRIVER.action_sweep(
                    sweep_brief(checkout, workspace_root, "dead-pass", tally.program)
                )
                prepared = DRIVER.action_prep(
                    prep_brief(checkout, workspace_root, "dead-pass")
                )
                worktree = Path(prepared["worktreePath"])
                self.assertTrue(worktree.is_dir())
                # Lane identity lives in git's own per-worktree configuration,
                # and nothing else: no bespoke marker file is written.
                self.assertEqual(
                    WORKTREES.read_identity(worktree),
                    {
                        "driver": "spec-build",
                        "campaign": "fixture",
                        "repository": "acme/spec",
                        "runid": "dead-pass",
                        "taskid": "task-1",
                        "taskkind": "implementation",
                        "branch": prepared["branch"],
                        "publishbranch": prepared["publishBranch"],
                        "baserev": prepared["baseRev"],
                    },
                )
                self.assertEqual(
                    sorted(
                        path
                        for path in (workspace_root / ".state").glob("*/*.json")
                        if path.parent.name != "passes"
                    ),
                    [],
                )
                # The enumeration round-trips: the lane git lists is the lane
                # whose identity the driver wrote.
                enumerated = [
                    lane
                    for lane in WORKTREES.lanes(checkout)
                    if lane["identity"].get("taskid") == "task-1"
                ]
                self.assertEqual(len(enumerated), 1)
                self.assertEqual(enumerated[0]["worktree"].resolve(), worktree.resolve())
                self.assertEqual(enumerated[0]["branch"], prepared["branch"])
                tally.update(currentFlowRunId=live_flow, flows={dead_flow: []})
                swept = DRIVER.action_sweep(
                    sweep_brief(checkout, workspace_root, "live-pass", tally.program)
                )
            self.assertFalse(worktree.exists())
            self.assertNotEqual(
                command(
                    "git",
                    "-C",
                    str(checkout),
                    "show-ref",
                    "--verify",
                    "--quiet",
                    f"refs/heads/{prepared['branch']}",
                    check=False,
                ).returncode,
                0,
            )
            self.assertFalse(
                DRIVER.pass_record_path(
                    workspace_root,
                    hashlib.sha256(b"dead-pass").hexdigest()[:12],
                ).exists()
            )
            self.assertTrue(
                DRIVER.pass_record_path(
                    workspace_root,
                    hashlib.sha256(b"live-pass").hexdigest()[:12],
                ).is_file()
            )
            self.assertTrue(any(item.startswith("worktree:") for item in swept["cleaned"]))
            self.assertEqual(swept["liveRuns"], [])
            self.assertEqual(swept["warnings"], [])

    def test_next_pass_sweeps_a_lane_git_never_registered(self) -> None:
        """A directory git never adopted still belongs to a proven-dead run.

        Its authority to be deleted is the campaign's own lane layout --
        `<repositoryRoot>/<runHash>/<lane>` -- which is derived, not stored, so
        removing the marker files removed nothing the sweep needed.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            run_id = "dead-unregistered-pass"
            run_hash = hashlib.sha256(run_id.encode()).hexdigest()[:12]
            dead_flow = "00000000-0000-4000-8000-000000000913"
            live_flow = "00000000-0000-4000-8000-000000000914"
            with FakeTally(root, dead_flow) as tally:
                DRIVER.action_sweep(
                    sweep_brief(checkout, workspace_root, run_id, tally.program)
                )
                worktree = workspace_root / "spec" / run_hash / "task-1"
                worktree.mkdir(parents=True)
                (worktree / "uncommitted.txt").write_text("stale\n", encoding="utf-8")
                branch = f"tally-work/fixture-{run_hash}/task-1"
                git(checkout, "branch", branch)
                tally.update(currentFlowRunId=live_flow, flows={dead_flow: []})
                swept = DRIVER.action_sweep(
                    sweep_brief(checkout, workspace_root, "live-pass", tally.program)
                )
            self.assertFalse(worktree.exists())
            self.assertNotEqual(
                command(
                    "git",
                    "-C",
                    str(checkout),
                    "show-ref",
                    "--verify",
                    "--quiet",
                    f"refs/heads/{branch}",
                    check=False,
                ).returncode,
                0,
            )
            self.assertIn(f"worktree:{worktree}", swept["cleaned"])
            self.assertEqual(swept["liveRuns"], [])
            self.assertEqual(swept["warnings"], [])

    def test_sweep_reclaims_lane_markers_left_by_a_pre_upgrade_tally(self) -> None:
        """Nothing writes these any more, so the sweep is the only thing that can.

        An estate that upgrades across #312 keeps whatever its last pre-upgrade
        pass left under `.state/<runHash>/`. Left alone that is a directory
        tree nobody will ever explain, so the sweep reclaims it on exactly the
        authority it already established for the run: same campaign, same
        repository, a run hash that is neither this pass's nor protected.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            dead_flow = "00000000-0000-4000-8000-000000000940"
            live_flow = "00000000-0000-4000-8000-000000000941"
            state_root = workspace_root / ".state"
            legacy = state_root / hashlib.sha256(b"dead-pass").hexdigest()[:16] / "task-9.json"
            legacy.parent.mkdir(parents=True)
            legacy.write_text(
                json.dumps(
                    {
                        "campaign": "fixture",
                        "repository": "acme/spec",
                        "runId": "dead-pass",
                        "taskId": "task-9",
                        "branch": "tally-work/fixture-abcdefabcdef/task-9",
                        "worktreePath": str(workspace_root / "spec/abcdefabcdef/task-9"),
                    }
                ),
                encoding="utf-8",
            )
            foreign = state_root / "0123456789abcdef" / "task-1.json"
            foreign.parent.mkdir(parents=True)
            foreign.write_text(
                json.dumps(
                    {
                        "campaign": "other-campaign",
                        "repository": "acme/spec",
                        "runId": "other-pass",
                        "taskId": "task-1",
                    }
                ),
                encoding="utf-8",
            )
            with FakeTally(root, dead_flow) as tally:
                DRIVER.action_sweep(
                    sweep_brief(checkout, workspace_root, "dead-pass", tally.program)
                )
                # This pass owns `dead-pass`, so its own marker is untouched.
                self.assertTrue(legacy.is_file())
                tally.update(currentFlowRunId=live_flow, flows={dead_flow: []})
                swept = DRIVER.action_sweep(
                    sweep_brief(checkout, workspace_root, "live-pass", tally.program)
                )
            self.assertFalse(legacy.exists())
            self.assertFalse(legacy.parent.exists())
            self.assertIn(f"marker:{legacy}", swept["cleaned"])
            self.assertEqual(swept["warnings"], [])
            # Another campaign's marker belongs to that campaign's sweep.
            self.assertTrue(foreign.is_file())

    def test_sweep_defers_and_preserves_every_lane_while_an_old_flow_job_is_live(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            dead_flow = "00000000-0000-4000-8000-000000000915"
            waiting_flow = "00000000-0000-4000-8000-000000000916"
            settled_flow = "00000000-0000-4000-8000-000000000917"
            with FakeTally(root, dead_flow) as tally:
                DRIVER.action_sweep(
                    sweep_brief(checkout, workspace_root, "dead-pass", tally.program)
                )
                prepared = DRIVER.action_prep(
                    prep_brief(checkout, workspace_root, "dead-pass")
                )
                worktree = Path(prepared["worktreePath"])
                old_job = {
                    "anchor": "00000000-0000-4000-8000-000000000918",
                    "liveState": "running",
                    "taskRef": "fixture/task-1",
                    "orchestration": {"flowRunId": dead_flow},
                }
                tally.update(
                    currentFlowRunId=waiting_flow,
                    flows={dead_flow: [old_job]},
                )
                deferred = DRIVER.action_sweep(
                    sweep_brief(checkout, workspace_root, "waiting-pass", tally.program)
                )
                self.assertTrue(worktree.is_dir())
                self.assertEqual(
                    deferred["liveRuns"],
                    [
                        {
                            "runHash": hashlib.sha256(b"dead-pass").hexdigest()[:12],
                            "flowRunId": dead_flow,
                            "jobs": [
                                {
                                    "anchor": old_job["anchor"],
                                    "liveState": "running",
                                    "taskRef": "fixture/task-1",
                                }
                            ],
                        }
                    ],
                )
                self.assertTrue(any("left live campaign run" in item for item in deferred["warnings"]))

                tally.update(
                    currentFlowRunId=settled_flow,
                    flows={dead_flow: [], waiting_flow: []},
                )
                swept = DRIVER.action_sweep(
                    sweep_brief(checkout, workspace_root, "settled-pass", tally.program)
                )
                self.assertEqual(swept["liveRuns"], [])
                self.assertFalse(worktree.exists())

    def test_next_pass_sweeps_a_preflight_lane_left_by_a_killed_runner(self) -> None:
        """The one preflight residue an operator can actually observe.

        A pass cleans its own preflight lane unconditionally, red or green,
        before it returns or throws. Only a runner killed while preflight is
        still running leaves the `_campaign-preflight` worktree and its branch
        behind. Nothing has to be removed by hand: the sweep recognises that
        lane name, and once the dead pass is proven to have no live child it
        reclaims both. Until then it defers rather than racing the job.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            dead_flow = "00000000-0000-4000-8000-000000000921"
            waiting_flow = "00000000-0000-4000-8000-000000000922"
            live_flow = "00000000-0000-4000-8000-000000000923"
            with FakeTally(root, dead_flow) as tally:
                DRIVER.action_sweep(
                    sweep_brief(checkout, workspace_root, "killed-pass", tally.program)
                )
                prepared = DRIVER.action_preflight(
                    preflight_brief(checkout, workspace_root, "killed-pass")
                )
                worktree = Path(prepared["worktreePath"])
                branch = prepared["branch"]
                self.assertEqual(prepared["taskId"], "campaign-preflight")
                self.assertEqual(worktree.name, "_campaign-preflight")
                self.assertTrue(worktree.is_dir())
                self.assertTrue(str(branch).endswith("/_campaign-preflight"))
                self.assertEqual(
                    command(
                        "git",
                        "-C",
                        str(checkout),
                        "show-ref",
                        "--verify",
                        "--quiet",
                        f"refs/heads/{branch}",
                        check=False,
                    ).returncode,
                    0,
                )

                # The runner is killed here, so no preflight cleanup node ever
                # runs. A still-live preflight job protects the whole namespace.
                held_job = {
                    "anchor": "00000000-0000-4000-8000-000000000924",
                    "liveState": "running",
                    "taskRef": "fixture/task-1",
                    "orchestration": {"flowRunId": dead_flow},
                }
                tally.update(
                    currentFlowRunId=waiting_flow,
                    flows={dead_flow: [held_job]},
                )
                deferred = DRIVER.action_sweep(
                    sweep_brief(checkout, workspace_root, "waiting-pass", tally.program)
                )
                self.assertEqual(deferred["cleaned"], [])
                self.assertTrue(worktree.is_dir())
                self.assertEqual(
                    [run["flowRunId"] for run in deferred["liveRuns"]],
                    [dead_flow],
                )

                tally.update(
                    currentFlowRunId=live_flow,
                    flows={dead_flow: [], waiting_flow: []},
                )
                swept = DRIVER.action_sweep(
                    sweep_brief(checkout, workspace_root, "recovery-pass", tally.program)
                )
            self.assertFalse(worktree.exists())
            self.assertNotEqual(
                command(
                    "git",
                    "-C",
                    str(checkout),
                    "show-ref",
                    "--verify",
                    "--quiet",
                    f"refs/heads/{branch}",
                    check=False,
                ).returncode,
                0,
            )
            self.assertTrue(any(item.startswith("worktree:") for item in swept["cleaned"]))
            self.assertEqual(swept["liveRuns"], [])
            self.assertEqual(swept["warnings"], [])

    def test_sweep_liveness_survives_an_issue_campaign_rename(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            old_flow = "00000000-0000-4000-8000-000000000930"
            new_flow = "00000000-0000-4000-8000-000000000931"
            identity = "00000000-0000-4000-8000-000000000932"
            with FakeTally(root, old_flow) as tally:
                DRIVER.action_sweep(
                    sweep_brief(
                        checkout,
                        workspace_root,
                        "old-pass",
                        tally.program,
                        campaign="old-name",
                        campaign_identity=identity,
                    )
                )
                old_prep = prep_brief(checkout, workspace_root, "old-pass")
                old_prep["campaign"] = "old-name"
                prepared = DRIVER.action_prep(old_prep)
                worktree = Path(prepared["worktreePath"])
                tally.update(
                    currentFlowRunId=new_flow,
                    flows={
                        old_flow: [
                            {
                                "anchor": "00000000-0000-4000-8000-000000000933",
                                "liveState": "running",
                                "taskRef": f"{identity}/task-1",
                                "orchestration": {"flowRunId": old_flow},
                            }
                        ]
                    },
                )
                deferred = DRIVER.action_sweep(
                    sweep_brief(
                        checkout,
                        workspace_root,
                        "new-pass",
                        tally.program,
                        campaign="new-name",
                        campaign_identity=identity,
                    )
                )
                self.assertTrue(worktree.is_dir())
                self.assertEqual(deferred["blockingJobs"][0]["flowRunId"], old_flow)
                self.assertEqual(deferred["blockingJobs"][0]["taskRef"], f"{identity}/task-1")

    def test_sweep_leaves_legacy_lane_without_daemon_liveness_proof_untouched(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            prepared = DRIVER.action_prep(
                prep_brief(checkout, workspace_root, "legacy-pass")
            )
            worktree = Path(prepared["worktreePath"])
            with FakeTally(
                root,
                "00000000-0000-4000-8000-000000000919",
            ) as tally:
                tally.update(
                    liveJobs=[
                        {
                            "anchor": "00000000-0000-4000-8000-000000000922",
                            "liveState": "running",
                            "taskRef": "fixture/task-1",
                            "orchestration": {
                                "flowRunId": "00000000-0000-4000-8000-000000000923"
                            },
                        }
                    ]
                )
                swept = DRIVER.action_sweep(
                    sweep_brief(checkout, workspace_root, "new-pass", tally.program)
                )
            self.assertTrue(worktree.is_dir())
            self.assertEqual(swept["liveRuns"], [])
            self.assertEqual(len(swept["blockingJobs"]), 1)
            self.assertTrue(
                any("no daemon liveness record exists" in item for item in swept["warnings"])
            )

    def test_sweep_query_failure_is_fail_closed_before_lane_removal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            dead_flow = "00000000-0000-4000-8000-000000000920"
            with FakeTally(root, dead_flow) as tally:
                DRIVER.action_sweep(
                    sweep_brief(checkout, workspace_root, "dead-pass", tally.program)
                )
                prepared = DRIVER.action_prep(
                    prep_brief(checkout, workspace_root, "dead-pass")
                )
                worktree = Path(prepared["worktreePath"])
                tally.update(
                    currentFlowRunId="00000000-0000-4000-8000-000000000921",
                    failQueries=True,
                )
                with self.assertRaisesRegex(DRIVER.DriverError, "injected tally query failure"):
                    DRIVER.action_sweep(
                        sweep_brief(checkout, workspace_root, "new-pass", tally.program)
                    )
                self.assertTrue(worktree.is_dir())

    def test_pass_exit_cleans_partial_prep_lanes_without_workspace_results(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            run_id = "partial-prep-pass"
            run_hash = hashlib.sha256(run_id.encode()).hexdigest()[:12]
            run_root = workspace_root / "spec" / run_hash

            detached = run_root / "task-1"
            detached.parent.mkdir(parents=True)
            git(checkout, "worktree", "add", "--detach", str(detached), "HEAD")
            DRIVER.action_cleanup(
                {
                    "campaign": "fixture",
                    "repository": "acme/spec",
                    "repositoryConfig": repository_config(checkout),
                    "runId": run_id,
                    "taskId": "task-1",
                    "workspaceRoot": str(workspace_root),
                }
            )
            self.assertFalse(detached.exists())

            partial = run_root / "task-2"
            partial.mkdir(parents=True)
            (partial / "partial.txt").write_text("stale\n", encoding="utf-8")
            partial_branch = f"tally-work/fixture-{run_hash}/task-2"
            git(checkout, "branch", partial_branch)
            DRIVER.action_cleanup(
                {
                    "campaign": "fixture",
                    "repository": "acme/spec",
                    "repositoryConfig": repository_config(checkout),
                    "runId": run_id,
                    "taskId": "task-2",
                    "workspaceRoot": str(workspace_root),
                }
            )
            self.assertFalse(partial.exists())
            self.assertNotEqual(
                command(
                    "git",
                    "-C",
                    str(checkout),
                    "show-ref",
                    "--verify",
                    "--quiet",
                    f"refs/heads/{partial_branch}",
                    check=False,
                ).returncode,
                0,
            )


class NarrationValidatorTests(unittest.TestCase):
    """The narrate slot: model proposes, deterministic validator enforces."""

    def test_a_conventional_proposal_is_composed_into_a_header(self) -> None:
        narration, reason = DRIVER.validated_narration(
            {
                "type": "feat",
                "scope": "campaign",
                "subject": "add the merge method option",
                "body": "Delivered one conventional commit per task.",
            }
        )
        self.assertIsNone(reason)
        self.assertEqual(narration["subject"], "feat(campaign): add the merge method option")
        self.assertEqual(narration["body"], "Delivered one conventional commit per task.")

    def test_prose_that_only_looks_dangerous_is_still_accepted(self) -> None:
        """The refusals are narrow on purpose.

        A bare `#<n>` backlinks and notifies nobody, an address is not a
        mention, and a subject may say what the change fixes as long as it does
        not name an issue GitHub would then close.
        """
        for name, body in {
            "bare-cross-reference": "Linked the context in #42.",
            "address": "Reported by tally@example.invalid.",
            "prose-fix": "Fixed the drift the reconciler kept re-reading.",
        }.items():
            with self.subTest(name):
                narration, reason = DRIVER.validated_narration(
                    {"type": "feat", "scope": "api", "subject": "add the widget", "body": body}
                )
                self.assertIsNone(reason)
                self.assertEqual(narration["body"], body)

    def test_a_scopeless_proposal_keeps_the_bare_type_prefix(self) -> None:
        narration, reason = DRIVER.validated_narration(
            {"type": "fix", "scope": None, "subject": "stop losing the receipt"}
        )
        self.assertIsNone(reason)
        self.assertEqual(narration["subject"], "fix: stop losing the receipt")
        self.assertEqual(narration["body"], "")

    def test_every_grammar_violation_is_refused_with_one_reason(self) -> None:
        cases = {
            "not-an-object": (["feat"], "not a JSON object"),
            "unknown-field": (
                {"type": "feat", "subject": "do a thing", "trailer": "Assisted-by: x"},
                "unknown fields",
            ),
            "unknown-type": ({"type": "wip", "subject": "do a thing"}, "type must be one of"),
            "bad-scope": (
                {"type": "feat", "scope": "Campaign Core", "subject": "do a thing"},
                "scope must be null",
            ),
            "empty-subject": ({"type": "feat", "subject": "   "}, "subject must be non-empty"),
            "control-subject": (
                {"type": "feat", "subject": "do a\tthing"},
                "control characters",
            ),
            "trailing-period": (
                {"type": "feat", "subject": "do a thing."},
                "must not end with a period",
            ),
            "capitalized": (
                {"type": "feat", "subject": "Do a thing"},
                "must not start with a capital",
            ),
            "long-header": (
                {"type": "feat", "subject": "x" * 80},
                "over the 72 cap",
            ),
            "wide-body": (
                {"type": "feat", "subject": "do a thing", "body": "y" * 120},
                "wraps past 100 columns",
            ),
            "managed-marker": (
                {
                    "type": "feat",
                    "subject": "do a thing",
                    "body": "Noted an update.\n\n<!-- tally:spec-build:v1 campaign=fixture -->",
                },
                "managed campaign marker",
            ),
            # A pull-request body is executable on GitHub. The node appends its
            # own `Closes #<sub-issue>`; a narrator that proposes one is
            # proposing to close an issue the campaign never named.
            "closing-keyword-in-body": (
                {
                    "type": "feat",
                    "subject": "do a thing",
                    "body": "Reported the change.\n\nCloses #1\nFixes #2",
                },
                "GitHub closing keyword",
            ),
            "closing-keyword-in-subject": (
                {"type": "fix", "subject": "fixes #12 at last"},
                "GitHub closing keyword",
            ),
            "cross-repo-closing-keyword": (
                {
                    "type": "feat",
                    "subject": "do a thing",
                    "body": "Investigated the issue.\n\nThis resolved acme/spec#9.",
                },
                "GitHub closing keyword",
            ),
            "closing-keyword-by-url": (
                {
                    "type": "feat",
                    "subject": "do a thing",
                    "body": "Fixed https://github.com/acme/spec/issues/3.",
                },
                "GitHub closing keyword",
            ),
            "mention": (
                {
                    "type": "feat",
                    "subject": "do a thing",
                    "body": "Notified reviewers.\n\ncc @torvalds",
                },
                "@mention",
            ),
            "team-mention": (
                {
                    "type": "feat",
                    "subject": "do a thing",
                    "body": "Notified the team.\n\nping @acme/security",
                },
                "@mention",
            ),
        }
        for name, (proposal, expected) in cases.items():
            with self.subTest(name):
                narration, reason = DRIVER.validated_narration(proposal)
                self.assertIsNone(narration)
                self.assertIn(expected, reason)


class StewardNarrationTests(unittest.TestCase):
    """The steward runs as a plain argv and never reaches git."""

    def shim(self, root: Path, name: str, body: str) -> list[str]:
        path = root / name
        path.write_text(f"#!{sys.executable}\n" + textwrap.dedent(body), encoding="utf-8")
        path.chmod(0o755)
        return [sys.executable, str(path)]

    def role(self, argv: list[str], **overrides: object) -> dict[str, object]:
        """Normalize the way the publish node does, not by hand."""
        return DRIVER.steward_role(
            {"adapter": "narrator", "argv": argv, "runtimeMaxSec": 30, **overrides}
        )

    def test_no_steward_uses_the_brief_derived_template(self) -> None:
        narration, transcript = DRIVER.narrate(None, task("task-1"), {})
        self.assertEqual(transcript, [])
        self.assertEqual(
            narration,
            {"source": "template", "subject": "task-1: Task task-1", "body": ""},
        )

    def test_an_accepted_proposal_is_read_from_the_final_message(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv = self.shim(
                root,
                "narrate",
                """
                import json
                import sys

                request = json.loads(sys.stdin.read())
                assert request["attempt"] == 1, request
                assert request["task"]["id"] == "task-1", request
                print("chatter the driver must ignore")
                print("TALLY_FINAL_MESSAGE=" + json.dumps({
                    "type": "feat",
                    "scope": "fixture",
                    "subject": "deliver the task",
                    "body": "Delivered body prose.",
                }))
                """,
            )
            narration, transcript = DRIVER.narrate(
                self.role(argv),
                task("task-1"),
                {"schemaVersion": 1, "task": {"id": "task-1"}},
            )
            self.assertEqual(narration["source"], "steward")
            self.assertEqual(narration["subject"], "feat(fixture): deliver the task")
            self.assertEqual(narration["body"], "Delivered body prose.")
            self.assertEqual(
                transcript, [{"attempt": 1, "status": "accepted", "reason": None}]
            )

    def test_the_adapter_environment_reaches_the_narrator_and_the_brief_does_not(
        self,
    ) -> None:
        """What makes "the adapter table decides endpoint and credentials" true.

        The narrator also must not inherit TALLY_BRIEF: it is handed its request
        on stdin, and the driver's own brief names the campaign checkout, the
        agent argv, and the adapter table.
        """
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv = self.shim(
                root,
                "narrate",
                """
                import json
                import os
                import sys

                sys.stdin.read()
                print("TALLY_FINAL_MESSAGE=" + json.dumps({
                    "type": "feat",
                    "subject": "reach " + os.environ["NARRATOR_ENDPOINT"],
                    "body": "Read brief=" + os.environ.get("TALLY_BRIEF", "absent") + ".",
                }))
                """,
            )
            with mock.patch.dict(
                os.environ, {"TALLY_BRIEF": "/tmp/should-not-be-visible"}, clear=False
            ):
                narration, transcript = DRIVER.narrate(
                    self.role(argv, env={"NARRATOR_ENDPOINT": "narrator.invalid"}),
                    task("task-1"),
                    {},
                )
            self.assertEqual(transcript[-1]["status"], "accepted")
            self.assertEqual(narration["subject"], "feat: reach narrator.invalid")
            self.assertEqual(narration["body"], "Read brief=absent.")

    def test_the_adapters_own_final_message_capture_is_what_is_read(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv = self.shim(
                root,
                "narrate",
                """
                import json
                import sys

                sys.stdin.read()
                print("TALLY_FINAL_MESSAGE=" + json.dumps({
                    "type": "chore",
                    "subject": "the shipped contract must not win here",
                }))
                print("narrator-result: " + json.dumps({
                    "type": "feat",
                    "subject": "read from the declared capture",
                }))
                """,
            )
            narration, transcript = DRIVER.narrate(
                self.role(argv, finalMessagePattern="^narrator-result: (.*)$"),
                task("task-1"),
                {},
            )
            self.assertEqual(transcript[-1]["status"], "accepted")
            self.assertEqual(narration["subject"], "feat: read from the declared capture")

    def test_an_unusable_steward_binding_is_refused_rather_than_degraded(self) -> None:
        cases = {
            "reserved-env": ({"env": {"TALLY_BRIEF": "/tmp/x"}}, "reserved variable"),
            "bad-env-name": ({"env": {"not a name": "x"}}, "environment identifiers"),
            "bad-pattern": (
                {"finalMessagePattern": "^unclosed(.*$"},
                "not a valid regular expression",
            ),
            "no-capture-group": (
                {"finalMessagePattern": "^narrator-result: .*$"},
                "exactly one capture group",
            ),
            "two-capture-groups": (
                {"finalMessagePattern": "^(a)(b)$"},
                "exactly one capture group",
            ),
        }
        for name, (override, expected) in cases.items():
            with self.subTest(name):
                with self.assertRaisesRegex(DRIVER.DriverError, expected):
                    DRIVER.steward_role(
                        {"adapter": "narrator", "argv": ["/bin/true"], **override}
                    )

    def test_a_refused_proposal_is_re_requested_with_the_reason(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv = self.shim(
                root,
                "narrate",
                """
                import json
                import sys

                request = json.loads(sys.stdin.read())
                if request["attempt"] == 1:
                    print("TALLY_FINAL_MESSAGE=" + json.dumps({
                        "type": "feat",
                        "subject": "Deliver the task.",
                    }))
                else:
                    assert "period" in request["previousRejection"], request
                    print("TALLY_FINAL_MESSAGE=" + json.dumps({
                        "type": "feat",
                        "subject": "deliver the task",
                    }))
                """,
            )
            narration, transcript = DRIVER.narrate(
                self.role(argv),
                task("task-1"),
                {},
            )
            self.assertEqual(narration["source"], "steward")
            self.assertEqual(narration["subject"], "feat: deliver the task")
            self.assertEqual([entry["status"] for entry in transcript], ["rejected", "accepted"])

    def test_two_failures_fall_back_to_the_template_and_hide_narrator_stderr(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv = self.shim(
                root,
                "narrate",
                """
                import sys

                sys.stdin.read()
                print("token=super-secret endpoint=https://narrator.invalid", file=sys.stderr)
                raise SystemExit(7)
                """,
            )
            narration, transcript = DRIVER.narrate(
                self.role(argv),
                task("task-1"),
                {},
            )
            self.assertEqual(
                narration,
                {
                    "source": "template",
                    "subject": "task-1: Task task-1",
                    # #385: the fallback is never silent -- the durable fact
                    # that both steward attempts were rejected rides along in
                    # the body a reader of the PR/commit actually sees.
                    "body": (
                        "Rejected 2 steward narration proposal(s) and used the "
                        "task-id template instead. Reasons: attempt 1 (failed): "
                        "steward exited 7; attempt 2 (failed): steward exited 7."
                    ),
                },
            )
            self.assertEqual(
                transcript,
                [
                    {"attempt": 1, "status": "failed", "reason": "steward exited 7"},
                    {"attempt": 2, "status": "failed", "reason": "steward exited 7"},
                ],
            )
            for entry in transcript:
                self.assertNotIn("secret", entry["reason"])
                self.assertNotIn("narrator.invalid", entry["reason"])
            self.assertNotIn("secret", narration["body"])
            self.assertNotIn("narrator.invalid", narration["body"])

    def test_two_invalid_proposals_fall_back_to_the_template(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv = self.shim(
                root,
                "narrate",
                """
                import sys

                sys.stdin.read()
                print("TALLY_FINAL_MESSAGE=not json at all")
                """,
            )
            narration, transcript = DRIVER.narrate(
                self.role(argv),
                task("task-1"),
                {},
            )
            self.assertEqual(narration["source"], "template")
            self.assertEqual([entry["status"] for entry in transcript], ["rejected", "rejected"])


class OutcomeFirstGrammarTests(unittest.TestCase):
    """#385: the machine-checkable half of the managed-agents content contract.

    One rule violated at a time, so a future rewrite that breaks exactly one
    of them fails exactly one assertion here instead of one vague test.
    """

    def test_a_compliant_leading_sentence_is_accepted(self) -> None:
        self.assertIsNone(
            DRIVER.validate_outcome_first(
                "Fixed the drift the reconciler kept re-reading.\n\n- detail one\n- detail two",
                max_chars=1000,
                context="body",
            )
        )

    def test_empty_text_is_refused(self) -> None:
        reason = DRIVER.validate_outcome_first("   ", max_chars=1000, context="body")
        self.assertIn("non-empty", reason)

    def test_over_length_text_is_refused(self) -> None:
        reason = DRIVER.validate_outcome_first("Fixed it.", max_chars=4, context="body")
        self.assertIn("character cap", reason)

    def test_an_exclamation_mark_anywhere_is_refused(self) -> None:
        reason = DRIVER.validate_outcome_first(
            "Fixed the drift!", max_chars=1000, context="body"
        )
        self.assertIn("exclamation", reason)

    def test_a_list_opening_the_text_is_refused(self) -> None:
        reason = DRIVER.validate_outcome_first(
            "- detail one\n- detail two", max_chars=1000, context="body"
        )
        self.assertIn("not a list", reason)

    def test_a_leading_line_with_no_terminating_period_is_refused(self) -> None:
        reason = DRIVER.validate_outcome_first(
            "Fixed the drift", max_chars=1000, context="body"
        )
        self.assertIn("end with a period", reason)

    def test_a_present_tense_or_non_verb_opening_is_refused(self) -> None:
        for opening in ("Fixing the drift.", "The drift is fixed.", "Fix the drift."):
            with self.subTest(opening):
                reason = DRIVER.validate_outcome_first(
                    opening, max_chars=1000, context="body"
                )
                self.assertIn("past-tense verb", reason)

    def test_an_irregular_past_tense_opening_is_accepted_case_insensitively(self) -> None:
        self.assertIsNone(
            DRIVER.validate_outcome_first("read the log.", max_chars=1000, context="body")
        )
        self.assertIsNone(
            DRIVER.validate_outcome_first("Read the log.", max_chars=1000, context="body")
        )

    def test_the_closing_summary_leads_with_an_outcome_first_sentence(self) -> None:
        digest = {
            "outcome": "complete",
            "source": {"sha256": "sha256:" + "a" * 64, "revision": "b" * 40},
            "baseRevision": "b" * 40,
            "repository": "acme/spec",
            "taskCount": 2,
            "merged": [
                {
                    "taskId": "task-1",
                    "title": "Task 1",
                    "pullRequest": "local://acme/spec/task-1",
                    "mergeCommit": "c" * 40,
                }
            ],
            "checkpoints": [],
            "blocked": [],
            "outstanding": [],
            "steering": [],
            "retries": [],
            "deferrals": [],
            "anomalies": [],
            "warnings": [],
        }
        summary = DRIVER.render_campaign_summary(digest)
        self.assertIn("Settled 1 of 2 task(s) against durable merge/checkpoint facts.", summary)
        lines = [line for line in summary.split("\n") if line]
        # The heading is structural, not prose; the first prose line is what
        # the grammar contract governs, and it must be the outcome sentence.
        self.assertTrue(lines[1].startswith("Settled "))


class SteeringGrammarTests(unittest.TestCase):
    """#385: the narrate slot's contract extended to steering notes."""

    def brief(self, root: Path, checkout: Path, **overrides: object) -> dict[str, object]:
        base = {
            "campaign": "fixture",
            "repository": "acme/spec",
            "repositoryConfig": repository_config(checkout, "local"),
            "issue": issue(),
            "taskId": "task-1",
            "attempt": 1,
            "diagnosis": "Investigated the failure.",
        }
        base.update(overrides)
        return base

    def posted_body(self, config: dict[str, object], steered: dict[str, object]) -> str:
        ref = steered["comment"].split("acme/spec/", 1)[1]
        return DRIVER.read_local_blob(config, ref)["diagnosis"]

    def test_a_grammar_violating_diagnosis_falls_back_with_a_durable_fact(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            config = repository_config(checkout, "local")
            steered = DRIVER.action_steer(
                self.brief(root, checkout, diagnosis="narrow the failing gate")
            )
            self.assertTrue(steered["posted"])
            body = self.posted_body(DRIVER.repo_config(config), steered)
            # Never silent: the durable fact that the fallback fired, and why,
            # rides in the same comment the rejected diagnosis would have.
            self.assertIn("Rejected the steward's diagnosis", body)
            self.assertIn("must end with a period", body)

    def test_gate_evidence_requires_the_failing_id_and_offending_path(self) -> None:
        detail = (
            "forbidPaths gate 'forbid-secrets' rejected 1 changed path(s): "
            '"secrets/key.pem" (matched "secrets/**")'
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            config = repository_config(checkout, "local")
            omitted = DRIVER.action_steer(
                self.brief(
                    root,
                    checkout,
                    diagnosis="Investigated the failing gate carefully.",
                    gateEvidence={"id": "gate:forbid-secrets", "detail": detail},
                )
            )
            body = self.posted_body(DRIVER.repo_config(config), omitted)
            self.assertIn("omits the failing check id", body)
            self.assertIn("gate:forbid-secrets", body)

    def test_a_diagnosis_naming_the_required_evidence_is_accepted_verbatim(self) -> None:
        detail = (
            "forbidPaths gate 'forbid-secrets' rejected 1 changed path(s): "
            '"secrets/key.pem" (matched "secrets/**")'
        )
        diagnosis = (
            "Investigated gate:forbid-secrets and found secrets/key.pem staged "
            "accidentally."
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            config = repository_config(checkout, "local")
            steered = DRIVER.action_steer(
                self.brief(
                    root,
                    checkout,
                    diagnosis=diagnosis,
                    gateEvidence={"id": "gate:forbid-secrets", "detail": detail},
                )
            )
            body = self.posted_body(DRIVER.repo_config(config), steered)
            self.assertEqual(body, diagnosis)


class SquashMergeTests(unittest.TestCase):
    """Squash integration, and the proofs that replace head ancestry."""

    def integration(self, checkout: Path, branch: str, head: str) -> dict[str, object]:
        return {
            "taskId": "task-1",
            "branch": branch,
            "baseRev": git(checkout, "rev-parse", "origin/main"),
            "head": head,
            "pullRequest": f"local://acme/spec/{branch}",
        }

    def publish_branch(self, checkout: Path, branch: str) -> str:
        git(checkout, "switch", "--quiet", "-c", "work", "origin/main")
        (checkout / "delivered.txt").write_text("one\n", encoding="utf-8")
        git(checkout, "add", "delivered.txt")
        git(checkout, "commit", "--quiet", "-m", "wip: first")
        (checkout / "delivered.txt").write_text("one\ntwo\n", encoding="utf-8")
        git(checkout, "add", "delivered.txt")
        git(checkout, "commit", "--quiet", "-m", "wip: second")
        git(checkout, "push", "--quiet", "origin", f"HEAD:refs/heads/{branch}")
        head = git(checkout, "rev-parse", "HEAD")
        git(checkout, "switch", "--quiet", "main")
        git(checkout, "fetch", "--quiet", "--prune", "origin")
        return head

    def test_local_squash_lands_one_commit_and_a_readable_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            workspace_root.mkdir()
            revision = "sha256:" + "a" * 64
            branch = DRIVER.stable_publish_branch("fixture", "7", "task-1", revision)
            head = self.publish_branch(checkout, branch)
            config = DRIVER.repo_config(repository_config(checkout))
            data = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "issue": issue(),
                "workspaceRoot": str(workspace_root),
                "task": {**task("task-1"), "revision": revision},
            }
            narration = {
                "source": "steward",
                "subject": "feat(fixture): deliver the first task",
                "body": "Steward-authored body.",
            }
            merge_commit = DRIVER.merge_local(
                data,
                config,
                self.integration(checkout, branch, head),
                "squash",
                narration,
            )
            git(checkout, "fetch", "--quiet", "--prune", "origin")
            base = git(checkout, "rev-parse", "origin/main")
            self.assertEqual(base, merge_commit)
            # One parent: a squash, not a merge commit.
            self.assertEqual(len(git(checkout, "log", "-1", "--format=%P", base).split()), 1)
            self.assertEqual(
                git(checkout, "log", "-1", "--format=%B", base).strip(),
                "feat(fixture): deliver the first task\n\nSteward-authored body.",
            )
            # The task head is deliberately not an ancestor of base: that is
            # exactly why the pre-squash assertion had to be replaced.
            self.assertNotEqual(
                command(
                    "git", "-C", str(checkout), "merge-base", "--is-ancestor", head, base,
                    check=False,
                ).returncode,
                0,
            )
            receipt = DRIVER.merge_receipt_ref("fixture", "7", "task-1", revision)
            self.assertEqual(
                DRIVER.local_remote_refs(config, receipt).get(receipt), merge_commit
            )
            facts = DRIVER.merged_local_tasks(
                "acme/spec",
                config,
                "fixture",
                "7",
                None,
                [{**task("task-1"), "revision": revision}],
            )
            self.assertEqual(
                facts,
                [
                    {
                        "taskId": "task-1",
                        "pullRequest": f"local://acme/spec/{branch}",
                        "mergeCommit": merge_commit,
                        "revision": revision,
                    }
                ],
            )

    def test_a_lost_base_push_race_leaves_the_task_mergeable_on_the_next_pass(self) -> None:
        """A receipt must never outrank the merge it only points at.

        Git enforces fast-forward on every non-tag ref, the campaign's hidden
        namespace included, and a retried squash always mints a different oid.
        A receipt pushed without --force therefore refuses its own successor and
        fails the node before the base push it needs to make progress, so one
        lost race would wedge the task behind a ref nothing documents.
        """
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout, remote = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            workspace_root.mkdir()
            revision = "sha256:" + "a" * 64
            branch = DRIVER.stable_publish_branch("fixture", "7", "task-1", revision)
            head = self.publish_branch(checkout, branch)
            config = DRIVER.repo_config(repository_config(checkout))
            data = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "issue": issue(),
                "workspaceRoot": str(workspace_root),
                "task": {**task("task-1"), "revision": revision},
            }
            narration = DRIVER.template_narration(task("task-1"))
            receipt = DRIVER.merge_receipt_ref("fixture", "7", "task-1", revision)

            # A sibling lane wins the base push in the window between the two
            # pushes. To the loser that is exactly a non-fast-forward refusal.
            hook = remote / "hooks/pre-receive"
            hook.parent.mkdir(parents=True, exist_ok=True)
            hook.write_text(
                "#!/bin/sh\nwhile read -r _ _ ref; do\n"
                '  case "$ref" in refs/heads/*) echo "non-fast-forward" >&2; exit 1 ;; esac\n'
                "done\nexit 0\n",
                encoding="utf-8",
            )
            hook.chmod(0o755)
            with self.assertRaises(DRIVER.DriverError):
                DRIVER.merge_local(
                    data,
                    config,
                    self.integration(checkout, branch, head),
                    "squash",
                    narration,
                )
            first_receipt = DRIVER.local_remote_refs(config, receipt).get(receipt)
            self.assertIsNotNone(first_receipt)
            # The receipt names a commit that never reached base, so it proves
            # nothing and the task correctly still reads as unmerged.
            self.assertEqual(
                DRIVER.merged_local_tasks(
                    "acme/spec",
                    config,
                    "fixture",
                    "7",
                    None,
                    [{**task("task-1"), "revision": revision}],
                ),
                [],
            )

            # The winner's commit lands and the next pass rebases onto it, so
            # the retry's squash necessarily has a different oid.
            hook.unlink()
            git(checkout, "switch", "--quiet", "main")
            (checkout / "sibling.txt").write_text("sibling\n", encoding="utf-8")
            git(checkout, "add", "sibling.txt")
            git(checkout, "commit", "--quiet", "-m", "sibling: advance base")
            git(checkout, "push", "--quiet", "origin", "HEAD:refs/heads/main")
            git(checkout, "switch", "--quiet", "work")
            git(checkout, "rebase", "--quiet", "origin/main")
            rebased = git(checkout, "rev-parse", "HEAD")
            git(checkout, "push", "--quiet", "--force", "origin", f"HEAD:refs/heads/{branch}")
            git(checkout, "switch", "--quiet", "main")
            git(checkout, "fetch", "--quiet", "--prune", "origin")

            merge_commit = DRIVER.merge_local(
                data,
                config,
                self.integration(checkout, branch, rebased),
                "squash",
                narration,
            )

            self.assertNotEqual(merge_commit, first_receipt)
            self.assertEqual(
                DRIVER.local_remote_refs(config, receipt).get(receipt), merge_commit
            )
            git(checkout, "fetch", "--quiet", "--prune", "origin")
            self.assertEqual(git(checkout, "rev-parse", "origin/main"), merge_commit)
            self.assertEqual(
                [
                    fact["mergeCommit"]
                    for fact in DRIVER.merged_local_tasks(
                        "acme/spec",
                        config,
                        "fixture",
                        "7",
                        None,
                        [{**task("task-1"), "revision": revision}],
                    )
                ],
                [merge_commit],
            )

    def test_local_merge_method_still_produces_a_merge_commit_and_no_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            workspace_root.mkdir()
            branch = DRIVER.stable_publish_branch("fixture", "7", "task-1")
            head = self.publish_branch(checkout, branch)
            config = DRIVER.repo_config(repository_config(checkout))
            data = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "issue": issue(),
                "workspaceRoot": str(workspace_root),
                "task": task("task-1"),
            }
            merge_commit = DRIVER.merge_local(
                data,
                config,
                self.integration(checkout, branch, head),
                "merge",
                DRIVER.template_narration(task("task-1")),
            )
            git(checkout, "fetch", "--quiet", "--prune", "origin")
            base = git(checkout, "rev-parse", "origin/main")
            self.assertEqual(len(git(checkout, "log", "-1", "--format=%P", base).split()), 2)
            self.assertEqual(
                command(
                    "git", "-C", str(checkout), "merge-base", "--is-ancestor", head, base,
                    check=False,
                ).returncode,
                0,
            )
            self.assertEqual(
                DRIVER.local_remote_refs(
                    config, f"{DRIVER.local_state_prefix('fixture', '7')}/merge/*"
                ),
                {},
            )
            self.assertEqual(
                [fact["mergeCommit"] for fact in DRIVER.merged_local_tasks(
                    "acme/spec", config, "fixture", "7", None, [task("task-1")]
                )],
                [head],
            )
            self.assertNotEqual(merge_commit, head)

    def github_state(
        self, branch: str, head: str, merge_commit: str, on_merge: list[str]
    ) -> dict[str, object]:
        return {
            "prView": {
                "state": "OPEN",
                "baseRefName": "main",
                "headRefName": branch,
                "headRefOid": head,
            },
            "mergeCommitOid": merge_commit,
            "onMerge": on_merge,
            "master": {
                "body": (
                    "<!-- tally:campaign-worklist:v1 -->\n"
                    f"- [ ] {DRIVER.TASK_MARKER_PREFIX}task-1 -->\n"
                    "<!-- tally:campaign-worklist:v1:end -->"
                )
            },
            "comments": [],
            "issueComments": [],
            "calls": [],
        }

    def integration_commit(self, checkout: Path, branch: str, method: str) -> str:
        """The commit the forge would mint, staged on a side branch."""
        git(checkout, "switch", "--quiet", "-c", f"forge-{method}", "origin/main")
        if method == "squash":
            git(checkout, "merge", "--quiet", "--squash", f"origin/{branch}")
            git(checkout, "commit", "--quiet", "-m", "feat(fixture): deliver the first task")
        else:
            git(checkout, "merge", "--quiet", "--no-ff", "--no-edit", f"origin/{branch}")
        minted = git(checkout, "rev-parse", "HEAD")
        git(checkout, "switch", "--quiet", "main")
        return minted

    def test_github_squash_passes_the_validated_message_and_proves_the_merge_commit(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout, _ = initialize_repository(root, remote=True)
            branch = DRIVER.stable_publish_branch("fixture", "7", "task-1")
            head = self.publish_branch(checkout, branch)
            squash = self.integration_commit(checkout, branch, "squash")
            config = DRIVER.repo_config(repository_config(checkout, "github"))
            data = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "issue": issue(),
                "task": task("task-1"),
                "workspace": {"publishBranch": branch},
            }
            integration = {
                "taskId": "task-1",
                "branch": branch,
                "baseRev": git(checkout, "rev-parse", "origin/main"),
                "head": head,
                "pullRequest": "https://github.com/acme/spec/pull/1",
            }
            narration = {
                "source": "steward",
                "subject": "feat(fixture): deliver the first task",
                "body": "Steward-authored body.",
            }
            state = self.github_state(
                branch,
                head,
                squash,
                ["git", "-C", str(checkout), "push", "--quiet", "origin",
                 f"{squash}:refs/heads/main"],
            )
            with FakeGitHub(root, state) as github:
                merged = DRIVER.merge_github(
                    data, config, integration, {"subIssueWalk": True}, "squash", narration
                )
            self.assertEqual(merged, squash)
            # The squash commit is on base and the task head is not: the
            # pre-squash assertion would have failed this successful merge.
            self.assertEqual(
                command(
                    "git", "-C", str(checkout), "merge-base", "--is-ancestor",
                    squash, "origin/main", check=False,
                ).returncode,
                0,
            )
            self.assertNotEqual(
                command(
                    "git", "-C", str(checkout), "merge-base", "--is-ancestor",
                    head, "origin/main", check=False,
                ).returncode,
                0,
            )
            merge_calls = [
                call for call in github.state()["calls"] if call[:2] == ["pr", "merge"]
            ]
            self.assertEqual(len(merge_calls), 1)
            self.assertIn("--squash", merge_calls[0])
            self.assertNotIn("--merge", merge_calls[0])
            self.assertEqual(
                merge_calls[0][merge_calls[0].index("--subject") + 1],
                "feat(fixture): deliver the first task",
            )
            self.assertEqual(
                merge_calls[0][merge_calls[0].index("--body") + 1],
                "Steward-authored body.",
            )
            self.assertEqual(
                merge_calls[0][merge_calls[0].index("--match-head-commit") + 1], head
            )

    def test_github_merge_method_keeps_the_pre_squash_argv_and_assertion(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout, _ = initialize_repository(root, remote=True)
            branch = DRIVER.stable_publish_branch("fixture", "7", "task-1")
            head = self.publish_branch(checkout, branch)
            minted = self.integration_commit(checkout, branch, "merge")
            config = DRIVER.repo_config(repository_config(checkout, "github"))
            data = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "issue": issue(),
                "task": task("task-1"),
                "workspace": {"publishBranch": branch},
            }
            integration = {
                "taskId": "task-1",
                "branch": branch,
                "baseRev": git(checkout, "rev-parse", "origin/main"),
                "head": head,
                "pullRequest": "https://github.com/acme/spec/pull/1",
            }
            state = self.github_state(
                branch,
                head,
                minted,
                ["git", "-C", str(checkout), "push", "--quiet", "origin",
                 f"{minted}:refs/heads/main"],
            )
            with FakeGitHub(root, state) as github:
                merged = DRIVER.merge_github(
                    data,
                    config,
                    integration,
                    {"subIssueWalk": True},
                    "merge",
                    DRIVER.template_narration(task("task-1")),
                )
            self.assertEqual(merged, minted)
            merge_calls = [
                call for call in github.state()["calls"] if call[:2] == ["pr", "merge"]
            ]
            self.assertEqual(
                merge_calls,
                [
                    [
                        "pr",
                        "merge",
                        "https://github.com/acme/spec/pull/1",
                        "--repo",
                        "acme/spec",
                        "--merge",
                        "--match-head-commit",
                        head,
                    ]
                ],
            )


GIT_AI_SHIM = """#!INTERPRETER
# A stand-in for the externally provisioned binary. It reproduces exactly the
# observable behaviour the squash-fidelity spike recorded for 1.6.17: nothing
# happens at fetch or read time, and after `await` the commit that was just
# made in this repository carries a per-line note on refs/notes/ai.
import json
import subprocess
import sys


def git(*arguments: str) -> str:
    return subprocess.run(
        ["git", *arguments], check=True, text=True, stdout=subprocess.PIPE
    ).stdout.strip()


arguments = sys.argv[1:]
if arguments[:1] == ["--version"]:
    print("1.6.17")
    raise SystemExit(0)
if arguments[:1] == ["await"]:
    head = git("rev-parse", "HEAD")
    body = (
        "delivered.txt\\n  s_fixture000001::t_fixture01 1\\n---\\n"
        + json.dumps(
            {
                "schema_version": "authorship/3.0.0",
                "git_ai_version": "1.6.17",
                "base_commit_sha": git("rev-parse", "HEAD^"),
                "sessions": {"s_fixture000001": {"agent_id": {"tool": "fixture"}}},
            },
            sort_keys=True,
        )
        + "\\n"
    )
    git("notes", "--ref", "refs/notes/ai", "add", "-f", "-m", body, head)
    raise SystemExit(0)
raise SystemExit(2)
"""


class GitAiBindingTests(unittest.TestCase):
    """The post-merge fourth proof axis (AUGUST-01-DESIGN.md §7, ruling §9.3.2).

    Every scenario runs against a squash the campaign did not mint locally --
    `merge_local` clones the remote and squashes there, which is the same shape
    `gh pr merge --squash` produces -- because that is the case the spike
    proved arrives unbound.
    """

    def arm(self, root: Path, *, provisioned: bool) -> dict[str, str]:
        """Isolate git-ai state, then decide whether a binary is on PATH."""
        home = root / "home"
        home.mkdir()
        binaries = root / "bin"
        binaries.mkdir()
        entries = os.environ["PATH"].split(os.pathsep)
        if provisioned:
            shim = binaries / "git-ai"
            shim.write_text(
                GIT_AI_SHIM.replace("#!INTERPRETER", f"#!{sys.executable}", 1),
                encoding="utf-8",
            )
            shim.chmod(0o755)
        else:
            # The operator's own host provisions git-ai, so "absent" has to be
            # constructed rather than assumed.
            entries = [
                entry
                for entry in entries
                if entry and not (Path(entry) / "git-ai").exists()
            ]
        path = os.pathsep.join(entries)
        return {
            # A real host arms git-ai through a global trace2 target under
            # HOME. Redirecting both keeps the operator's own daemon out of
            # this test and makes the shim the only thing that writes notes.
            "HOME": str(home),
            "GIT_CONFIG_GLOBAL": str(home / ".gitconfig"),
            "GIT_CONFIG_SYSTEM": "/dev/null",
            "PATH": f"{binaries}{os.pathsep}{path}",
        }

    def campaign(self, root: Path) -> tuple[Path, Path, dict[str, object], dict[str, object], str, str]:
        checkout, remote = initialize_repository(root, remote=True)
        workspace_root = root / "workspaces"
        workspace_root.mkdir()
        revision = "sha256:" + "a" * 64
        branch = DRIVER.stable_publish_branch("fixture", "7", "task-1", revision)
        git(checkout, "switch", "--quiet", "-c", "work", "origin/main")
        (checkout / "delivered.txt").write_text("one\n", encoding="utf-8")
        git(checkout, "add", "delivered.txt")
        git(checkout, "commit", "--quiet", "-m", "wip: first")
        (checkout / "delivered.txt").write_text("one\ntwo\n", encoding="utf-8")
        git(checkout, "add", "delivered.txt")
        git(checkout, "commit", "--quiet", "-m", "wip: second")
        git(checkout, "push", "--quiet", "origin", f"HEAD:refs/heads/{branch}")
        head = git(checkout, "rev-parse", "HEAD")
        git(checkout, "switch", "--quiet", "main")
        git(checkout, "fetch", "--quiet", "--prune", "origin")
        config = DRIVER.repo_config(repository_config(checkout))
        data = {
            "campaign": "fixture",
            "repository": "acme/spec",
            "issue": issue(),
            "workspaceRoot": str(workspace_root),
            "task": {**task("task-1"), "revision": revision},
        }
        integration = {
            "taskId": "task-1",
            "branch": branch,
            "baseRev": git(checkout, "rev-parse", "origin/main"),
            "head": head,
            "pullRequest": f"local://acme/spec/{branch}",
        }
        return checkout, remote, config, data, integration, head

    NARRATION = {
        "source": "steward",
        "subject": "feat(fixture): deliver the first task",
        "body": "Steward-authored body.",
    }

    ASSISTED = {
        "adapter": "codex",
        "model": "provider/model-1",
        "taskUuid": "00000000-0000-4000-8000-000000000311",
        "witnessSeq": 42,
    }

    def spy_on_reconstruction(self) -> tuple[Any, list[str]]:
        """Capture the object ID the binding actually copies from.

        A reconstruction that collides with the integrated commit turns
        `git notes copy` into a no-op onto itself, so a test that never looks
        cannot tell whether the copy path ran at all.
        """
        seen: list[str] = []
        original = DRIVER.reconstruct_squash

        def spy(*arguments: Any, **keywords: Any) -> tuple[str | None, str | None]:
            local, reason = original(*arguments, **keywords)
            seen.append(local)
            return local, reason

        return mock.patch.object(DRIVER, "reconstruct_squash", spy), seen

    def second_publisher(self, root: Path, remote: Path, revision: str, body: str) -> None:
        """Another lane publishes its own note for the same commit."""
        clone = root / f"other-{len(list(root.glob('other-*')))}"
        command("git", "clone", "--quiet", str(remote), str(clone))
        git(clone, "config", "user.name", "Other Lane")
        git(clone, "config", "user.email", "other@invalid")
        git(clone, "fetch", "--quiet", "origin", "refs/notes/ai:refs/notes/ai", check=False)
        git(clone, "notes", "--ref", "refs/notes/ai", "add", "-f", "-m", body, revision)
        git(clone, "push", "--quiet", "origin", "refs/notes/ai:refs/notes/ai")

    @staticmethod
    def remote_notes(remote: Path) -> list[str]:
        listing = command(
            "git", "-C", str(remote), "notes", "--ref", "refs/notes/ai", "list",
            check=False,
        ).stdout.strip()
        return sorted(line.split()[1] for line in listing.splitlines() if line.strip())

    def test_a_forge_side_squash_is_bound_and_the_note_reaches_the_remote(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.arm(root, provisioned=True)
            checkout, remote, config, data, integration, head = self.campaign(root)
            trailer = DRIVER.assisted_by_trailer(self.ASSISTED)
            message = DRIVER.merge_commit_message(self.NARRATION, trailer)
            with mock.patch.dict(os.environ, environment):
                merge_commit = DRIVER.merge_local(
                    data, config, integration, "squash", self.NARRATION, trailer
                )
                # The squash was minted in a throwaway clone, exactly like a
                # forge-side one: it arrives with no note at all.
                git(checkout, "fetch", "--quiet", "--prune", "origin")
                self.assertNotEqual(
                    command(
                        "git", "-C", str(checkout), "notes", "--ref", "refs/notes/ai",
                        "show", merge_commit, check=False,
                    ).returncode,
                    0,
                )
                patched, reconstructions = self.spy_on_reconstruction()
                with patched:
                    receipt = DRIVER.bind_authorship(
                        data, config, integration, "squash", merge_commit, "advisory", message
                    )
            # The reconstruction is a distinct object, so the copy the whole
            # binding turns on is genuinely exercised rather than a no-op onto
            # itself.
            self.assertEqual(len(reconstructions), 1)
            reconstruction = reconstructions[0]
            self.assertNotEqual(reconstruction, merge_commit)
            self.assertEqual(receipt["status"], "bound")
            self.assertEqual(receipt["binding"], "advisory")
            self.assertEqual(receipt["revision"], merge_commit)
            self.assertEqual(receipt["noteRef"], "refs/notes/ai")
            self.assertTrue(receipt["published"])
            self.assertIsNone(receipt["reason"])
            # The note the campaign remote now carries for the squash oid, and
            # the digest the receipt witnessed, are the same bytes.
            published = command(
                "git", "-C", str(remote), "notes", "--ref", "refs/notes/ai",
                "show", merge_commit,
            ).stdout
            self.assertIn("s_fixture000001::t_fixture01 1", published)
            self.assertEqual(
                receipt["noteSha256"],
                "sha256:" + hashlib.sha256(published.encode("utf-8")).hexdigest(),
            )
            # The receipt names what the campaign remote resolves to, because
            # that is what a reader fetching the notes ref will see.
            self.assertEqual(
                receipt["notesRefTarget"], git(remote, "rev-parse", "refs/notes/ai")
            )
            # Exactly one entry is published: the integrated commit's. The
            # throwaway reconstruction's note is removed rather than left to
            # accumulate one dead entry per merged task on a public forge.
            self.assertEqual(self.remote_notes(remote), [merge_commit])
            self.assertNotEqual(
                command(
                    "git", "-C", str(checkout), "notes", "--ref", "refs/notes/ai",
                    "show", reconstruction, check=False,
                ).returncode,
                0,
            )
            # The squash message on base carries the node's trailer, and the
            # trailer is exactly what the gh producer publishes.
            body = git(checkout, "log", "-1", "--format=%B", merge_commit)
            self.assertIn(
                "Assisted-by: codex:provider/model-1 "
                "(tally:00000000-0000-4000-8000-000000000311 witness:42)",
                body,
            )
            # The reconstruction is torn down: no leaked worktree, no leaked
            # remote-notes scratch ref.
            self.assertNotIn(
                "git-ai-bind-", git(checkout, "worktree", "list", "--porcelain")
            )
            for scratch in (DRIVER.GIT_AI_REMOTE_REF, DRIVER.GIT_AI_PUBLISH_REF):
                self.assertNotEqual(
                    command(
                        "git", "-C", str(checkout), "rev-parse", "--verify", scratch,
                        check=False,
                    ).returncode,
                    0,
                )

    def test_an_absent_binary_is_advisory_and_never_blocks_but_fails_required(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.arm(root, provisioned=False)
            checkout, remote, config, data, integration, head = self.campaign(root)
            message = DRIVER.merge_commit_message(self.NARRATION, None)
            with mock.patch.dict(os.environ, environment):
                merge_commit = DRIVER.merge_local(
                    data, config, integration, "squash", self.NARRATION
                )
                advisory = DRIVER.bind_authorship(
                    data, config, integration, "squash", merge_commit, "advisory", message
                )
                with self.assertRaisesRegex(DRIVER.DriverError, "under required mode"):
                    DRIVER.bind_authorship(
                        data, config, integration, "squash", merge_commit, "required", message
                    )
                # `off` is the shipped state: no receipt, no notes pushed.
                self.assertIsNone(
                    DRIVER.bind_authorship(
                        data, config, integration, "squash", merge_commit, "off", message
                    )
                )
            self.assertEqual(advisory["status"], "unavailable")
            self.assertFalse(advisory["published"])
            self.assertIn("tally.nix does not ship it", advisory["reason"])
            self.assertIsNone(advisory["noteSha256"])
            self.assertNotEqual(
                command(
                    "git", "-C", str(remote), "rev-parse", "--verify", "refs/notes/ai",
                    check=False,
                ).returncode,
                0,
            )

    def test_a_second_pass_over_an_already_merged_task_binds_again(self) -> None:
        """A campaign pass is re-enterable, so the binding must be too.

        A later reconcile can dispatch the merge node again for a task whose
        pull request is already MERGED. That pass reconstructs the identical
        commit, and git-ai does not re-annotate an object its service already
        processed -- so the copy has no source, and the binding has to recognise
        that the integrated commit already carries the note it would have
        produced instead of reporting the note missing.
        """
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.arm(root, provisioned=True)
            checkout, remote, config, data, integration, head = self.campaign(root)
            message = DRIVER.merge_commit_message(self.NARRATION, None)
            with mock.patch.dict(os.environ, environment):
                merge_commit = DRIVER.merge_local(
                    data, config, integration, "squash", self.NARRATION
                )
                first = DRIVER.bind_authorship(
                    data, config, integration, "squash", merge_commit, "advisory", message
                )
                # The shim mints a note only for a commit it has not seen, the
                # way the real service does.
                shim = Path(environment["PATH"].split(os.pathsep)[0]) / "git-ai"
                shim.write_text(
                    shim.read_text(encoding="utf-8").replace(
                        'git("notes", "--ref", "refs/notes/ai", "add", "-f", "-m", body, head)',
                        'raise SystemExit(0)',
                    ),
                    encoding="utf-8",
                )
                second = DRIVER.bind_authorship(
                    data, config, integration, "squash", merge_commit, "advisory", message
                )
            self.assertEqual(first["status"], "bound")
            self.assertEqual(second["status"], "bound")
            self.assertTrue(second["published"])
            self.assertEqual(second["noteSha256"], first["noteSha256"])
            self.assertEqual(self.remote_notes(remote), [merge_commit])

    def test_a_diverged_remote_note_is_refused_and_the_witnessed_note_survives(self) -> None:
        """Two authorship records for one commit cannot be merged, only chosen.

        A `cat_sort_uniq` fold is line-oriented; a git-ai `authorship/3.0.0`
        note is a two-section record whose line order is semantic. Folding two
        of them publishes a structurally invalid note under a schema version it
        no longer satisfies -- and, because `git notes merge` writes into the
        *local* ref, rewrites the daemon's witnessed code-result bindings in the
        campaign checkout at the same time.
        """
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.arm(root, provisioned=True)
            checkout, remote, config, data, integration, head = self.campaign(root)
            message = DRIVER.merge_commit_message(self.NARRATION, None)
            # What the daemon's settlement barrier bound at code-result
            # completion, in the shared checkout that the merge node also uses.
            witnessed = "coder.txt\n  s_coder0000001::t_coder01 1-2\n---\n{}\n"
            git(checkout, "notes", "--ref", "refs/notes/ai", "add", "-f", "-m", witnessed, head)
            witnessed_digest = hashlib.sha256(
                (
                    command(
                        "git", "-C", str(checkout), "notes", "--ref", "refs/notes/ai",
                        "show", head,
                    ).stdout
                ).encode("utf-8")
            ).hexdigest()
            with mock.patch.dict(os.environ, environment):
                merge_commit = DRIVER.merge_local(
                    data, config, integration, "squash", self.NARRATION
                )
                # Another publisher of refs/notes/ai got there first with its
                # own record for the same commit.
                self.second_publisher(
                    root,
                    remote,
                    merge_commit,
                    "delivered.txt\n  s_otherlane0000::t_other01 1\n---\n{}\n",
                )
                receipt = DRIVER.bind_authorship(
                    data, config, integration, "squash", merge_commit, "advisory", message
                )
            self.assertEqual(receipt["status"], "conflict")
            self.assertFalse(receipt["published"])
            self.assertIn("refusing to merge two authorship records", receipt["reason"])
            self.assertIsNone(receipt["noteSha256"])
            # The other lane's record is intact, not mashed together with this
            # lane's.
            published = command(
                "git", "-C", str(remote), "notes", "--ref", "refs/notes/ai",
                "show", merge_commit,
            ).stdout
            self.assertEqual(
                published, "delivered.txt\n  s_otherlane0000::t_other01 1\n---\n{}\n"
            )
            # And the witnessed binding the daemon recorded is byte-identical,
            # so `tally witness verify-authorship` still passes for that task.
            after = command(
                "git", "-C", str(checkout), "notes", "--ref", "refs/notes/ai", "show", head,
            ).stdout
            self.assertEqual(hashlib.sha256(after.encode("utf-8")).hexdigest(), witnessed_digest)

    def test_only_the_integrated_commit_s_note_reaches_the_public_remote(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.arm(root, provisioned=True)
            checkout, remote, config, data, integration, head = self.campaign(root)
            message = DRIVER.merge_commit_message(self.NARRATION, None)
            # A note the shared campaign checkout holds for work that never
            # left the host: an abandoned attempt, a diagnosis commit, any
            # commit the campaign did not choose to publish.
            git(checkout, "switch", "--quiet", "--detach", "origin/main")
            (checkout / "private.txt").write_text("private\n", encoding="utf-8")
            git(checkout, "add", "private.txt")
            git(checkout, "commit", "--quiet", "-m", "wip: never published")
            private = git(checkout, "rev-parse", "HEAD")
            git(
                checkout, "notes", "--ref", "refs/notes/ai", "add", "-f", "-m",
                "private.txt\n  s_localonly0001::t_local01 1\n---\n{}\n", private,
            )
            git(checkout, "switch", "--quiet", "main")
            with mock.patch.dict(os.environ, environment):
                merge_commit = DRIVER.merge_local(
                    data, config, integration, "squash", self.NARRATION
                )
                receipt = DRIVER.bind_authorship(
                    data, config, integration, "squash", merge_commit, "advisory", message
                )
            self.assertEqual(receipt["status"], "bound")
            self.assertEqual(self.remote_notes(remote), [merge_commit])
            self.assertNotIn(private, self.remote_notes(remote))
            # The local note is untouched: scoping the publication is not
            # deleting the checkout's own bookkeeping.
            self.assertEqual(
                command(
                    "git", "-C", str(checkout), "notes", "--ref", "refs/notes/ai",
                    "show", private, check=False,
                ).returncode,
                0,
            )

    def test_advisory_cannot_raise_even_when_the_remote_vanishes(self) -> None:
        """The merge has already landed; an advisory subsystem may not fail it.

        Reporting a merged task as failed is worse than reporting an unbound
        one, so `advisory` turns every outcome -- including an unexpected
        one -- into a typed receipt.
        """
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.arm(root, provisioned=True)
            checkout, remote, config, data, integration, head = self.campaign(root)
            message = DRIVER.merge_commit_message(self.NARRATION, None)
            with mock.patch.dict(os.environ, environment):
                merge_commit = DRIVER.merge_local(
                    data, config, integration, "squash", self.NARRATION
                )
                shutil.rmtree(remote)
                receipt = DRIVER.bind_authorship(
                    data, config, integration, "squash", merge_commit, "advisory", message
                )
                with self.assertRaisesRegex(DRIVER.DriverError, "under required mode"):
                    DRIVER.bind_authorship(
                        data, config, integration, "squash", merge_commit, "required", message
                    )
            self.assertEqual(receipt["status"], "error")
            self.assertFalse(receipt["published"])
            self.assertIn("cannot refresh origin", receipt["reason"])
            # The reason is quotable in a public failure report, so it names
            # the remote and the exit status, never the transport's own stderr.
            self.assertNotIn(str(remote), receipt["reason"])

    def test_an_unusable_campaign_workspace_is_a_receipt_not_a_traceback(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.arm(root, provisioned=True)
            checkout, remote, config, data, integration, head = self.campaign(root)
            message = DRIVER.merge_commit_message(self.NARRATION, None)
            with mock.patch.dict(os.environ, environment):
                merge_commit = DRIVER.merge_local(
                    data, config, integration, "squash", self.NARRATION
                )
                blocked = {**data, "workspaceRoot": str(checkout / "root.go" / "nested")}
                receipt = DRIVER.bind_authorship(
                    blocked, config, integration, "squash", merge_commit, "advisory", message
                )
            self.assertEqual(receipt["status"], "error")
            self.assertFalse(receipt["published"])
            self.assertIn("campaign workspace", receipt["reason"])

    def test_a_reconstruction_that_is_not_the_integrated_tree_copies_nothing(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.arm(root, provisioned=True)
            checkout, remote, config, data, integration, head = self.campaign(root)
            message = DRIVER.merge_commit_message(self.NARRATION, None)
            with mock.patch.dict(os.environ, environment):
                merge_commit = DRIVER.merge_local(
                    data, config, integration, "squash", self.NARRATION
                )
                # Somebody amended the integrated commit after the merge. The
                # reconstruction still produces the gated tree, which is no
                # longer the tree that reached base, so nothing may be copied.
                git(checkout, "fetch", "--quiet", "--prune", "origin")
                git(checkout, "switch", "--quiet", "--detach", merge_commit)
                (checkout / "delivered.txt").write_text("one\ntwo\nthree\n", encoding="utf-8")
                git(checkout, "add", "delivered.txt")
                git(checkout, "commit", "--quiet", "--amend", "--no-edit")
                amended = git(checkout, "rev-parse", "HEAD")
                git(checkout, "push", "--quiet", "--force", "origin", "HEAD:refs/heads/main")
                git(checkout, "switch", "--quiet", "main")
                receipt = DRIVER.bind_authorship(
                    data, config, integration, "squash", amended, "advisory", message
                )
            self.assertEqual(receipt["status"], "mismatch")
            self.assertIn("nothing may be copied", receipt["reason"])
            self.assertFalse(receipt["published"])
            self.assertNotEqual(
                command(
                    "git", "-C", str(checkout), "notes", "--ref", "refs/notes/ai",
                    "show", amended, check=False,
                ).returncode,
                0,
            )

    def test_a_squash_onto_a_base_the_campaign_never_gated_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.arm(root, provisioned=True)
            checkout, remote, config, data, integration, head = self.campaign(root)
            message = DRIVER.merge_commit_message(self.NARRATION, None)
            with mock.patch.dict(os.environ, environment):
                merge_commit = DRIVER.merge_local(
                    data, config, integration, "squash", self.NARRATION
                )
                moved = {**integration, "baseRev": "b" * 40}
                receipt = DRIVER.bind_authorship(
                    data, config, moved, "squash", merge_commit, "advisory", message
                )
            self.assertEqual(receipt["status"], "mismatch")
            self.assertIn("not the gated base", receipt["reason"])
            self.assertFalse(receipt["published"])

    def test_the_trailer_is_the_published_format_and_a_narrator_may_not_forge_one(self) -> None:
        self.assertEqual(
            DRIVER.assisted_by_trailer(DRIVER.assisted_by_record(self.ASSISTED, "assistedBy")),
            "Assisted-by: codex:provider/model-1 "
            "(tally:00000000-0000-4000-8000-000000000311 witness:42)",
        )
        # Absent is ordinary: a checkpoint task has no agent, and an estate
        # that never named a model leaves the pointer unwritable.
        self.assertIsNone(DRIVER.assisted_by_record(None, "assistedBy"))
        self.assertIsNone(DRIVER.assisted_by_trailer(None))
        for broken in (
            {**self.ASSISTED, "taskUuid": "not-a-uuid"},
            {**self.ASSISTED, "witnessSeq": 0},
            {**self.ASSISTED, "model": ""},
            {**self.ASSISTED, "model": "provider/model (1)"},
        ):
            with self.assertRaises(DRIVER.DriverError):
                DRIVER.assisted_by_record(broken, "assistedBy")
        # The provenance line is the node's authority. A narrator proposing one
        # is proposing a claim nothing witnessed, in a commit that lands on the
        # default branch. Git matches trailer keys case-insensitively, so the
        # refusal has to as well: `assisted-by:` is the same trailer to every
        # git-native reader, and with the shipped `agentModel = null` default
        # the node appends nothing after it, leaving the forged line as the
        # message's entire trailer block.
        for spelling in ("Assisted-by", "assisted-by", "ASSISTED-BY", "AsSiStEd-By"):
            forged = (
                f"Noted context.\n\n{spelling}: someone:something (tally:x witness:1)"
            )
            narration, reason = DRIVER.validated_narration(
                {
                    "type": "feat",
                    "scope": "fixture",
                    "subject": "deliver the task",
                    "body": forged,
                }
            )
            self.assertIsNone(narration, spelling)
            self.assertEqual(reason, "proposal contains an Assisted-by trailer", spelling)
        # A subject that merely mentions the words is prose, not a trailer.
        narration, reason = DRIVER.validated_narration(
            {
                "type": "docs",
                "scope": "fixture",
                "subject": "explain the assisted-by pointer",
                "body": "Documented that the trailer is a pointer, never the proof.",
            }
        )
        self.assertIsNotNone(narration)
        self.assertIsNone(reason)
        # The template path keeps its byte-for-byte message when no trailer is
        # available, so an unarmed campaign commits exactly what it used to.
        self.assertEqual(
            DRIVER.merge_commit_message(self.NARRATION, None),
            "feat(fixture): deliver the first task\n\nSteward-authored body.\n",
        )

    def test_the_binding_is_configuration_and_the_shipped_state_is_off(self) -> None:
        self.assertEqual(DRIVER.git_ai_binding(None, "gitAiBinding"), "off")
        for value in ("off", "advisory", "required"):
            self.assertEqual(DRIVER.git_ai_binding(value, "gitAiBinding"), value)
        with self.assertRaisesRegex(
            DRIVER.DriverError, "must be off, advisory, or required"
        ):
            DRIVER.git_ai_binding("on", "gitAiBinding")
        # The settlement barrier's budget is the campaign's, not a constant the
        # module cannot see; absent is the shipped default.
        self.assertEqual(DRIVER.git_ai_await_sec(None, "gitAiAwaitSec"), 60)
        self.assertEqual(DRIVER.git_ai_await_sec(12, "gitAiAwaitSec"), 12)
        for broken in (0, -1, "60"):
            with self.assertRaises(DRIVER.DriverError):
                DRIVER.git_ai_await_sec(broken, "gitAiAwaitSec")


if __name__ == "__main__":
    unittest.main()
