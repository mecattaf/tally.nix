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
                elif args[:2] == ["api", "user"]:
                    print(state.get("actor", "tally-test"))
                elif args and args[0] == "api":
                    endpoint = next(
                        (item for item in args[1:] if item.startswith("repos/")), ""
                    )
                    if endpoint.endswith("/sub_issues?per_page=100"):
                        print(json.dumps(state.get("subIssues", [])))
                    elif endpoint.endswith("/issues/7"):
                        print(json.dumps(state.get("master", {})))
                    elif "--slurp" in args:
                        issue_comments = state.get("issueComments", [])
                        print(json.dumps([issue_comments]))
                    else:
                        issue_comments = state.get("issueComments", [])
                        for comment in issue_comments:
                            print(comment.get("body", ""))
                elif args[:2] == ["issue", "comment"]:
                    body = args[args.index("--body") + 1]
                    state.setdefault("comments", []).append(body)
                    comment_number = len(state["comments"])
                    url = f"https://github.com/acme/spec/issues/7#issuecomment-test-{comment_number}"
                    state.setdefault("issueComments", []).append({
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

    def test_machine_receipts_trust_the_current_actor_and_escalate_once(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root)
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

    def test_forge_native_issue_graph_derives_blocking_and_escalation_config(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root)
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
                    "approvalPolicy": "on-request",
                    "sandboxPolicy": "workspace-write",
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
                        "issue": 8,
                        "dependencies": [],
                        "conflictDomains": ["src/one"],
                    },
                    {
                        "id": "task-2",
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
            reconcile_brief = {
                "repository": "acme/spec",
                "issue": issue(),
                "worklist": {"kind": "github-issue"},
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
                self.assertIsNone(result["config"]["reconcileCommand"])
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
            with self.assertRaisesRegex(DRIVER.DriverError, "published branch was abandoned"):
                DRIVER.action_rebase(rebase_brief)
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

    def test_next_pass_sweeps_old_worktree_branch_and_state_marker(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            prepared = DRIVER.action_prep(prep_brief(checkout, workspace_root, "dead-pass"))
            worktree = Path(prepared["worktreePath"])
            self.assertTrue(worktree.is_dir())
            self.assertTrue(any((workspace_root / ".state").glob("*/*.json")))

            swept = DRIVER.action_sweep(
                {
                    "campaign": "fixture",
                    "repository": "acme/spec",
                    "repositoryConfig": repository_config(checkout),
                    "runId": "live-pass",
                    "workspaceRoot": str(workspace_root),
                }
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
            self.assertFalse(any((workspace_root / ".state").glob("*/*.json")))
            self.assertTrue(any(item.startswith("worktree:") for item in swept["cleaned"]))
            self.assertEqual(swept["warnings"], [])

    def test_next_pass_sweeps_identity_validated_unregistered_lane(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            run_id = "dead-unregistered-pass"
            run_hash = hashlib.sha256(run_id.encode()).hexdigest()[:12]
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

            swept = DRIVER.action_sweep(
                {
                    "campaign": "fixture",
                    "repository": "acme/spec",
                    "repositoryConfig": repository_config(checkout),
                    "runId": "live-pass",
                    "workspaceRoot": str(workspace_root),
                }
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
            self.assertEqual(swept["warnings"], [])

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
