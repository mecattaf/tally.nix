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
) -> dict[str, object]:
    return {
        "campaign": "fixture",
        "repository": "acme/spec",
        "repositoryConfig": repository_config(checkout),
        "runId": run_id,
        "workspaceRoot": str(workspace_root),
        "tally": str(tally),
    }


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

                if args[:2] == ["pr", "list"]:
                    if args[args.index("--state") + 1] == "all":
                        value = state.get("pulls", [])
                    elif "--head" in args:
                        head = args[args.index("--head") + 1]
                        value = state.get("byHead", {}).get(head, [])
                    else:
                        value = state.get("merged", [])
                    print(json.dumps(value))
                elif args[:2] == ["pr", "reopen"]:
                    url = args[2]
                    for candidate in state.get("pulls", []):
                        if candidate.get("url") == url:
                            candidate["state"] = "OPEN"
                    print(url)
                elif args and args[0] == "api":
                    for body in state.get("comments", []):
                        print(body)
                elif args[:2] == ["issue", "comment"]:
                    failures = state.get("commentFailures", 0)
                    if failures:
                        state["commentFailures"] = failures - 1
                        state_path.write_text(json.dumps(state), encoding="utf-8")
                        print("injected comment failure", file=sys.stderr)
                        raise SystemExit(93)
                    body = args[args.index("--body") + 1]
                    state.setdefault("comments", []).append(body)
                    print("https://github.com/acme/spec/issues/7#issuecomment-test")
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


class GitHubForgeTests(unittest.TestCase):
    def test_reconcile_requires_exact_marker_and_degrades_unusable_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root)
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
            task_1_marker = DRIVER.pull_request_marker("fixture", "7", "task-1")
            task_2_marker = DRIVER.pull_request_marker("fixture", "7", "task-2")
            state = {
                "merged": [
                    {
                        "url": "https://github.com/acme/spec/pull/1",
                        "body": task_1_marker,
                        "baseRefName": "main",
                        "headRefName": "tally/fixture-issue-7/task-1",
                        "mergeCommit": {"oid": "a" * 40},
                    },
                    {
                        "url": "https://github.com/acme/spec/pull/2",
                        "body": (
                            "Spec-build campaign progress for acme/spec#7.\n"
                            "Task `task-2`: quoted text without an identity marker"
                        ),
                        "baseRefName": "main",
                        "headRefName": "unrelated",
                        "mergeCommit": {"oid": "b" * 40},
                    },
                    {
                        "url": "https://github.com/acme/spec/pull/3",
                        "body": task_2_marker,
                        "baseRefName": "release",
                        "headRefName": "quoted-branch",
                        "mergeCommit": {"oid": "c" * 40},
                    },
                    {
                        "url": "https://github.com/acme/spec/pull/4",
                        "body": DRIVER.pull_request_marker("fixture", "7", "unknown-task"),
                        "baseRefName": "main",
                        "headRefName": "tally/fixture-issue-7/unknown-task",
                        "mergeCommit": {"oid": "d" * 40},
                    },
                ],
                "byHead": {"tally/fixture-issue-7/task-2": []},
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
                        "mergeCommit": {"oid": "e" * 40},
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

                integration = {"pullRequest": url}
                DRIVER.github_progress_comment(data, integration, "b" * 40)
                DRIVER.github_progress_comment(data, integration, "b" * 40)
                continued = DRIVER.action_continue(
                    {
                        "campaign": "fixture",
                        "repository": "acme/spec",
                        "repositoryConfig": repository_config(checkout, "github"),
                        "issue": issue(),
                        "runId": "pass-1",
                        "reconcileCommand": "/tally reconcile fixture",
                    }
                )
                self.assertEqual(continued, {"command": "/tally reconcile fixture", "posted": True})
                final_state = github.state()
                self.assertEqual(len(final_state["comments"]), 2)
                self.assertIn("task=task-1 merged", final_state["comments"][0])
                self.assertEqual(final_state["comments"][1], "/tally reconcile fixture")
                self.assertEqual(
                    sum(call[:2] == ["pr", "reopen"] for call in final_state["calls"]),
                    1,
                )

    def test_continuation_comment_is_retried_and_read_after_write_verified(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root)
            state = {
                "comments": ["/tally reconcile fixture"],
                "commentFailures": 1,
                "calls": [],
            }
            with FakeGitHub(root, state) as github, mock.patch.object(
                DRIVER.time, "sleep", return_value=None
            ) as sleep:
                continued = DRIVER.action_continue(
                    {
                        "campaign": "fixture",
                        "repository": "acme/spec",
                        "repositoryConfig": repository_config(checkout, "github"),
                        "issue": issue(),
                        "runId": "pass-2",
                        "reconcileCommand": "/tally reconcile fixture",
                    }
                )
                self.assertEqual(
                    continued,
                    {"command": "/tally reconcile fixture", "posted": True},
                )
                final_state = github.state()
                self.assertEqual(
                    final_state["comments"].count("/tally reconcile fixture"),
                    2,
                )
                self.assertEqual(
                    sum(call[:2] == ["issue", "comment"] for call in final_state["calls"]),
                    2,
                )
                sleep.assert_called_once_with(1)


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
                                "ownedPaths": [],
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
