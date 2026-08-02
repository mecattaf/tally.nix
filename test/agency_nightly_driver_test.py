#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


DRIVER = Path(
    os.environ.get(
        "AGENCY_NIGHTLY_DRIVER",
        Path(__file__).parents[1] / "examples/flows/agency_nightly_driver.py",
    )
)
SPEC = importlib.util.spec_from_file_location("agency_nightly_driver", DRIVER)
assert SPEC is not None and SPEC.loader is not None
driver = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(driver)


def git(*argv: str, cwd: Path | None = None) -> str:
    return subprocess.run(
        ["git", *argv],
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def task(task_id: str, **overrides: object) -> dict[str, object]:
    entry: dict[str, object] = {
        "taskId": task_id,
        "title": f"Implement {task_id}",
        "mission": f"Do the bounded work of {task_id}.",
        "acceptanceCriteria": [f"{task_id} is implemented", "the tests pass"],
    }
    entry.update(overrides)
    return entry


class WaveShapeTests(unittest.TestCase):
    """The wave arrives in the brief; the driver only refuses malformed waves."""

    def test_the_declared_wave_is_the_worklist(self) -> None:
        parsed = driver.read_wave({"wave": [task("alpha"), task("beta", issue="78")]})
        self.assertEqual([entry["taskId"] for entry in parsed], ["alpha", "beta"])
        self.assertEqual(parsed[1]["issue"], "78")

    def test_no_labelled_issue_parser_survives_the_ruling(self) -> None:
        # The 2026-07-27 ruling removed the external worklist source. If any of
        # these ever come back, this driver has grown a second worklist contract.
        for name in (
            "parse_issue",
            "parse_acceptance",
            "topological_order",
            "select_wave",
            "gh_issue_state",
            "load_github_credential",
        ):
            self.assertFalse(
                hasattr(driver, name), f"{name} reintroduces an external worklist"
            )

    def test_an_oversized_wave_is_a_typed_error(self) -> None:
        with self.assertRaises(driver.DriverError) as raised:
            driver.read_wave({"wave": [task(f"t{index}") for index in range(7)]})
        self.assertEqual(raised.exception.code, "worklist-wave-too-large")
        self.assertEqual(raised.exception.details["maxWaveSize"], 6)

    def test_a_duplicate_task_id_is_a_typed_error(self) -> None:
        with self.assertRaises(driver.DriverError) as raised:
            driver.read_wave({"wave": [task("alpha"), task("alpha")]})
        self.assertEqual(raised.exception.code, "worklist-task-id-duplicate")

    def test_a_task_without_acceptance_criteria_is_a_typed_error(self) -> None:
        with self.assertRaises(driver.DriverError) as raised:
            driver.read_wave({"wave": [task("alpha", acceptanceCriteria=[])]})
        self.assertEqual(raised.exception.code, "worklist-task-invalid")

    def test_a_non_slug_task_id_is_a_typed_error(self) -> None:
        with self.assertRaises(driver.DriverError) as raised:
            driver.read_wave({"wave": [task("Alpha/One")]})
        self.assertEqual(raised.exception.code, "worklist-task-invalid")


class DriverProcessTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.remote = self.root / "remote.git"
        self.checkout = self.root / "checkout"
        self.worktrees = self.root / "worktrees"
        self.report = self.root / "reports/morning.md"
        git("init", "--bare", str(self.remote))
        git("init", "-b", "main", str(self.checkout))
        git("config", "user.name", "Agency Test", cwd=self.checkout)
        git("config", "user.email", "agency@example.test", cwd=self.checkout)
        (self.checkout / "README.md").write_text("base\n", encoding="utf-8")
        git("add", "README.md", cwd=self.checkout)
        git("commit", "-m", "base", cwd=self.checkout)
        git("remote", "add", "origin", str(self.remote), cwd=self.checkout)
        git("push", "-u", "origin", "main", cwd=self.checkout)

        fake_bin = self.root / "bin"
        fake_bin.mkdir()
        fake_gh = fake_bin / "gh"
        fake_gh.write_text(
            f"#!{sys.executable}\n"
            + """\
import json
import sys

args = sys.argv[1:]
if args[:2] == ["pr", "list"]:
    print("[]")
elif args[:2] == ["pr", "create"]:
    branch = args[args.index("--head") + 1]
    print("https://example.test/pull/" + branch.rsplit("/", 1)[-1])
else:
    raise SystemExit("unexpected gh invocation: " + repr(args))
""",
            encoding="utf-8",
        )
        fake_gh.chmod(0o755)
        self.environment = os.environ.copy()
        self.environment["PATH"] = f"{fake_bin}{os.pathsep}{self.environment['PATH']}"

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def invoke(
        self, action: str, brief: dict[str, object], *, expect_ok: bool = True
    ) -> dict[str, object]:
        environment = self.environment.copy()
        environment["TALLY_BRIEF"] = json.dumps(brief)
        result = subprocess.run(
            [sys.executable, str(DRIVER), action],
            check=True,
            capture_output=True,
            text=True,
            env=environment,
        )
        # Exit zero regardless: the envelope carries the verdict.
        self.assertEqual(result.returncode, 0)
        final = result.stdout.strip().splitlines()[-1]
        self.assertTrue(final.startswith(driver.FINAL_MESSAGE_PREFIX), result.stdout)
        value = json.loads(final.removeprefix(driver.FINAL_MESSAGE_PREFIX))
        self.assertEqual(value["ok"], expect_ok, value)
        return value["value"] if expect_ok else value["error"]

    def wave(self, count: int = 6) -> list[dict[str, object]]:
        return [task(f"task-{index}") for index in range(1, count + 1)]

    def worklist_brief(self, wave: list[dict[str, object]]) -> dict[str, object]:
        return {
            "action": "worklist",
            "repository": "agency/example",
            "checkout": str(self.checkout),
            "baseRev": "origin/main",
            "baseBranch": "main",
            "worktreeRoot": str(self.worktrees),
            "branchPrefix": "agency/nightly",
            "wave": wave,
        }

    def implement(self, workspace: dict[str, object], revision: int = 1) -> str:
        worktree = Path(str(workspace["worktreePath"]))
        task_id = workspace["taskId"]
        (worktree / f"{task_id}.txt").write_text(
            f"implemented {task_id} revision {revision}\n", encoding="utf-8"
        )
        git("add", f"{task_id}.txt", cwd=worktree)
        git("commit", "-m", f"implement {task_id} ({revision})", cwd=worktree)
        return git("rev-parse", "HEAD", cwd=worktree)

    def test_lanes_carry_git_native_identity_from_the_shared_manager(self) -> None:
        """Both drivers cut lanes through one manager, so both record identity.

        The nightly wave is not a campaign, but its lanes answer the same
        question -- whose is this, and may it be resumed -- from the same place
        git keeps its own per-worktree state.
        """
        wave = self.wave(2)
        worklist = self.invoke("worklist", self.worklist_brief(wave))
        for workspace in worklist["workspaces"]:
            worktree = Path(str(workspace["worktreePath"]))
            self.assertEqual(
                driver.worktrees.read_identity(worktree),
                {
                    "driver": "agency-nightly",
                    "repository": "agency/example",
                    "taskid": str(workspace["taskId"]),
                    "branch": str(workspace["branch"]),
                },
            )
        enumerated = {
            lane["identity"]["taskid"]: lane["branch"]
            for lane in driver.worktrees.lanes(self.checkout)
            if lane["identity"].get("driver") == "agency-nightly"
        }
        self.assertEqual(
            enumerated,
            {
                str(workspace["taskId"]): str(workspace["branch"])
                for workspace in worklist["workspaces"]
            },
        )

    def test_a_foreign_worktree_at_a_lane_path_is_a_typed_conflict(self) -> None:
        wave = self.wave(1)
        worklist = self.invoke("worklist", self.worklist_brief(wave))
        worktree = Path(str(worklist["workspaces"][0]["worktreePath"]))
        # Another wave's lane sitting at this task's path is a conflict the
        # driver reports, never something it clobbers.
        driver.worktrees.write_identity(worktree, {"taskid": "someone-else"})
        error = self.invoke("worklist", self.worklist_brief(wave), expect_ok=False)
        self.assertEqual(error["code"], "worklist-worktree-conflict")
        self.assertIn("different lane identity", error["message"])

    def test_six_worktrees_and_pull_requests_reach_the_morning_report(self) -> None:
        wave = self.wave()
        worklist = self.invoke("worklist", self.worklist_brief(wave))
        self.assertEqual(
            [entry["taskId"] for entry in worklist["tasks"]],
            [entry["taskId"] for entry in wave],
        )
        self.assertEqual(len(worklist["workspaces"]), 6)
        self.assertRegex(worklist["baseRev"], r"^[0-9a-f]{40}$")

        # Preparation is idempotent: the resume path adopts existing worktrees.
        replayed = self.invoke("worklist", self.worklist_brief(wave))
        self.assertEqual(replayed, worklist)

        tasks = []
        for entry, workspace in zip(worklist["tasks"], worklist["workspaces"]):
            self.assertTrue(Path(str(workspace["worktreePath"])).is_dir())
            head = self.implement(workspace)
            tasks.append(
                {
                    "task": entry,
                    "workspace": workspace,
                    "implementation": {
                        "taskId": entry["taskId"],
                        "branch": workspace["branch"],
                        "head": head,
                        "summary": f"Implemented {entry['taskId']}.",
                        "tests": ["focused test: pass"],
                    },
                    "review": {
                        "taskId": entry["taskId"],
                        "reviewedHead": head,
                        "verdict": "approve",
                        "summary": f"Reviewed {entry['taskId']}.",
                        "findings": [],
                    },
                    "failure": None,
                }
            )

        culmination = self.invoke(
            "culminate", self.culminate_brief(worklist["baseRev"], tasks)
        )
        self.assertEqual(culmination["status"], "ready")
        self.assertEqual(
            [pull_request["status"] for pull_request in culmination["pullRequests"]],
            ["created"] * 6,
        )
        self.assertEqual(culmination["failures"], [])
        report = self.report.read_text(encoding="utf-8")
        self.assertIn("# Agency morning report", report)
        self.assertIn("## Human culmination", report)
        self.assertEqual(report.count("Review verdict: **approve**"), 6)
        for index, entry in enumerate(tasks, start=1):
            self.assertEqual(
                git(
                    "--git-dir",
                    str(self.remote),
                    "rev-parse",
                    f"refs/heads/agency/nightly/task-{index}",
                ),
                entry["implementation"]["head"],
            )

    def culminate_brief(
        self,
        base_rev: object,
        tasks: list[dict[str, object]],
        *,
        worklist_error: dict[str, object] | None = None,
    ) -> dict[str, object]:
        return {
            "action": "culminate",
            "repository": "agency/example",
            "checkout": str(self.checkout),
            "baseBranch": "main",
            "baseRev": base_rev,
            "reportPath": str(self.report),
            "worklistError": worklist_error,
            "tasks": tasks,
        }

    def test_one_failed_task_still_produces_the_report_and_the_other_pull_request(
        self,
    ) -> None:
        wave = self.wave(2)
        worklist = self.invoke("worklist", self.worklist_brief(wave))
        good, bad = worklist["workspaces"]
        head = self.implement(good)
        tasks = [
            {
                "task": worklist["tasks"][0],
                "workspace": good,
                "implementation": {
                    "taskId": good["taskId"],
                    "branch": good["branch"],
                    "head": head,
                    "summary": "Implemented task-1.",
                    "tests": [],
                },
                "review": {
                    "taskId": good["taskId"],
                    "reviewedHead": head,
                    "verdict": "changes-requested",
                    "summary": "One blocking concern.",
                    "findings": [{"severity": "blocking", "text": "needs a test"}],
                },
                "failure": None,
            },
            {
                "task": worklist["tasks"][1],
                "workspace": bad,
                "implementation": None,
                "review": None,
                "failure": {
                    "taskId": bad["taskId"],
                    "stage": "implementation",
                    "code": "node-failed",
                    "message": "the implementation node failed",
                },
            },
        ]
        culmination = self.invoke(
            "culminate", self.culminate_brief(worklist["baseRev"], tasks)
        )
        self.assertEqual(culmination["status"], "partial")
        self.assertEqual(len(culmination["pullRequests"]), 1)
        self.assertEqual(culmination["failures"][0]["taskId"], "task-2")
        report = self.report.read_text(encoding="utf-8")
        # A finder never certifies: changes-requested still opens the pull request.
        self.assertEqual(culmination["pullRequests"][0]["status"], "created")
        self.assertIn("Review verdict: **changes-requested**", report)
        self.assertIn("**Failed at implementation**", report)

    def test_a_failed_worklist_still_produces_a_morning_report(self) -> None:
        culmination = self.invoke(
            "culminate",
            self.culminate_brief(
                None,
                [],
                worklist_error={
                    "code": "worklist-base-revision-invalid",
                    "message": "origin/main did not resolve",
                },
            ),
        )
        self.assertEqual(culmination["status"], "worklist-failed")
        self.assertEqual(culmination["pullRequests"], [])
        report = self.report.read_text(encoding="utf-8")
        self.assertIn("## The wave did not start", report)
        self.assertIn("worklist-base-revision-invalid", report)

    def test_a_head_that_moved_after_the_report_is_refused(self) -> None:
        worklist = self.invoke("worklist", self.worklist_brief(self.wave(1)))
        workspace = worklist["workspaces"][0]
        head = self.implement(workspace)
        self.implement(workspace, revision=2)
        error = self.invoke(
            "culminate",
            self.culminate_brief(
                worklist["baseRev"],
                [
                    {
                        "task": worklist["tasks"][0],
                        "workspace": workspace,
                        "implementation": {
                            "taskId": workspace["taskId"],
                            "branch": workspace["branch"],
                            "head": head,
                            "summary": "Implemented task-1.",
                            "tests": [],
                        },
                        "review": None,
                        "failure": None,
                    }
                ],
            ),
            expect_ok=False,
        )
        self.assertEqual(error["code"], "culmination-head-drift")

    def test_an_unknown_action_is_a_typed_envelope_not_a_crash(self) -> None:
        environment = self.environment.copy()
        environment["TALLY_BRIEF"] = "{}"
        result = subprocess.run(
            [sys.executable, str(DRIVER), "not-an-action"],
            check=False,
            capture_output=True,
            text=True,
            env=environment,
        )
        self.assertEqual(result.returncode, 0)
        final = result.stdout.strip().splitlines()[-1]
        value = json.loads(final.removeprefix(driver.FINAL_MESSAGE_PREFIX))
        self.assertFalse(value["ok"])
        self.assertEqual(value["error"]["code"], "agency-driver-action-invalid")


if __name__ == "__main__":
    unittest.main()
