#!/usr/bin/env python3
"""Focused local-state and lifecycle regressions for the spec-build policy driver."""

from __future__ import annotations

import fcntl
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
import threading
import unittest
from typing import Any
from unittest import mock


SOURCE = Path(
    os.environ.get(
        "SPEC_BUILD_DRIVER_SOURCE",
        Path(__file__).resolve().parents[1] / "drivers/spec_build_driver.py",
    )
)
SPEC = importlib.util.spec_from_file_location("spec_build_driver", SOURCE)
assert SPEC is not None and SPEC.loader is not None
DRIVER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DRIVER)
# The shared worktree manager the driver resolves as a sibling module. Reading
# lane identity back through it is what proves the round-trip.
WORKTREES = DRIVER.worktrees
MARKER_SAFE_CHANGELOG_PREDICATE = (
    'base="$(git config --get tally.baserev)"; '
    'git diff --quiet "$base" HEAD -- && exit 0;\n'
    'git diff --name-only "$base" HEAD -- CHANGELOG.md | grep -qx CHANGELOG.md'
)


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
    return {"number": "7", "url": "local://acme/spec/issues/7"}


def attempt_receipts(root: Path, campaign: str = "fixture") -> dict[str, object]:
    return {
        "schemaVersion": 1,
        "kind": "local-jsonl",
        "path": str(
            root
            / "campaigns"
            / "attempt-receipts"
            / campaign
            / DRIVER.ATTEMPT_RECEIPTS_FILE
        ),
    }


LOCAL_STEERING_REGISTRATION = "0198a62b-41ee-7000-8000-000000000571"
LOCAL_STEERING_ACTOR = "uid:1000"
CAMPAIGN_ID = "0198a62b-41ee-7000-8000-000000000573"


def local_steering_comment(identifier: int, body: str) -> dict[str, object]:
    timestamp = f"2026-08-13T00:00:0{identifier}Z"
    return {
        "id": identifier,
        "url": (
            f"local://campaign/{LOCAL_STEERING_REGISTRATION}/steering/{identifier}"
        ),
        "author": LOCAL_STEERING_ACTOR,
        "body": body,
        "createdAt": timestamp,
        "updatedAt": timestamp,
    }


def local_steering_record(
    identifier: int, body: str, task_id: str | None
) -> dict[str, object]:
    return {
        "schemaVersion": 1,
        "sequence": identifier,
        "registrationId": LOCAL_STEERING_REGISTRATION,
        "taskId": task_id,
        "doNotDispatchBefore": f"2026-08-13T00:00:0{identifier + 1}Z",
        "comment": local_steering_comment(identifier, body),
    }


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


def admit_file_worklist(
    checkout: Path, *, max_tasks: int, max_parallel: int
) -> dict[str, Any]:
    return DRIVER.action_worklist(
        {
            "repository": "acme/spec",
            "repositoryConfig": repository_config(checkout),
            "worklist": "specs/*/tasks.json",
            "maxTasks": max_tasks,
            "maxParallel": max_parallel,
        }
    )


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
        "campaignIdentity": CAMPAIGN_ID,
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


class AttemptReceiptLogTests(unittest.TestCase):
    @staticmethod
    def diagnosis(task_id: str, attempt: int, text: str) -> dict[str, object]:
        return {
            "kind": "diagnosis",
            "taskId": task_id,
            "attempt": attempt,
            "diagnosis": text,
            "redaction": DRIVER.PUBLIC_REDACTION,
        }

    def test_a_torn_tail_is_ignored_then_repaired_before_the_next_append(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = attempt_receipts(root)
            DRIVER.append_attempt_receipt(
                source,
                "fixture",
                "7",
                self.diagnosis("task-1", 1, "Observed the first failure."),
            )
            path = Path(source["path"])
            baseline = DRIVER.campaign_attempt_state(
                source,
                "fixture",
                "7",
                {"task-1"},
            )
            with path.open("ab") as log:
                log.write(b'{"schemaVersion":1,"sequence":2,"kind":"diagn')

            # A crash between write and newline contributes no fact.
            self.assertEqual(
                DRIVER.campaign_attempt_state(
                    source,
                    "fixture",
                    "7",
                    {"task-1"},
                ),
                baseline,
            )
            DRIVER.append_attempt_receipt(
                source,
                "fixture",
                "7",
                self.diagnosis("task-1", 2, "Observed the second failure."),
            )
            self.assertTrue(path.read_bytes().endswith(b"\n"))
            records = DRIVER.read_attempt_receipts(source, "fixture", "7")
            self.assertEqual([record["sequence"] for record in records], [1, 2])
            self.assertEqual(
                [record["attempt"] for record in records],
                [1, 2],
            )

    def test_pardon_generations_fold_in_log_order_without_deleting_history(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = attempt_receipts(root)
            payloads = [
                self.diagnosis("task-1", 1, "Observed failure one."),
                self.diagnosis("task-1", 2, "Observed failure two."),
                {
                    "kind": "retry",
                    "taskId": "task-1",
                    "attempt": 1,
                    "reason": "Stage `merge` faulted.",
                    "redaction": DRIVER.PUBLIC_REDACTION,
                },
                {
                    "kind": "retry",
                    "taskId": "task-1",
                    "attempt": 2,
                    "reason": "Stage `rebase` faulted.",
                    "redaction": DRIVER.PUBLIC_REDACTION,
                },
                {"kind": "escalation", "body": "Escalated the blocked frontier."},
                {
                    "kind": "pardon",
                    "tasks": None,
                    "reason": "Corrected the external dependency.",
                    "actor": "uid:1000",
                    "nonce": "018f47a0-7b9d-7cc2-92d6-2f7f19f505fd",
                },
                self.diagnosis("task-1", 1, "Observed the first post-pardon failure."),
            ]
            for payload in payloads:
                DRIVER.append_attempt_receipt(source, "fixture", "7", payload)

            diagnoses, retries, escalation, warnings = DRIVER.campaign_attempt_state(
                source,
                "fixture",
                "7",
                {"task-1"},
            )
            self.assertEqual(
                [(record["attempt"], record["diagnosis"]) for record in diagnoses],
                [(1, "Observed the first post-pardon failure.")],
            )
            self.assertEqual(retries, [])
            self.assertIsNone(escalation)
            self.assertEqual(
                warnings,
                [
                    "campaign pardon local://campaign/fixture/attempt-receipts/6 "
                    "pardoned 5 earlier machine receipt(s)"
                ],
            )
            self.assertEqual(
                len(DRIVER.read_attempt_receipts(source, "fixture", "7")),
                7,
                "a pardon must retain the append-only audit trail",
            )

    def test_append_holds_flock_and_fsyncs_the_new_log(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = attempt_receipts(root)
            path = Path(source["path"])
            real_fsync = os.fsync
            with mock.patch.object(DRIVER.os, "fsync", wraps=real_fsync) as fsync:
                DRIVER.append_attempt_receipt(
                    source,
                    "fixture",
                    "7",
                    self.diagnosis("task-1", 1, "Observed the first failure."),
                )
            self.assertGreaterEqual(fsync.call_count, 2, "file and directory must be synced")
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

            held = os.open(path, os.O_RDWR | os.O_CLOEXEC)
            real_flock = fcntl.flock
            real_flock(held, fcntl.LOCK_EX)
            attempted = threading.Event()
            finished = threading.Event()
            failures: list[Exception] = []

            def append_second() -> None:
                try:
                    DRIVER.append_attempt_receipt(
                        source,
                        "fixture",
                        "7",
                        self.diagnosis("task-1", 2, "Observed the second failure."),
                    )
                except Exception as error:
                    failures.append(error)
                finally:
                    finished.set()

            def observed_flock(lock: object, operation: int) -> None:
                if operation == fcntl.LOCK_EX:
                    attempted.set()
                real_flock(lock, operation)

            with mock.patch.object(DRIVER.fcntl, "flock", side_effect=observed_flock):
                worker = threading.Thread(target=append_second)
                worker.start()
                try:
                    self.assertTrue(attempted.wait(2), "append never attempted the log lock")
                    self.assertFalse(
                        finished.wait(0.1), "append crossed a held attempt-receipts flock"
                    )
                finally:
                    real_flock(held, fcntl.LOCK_UN)
                    os.close(held)
                    worker.join(5)
            self.assertFalse(worker.is_alive())
            self.assertEqual(failures, [])


class CampaignDriverTests(unittest.TestCase):
    def test_completion_markers_carry_the_task_revision(self) -> None:
        revision = "sha256:" + "a" * 64
        marker = DRIVER.pull_request_marker("fixture", "7", "task-1", revision)

        self.assertEqual(
            marker,
            "<!-- tally:spec-build:v2 campaign=fixture issue=7 task=task-1 "
            f"revision={revision} -->",
        )
        with self.assertRaisesRegex(
            DRIVER.DriverError, "revision must be a lowercase SHA-256 identity"
        ):
            DRIVER.pull_request_marker("fixture", "7", "task-1", None)

    def test_file_worklist_tasks_carry_v2_completion_revisions(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            worklist = checkout / "specs/campaign/tasks.json"
            worklist.parent.mkdir(parents=True)
            worklist.write_text(
                json.dumps(
                    {
                        "schemaVersion": 1,
                        "tasks": [
                            task("task-1"),
                            {
                                "id": "verify",
                                "kind": "checkpoint",
                                "title": "Verify the task",
                                "argv": ["true"],
                                "runtimeMaxSec": 60,
                                "dependencies": ["task-1"],
                            },
                        ],
                    }
                ),
                encoding="utf-8",
            )
            git(checkout, "add", str(worklist.relative_to(checkout)))
            git(checkout, "commit", "--quiet", "-m", "add worklist")
            git(checkout, "push", "--quiet", "origin", "main")

            admitted = admit_file_worklist(checkout, max_tasks=2, max_parallel=1)

            for admitted_task in admitted["tasks"]:
                self.assertRegex(admitted_task["revision"], r"^sha256:[0-9a-f]{64}$")
            implementation = admitted["tasks"][0]
            marker = DRIVER.pull_request_marker(
                "fixture", "7", implementation["id"], implementation["revision"]
            )
            self.assertEqual(
                marker,
                "<!-- tally:spec-build:v2 campaign=fixture issue=7 task=task-1 "
                f"revision={implementation['revision']} -->",
            )

    def test_pre_post_refresh_refuses_quiescence_after_the_frontier_reopens(self) -> None:
        """A terminal decision may not publish from its earlier empty-frontier read."""
        brief = {
            "campaign": "fixture",
            "repository": "acme/spec",
            "repositoryConfig": {
                "checkout": "/tmp/fixture",
                "baseBranch": "main",
                "remote": "origin",
                "forge": "local",
            },
            "issue": issue(),
            "worklist": "specs/*/tasks.json",
            "maxTasks": 1,
            "maxParallel": 1,
        }
        empty_frontier = {
            "complete": False,
            "quiescent": True,
            "escalation": None,
            "diagnoses": [{"taskId": "task-1"}],
            "retries": [],
        }
        reopened_frontier = {
            "complete": False,
            "quiescent": False,
            "escalation": None,
            "frontier": [task("task-1")],
        }
        with (
            mock.patch.object(
                DRIVER,
                "action_reconcile",
                side_effect=[empty_frontier, reopened_frontier],
            ) as reconcile,
            mock.patch.object(DRIVER, "publish_closing_summary") as publish,
            mock.patch.object(DRIVER, "run") as run,
        ):
            with self.assertRaisesRegex(
                DRIVER.DriverError,
                "pre-post durable refresh.*refusing to post outcome=quiescent",
            ):
                DRIVER.action_escalate(brief)

        self.assertEqual(reconcile.call_count, 2)
        publish.assert_not_called()
        run.assert_not_called()

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
                "attemptReceipts": attempt_receipts(root),
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
                        "attemptReceipts": attempt_receipts(root),
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

    def test_continuation_keeps_its_durable_local_receipt(self) -> None:
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
            self.assertTrue(listed, "the local repository kept no continuation receipt")
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
                "repositoryConfig": repository_config(checkout),
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

class LaneLifecycleTests(unittest.TestCase):
    def test_fresh_lane_cuts_serialize_on_the_checkout_git_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root)
            worktree = root / "workspaces" / "task-1"
            lock_path = WORKTREES.worktree_preparation_lock_path(checkout)
            lock_path.parent.mkdir(parents=True, exist_ok=True)
            attempted = threading.Event()
            finished = threading.Event()
            failures: list[Exception] = []
            real_flock = fcntl.flock

            def observed_flock(lock: object, operation: int) -> None:
                attempted.set()
                real_flock(lock, operation)

            def add_lane() -> None:
                try:
                    WORKTREES.add(checkout, worktree, "lane-task-1", "HEAD")
                except Exception as error:
                    failures.append(error)
                finally:
                    finished.set()

            descriptor = os.open(
                lock_path,
                os.O_CREAT | os.O_RDWR | os.O_CLOEXEC | os.O_NOFOLLOW,
                0o600,
            )
            with os.fdopen(descriptor, "a+", encoding="utf-8") as held_lock:
                real_flock(held_lock, fcntl.LOCK_EX)
                with mock.patch.object(WORKTREES.fcntl, "flock", side_effect=observed_flock):
                    worker = threading.Thread(target=add_lane)
                    worker.start()
                    try:
                        self.assertTrue(
                            attempted.wait(2), "lane cut never attempted the metadata lock"
                        )
                        self.assertFalse(
                            finished.wait(0.1),
                            "lane cut mutated shared git metadata while its lock was held",
                        )
                    finally:
                        real_flock(held_lock, fcntl.LOCK_UN)
                        worker.join(5)

            self.assertFalse(worker.is_alive(), "lane cut did not resume after lock release")
            self.assertEqual(failures, [])
            self.assertTrue(worktree.is_dir())

    def test_publish_posts_bounded_redacted_worker_findings_or_stays_silent(self) -> None:
        agent_task_uuid = "019fecc0-bbad-7153-969b-51174cf064ca"
        secret = "ghp_0123456789abcdefghijklmnopqrstuvwxyz"
        cases = (
            (
                "present",
                {
                    "taskUuid": agent_task_uuid,
                    "message": f"GITHUB_TOKEN={secret}\nJudgement: " + "é" * 10_000,
                },
            ),
            ("absent", None),
        )
        for name, findings in cases:
            with self.subTest(case=name), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                checkout, _ = initialize_repository(root, remote=True)
                campaign_task = {
                    **task("task-1"),
                    "revision": "sha256:" + "a" * 64,
                }
                prepared_brief = prep_brief(
                    checkout,
                    root / "workspaces",
                    f"findings-{name}",
                )
                prepared_brief["task"] = campaign_task
                prepared = DRIVER.action_prep(prepared_brief)
                worktree = Path(prepared["worktreePath"])
                (worktree / "task-1").write_text("implemented\n", encoding="utf-8")
                git(worktree, "add", "task-1")
                git(worktree, "commit", "--quiet", "-m", "implement task 1")

                config = repository_config(checkout)
                publication = DRIVER.action_publish(
                    {
                        "campaign": "fixture",
                        "campaignIdentity": CAMPAIGN_ID,
                        "repository": "acme/spec",
                        "repositoryConfig": config,
                        "issue": issue(),
                        "runId": f"findings-{name}",
                        "workspaceRoot": str(root / "workspaces"),
                        "task": campaign_task,
                        "domainsRequired": True,
                        "gates": [],
                        "steward": None,
                        "workspace": prepared,
                        "constraints": [],
                        "workerFindings": findings,
                    }
                )
                ref = (
                    f"{DRIVER.local_state_prefix('fixture', '7')}/findings/"
                    f"task-1/{agent_task_uuid}"
                )

                if findings is None:
                    self.assertEqual(DRIVER.local_remote_refs(config, ref), {})
                    continue

                receipt = DRIVER.read_local_blob(config, ref)
                self.assertEqual(receipt["kind"], "worker-findings")
                comment = receipt["body"]
                self.assertTrue(comment.startswith("### Worker findings"))
                self.assertIn("[redacted sensitive diagnosis line]", comment)
                self.assertIn(DRIVER.WORKER_FINDINGS_TRUNCATION.strip(), comment)
                self.assertNotIn(secret, comment)
                self.assertLessEqual(
                    len(comment.encode("utf-8")), DRIVER.MAX_WORKER_FINDINGS_BYTES
                )
                self.assertEqual(
                    publication["pullRequest"],
                    f"local://acme/spec/{prepared['publishBranch']}",
                )

    def test_content_lane_without_a_changelog_touch_fails_the_gate(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            (checkout / "CHANGELOG.md").write_text("# Changelog\n", encoding="utf-8")
            git(checkout, "add", "CHANGELOG.md")
            git(checkout, "commit", "--quiet", "-m", "add changelog")
            git(checkout, "push", "--quiet", "origin", "main")
            prepared = DRIVER.action_prep(
                prep_brief(checkout, root / "workspaces", "changelog-content-fail")
            )
            worktree = Path(prepared["worktreePath"])
            (worktree / "content.txt").write_text("content\n", encoding="utf-8")
            git(worktree, "add", "content.txt")
            git(worktree, "commit", "--quiet", "-m", "content without changelog")

            gated = command(
                "sh",
                "-euc",
                MARKER_SAFE_CHANGELOG_PREDICATE,
                cwd=worktree,
                check=False,
            )

            self.assertEqual(gated.returncode, 1, gated.stderr)

    def test_local_prep_uses_integration_after_the_remote_worklist_diverges(
        self,
    ) -> None:
        """Remote worklist commits do not become the local lane merge target."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            config = DRIVER.repo_config(repository_config(checkout))
            initial = git(checkout, "rev-parse", "origin/main")
            integration_branch = DRIVER.integration_branch(
                "fixture", CAMPAIGN_ID
            )
            DRIVER.ensure_integration_branch(
                config, "fixture", CAMPAIGN_ID, initial
            )

            git(checkout, "switch", "--quiet", "-c", "integrated-task")
            git(
                checkout,
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "fixture: integrate task-0",
            )
            integration_tip = git(checkout, "rev-parse", "HEAD")
            git(
                checkout,
                "update-ref",
                f"refs/heads/{integration_branch}",
                integration_tip,
                initial,
            )

            git(checkout, "switch", "--quiet", "main")
            (checkout / "worklist.json").write_text("{}\n", encoding="utf-8")
            git(checkout, "add", "worklist.json")
            git(checkout, "commit", "--quiet", "-m", "operator: revise worklist")
            git(checkout, "push", "--quiet", "origin", "main")
            remote_tip = git(checkout, "rev-parse", "origin/main")
            self.assertNotEqual(remote_tip, integration_tip)

            # Reconciliation observes the new worklist revision but preserves
            # the already-advanced campaign branch as the code-history base.
            self.assertEqual(
                DRIVER.ensure_integration_branch(
                    config, "fixture", CAMPAIGN_ID, remote_tip
                ),
                integration_tip,
            )
            prepared = DRIVER.action_prep(
                prep_brief(
                    checkout,
                    workspace_root,
                    "diverged-worklist-pass",
                    source_revision=integration_tip,
                )
            )
            self.assertEqual(prepared["baseRev"], integration_tip)
            self.assertEqual(
                git(Path(prepared["worktreePath"]), "rev-parse", "HEAD"),
                integration_tip,
            )
            self.assertEqual(git(checkout, "rev-parse", "origin/main"), remote_tip)

    def test_local_checkpoint_validates_the_integration_tip(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            config = DRIVER.repo_config(repository_config(checkout))
            initial = git(checkout, "rev-parse", "origin/main")
            integration_branch = DRIVER.integration_branch(
                "fixture", CAMPAIGN_ID
            )
            DRIVER.ensure_integration_branch(
                config, "fixture", CAMPAIGN_ID, initial
            )

            git(checkout, "switch", "--quiet", "-c", "checkpoint-lane")
            git(
                checkout,
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "fixture: integrate task-1",
            )
            integration_tip = git(checkout, "rev-parse", "HEAD")
            git(
                checkout,
                "update-ref",
                f"refs/heads/{integration_branch}",
                integration_tip,
                initial,
            )

            recorded = DRIVER.action_checkpoint(
                {
                    "campaign": "fixture",
                    "campaignIdentity": CAMPAIGN_ID,
                    "repository": "acme/spec",
                    "repositoryConfig": repository_config(checkout),
                    "issue": issue(),
                    "task": {
                        "id": "phase-checkpoint",
                        "kind": "checkpoint",
                        "title": "Validate phase one",
                        "argv": ["true"],
                        "runtimeMaxSec": 60,
                        "dependencies": ["task-1"],
                    },
                    "source": {
                        "path": "worklist.json",
                        "sha256": "sha256:" + "a" * 64,
                        "revision": initial,
                    },
                    "baseRevision": integration_tip,
                    "workspace": {
                        "taskId": "phase-checkpoint",
                        "baseRev": integration_tip,
                        "branch": "checkpoint-lane",
                        "publishBranch": "unused-checkpoint-branch",
                        "worktreePath": str(checkout),
                    },
                }
            )
            self.assertEqual(recorded["revision"], integration_tip)
            self.assertEqual(git(checkout, "rev-parse", "origin/main"), initial)
            self.assertEqual(
                git(checkout, "ls-remote", "origin", recorded["ref"]).split()[0],
                integration_tip,
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

    def test_closing_summary_is_a_durable_local_blob(self) -> None:
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
                DRIVER.publish_closing_summary(
                    "acme/spec",
                    config,
                    "fixture",
                    "7",
                    digest,
                ),
                receipt,
            )
            self.assertEqual(DRIVER.read_local_blob(config, ref), stored)

    def test_conflicting_published_head_is_aborted_abandoned_and_rebuilt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            stable_branch = DRIVER.stable_publish_branch(
                "fixture", CAMPAIGN_ID, "task-1"
            )
            config = DRIVER.repo_config(repository_config(checkout))
            initial = DRIVER.ensure_integration_branch(
                config,
                "fixture",
                CAMPAIGN_ID,
                git(checkout, "rev-parse", "origin/main"),
            )

            git(checkout, "switch", "--quiet", "-c", "published")
            (checkout / "root.go").write_text("task\n", encoding="utf-8")
            git(checkout, "commit", "--quiet", "-am", "task change")
            published_head = git(checkout, "rev-parse", "HEAD")
            git(checkout, "update-ref", f"refs/heads/{stable_branch}", published_head)
            git(checkout, "switch", "--quiet", "main")
            (checkout / "root.go").write_text("main\n", encoding="utf-8")
            git(checkout, "commit", "--quiet", "-am", "base change")
            current_main = git(checkout, "rev-parse", "HEAD")
            git(
                checkout,
                "update-ref",
                f"refs/heads/{DRIVER.integration_branch('fixture', CAMPAIGN_ID)}",
                current_main,
                initial,
            )

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
                    "show-ref",
                    "--verify",
                    "--quiet",
                    f"refs/heads/{stable_branch}",
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
            stable_branch = DRIVER.stable_publish_branch(
                "fixture", CAMPAIGN_ID, "task-1"
            )
            config = DRIVER.repo_config(repository_config(checkout))
            initial = DRIVER.ensure_integration_branch(
                config,
                "fixture",
                CAMPAIGN_ID,
                git(checkout, "rev-parse", "origin/main"),
            )

            git(checkout, "switch", "--quiet", "-c", "published")
            (checkout / "root.go").write_text("task\n", encoding="utf-8")
            git(checkout, "commit", "--quiet", "-am", "task change")
            published_head = git(checkout, "rev-parse", "HEAD")
            git(checkout, "update-ref", f"refs/heads/{stable_branch}", published_head)
            git(checkout, "switch", "--quiet", "main")
            (checkout / "main-only.txt").write_text("main\n", encoding="utf-8")
            git(checkout, "add", "main-only.txt")
            git(checkout, "commit", "--quiet", "-m", "independent base change")
            current_main = git(checkout, "rev-parse", "HEAD")
            git(
                checkout,
                "update-ref",
                f"refs/heads/{DRIVER.integration_branch('fixture', CAMPAIGN_ID)}",
                current_main,
                initial,
            )

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
                    "show-ref",
                    "--verify",
                    "--quiet",
                    f"refs/heads/{stable_branch}",
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
                    "body": "Noted an update.\n\n<!-- tally:spec-build:v2 campaign=fixture -->",
                },
                "managed campaign marker",
            ),
            # Release narration can become public projection prose. A narrator
            # that proposes a closing keyword is claiming authority over an
            # issue the release renderer did not name.
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
            # #385: the body is PR prose and the squash commit body -- the one
            # surface where the outcome-first grammar reaches a public pull
            # request. `OutcomeFirstGrammarTests` exercises the rules in
            # isolation; these four prove `validated_narration` itself refuses
            # a body that breaks each one, so the call site cannot be deleted
            # while the suite stays green.
            "body-present-tense-opening": (
                {"type": "feat", "subject": "do a thing", "body": "fixing the drift."},
                "past-tense verb",
            ),
            "body-opens-with-a-list": (
                {"type": "feat", "subject": "do a thing", "body": "- fixed the drift"},
                "must open with a sentence, not a list",
            ),
            "body-leading-sentence-unterminated": (
                {"type": "feat", "subject": "do a thing", "body": "Fixed the drift"},
                "leading sentence must end with a period",
            ),
            "body-exclamation": (
                {"type": "feat", "subject": "do a thing", "body": "Fixed the drift!"},
                "exclamation mark",
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
        """Decode the complete role shape emitted by Rust/the Nix module."""
        return DRIVER.steward_role(
            {
                "adapter": "narrator",
                "argv": argv,
                "env": {},
                "finalMessagePattern": "^TALLY_FINAL_MESSAGE=(.*)$",
                "runtimeMaxSec": 30,
                **overrides,
            }
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
                "internal campaign contract violation",
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
                    self.role(["/bin/true"], **override)

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


class LocalSteeringRecheckTests(unittest.TestCase):
    def source(self, root: Path, prepared_cursor: int) -> dict[str, object]:
        directory = root / "campaigns" / "steering" / LOCAL_STEERING_REGISTRATION
        directory.mkdir(parents=True)
        log = directory / "steering-v1.jsonl"
        lock = directory / "steering.lock"
        log.touch()
        lock.touch()
        return {
            "schemaVersion": 1,
            "kind": "local-jsonl",
            "registrationId": LOCAL_STEERING_REGISTRATION,
            "localActor": LOCAL_STEERING_ACTOR,
            "logPath": str(log),
            "lockPath": str(lock),
            "preparedCursor": prepared_cursor,
        }

    def brief(
        self,
        source: dict[str, object],
        prepared: list[dict[str, object]],
    ) -> dict[str, object]:
        return {
            "campaign": "fixture",
            "campaignIdentity": LOCAL_STEERING_REGISTRATION,
            "taskId": "task-1",
            "localActor": LOCAL_STEERING_ACTOR,
            "steeringSource": source,
            "preparedComments": prepared,
        }

    def write_records(
        self, source: dict[str, object], records: list[dict[str, object]]
    ) -> None:
        Path(str(source["logPath"])).write_text(
            "".join(
                json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n"
                for record in records
            ),
            encoding="utf-8",
        )

    def test_local_high_water_fold_preserves_comments_and_witnesses_late_ids(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            source = self.source(Path(temporary), 1)
            prepared = local_steering_comment(1, "Keep the bounded path.")
            late = local_steering_record(2, "Use the local receipt.", "task-1")
            unrelated = local_steering_record(3, "Only task two sees this.", "task-2")
            self.write_records(
                source,
                [
                    local_steering_record(1, "Keep the bounded path.", None),
                    late,
                    unrelated,
                ],
            )

            result = DRIVER.action_steering_recheck(
                self.brief(source, [prepared])
            )

            self.assertEqual(
                result["authorizedComments"],
                [prepared, late["comment"]],
            )
            self.assertEqual(
                result["receipt"],
                {
                    "source": {
                        "kind": "local-jsonl",
                        "registrationId": LOCAL_STEERING_REGISTRATION,
                        "path": source["logPath"],
                        "preparedCursor": 1,
                        "recheckedCursor": 3,
                    },
                    "rechecked": True,
                    "recheckTruncated": False,
                    "preparedCommentIds": [1],
                    "lateRecheckCommentIds": [2],
                },
            )

    def test_append_only_edit_detection_fold_is_retained(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            source = self.source(Path(temporary), 1)
            before = local_steering_comment(1, "Use the first direction.")
            after = local_steering_record(1, "Use the corrected direction.", None)
            self.write_records(source, [after])

            result = DRIVER.action_steering_recheck(self.brief(source, [before]))

            self.assertEqual(result["authorizedComments"], [after["comment"]])
            self.assertEqual(result["receipt"]["lateRecheckCommentIds"], [1])

    def test_partial_local_record_is_refused_instead_of_silently_skipped(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            source = self.source(Path(temporary), 0)
            Path(str(source["logPath"])).write_text("{", encoding="utf-8")

            with self.assertRaisesRegex(
                DRIVER.DriverError, "incomplete final record"
            ):
                DRIVER.action_steering_recheck(self.brief(source, []))

    def test_local_record_embargo_must_be_one_second_after_creation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            source = self.source(Path(temporary), 0)
            record = local_steering_record(1, "Keep the embargo.", None)
            record["doNotDispatchBefore"] = "2026-08-13T00:00:09Z"
            self.write_records(source, [record])

            with self.assertRaisesRegex(
                DRIVER.DriverError, "inconsistent append-only timestamps"
            ):
                DRIVER.action_steering_recheck(self.brief(source, []))

    def test_each_append_must_push_the_dispatch_embargo(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            source = self.source(Path(temporary), 0)
            first = local_steering_record(1, "First.", None)
            second = local_steering_record(2, "Second.", "task-1")
            second["comment"]["createdAt"] = first["comment"]["createdAt"]
            second["comment"]["updatedAt"] = first["comment"]["updatedAt"]
            second["doNotDispatchBefore"] = first["doNotDispatchBefore"]
            self.write_records(source, [first, second])

            with self.assertRaisesRegex(
                DRIVER.DriverError, "does not advance doNotDispatchBefore"
            ):
                DRIVER.action_steering_recheck(self.brief(source, []))


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
            "attemptReceipts": attempt_receipts(root),
        }
        base.update(overrides)
        return base

    def receipt_record(
        self, config: dict[str, object], steered: dict[str, object]
    ) -> dict[str, object]:
        sequence = int(str(steered["comment"]).rsplit("/", 1)[1])
        source = attempt_receipts(Path(config["checkout"]).parent)
        records = DRIVER.read_attempt_receipts(source, "fixture", "7")
        return records[sequence - 1]

    def receipt_body(self, config: dict[str, object], steered: dict[str, object]) -> str:
        blob = self.receipt_record(config, steered)
        return str(blob.get("diagnosis", blob.get("reason")))

    def test_a_grammar_rejected_excerpt_is_recorded_as_machine_steering(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            config = repository_config(checkout, "local")
            secret = "ghp_0123456789abcdefghijklmnopqrstuvwxyz"
            rejected = f"narrow the failing gate without exposing {secret}"
            reason, _, _ = DRIVER.diagnosis_rejection_reason(rejected, None)
            self.assertIsNotNone(reason)
            steered = DRIVER.action_steer(
                self.brief(root, checkout, diagnosis=rejected)
            )
            self.assertTrue(steered["posted"])
            self.assertEqual(steered["kind"], "diagnosis")
            blob = self.receipt_record(DRIVER.repo_config(config), steered)
            self.assertEqual(blob["kind"], "diagnosis")
            body = str(blob["diagnosis"])
            self.assertIn("grammar-rejected", body)
            self.assertIn("must end with a period", body)
            self.assertIn("Redacted proposal excerpt:", body)
            self.assertIn("[redacted-token]", body)
            self.assertNotIn(secret, body)
            recorded_reason, _, _ = DRIVER.diagnosis_rejection_reason(body, None)
            self.assertIsNone(recorded_reason)
            diagnoses, retries, _, _ = DRIVER.campaign_attempt_state(
                attempt_receipts(root),
                "fixture",
                "7",
                {"task-1"},
            )
            self.assertEqual(len(diagnoses), 1)
            self.assertEqual(retries, [])

    def test_gate_evidence_requires_the_failing_id_and_offending_path(self) -> None:
        detail = (
            "forbidPaths gate 'forbid-secrets' rejected 1 path(s) touched in lane "
            "history (a later removal does not clear this; the path must never "
            "appear in any lane commit): "
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
            body = self.receipt_body(DRIVER.repo_config(config), omitted)
            self.assertEqual(omitted["kind"], "diagnosis")
            self.assertIn("grammar-rejected", body)
            self.assertIn("omits the failing check id", body)
            self.assertIn("gate:forbid-secrets", body)
            self.assertIn("secrets/key.pem", body)

    def test_a_diagnosis_naming_the_required_evidence_is_accepted_verbatim(self) -> None:
        detail = (
            "forbidPaths gate 'forbid-secrets' rejected 1 path(s) touched in lane "
            "history (a later removal does not clear this; the path must never "
            "appear in any lane commit): "
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
            body = self.receipt_body(DRIVER.repo_config(config), steered)
            reason, _, _ = DRIVER.diagnosis_rejection_reason(
                diagnosis, {"id": "gate:forbid-secrets", "detail": detail}
            )
            self.assertIsNone(reason)
            self.assertEqual(steered["kind"], "diagnosis")
            self.assertEqual(body, diagnosis)
            self.assertNotIn("grammar-rejected", body)


class BreachSteeringTests(unittest.TestCase):
    """#386's breach-abort surface, pinned. Round-1 F3 and F5.

    The eval mutated `breach = False` and dropped the witnessed evidence from
    the durable receipt; every suite stayed green both times, so the whole
    downstream of `failureClass` returning `"breach"` was unpinned. These
    tests run `action_steer` against the local repository harness as a real
    process would.
    """

    DETAIL = (
        "tree-delta gate detected 2 out-of-allowlist change(s) (declared "
        'allowlist): changed "internal/cli/root.go"; appeared "secrets/leak.pem"'
    )

    def brief(self, checkout: Path, **overrides: object) -> dict[str, object]:
        base = {
            "campaign": "fixture",
            "repository": "acme/spec",
            "repositoryConfig": repository_config(checkout, "local"),
            "issue": issue(),
            "taskId": "task-1",
            "attempt": 1,
            "diagnosis": "Investigated the out-of-allowlist writes.",
            "breach": True,
            "breachDetail": self.DETAIL,
            "attemptReceipts": attempt_receipts(checkout.parent),
        }
        base.update(overrides)
        return base

    def blob(self, config: dict[str, object], attempt: int) -> dict[str, object]:
        records = DRIVER.read_attempt_receipts(
            attempt_receipts(Path(config["checkout"]).parent), "fixture", "7"
        )
        return next(
            record
            for record in records
            if record["kind"] == "diagnosis"
            and record["taskId"] == "task-1"
            and record["attempt"] == attempt
        )

    def test_a_breach_records_both_receipts_in_one_call_and_blocks(self) -> None:
        """Kills MUT-4: a breach handled as an ordinary one-attempt gate-fail.

        Attempt 2 must exist as of this single call, because the reconciler's
        `attempt == 2` rule is what makes the task permanently blocked. One
        receipt would leave the lane redispatchable — a retried breach, which
        is the distinction the whole issue turns on.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            config = DRIVER.repo_config(repository_config(checkout, "local"))

            steered = DRIVER.action_steer(self.brief(checkout))

            self.assertTrue(steered["posted"])
            self.assertTrue(steered["blocked"])
            self.assertEqual(steered["attempt"], 2)
            # Both receipts, from this one call. The ordered fold would
            # drop a lone attempt 2, so attempt 1 has to be there too.
            for attempt in (1, 2):
                blob = self.blob(config, attempt)
                self.assertEqual(blob["kind"], "diagnosis")
                self.assertEqual(blob["attempt"], attempt)
            # And the reconciler reads them back as a blocking pair.
            diagnoses, _, _, _ = DRIVER.campaign_attempt_state(
                attempt_receipts(root),
                "fixture",
                "7",
                {"task-1"},
            )
            self.assertEqual(
                [(item["taskId"], item["attempt"]) for item in diagnoses],
                [("task-1", 1), ("task-1", 2)],
            )

    def test_the_offending_paths_are_witnessed_in_the_recorded_breach_body(self) -> None:
        """Kills MUT-3b: the witnessed evidence dropped from the receipt.

        The gate's own failure message naming the paths is already pinned;
        this pins the other surface the issue requires — the paths reaching
        the durable record folded by the next pass.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            config = DRIVER.repo_config(repository_config(checkout, "local"))

            DRIVER.action_steer(self.brief(checkout))

            for attempt in (1, 2):
                body = self.blob(config, attempt)["diagnosis"]
                self.assertIn("Aborted the lane", body)
                self.assertIn("will not be retried", body)
                self.assertIn("Witnessed evidence:", body)
                self.assertIn("internal/cli/root.go", body)
                self.assertIn("secrets/leak.pem", body)

    def test_an_ungated_abort_never_claims_a_write_it_did_not_establish(self) -> None:
        """#424: the two lane-aborting tree-delta verdicts are different facts.

        A gate that could not judge a pass -- no ownership, no declared
        domains, no allowlist -- aborts the lane for the same reason a breach
        does, but it has established nothing about what the agent wrote. The
        durable receipt is the operator's record, so it must say which one
        happened, and the breach sentence must not appear over a refusal.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            config = DRIVER.repo_config(repository_config(checkout, "local"))

            steered = DRIVER.action_steer(
                self.brief(
                    checkout,
                    abortReason="tree-delta-ungated",
                    breachDetail=(
                        "tree-delta gate refuses to judge task 'task-1': its agent "
                        "node failed, so the ownership node never ran"
                    ),
                    diagnosis="Recorded what the failing attempt was doing.",
                )
            )
            # It still aborts: both receipts, blocked as of this call.
            self.assertTrue(steered["blocked"])
            self.assertEqual(steered["attempt"], 2)

            for attempt in (1, 2):
                body = self.blob(config, attempt)["diagnosis"]
                self.assertIn("could not judge this pass", body)
                self.assertIn("declares no conflictDomains", body)
                self.assertIn("No out-of-allowlist change has been established", body)
                self.assertIn("will not be retried", body)
                # The #386 sentence claims a write was found. It must not be
                # recorded over a verdict that found nothing.
                self.assertNotIn("permission breach found", body)

    def test_an_unknown_abort_reason_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            with self.assertRaisesRegex(DRIVER.DriverError, "abortReason"):
                DRIVER.action_steer(self.brief(checkout, abortReason="whatever"))

    def test_a_breach_without_an_abort_reason_keeps_its_own_sentence(self) -> None:
        """The #386 caller sent no `abortReason` and still must not change."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            config = DRIVER.repo_config(repository_config(checkout, "local"))

            DRIVER.action_steer(self.brief(checkout))

            body = self.blob(config, 1)["diagnosis"]
            self.assertIn("permission breach found", body)
            self.assertNotIn("could not judge this pass", body)

    def test_a_breach_with_a_rejected_diagnosis_still_aborts_and_witnesses(self) -> None:
        """Round-1 F3: the breach path ran no validation at all.

        The same prose the ordinary path refuses outright was redacted,
        bounded and recorded verbatim. Now it is refused identically — and
        refusing it must replace the prose without swallowing the breach.
        """
        bad = "fix it now!!! this lane is a disaster and I will not explain why"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            config = DRIVER.repo_config(repository_config(checkout, "local"))

            steered = DRIVER.action_steer(self.brief(checkout, diagnosis=bad))

            body = self.blob(config, 1)["diagnosis"]
            self.assertIn("disaster", body)
            self.assertIn("Validation rejected the proposal", body)
            self.assertIn("exclamation mark", body)
            # Rejection replaces the prose; it does not swallow the breach.
            self.assertTrue(steered["blocked"])
            self.assertTrue(steered["posted"])
            self.assertIn("Aborted the lane", body)
            self.assertIn("secrets/leak.pem", body)

    def test_the_composed_breach_body_respects_the_public_length_bound(self) -> None:
        """The ordinary path bounds what it records; the breach path must too.

        Concatenating two separately bounded strings gave ~2x the bound. The
        squeeze falls on the steward's prose, never on the evidence.
        """
        # The largest diagnosis the input validator admits. Composed with the
        # label and the evidence it overflows, which is the case that used to
        # record ~2x the bound.
        lead = "Investigated the writes. "
        prose = lead + ("x" * (DRIVER.MAX_DIAGNOSIS_CHARS - len(lead)))
        self.assertEqual(len(prose), DRIVER.MAX_DIAGNOSIS_CHARS)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            config = DRIVER.repo_config(repository_config(checkout, "local"))

            DRIVER.action_steer(self.brief(checkout, diagnosis=prose))

            body = self.blob(config, 1)["diagnosis"]
            self.assertLessEqual(len(body), DRIVER.MAX_DIAGNOSIS_CHARS)
            # The load-bearing halves survived the squeeze.
            self.assertIn("Aborted the lane", body)
            self.assertIn("secrets/leak.pem", body)


class SquashMergeTests(unittest.TestCase):
    """Squash integration, and the proofs that replace head ancestry."""

    def integration(self, checkout: Path, branch: str, head: str) -> dict[str, object]:
        config = DRIVER.repo_config(repository_config(checkout))
        base = DRIVER.ensure_integration_branch(
            config,
            "fixture",
            CAMPAIGN_ID,
            git(checkout, "rev-parse", "origin/main"),
        )
        return {
            "taskId": "task-1",
            "branch": branch,
            "baseRev": base,
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
        head = git(checkout, "rev-parse", "HEAD")
        git(checkout, "update-ref", f"refs/heads/{branch}", head)
        git(checkout, "switch", "--quiet", "main")
        return head

    def test_local_squash_lands_one_commit_and_a_readable_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            workspace_root.mkdir()
            revision = "sha256:" + "a" * 64
            branch = DRIVER.stable_publish_branch(
                "fixture", CAMPAIGN_ID, "task-1", revision
            )
            head = self.publish_branch(checkout, branch)
            config = DRIVER.repo_config(repository_config(checkout))
            data = {
                "campaign": "fixture",
                "campaignIdentity": CAMPAIGN_ID,
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
            base = git(
                checkout,
                "rev-parse",
                DRIVER.integration_branch("fixture", CAMPAIGN_ID),
            )
            self.assertEqual(base, merge_commit)
            self.assertNotEqual(base, git(checkout, "rev-parse", "origin/main"))
            # One parent: a squash, not a merge commit.
            self.assertEqual(len(git(checkout, "log", "-1", "--format=%P", base).split()), 1)
            self.assertEqual(
                git(checkout, "log", "-1", "--format=%B", base).strip(),
                "feat(fixture): deliver the first task\n\nSteward-authored body.\n\n"
                + DRIVER.pull_request_marker(
                    "fixture", "7", "task-1", revision
                ),
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
            receipt = DRIVER.merge_receipt_ref(
                "fixture", CAMPAIGN_ID, "task-1", revision
            )
            self.assertEqual(
                DRIVER.local_refs(checkout, receipt).get(receipt), merge_commit
            )
            facts = DRIVER.merged_local_tasks(
                "acme/spec",
                config,
                "fixture",
                CAMPAIGN_ID,
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
            # The marker on canonical integration history is the oracle. A
            # damaged receipt may lose its indexing value, but cannot veto
            # that independently readable completion fact.
            git(
                checkout,
                "update-ref",
                receipt,
                git(checkout, "rev-parse", f"{merge_commit}^"),
            )
            self.assertEqual(
                DRIVER.merged_local_tasks(
                    "acme/spec",
                    config,
                    "fixture",
                    CAMPAIGN_ID,
                    "7",
                    None,
                    [{**task("task-1"), "revision": revision}],
                ),
                facts,
            )

    def test_a_moved_integration_branch_is_refused_and_mergeable_next_pass(self) -> None:
        """The actual-base guard catches a sibling merge after the regate."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            workspace_root.mkdir()
            revision = "sha256:" + "a" * 64
            branch = DRIVER.stable_publish_branch(
                "fixture", CAMPAIGN_ID, "task-1", revision
            )
            head = self.publish_branch(checkout, branch)
            config = DRIVER.repo_config(repository_config(checkout))
            data = {
                "campaign": "fixture",
                "campaignIdentity": CAMPAIGN_ID,
                "repository": "acme/spec",
                "issue": issue(),
                "workspaceRoot": str(workspace_root),
                "task": {**task("task-1"), "revision": revision},
            }
            narration = DRIVER.template_narration(task("task-1"))
            receipt = DRIVER.merge_receipt_ref(
                "fixture", CAMPAIGN_ID, "task-1", revision
            )
            witnessed = self.integration(checkout, branch, head)

            # A sibling wins after this task was regated. Its commit advances
            # only the private integration branch; origin/main remains still.
            (checkout / "sibling.txt").write_text("sibling\n", encoding="utf-8")
            git(checkout, "add", "sibling.txt")
            git(checkout, "commit", "--quiet", "-m", "sibling: advance integration")
            sibling = git(checkout, "rev-parse", "HEAD")
            git(
                checkout,
                "update-ref",
                f"refs/heads/{DRIVER.integration_branch('fixture', CAMPAIGN_ID)}",
                sibling,
                witnessed["baseRev"],
            )
            with self.assertRaisesRegex(
                DRIVER.DriverError, "integration branch moved"
            ):
                DRIVER.merge_local(
                    data,
                    config,
                    witnessed,
                    "squash",
                    narration,
                )
            self.assertIsNone(DRIVER.local_refs(checkout, receipt).get(receipt))
            self.assertEqual(
                DRIVER.merged_local_tasks(
                    "acme/spec",
                    config,
                    "fixture",
                    CAMPAIGN_ID,
                    "7",
                    None,
                    [{**task("task-1"), "revision": revision}],
                ),
                [],
            )

            # The next pass rebases the exact stable task branch and retries
            # against the now-current integration tip.
            git(checkout, "switch", "--quiet", "work")
            git(checkout, "rebase", "--quiet", sibling)
            rebased = git(checkout, "rev-parse", "HEAD")
            git(checkout, "update-ref", f"refs/heads/{branch}", rebased, head)
            git(checkout, "switch", "--quiet", "main")

            merge_commit = DRIVER.merge_local(
                data,
                config,
                self.integration(checkout, branch, rebased),
                "squash",
                narration,
            )

            self.assertEqual(
                DRIVER.local_refs(checkout, receipt).get(receipt), merge_commit
            )
            self.assertEqual(
                git(
                    checkout,
                    "rev-parse",
                    DRIVER.integration_branch("fixture", CAMPAIGN_ID),
                ),
                merge_commit,
            )
            self.assertEqual(
                [
                    fact["mergeCommit"]
                    for fact in DRIVER.merged_local_tasks(
                        "acme/spec",
                        config,
                        "fixture",
                        CAMPAIGN_ID,
                        "7",
                        None,
                        [{**task("task-1"), "revision": revision}],
                    )
                ],
                [merge_commit],
            )

    def test_a_moved_published_branch_is_refused_by_the_actual_head_guard(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            workspace_root.mkdir()
            revision = "sha256:" + "b" * 64
            branch = DRIVER.stable_publish_branch(
                "fixture", CAMPAIGN_ID, "task-1", revision
            )
            head = self.publish_branch(checkout, branch)
            config = DRIVER.repo_config(repository_config(checkout))
            integration = self.integration(checkout, branch, head)
            data = {
                "campaign": "fixture",
                "campaignIdentity": CAMPAIGN_ID,
                "repository": "acme/spec",
                "issue": issue(),
                "workspaceRoot": str(workspace_root),
                "task": {**task("task-1"), "revision": revision},
            }

            git(checkout, "switch", "--quiet", "work")
            (checkout / "delivered.txt").write_text(
                "one\ntwo\nthree\n", encoding="utf-8"
            )
            git(checkout, "add", "delivered.txt")
            git(checkout, "commit", "--quiet", "-m", "wip: moved after gate")
            moved = git(checkout, "rev-parse", "HEAD")
            git(checkout, "update-ref", f"refs/heads/{branch}", moved, head)
            git(checkout, "switch", "--quiet", "main")

            with self.assertRaisesRegex(
                DRIVER.DriverError, "published branch moved"
            ):
                DRIVER.merge_local(
                    data,
                    config,
                    integration,
                    "squash",
                    DRIVER.template_narration(task("task-1")),
                )
            self.assertEqual(
                git(
                    checkout,
                    "rev-parse",
                    DRIVER.integration_branch("fixture", CAMPAIGN_ID),
                ),
                integration["baseRev"],
            )

    def test_a_reachable_but_unmarked_commit_does_not_complete_a_task(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout, _ = initialize_repository(root, remote=True)
            config = DRIVER.repo_config(repository_config(checkout))
            base = DRIVER.ensure_integration_branch(
                config,
                "fixture",
                CAMPAIGN_ID,
                git(checkout, "rev-parse", "origin/main"),
            )
            revision = "sha256:" + "d" * 64
            branch = DRIVER.stable_publish_branch(
                "fixture", CAMPAIGN_ID, "task-1", revision
            )
            git(
                checkout,
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "unmarked integration commit",
            )
            unmarked = git(checkout, "rev-parse", "HEAD")
            git(
                checkout,
                "update-ref",
                f"refs/heads/{DRIVER.integration_branch('fixture', CAMPAIGN_ID)}",
                unmarked,
                base,
            )
            git(checkout, "update-ref", f"refs/heads/{branch}", unmarked)
            receipt = DRIVER.merge_receipt_ref(
                "fixture", CAMPAIGN_ID, "task-1", revision
            )
            git(checkout, "update-ref", receipt, unmarked)

            self.assertEqual(
                DRIVER.merged_local_tasks(
                    "acme/spec",
                    config,
                    "fixture",
                    CAMPAIGN_ID,
                    "7",
                    None,
                    [{**task("task-1"), "revision": revision}],
                ),
                [],
            )

    def test_local_merge_method_still_produces_a_merge_commit_and_no_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            workspace_root.mkdir()
            revision = "sha256:" + "c" * 64
            branch = DRIVER.stable_publish_branch(
                "fixture", CAMPAIGN_ID, "task-1", revision
            )
            head = self.publish_branch(checkout, branch)
            config = DRIVER.repo_config(repository_config(checkout))
            data = {
                "campaign": "fixture",
                "campaignIdentity": CAMPAIGN_ID,
                "repository": "acme/spec",
                "issue": issue(),
                "workspaceRoot": str(workspace_root),
                "task": {**task("task-1"), "revision": revision},
            }
            merge_commit = DRIVER.merge_local(
                data,
                config,
                self.integration(checkout, branch, head),
                "merge",
                DRIVER.template_narration(task("task-1")),
            )
            base = git(
                checkout,
                "rev-parse",
                DRIVER.integration_branch("fixture", CAMPAIGN_ID),
            )
            self.assertEqual(len(git(checkout, "log", "-1", "--format=%P", base).split()), 2)
            self.assertEqual(
                command(
                    "git", "-C", str(checkout), "merge-base", "--is-ancestor", head, base,
                    check=False,
                ).returncode,
                0,
            )
            self.assertEqual(
                DRIVER.local_refs(
                    checkout,
                    f"{DRIVER.local_state_prefix('fixture', CAMPAIGN_ID)}/merge/",
                ),
                {},
            )
            self.assertEqual(
                [fact["mergeCommit"] for fact in DRIVER.merged_local_tasks(
                    "acme/spec",
                    config,
                    "fixture",
                    CAMPAIGN_ID,
                    "7",
                    None,
                    [{**task("task-1"), "revision": revision}],
                )],
                [merge_commit],
            )
            self.assertNotEqual(merge_commit, head)

class AssistedByTrailerTests(unittest.TestCase):
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

    def test_the_trailer_is_the_published_format_and_a_narrator_may_not_forge_one(self) -> None:
        trailer = DRIVER.assisted_by_trailer(
            DRIVER.assisted_by_record(self.ASSISTED, "assistedBy")
        )
        self.assertEqual(
            trailer,
            "Assisted-by: codex:provider/model-1 "
            "(tally:00000000-0000-4000-8000-000000000311 witness:42)",
        )
        self.assertEqual(
            DRIVER.merge_commit_message(self.NARRATION, trailer),
            "feat(fixture): deliver the first task\n\nSteward-authored body.\n\n"
            "Assisted-by: codex:provider/model-1 "
            "(tally:00000000-0000-4000-8000-000000000311 witness:42)\n",
        )
        self.assertEqual(
            DRIVER.merge_commit_message(self.NARRATION, None),
            "feat(fixture): deliver the first task\n\nSteward-authored body.\n",
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
        # The provenance line is the node's authority. Git matches trailer
        # keys case-insensitively, so every spelling is refused.
        for spelling in ("Assisted-by", "assisted-by", "ASSISTED-BY", "AsSiStEd-By"):
            narration, reason = DRIVER.validated_narration(
                {
                    "type": "feat",
                    "scope": "fixture",
                    "subject": "deliver the task",
                    "body": (
                        f"Noted context.\n\n{spelling}: someone:something "
                        "(tally:x witness:1)"
                    ),
                }
            )
            self.assertIsNone(narration, spelling)
            self.assertEqual(reason, "proposal contains an Assisted-by trailer", spelling)
        narration, reason = DRIVER.validated_narration(
            {
                "type": "docs",
                "scope": "fixture",
                "subject": "explain the assisted-by pointer",
                "body": "Documented that the trailer points into the witness ledger.",
            }
        )
        self.assertIsNotNone(narration)
        self.assertIsNone(reason)


if __name__ == "__main__":
    unittest.main()
