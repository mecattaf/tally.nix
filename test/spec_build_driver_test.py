#!/usr/bin/env python3
"""Focused forge and lifecycle regressions for the spec-build policy driver."""

from __future__ import annotations

import importlib.util
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest
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
) -> dict[str, object]:
    return {
        "campaign": "fixture",
        "repository": "acme/spec",
        "repositoryConfig": repository_config(checkout, forge),
        "issue": issue(),
        "runId": run_id,
        "workspaceRoot": str(workspace_root),
        "task": task("task-1"),
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

                if args[:2] == ["pr", "reopen"]:
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
                            "Retry after removing ghp_0123456789abcdefghijklmnopqrstuvwxyz "
                            "from diagnostic output."
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
                repeated = DRIVER.action_escalate(reconcile_brief)
                self.assertFalse(repeated["posted"])
                self.assertEqual(repeated["comment"], escalated["comment"])
                self.assertEqual(len(github.state()["comments"]), 2)

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
                            "diagnosis": f"Steering for attempt {attempt}.",
                        }
                    )
                    self.assertTrue(steered["posted"])
                escalated = DRIVER.action_escalate(reconcile_brief)
                self.assertTrue(escalated["posted"])

                published = github.state()
                # Two steering notes and one escalation: three distinct
                # comments, each created rather than edited.
                self.assertEqual(len(published["comments"]), 3)
                self.assertEqual(
                    len({comment["html_url"] for comment in published["issueComments"]}),
                    3,
                )
                creates = [
                    call for call in published["calls"] if call[:2] == ["issue", "comment"]
                ]
                self.assertEqual(len(creates), 3)
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

            # The closing summary publishes the same way: create, never edit.
            completed = subprocess.CompletedProcess([], 0, "", "")
            with mock.patch.object(DRIVER, "run", return_value=completed) as run:
                DRIVER.close_completed_issue_campaign(
                    "acme/spec", "7", {"sha256": "sha256:" + "a" * 64}, []
                )
            commands = [call.args[0] for call in run.call_args_list]
            summary = next(
                command for command in commands if command[:3] == ["gh", "issue", "comment"]
            )
            self.assertIn("Campaign complete", summary[-1])
            self.assertFalse(
                any("--edit-last" in command or "updateIssueComment" in command for command in commands)
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
                self.assertIn("frontier quiescent", github.state()["comments"][0])

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

        digest = "sha256:" + "a" * 64
        with mock.patch.object(
            DRIVER,
            "github_json",
            side_effect=[{"state": "open"}, {"state": "closed"}],
        ), mock.patch.object(DRIVER, "run", return_value=completed) as run:
            DRIVER.close_completed_issue_campaign(
                "acme/spec", "1", {"sha256": digest}, tasks
            )
        commands = [call.args[0] for call in run.call_args_list]
        self.assertIn(["gh", "issue", "close", "2", "--repo", "acme/spec"], commands)
        self.assertNotIn(["gh", "issue", "close", "3", "--repo", "acme/spec"], commands)
        self.assertIn(["gh", "issue", "close", "1", "--repo", "acme/spec"], commands)
        comment = next(command for command in commands if command[:3] == ["gh", "issue", "comment"])
        self.assertIn(digest, comment[-1])


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
    ) -> dict[str, object]:
        return {
            "number": number,
            "state": state,
            "url": f"https://github.com/acme/spec/issues/{number}",
            "closedByPullRequestsReferences": {
                "pageInfo": {"hasNextPage": truncated},
                "nodes": pulls or [],
            },
            "comments": {"nodes": comments or []},
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
            self.assertEqual(
                [(item["taskId"], item["comment"]) for item in result["diagnoses"]],
                [("task-1", "https://github.com/acme/spec/issues/8#machine")],
            )
            self.assertTrue(
                any(
                    "master-thread machine diagnosis for 'task-1'" in warning
                    for warning in result["warnings"]
                )
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
                    {**base, "attempt": 1, "diagnosis": "narrow the failing gate"}
                )
                retried = DRIVER.action_retry(
                    {**base, "stage": "prep", "detail": "the lane vanished"}
                )
                # Reading the same thread back returns exactly what was posted,
                # so a retry brief sees its own task's steering history.
                replayed = DRIVER.action_steer(
                    {**base, "attempt": 1, "diagnosis": "narrow the failing gate"}
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
                DRIVER.merge_github(data, config, integration, {"subIssueWalk": True})
                native = github.state()
                self.assertEqual(native["comments"], [])
                self.assertIn("- [ ] ", native["master"]["body"])

                DRIVER.merge_github(data, config, integration, {"subIssueWalk": False})
                degraded = github.state()
                self.assertEqual(degraded["comments"], [])
                self.assertIn(
                    f"- [x] {DRIVER.TASK_MARKER_PREFIX}task-1 -->",
                    degraded["master"]["body"],
                )


class LaneLifecycleTests(unittest.TestCase):
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
                **old_brief,
                "domainsRequired": True,
                "workspace": prepared,
                "publication": {
                    "taskId": "task-1",
                    "branch": stable_branch,
                    "head": published_head,
                    "pullRequest": "local://acme/spec/task-1",
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
                        **old_brief,
                        "domainsRequired": True,
                        "workspace": prepared,
                        "publication": {
                            "taskId": "task-1",
                            "branch": stable_branch,
                            "head": published_head,
                            "pullRequest": "local://acme/spec/task-1",
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

    def test_next_pass_sweeps_old_worktree_branch_and_state_marker(self) -> None:
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
                self.assertTrue(any(DRIVER.task_state_markers(workspace_root / ".state")))
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
            self.assertFalse(any(DRIVER.task_state_markers(workspace_root / ".state")))
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

    def test_next_pass_sweeps_identity_validated_unregistered_lane(self) -> None:
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
                marker = workspace_root / ".state" / "marker-directory" / "task-1.json"
                marker.parent.mkdir(parents=True)
                marker.write_text(
                    json.dumps(
                        {
                            "campaign": "fixture",
                            "repository": "acme/spec",
                            "runId": run_id,
                            "taskId": "task-1",
                            "branch": branch,
                            "worktreePath": str(worktree),
                        }
                    ),
                    encoding="utf-8",
                )
                tally.update(currentFlowRunId=live_flow, flows={dead_flow: []})
                swept = DRIVER.action_sweep(
                    sweep_brief(checkout, workspace_root, "live-pass", tally.program)
                )
            self.assertFalse(worktree.exists())
            self.assertFalse(marker.exists())
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


if __name__ == "__main__":
    unittest.main()
