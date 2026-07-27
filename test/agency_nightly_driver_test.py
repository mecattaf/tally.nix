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


def issue(number: int, *, title: str | None = None, extra: str = "") -> dict[str, object]:
    return {
        "number": number,
        "title": title or f"[P] Implement task {number}",
        "body": (
            "Bounded work item.\n\n"
            "## Acceptance\n\n"
            f"- [ ] task {number} is implemented\n"
            "- [x] the contract is understood\n\n"
            f"Files: src/task-{number}.rs\n"
            f"{extra}"
        ),
    }


class ShapeTests(unittest.TestCase):
    def test_micro_ruling_shape_is_parsed_without_tool_specific_fields(self) -> None:
        parsed = driver.parse_issue(
            issue(41, extra="Depends-on: #7, #12\n")
        )
        self.assertEqual(
            parsed,
            {
                "taskId": "41",
                "title": "[P] Implement task 41",
                "acceptanceCriteria": [
                    {"text": "task 41 is implemented", "checked": False},
                    {"text": "the contract is understood", "checked": True},
                ],
                "parallelism": "parallel",
                "files": ["src/task-41.rs"],
                "dependsOn": ["7", "12"],
            },
        )

    def test_issue_without_acceptance_checklist_is_a_typed_error(self) -> None:
        with self.assertRaises(driver.DriverError) as raised:
            driver.parse_issue(
                {
                    "number": 42,
                    "title": "[P] Invalid",
                    "body": "## Acceptance\n\nNo task list here.\n",
                }
            )
        self.assertEqual(raised.exception.code, "worklist-acceptance-missing")
        self.assertEqual(raised.exception.details["taskId"], "42")

    def test_dependency_cycle_is_a_typed_error(self) -> None:
        entries = [
            driver.parse_issue(issue(51, extra="Depends-on: #52\n")),
            driver.parse_issue(issue(52, extra="Depends-on: #51\n")),
        ]
        with self.assertRaises(driver.DriverError) as raised:
            driver.topological_order(entries)
        self.assertEqual(raised.exception.code, "worklist-dependency-cycle")
        self.assertEqual(raised.exception.details["taskIds"], ["51", "52"])

    def test_sequential_entry_is_a_barrier_and_parallel_entries_form_a_wave(self) -> None:
        ordered = [
            driver.parse_issue(issue(61, title="Sequential task")),
            driver.parse_issue(issue(62)),
            driver.parse_issue(issue(63)),
        ]
        states = {entry["taskId"]: "OPEN" for entry in ordered}
        self.assertEqual(driver.select_wave(ordered, states, 6), ["61"])
        self.assertEqual(driver.select_wave(ordered[1:], states, 6), ["62", "63"])


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

        self.fixture = self.root / "github.json"
        self.fixture.write_text(
            json.dumps(
                {
                    "issues": [issue(number) for number in range(201, 207)],
                    "states": {},
                }
            ),
            encoding="utf-8",
        )
        fake_bin = self.root / "bin"
        fake_bin.mkdir()
        fake_gh = fake_bin / "gh"
        fake_gh.write_text(
            f"#!{sys.executable}\n"
            + """\
import json
import os
import sys

with open(os.environ["GH_FIXTURE"], encoding="utf-8") as handle:
    fixture = json.load(handle)
args = sys.argv[1:]
if args[:2] == ["issue", "list"]:
    print(json.dumps(fixture["issues"]))
elif args[:2] == ["issue", "view"]:
    print(json.dumps({"state": fixture["states"].get(args[2], "CLOSED")}))
elif args[:2] == ["pr", "list"]:
    print("[]")
elif args[:2] == ["pr", "create"]:
    branch = args[args.index("--head") + 1]
    print("https://example.test/pull/" + branch.rsplit("-", 1)[-1])
else:
    raise SystemExit("unexpected gh invocation: " + repr(args))
""",
            encoding="utf-8",
        )
        fake_gh.chmod(0o755)
        self.environment = os.environ.copy()
        self.environment["PATH"] = f"{fake_bin}{os.pathsep}{self.environment['PATH']}"
        self.environment["GH_FIXTURE"] = str(self.fixture)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def invoke(self, action: str, brief: dict[str, object]) -> dict[str, object]:
        environment = self.environment.copy()
        environment["TALLY_BRIEF"] = json.dumps(brief)
        result = subprocess.run(
            [sys.executable, str(DRIVER), action],
            check=True,
            capture_output=True,
            text=True,
            env=environment,
        )
        final = result.stdout.strip().splitlines()[-1]
        self.assertTrue(final.startswith(driver.FINAL_MESSAGE_PREFIX), result.stdout)
        value = json.loads(final.removeprefix(driver.FINAL_MESSAGE_PREFIX))
        self.assertTrue(value["ok"], value)
        return value["value"]

    def worklist_brief(self) -> dict[str, object]:
        return {
            "action": "worklist",
            "source": {
                "kind": "github-issues",
                "repository": "agency/example",
                "label": "tally:worklist",
                "state": "open",
            },
            "checkout": str(self.checkout),
            "baseRev": "origin/main",
            "baseBranch": "main",
            "worktreeRoot": str(self.worktrees),
            "branchPrefix": "agency/nightly",
            "maxWaveSize": 6,
        }

    def test_six_worktrees_and_pull_requests_reach_the_morning_report(self) -> None:
        worklist = self.invoke("worklist", self.worklist_brief())
        self.assertEqual(worklist["wave"], [str(number) for number in range(201, 207)])
        self.assertEqual(len(worklist["workspaces"]), 6)
        self.assertRegex(worklist["baseRev"], r"^[0-9a-f]{40}$")

        # Worklist preparation is idempotent for the same branches and paths.
        replayed = self.invoke("worklist", self.worklist_brief())
        self.assertEqual(replayed, worklist)

        entries = {entry["taskId"]: entry for entry in worklist["entries"]}
        tasks = []
        for workspace in worklist["workspaces"]:
            task_id = workspace["taskId"]
            worktree = Path(workspace["worktreePath"])
            self.assertTrue(worktree.is_dir())
            (worktree / f"task-{task_id}.txt").write_text(
                f"implemented {task_id}\n", encoding="utf-8"
            )
            git("add", f"task-{task_id}.txt", cwd=worktree)
            git("commit", "-m", f"implement task {task_id}", cwd=worktree)
            head = git("rev-parse", "HEAD", cwd=worktree)
            tasks.append(
                {
                    "entry": entries[task_id],
                    "workspace": workspace,
                    "implementation": {
                        "taskId": task_id,
                        "branch": workspace["branch"],
                        "head": head,
                        "summary": f"Implemented task {task_id}.",
                        "tests": ["focused test: pass"],
                    },
                    "review": {
                        "taskId": task_id,
                        "reviewedHead": head,
                        "verdict": "approve",
                        "summary": f"Reviewed task {task_id}.",
                        "findings": [],
                    },
                }
            )

        culmination = self.invoke(
            "culminate",
            {
                "action": "culminate",
                "source": worklist["source"],
                "repository": "agency/example",
                "checkout": str(self.checkout),
                "baseRev": worklist["baseRev"],
                "baseBranch": "main",
                "reportPath": str(self.report),
                "tasks": tasks,
            },
        )
        self.assertEqual(culmination["status"], "ready")
        self.assertEqual(
            [pull_request["status"] for pull_request in culmination["pullRequests"]],
            ["created"] * 6,
        )
        report = self.report.read_text(encoding="utf-8")
        self.assertIn("# Agency morning report", report)
        self.assertIn("## Human culmination", report)
        self.assertEqual(report.count("Review verdict: **approve**"), 6)
        for task_id in range(201, 207):
            self.assertEqual(
                git(
                    "--git-dir",
                    str(self.remote),
                    "rev-parse",
                    f"refs/heads/agency/nightly/issue-{task_id}",
                ),
                tasks[task_id - 201]["implementation"]["head"],
            )


if __name__ == "__main__":
    unittest.main()
