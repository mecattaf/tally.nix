#!/usr/bin/env python3
"""Language-agnostic action-seam regressions for the spec-build policy driver."""

from __future__ import annotations

import fcntl
import hashlib
import json
import os
from datetime import datetime
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
import threading
import unittest
from typing import Any


ROOT = Path(__file__).resolve().parents[1]


def driver_under_test() -> Path:
    """Resolve the Rust driver, building the canonical package when necessary."""
    configured = os.environ.get("SPEC_BUILD_DRIVER")
    if configured:
        return Path(configured)

    target_root = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    if not target_root.is_absolute():
        target_root = ROOT / target_root
    workspace_binary = target_root / "debug/spec-build-driver"

    cargo = shutil.which("cargo")
    if cargo is not None:
        built = subprocess.run(
            [
                cargo,
                "build",
                "--quiet",
                "--package",
                "spec-build-driver",
                "--bin",
                "spec-build-driver",
            ],
            cwd=ROOT,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if built.returncode != 0:
            raise RuntimeError(
                "could not build the Rust spec-build driver with Cargo: "
                + (built.stderr.strip() or built.stdout.strip() or "no output")
            )
        if workspace_binary.is_file():
            return workspace_binary

    if workspace_binary.is_file():
        return workspace_binary

    nix = shutil.which("nix")
    if nix is not None:
        built = subprocess.run(
            [
                nix,
                "build",
                "--no-link",
                "--print-out-paths",
                ".#spec-build-driver",
            ],
            cwd=ROOT,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        paths = [Path(line) for line in built.stdout.splitlines() if line.strip()]
        if built.returncode == 0 and len(paths) == 1:
            packaged_binary = paths[0] / "bin/spec-build-driver"
            if packaged_binary.is_file():
                return packaged_binary
        raise RuntimeError(
            "could not build the Rust spec-build driver with Nix: "
            + (built.stderr.strip() or built.stdout.strip() or "no output")
        )

    raise RuntimeError(
        "the Rust spec-build driver is unavailable; set SPEC_BUILD_DRIVER or install Cargo"
    )


DRIVER = driver_under_test()
FINAL_MESSAGE_PREFIX = "TALLY_FINAL_MESSAGE="
ATTEMPT_RECEIPTS_FILE = "attempt-receipts-v1.jsonl"
ATTEMPT_RECEIPT_AUTHORITY_FILE = "receipt-authority-v1.json"
ATTEMPT_RECEIPT_WORKLIST_SHA256 = "sha256:" + "a" * 64
MAX_CONTINUATION_EVENT_BYTES = 1024 * 1024
MAX_DIAGNOSIS_CHARS = 12_000
MAX_WORKER_FINDINGS_BYTES = 8 * 1024
WORKER_FINDINGS_TRUNCATION = "[... worker findings truncated after redaction ...]"
LOCAL_STEERING_REGISTRATION = "0198a62b-41ee-7000-8000-000000000571"
LOCAL_STEERING_ACTOR = "uid:1000"
CAMPAIGN_ID = "0198a62b-41ee-7000-8000-000000000573"
MARKER_SAFE_CHANGELOG_PREDICATE = (
    'base="$(git config --get tally.baserev)"; '
    'git diff --quiet "$base" HEAD -- && exit 0;\n'
    'git diff --name-only "$base" HEAD -- CHANGELOG.md | grep -qx CHANGELOG.md'
)


class DriverFailure(AssertionError):
    """A non-zero result from the executable selected as the driver under test."""

    def __init__(self, process: subprocess.CompletedProcess[str]) -> None:
        self.process = process
        detail = process.stderr.strip() or process.stdout.strip() or "no output"
        super().__init__(f"driver action exited {process.returncode}: {detail}")


def run_driver(
    action: str,
    brief: dict[str, Any],
    *,
    environment: dict[str, str] | None = None,
) -> dict[str, Any]:
    """Drive one action only through argv, its brief, stdout, and exit status."""
    with tempfile.NamedTemporaryFile(
        mode="w", encoding="utf-8", suffix=".json", delete=False
    ) as handle:
        json.dump(brief, handle, sort_keys=True, separators=(",", ":"))
        handle.write("\n")
        brief_path = Path(handle.name).resolve()
    process_environment = os.environ.copy()
    if environment:
        process_environment.update(environment)
    process_environment["TALLY_BRIEF"] = str(brief_path)
    try:
        process = subprocess.run(
            [str(DRIVER), action],
            input=json.dumps(brief, sort_keys=True, separators=(",", ":")),
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=process_environment,
        )
    finally:
        brief_path.unlink(missing_ok=True)
    if process.returncode != 0:
        raise DriverFailure(process)
    messages = [
        line.removeprefix(FINAL_MESSAGE_PREFIX)
        for line in process.stdout.splitlines()
        if line.startswith(FINAL_MESSAGE_PREFIX)
    ]
    if len(messages) != 1:
        raise AssertionError(
            "driver action must emit exactly one TALLY_FINAL_MESSAGE line; "
            f"stdout was {process.stdout!r}"
        )
    try:
        result = json.loads(messages[0])
    except json.JSONDecodeError as error:
        raise AssertionError(f"driver action emitted invalid JSON: {error}") from error
    if not isinstance(result, dict):
        raise AssertionError("driver action result must be a JSON object")
    return result


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
    root.mkdir(parents=True, exist_ok=True)
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
    directory = root / "campaigns" / "attempt-receipts" / campaign
    directory.mkdir(parents=True, exist_ok=True)
    authority = directory / ATTEMPT_RECEIPT_AUTHORITY_FILE
    authority.write_text(
        json.dumps(
            {
                "schemaVersion": 1,
                "campaign": campaign,
                "issueNumber": "7",
                "armSerial": 1,
                "worklistSha256": ATTEMPT_RECEIPT_WORKLIST_SHA256,
            },
            sort_keys=True,
            separators=(",", ":"),
        )
        + "\n",
        encoding="utf-8",
    )
    authority.chmod(0o600)
    return {
        "schemaVersion": 1,
        "kind": "local-jsonl",
        "path": str(directory / ATTEMPT_RECEIPTS_FILE),
    }


def attempt_records(root: Path, campaign: str = "fixture") -> list[dict[str, Any]]:
    path = Path(str(attempt_receipts(root, campaign)["path"]))
    if not path.exists():
        return []
    records = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
    for record in records:
        if record.get("schemaVersion") != 2:
            continue
        assert record["armSerial"] == 1
        assert record["worklistSha256"] == ATTEMPT_RECEIPT_WORKLIST_SHA256
        assert record["actor"] == "spec-build-driver"
        datetime.fromisoformat(str(record["writtenAt"]).replace("Z", "+00:00"))
    return records


def state_scope(campaign: str, issue_number: str) -> str:
    return hashlib.sha256(f"{campaign}\0{issue_number}".encode()).hexdigest()[:24]


def local_state_prefix(campaign: str, issue_number: str) -> str:
    return f"refs/tally/spec-build/v1/{state_scope(campaign, issue_number)}"


def integration_branch(campaign: str = "fixture", identity: str = CAMPAIGN_ID) -> str:
    return f"tally/{campaign}-campaign-{identity}/integration"


def stable_publish_branch(
    task_id: str = "task-1",
    revision: str | None = None,
    campaign: str = "fixture",
    identity: str = CAMPAIGN_ID,
) -> str:
    suffix = "" if revision is None else "-" + revision.removeprefix("sha256:")[:16]
    return f"tally/{campaign}-campaign-{identity}/{task_id}{suffix}"


def merge_receipt_ref(task_id: str, revision: str) -> str:
    suffix = revision.removeprefix("sha256:")[:16]
    return f"{local_state_prefix('fixture', CAMPAIGN_ID)}/merge/{task_id}-{suffix}"


def local_refs(checkout: Path, prefix: str) -> dict[str, str]:
    rows = git(checkout, "for-each-ref", "--format=%(objectname)%09%(refname)", prefix)
    return {
        reference: object_id
        for line in rows.splitlines()
        for object_id, reference in [line.split("\t", 1)]
    }


def read_remote_blob(checkout: Path, reference: str) -> dict[str, Any]:
    git(checkout, "fetch", "--quiet", "origin", reference)
    return json.loads(git(checkout, "cat-file", "blob", "FETCH_HEAD"))


def worktree_identity(worktree: Path) -> dict[str, str]:
    viewed = command(
        "git",
        "-C",
        str(worktree),
        "config",
        "--worktree",
        "--get-regexp",
        r"^tally\.",
        check=False,
    )
    if viewed.returncode not in {0, 1}:
        raise AssertionError(viewed.stderr)
    return {
        key.removeprefix("tally."): value
        for line in viewed.stdout.splitlines()
        for key, value in [line.split(None, 1)]
    }


def set_worktree_identity(worktree: Path, values: dict[str, str]) -> None:
    command(
        "git",
        "-C",
        str(worktree),
        "config",
        "--worktree",
        "--remove-section",
        "tally",
        check=False,
    )
    for key, value in values.items():
        command(
            "git", "-C", str(worktree), "config", "--worktree", f"tally.{key}", value
        )


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
    return run_driver(
        "worklist",
        {
            "repository": "acme/spec",
            "repositoryConfig": repository_config(checkout),
            "worklist": "specs/*/tasks.json",
            "maxTasks": max_tasks,
            "maxParallel": max_parallel,
        }
    )


def install_worklist(
    checkout: Path,
    tasks: list[dict[str, Any]],
    *,
    campaign: dict[str, Any] | None = None,
    max_parallel: int = 1,
) -> tuple[dict[str, Any], dict[str, Any]]:
    path = checkout / "specs/campaign/tasks.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    document: dict[str, Any] = {"schemaVersion": 1, "tasks": tasks}
    if campaign is not None:
        document["campaign"] = campaign
    path.write_text(json.dumps(document) + "\n", encoding="utf-8")
    git(checkout, "add", str(path.relative_to(checkout)))
    git(checkout, "commit", "--quiet", "-m", "fixture: add worklist")
    git(checkout, "push", "--quiet", "origin", "main")
    brief = {
        "repository": "acme/spec",
        "repositoryConfig": repository_config(checkout),
        "worklist": "specs/*/tasks.json",
        "maxTasks": len(tasks),
        "maxParallel": max_parallel,
    }
    return run_driver("worklist", brief), brief


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


def commit_lane(
    workspace: dict[str, Any],
    *,
    path: str = "task-1",
    content: str = "implemented\n",
    message: str = "implement task 1",
) -> str:
    worktree = Path(workspace["worktreePath"])
    target = worktree / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(content, encoding="utf-8")
    git(worktree, "add", path)
    git(worktree, "commit", "--quiet", "-m", message)
    return git(worktree, "rev-parse", "HEAD")


def publication_brief(
    checkout: Path,
    workspace_root: Path,
    run_id: str,
    campaign_task: dict[str, Any],
    workspace: dict[str, Any],
    *,
    steward: dict[str, Any] | None = None,
    worker_findings: dict[str, Any] | None = None,
) -> dict[str, Any]:
    return {
        "campaign": "fixture",
        "campaignIdentity": CAMPAIGN_ID,
        "repository": "acme/spec",
        "repositoryConfig": repository_config(checkout),
        "issue": issue(),
        "runId": run_id,
        "workspaceRoot": str(workspace_root),
        "task": campaign_task,
        "domainsRequired": True,
        "gates": [],
        "steward": steward,
        "workspace": workspace,
        "constraints": [],
        "workerFindings": worker_findings,
    }


def rebase_brief(
    checkout: Path,
    workspace_root: Path,
    run_id: str,
    campaign_task: dict[str, Any],
    workspace: dict[str, Any],
    publication: dict[str, Any],
) -> dict[str, Any]:
    return {
        "campaign": "fixture",
        "campaignIdentity": CAMPAIGN_ID,
        "repository": "acme/spec",
        "repositoryConfig": repository_config(checkout),
        "issue": issue(),
        "runId": run_id,
        "workspaceRoot": str(workspace_root),
        "task": campaign_task,
        "workspace": workspace,
        "publication": publication,
        "domainsRequired": True,
        "constraints": [],
    }


def merge_brief(
    checkout: Path,
    workspace_root: Path,
    run_id: str,
    campaign_task: dict[str, Any],
    workspace: dict[str, Any],
    integration: dict[str, Any],
    *,
    method: str = "squash",
    assisted_by: dict[str, Any] | None = None,
) -> dict[str, Any]:
    return {
        "campaign": "fixture",
        "campaignIdentity": CAMPAIGN_ID,
        "repository": "acme/spec",
        "repositoryConfig": repository_config(checkout),
        "issue": issue(),
        "runId": run_id,
        "workspaceRoot": str(workspace_root),
        "task": campaign_task,
        "workspace": workspace,
        "integration": integration,
        "domainsRequired": True,
        "mergeMethod": method,
        "assistedBy": assisted_by,
    }


def prepared_publication(
    root: Path,
    *,
    campaign_task: dict[str, Any] | None = None,
    run_id: str = "publication-pass",
    steward: dict[str, Any] | None = None,
    commits: int = 1,
) -> dict[str, Any]:
    checkout, _ = initialize_repository(root, remote=True)
    workspace_root = root / "workspaces"
    selected_task = campaign_task or {
        **task("task-1"),
        "revision": "sha256:" + "a" * 64,
    }
    selected_task["conflictDomains"] = ["task-1"]
    brief = prep_brief(checkout, workspace_root, run_id)
    brief["task"] = selected_task
    workspace = run_driver("prep", brief)
    for index in range(commits):
        commit_lane(
            workspace,
            content="".join(f"line {line}\n" for line in range(index + 1)),
            message=f"wip: task step {index + 1}",
        )
    publication = run_driver(
        "publish",
        publication_brief(
            checkout,
            workspace_root,
            run_id,
            selected_task,
            workspace,
            steward=steward,
        ),
    )
    integration = run_driver(
        "rebase",
        rebase_brief(
            checkout,
            workspace_root,
            run_id,
            selected_task,
            workspace,
            publication,
        ),
    )
    return {
        "checkout": checkout,
        "workspaceRoot": workspace_root,
        "runId": run_id,
        "task": selected_task,
        "workspace": workspace,
        "publication": publication,
        "integration": integration,
    }


def prepared_lane_context(
    root: Path,
    run_id: str = "subject-adoption-pass",
    *,
    message: str = "implement task 1",
) -> dict[str, Any]:
    checkout, _ = initialize_repository(root, remote=True)
    workspace_root = root / "workspaces"
    campaign_task = {
        **task("task-1"),
        "revision": "sha256:" + "a" * 64,
        "conflictDomains": ["task-1"],
    }
    brief = prep_brief(checkout, workspace_root, run_id)
    brief["task"] = campaign_task
    workspace = run_driver("prep", brief)
    commit_lane(workspace, message=message)
    return {
        "checkout": checkout,
        "workspaceRoot": workspace_root,
        "runId": run_id,
        "task": campaign_task,
        "workspace": workspace,
    }


def steward_catalog_role(argv: list[str], **overrides: object) -> dict[str, object]:
    return {
        "adapter": "judge",
        "argv": argv,
        "env": {},
        "finalMessagePattern": "^TALLY_FINAL_MESSAGE=(.*)$",
        "runtimeMaxSec": 30,
        **overrides,
    }


def publish_lane(
    context: dict[str, Any], steward: dict[str, object] | None
) -> dict[str, Any]:
    return run_driver(
        "publish",
        publication_brief(
            context["checkout"],
            context["workspaceRoot"],
            context["runId"],
            context["task"],
            context["workspace"],
            steward=steward,
        ),
    )


def merge_lane(
    context: dict[str, Any], steward: dict[str, object] | None = None
) -> tuple[dict[str, Any], dict[str, Any]]:
    publication = publish_lane(context, steward)
    integration = run_driver(
        "rebase",
        rebase_brief(
            context["checkout"],
            context["workspaceRoot"],
            context["runId"],
            context["task"],
            context["workspace"],
            publication,
        ),
    )
    merged = run_driver(
        "merge",
        merge_brief(
            context["checkout"],
            context["workspaceRoot"],
            context["runId"],
            context["task"],
            context["workspace"],
            integration,
        ),
    )
    return publication, merged


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
        self.saved_environment = {
            name: os.environ.get(name)
            for name in ("FAKE_TALLY_STATE", "TALLY_TASK_UUID")
        }
        os.environ["FAKE_TALLY_STATE"] = str(self.state_path)
        os.environ["TALLY_TASK_UUID"] = self.TASK_UUID
        return self

    def __exit__(self, *_: object) -> None:
        for name, value in self.saved_environment.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value

    def state(self) -> dict[str, object]:
        return json.loads(self.state_path.read_text(encoding="utf-8"))

    def update(self, **values: object) -> None:
        state = self.state()
        state.update(values)
        self.state_path.write_text(json.dumps(state), encoding="utf-8")

class CampaignDriverTests(unittest.TestCase):
    def test_repository_config_admits_only_local_forge(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            valid = prep_brief(checkout, root / "workspaces", "local-forge")
            prepared = run_driver("prep", valid)
            self.assertEqual(prepared["taskId"], "task-1")

            invalid = prep_brief(checkout, root / "other-workspaces", "github-forge")
            invalid["repositoryConfig"] = repository_config(checkout, "github")
            with self.assertRaisesRegex(DriverFailure, "forge must be local"):
                run_driver("prep", invalid)

    def test_completion_trailers_carry_the_task_revision(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            context = prepared_publication(Path(temporary))
            revision = str(context["task"]["revision"])

            bad_revision = merge_brief(
                context["checkout"],
                context["workspaceRoot"],
                context["runId"],
                {**context["task"], "revision": None},
                context["workspace"],
                context["integration"],
            )
            with self.assertRaisesRegex(
                DriverFailure, "completion revision"
            ):
                run_driver("merge", bad_revision)

            unsafe = prep_brief(
                context["checkout"],
                Path(temporary) / "unsafe-workspaces",
                "unsafe-task",
            )
            unsafe["task"] = {**task("task-1"), "id": "not a task"}
            with self.assertRaisesRegex(DriverFailure, "safe"):
                run_driver("prep", unsafe)

            merged = run_driver(
                "merge",
                merge_brief(
                    context["checkout"],
                    context["workspaceRoot"],
                    context["runId"],
                    context["task"],
                    context["workspace"],
                    context["integration"],
                ),
            )
            message = git(
                context["checkout"], "log", "-1", "--format=%B", merged["mergeCommit"]
            )
            self.assertTrue(
                message.endswith(
                    f"Tally-Task: task-1\nTally-Revision: {revision}"
                ),
                message,
            )

    def test_file_worklist_tasks_carry_completion_revisions(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            admitted, _ = install_worklist(
                checkout,
                [
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
            )
            for admitted_task in admitted["tasks"]:
                self.assertRegex(admitted_task["revision"], r"^sha256:[0-9a-f]{64}$")
            self.assertNotEqual(
                admitted["tasks"][0]["revision"], admitted["tasks"][1]["revision"]
            )

    def test_worklist_campaign_policy_is_closed_and_bound_to_the_brief(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            worklist = checkout / "specs/campaign/epsilon.json"
            worklist.parent.mkdir(parents=True)
            document = {
                "schemaVersion": 1,
                "campaign": {
                    "maxTasks": 2,
                    "maxParallel": 2,
                    "agent": {},
                    "gates": [
                        {
                            "kind": "command",
                            "id": "tests",
                            "preflightArgv": ["true"],
                            "argv": ["true"],
                        }
                    ],
                },
                "tasks": [task("task-1"), task("task-2")],
            }
            worklist.write_text(json.dumps(document), encoding="utf-8")
            git(checkout, "add", "specs/campaign/epsilon.json")
            git(checkout, "commit", "--quiet", "-m", "add campaign policy")
            git(checkout, "push", "--quiet", "origin", "main")
            base = {
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout),
                "worklist": "specs/*/epsilon.json",
                "maxTasks": 2,
                "maxParallel": 2,
            }
            admitted = run_driver("worklist", base)
            self.assertEqual(
                [item["id"] for item in admitted["tasks"]], ["task-1", "task-2"]
            )

            with self.assertRaisesRegex(
                DriverFailure, "campaign maxTasks disagrees.*campaign=2 brief=3"
            ):
                run_driver("worklist", {**base, "maxTasks": 3})
            with self.assertRaisesRegex(
                DriverFailure, "campaign maxParallel disagrees.*campaign=2 brief=1"
            ):
                run_driver("worklist", {**base, "maxParallel": 1})

            document["campaign"]["label"] = "forge-only"
            worklist.write_text(json.dumps(document), encoding="utf-8")
            git(checkout, "add", "specs/campaign/epsilon.json")
            git(checkout, "commit", "--quiet", "-m", "add forbidden campaign field")
            git(checkout, "push", "--quiet", "origin", "main")
            with self.assertRaisesRegex(DriverFailure, "unknown fields: label"):
                run_driver("worklist", base)

    def test_a_policy_less_worklist_admits_against_a_steward_bound_campaign(self) -> None:
        """No policy key is required, and none is invented on the agent's behalf.

        The three agent policy names are adapter vocabulary. A campaign that
        writes none of them binds its diagnosis role to a steward adapter that
        declares no launch policies at all, so any default the driver supplied
        here would be a policy that steward could never render.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            worklist = checkout / "specs/campaign/policy-less.json"
            worklist.parent.mkdir(parents=True)
            campaign: dict[str, Any] = {
                "maxTasks": 1,
                "maxParallel": 1,
                "steward": "narrator",
                "stewardArgv": ["narrate", "--json"],
                "agent": {"adapter": "pi"},
                "gates": [
                    {
                        "kind": "command",
                        "id": "tests",
                        "preflightArgv": ["true"],
                        "argv": ["true"],
                    }
                ],
            }
            document = {
                "schemaVersion": 1,
                "campaign": campaign,
                "tasks": [task("task-1")],
            }
            base = {
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout),
                "worklist": "specs/*/policy-less.json",
                "maxTasks": 1,
                "maxParallel": 1,
            }

            def commit_and_admit(message: str) -> dict[str, Any]:
                worklist.write_text(json.dumps(document), encoding="utf-8")
                git(checkout, "add", "specs/campaign/policy-less.json")
                git(checkout, "commit", "--quiet", "-m", message)
                git(checkout, "push", "--quiet", "origin", "main")
                return run_driver("worklist", base)

            admitted = commit_and_admit("add a policy-less steward-bound campaign")
            self.assertEqual([item["id"] for item in admitted["tasks"]], ["task-1"])

            # An explicit value still wins outright; absence is the only thing
            # that defers to the adapter.
            campaign["agent"] = {
                "adapter": "codex",
                "approvalPolicy": "never",
                "sandboxPolicy": "danger-full-access",
                "diagnosisSandboxPolicy": "workspace-write",
            }
            admitted = commit_and_admit("spell every policy explicitly")
            self.assertEqual([item["id"] for item in admitted["tasks"]], ["task-1"])

            # A policy name is still a bounded string when it is present.
            campaign["agent"]["diagnosisSandboxPolicy"] = 17
            worklist.write_text(json.dumps(document), encoding="utf-8")
            git(checkout, "add", "specs/campaign/policy-less.json")
            git(checkout, "commit", "--quiet", "-m", "write a policy that is not a name")
            git(checkout, "push", "--quiet", "origin", "main")
            with self.assertRaisesRegex(
                DriverFailure, "worklist.campaign.agent.diagnosisSandboxPolicy"
            ):
                run_driver("worklist", base)

    def test_pre_post_refresh_refuses_quiescence_after_the_frontier_reopens(self) -> None:
        """A PATH shim pardons the blocked task between the two durable reads."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            _, worklist_brief = install_worklist(checkout, [task("task-1")])
            receipts = attempt_receipts(root)
            steering = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout),
                "issue": issue(),
                "taskId": "task-1",
                "diagnosis": "Investigated the blocked task.",
                "attemptReceipts": receipts,
            }
            for attempt in (1, 2):
                run_driver("steer", {**steering, "attempt": attempt})

            shim_root = root / "shim"
            shim_root.mkdir()
            real_git = shutil.which("git")
            assert real_git is not None
            counter = root / "fetch-count"
            receipt_path = Path(str(receipts["path"]))
            shim = shim_root / "git"
            shim.write_text(
                f"#!{sys.executable}\n"
                + textwrap.dedent(
                    f"""\
                    import json
                    import os
                    from pathlib import Path
                    import sys

                    counter = Path({str(counter)!r})
                    args = sys.argv[1:]
                    if "--no-tags" in args and "fetch" in args:
                        count = int(counter.read_text() if counter.exists() else "0") + 1
                        counter.write_text(str(count))
                        if count == 2:
                            record = {{
                                "schemaVersion": 1,
                                "sequence": 3,
                                "kind": "pardon",
                                "campaign": "fixture",
                                "issueNumber": "7",
                                "tasks": None,
                                "reason": "Corrected the external dependency.",
                                "actor": "uid:1000",
                                "nonce": "018f47a0-7b9d-7cc2-92d6-2f7f19f505fd",
                            }}
                            with Path({str(receipt_path)!r}).open("a", encoding="utf-8") as log:
                                log.write(json.dumps(record, sort_keys=True, separators=(",", ":")) + "\\n")
                    os.execv({real_git!r}, [{real_git!r}, *args])
                    """
                ),
                encoding="utf-8",
            )
            shim.chmod(0o755)
            escalation = {
                "campaign": "fixture",
                "campaignIdentity": CAMPAIGN_ID,
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout),
                "issue": issue(),
                "worklist": worklist_brief["worklist"],
                "maxTasks": 1,
                "maxParallel": 1,
                "attemptReceipts": receipts,
            }
            with self.assertRaisesRegex(
                DriverFailure,
                "pre-post durable refresh.*refusing to post outcome=quiescent",
            ):
                run_driver(
                    "escalate",
                    escalation,
                    environment={"PATH": f"{shim_root}:{os.environ['PATH']}"},
                )
            self.assertEqual(counter.read_text(encoding="utf-8"), "2")

    def test_machinery_retries_are_bounded_and_spend_no_steering_attempt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            _, worklist_brief = install_worklist(checkout, [task("task-1")])
            receipts = attempt_receipts(root)
            base_retry = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout),
                "issue": issue(),
                "taskId": "task-1",
                "detail": "the integration checkout could not be staged",
                "attemptReceipts": receipts,
            }
            first = run_driver("retry", {**base_retry, "stage": "merge"})
            second = run_driver("retry", {**base_retry, "stage": "rebase"})
            third = run_driver("retry", {**base_retry, "stage": "merge"})
            self.assertEqual((first["attempt"], first["posted"]), (1, True))
            self.assertEqual((second["attempt"], second["exhausted"]), (2, True))
            self.assertFalse(third["posted"])
            self.assertTrue(third["exhausted"])

            reconciled = run_driver(
                "reconcile",
                {
                    "campaign": "fixture",
                    "campaignIdentity": CAMPAIGN_ID,
                    "repository": "acme/spec",
                    "repositoryConfig": repository_config(checkout),
                    "issue": issue(),
                    **{key: worklist_brief[key] for key in ("worklist", "maxTasks", "maxParallel")},
                    "attemptReceipts": receipts,
                },
            )
            self.assertEqual(
                [(item["taskId"], item["attempt"]) for item in reconciled["retries"]],
                [("task-1", 1), ("task-1", 2)],
            )
            self.assertEqual(reconciled["diagnoses"], [])
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
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            _, worklist_brief = install_worklist(
                checkout, tasks, max_parallel=2
            )
            receipts = attempt_receipts(root)
            reconcile = {
                "campaign": "fixture",
                "campaignIdentity": CAMPAIGN_ID,
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout),
                "issue": issue(),
                **{key: worklist_brief[key] for key in ("worklist", "maxTasks", "maxParallel")},
                "attemptReceipts": receipts,
            }
            first = run_driver("reconcile", reconcile)
            self.assertEqual(
                first["deferrals"],
                [{"taskId": "phase-one", "waitingOn": ["task-3"]}],
            )
            steering = {
                "campaign": "fixture",
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout),
                "issue": issue(),
                "taskId": "task-3",
                "diagnosis": "Investigated the unrelated failure.",
                "attemptReceipts": receipts,
            }
            for attempt in (1, 2):
                run_driver("steer", {**steering, "attempt": attempt})
            second = run_driver("reconcile", reconcile)
            self.assertEqual(second["deferrals"], [])

    def test_continuation_keeps_its_durable_local_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            continued = run_driver(
                "continue",
                {
                    "campaign": "fixture",
                    "repository": "acme/spec",
                    "repositoryConfig": repository_config(checkout),
                    "issue": issue(),
                    "runId": "pass-3",
                    "continuation": continuation_spec(root / "events"),
                    "brief": None,
                },
            )
            self.assertTrue(continued["created"])
            reference = continued["receipt"].split("acme/spec/", 1)[1]
            self.assertEqual(
                reference,
                f"refs/tally/spec-build/v1/{state_scope('fixture', '7')}"
                f"/continuation/{continued['runId']}",
            )
            listed = git(checkout, "ls-remote", "origin", reference)
            blob = git(checkout, "cat-file", "blob", listed.split()[0])
            self.assertEqual(json.loads(blob)["dedupKey"], continued["dedupKey"])

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
            with self.assertRaises(DriverFailure):
                run_driver("continue", {**base, "continuation": relative})
            oversized = continuation_spec(root / "events")
            with self.assertRaises(DriverFailure):
                run_driver(
                    "continue",
                    {
                        **base,
                        "continuation": oversized,
                        "brief": {"pad": "x" * (MAX_CONTINUATION_EVENT_BYTES + 1)},
                    },
                )
            self.assertFalse((root / "events").exists())


class LaneLifecycleTests(unittest.TestCase):
    def test_fresh_lane_cuts_serialize_on_the_checkout_git_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            lock_path = Path(
                git(
                    checkout,
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-common-dir",
                )
            ) / "tally-worktree-preparation.lock"
            finished = threading.Event()
            failures: list[Exception] = []
            results: list[dict[str, Any]] = []
            real_flock = fcntl.flock

            def add_lane() -> None:
                try:
                    results.append(
                        run_driver(
                            "prep",
                            prep_brief(
                                checkout,
                                root / "workspaces",
                                "serialized-pass",
                            ),
                        )
                    )
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
                worker = threading.Thread(target=add_lane)
                worker.start()
                try:
                    self.assertFalse(
                        finished.wait(0.2),
                        "lane cut crossed the held shared-metadata lock",
                    )
                finally:
                    real_flock(held_lock, fcntl.LOCK_UN)
                    worker.join(5)

            self.assertFalse(worker.is_alive(), "lane cut did not resume after lock release")
            self.assertEqual(failures, [])
            self.assertEqual(len(results), 1)
            self.assertTrue(Path(results[0]["worktreePath"]).is_dir())

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
                prepared = run_driver("prep", prepared_brief)
                worktree = Path(prepared["worktreePath"])
                (worktree / "task-1").write_text("implemented\n", encoding="utf-8")
                git(worktree, "add", "task-1")
                git(worktree, "commit", "--quiet", "-m", "implement task 1")

                config = repository_config(checkout)
                publication = run_driver("publish",
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
                    f"{local_state_prefix('fixture', '7')}/findings/"
                    f"task-1/{agent_task_uuid}"
                )

                if findings is None:
                    self.assertEqual(git(checkout, "ls-remote", "origin", ref), "")
                    continue

                receipt = read_remote_blob(checkout, ref)
                self.assertEqual(receipt["kind"], "worker-findings")
                comment = receipt["body"]
                self.assertTrue(comment.startswith("### Worker findings"))
                self.assertIn("[redacted sensitive diagnosis line]", comment)
                self.assertIn(WORKER_FINDINGS_TRUNCATION.strip(), comment)
                self.assertNotIn(secret, comment)
                self.assertLessEqual(
                    len(comment.encode("utf-8")), MAX_WORKER_FINDINGS_BYTES
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
            prepared = run_driver("prep",
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
            initial = git(checkout, "rev-parse", "origin/main")
            run_driver(
                "prep",
                prep_brief(
                    checkout,
                    workspace_root,
                    "seed-integration",
                    source_revision=initial,
                ),
            )
            campaign_branch = integration_branch()

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
                f"refs/heads/{campaign_branch}",
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
                git(checkout, "rev-parse", campaign_branch),
                integration_tip,
            )
            prepared = run_driver("prep",
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
            initial = git(checkout, "rev-parse", "origin/main")
            run_driver(
                "prep",
                prep_brief(
                    checkout,
                    root / "workspaces",
                    "checkpoint-seed",
                    source_revision=initial,
                ),
            )
            campaign_branch = integration_branch()

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
                f"refs/heads/{campaign_branch}",
                integration_tip,
                initial,
            )

            recorded = run_driver("checkpoint",
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
            with self.assertRaises(DriverFailure) as raised:
                run_driver("prep", brief)
            self.assertIn("sourceRevision", str(raised.exception))

            malformed = prep_brief(
                checkout, root / "workspaces", "bad-revision", source_revision="main"
            )
            with self.assertRaises(DriverFailure) as raised:
                run_driver("prep", malformed)
            self.assertIn("full Git object ID", str(raised.exception))

    def test_prep_projects_domains_without_collapsing_absence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            base_fields = {
                "taskId",
                "baseRev",
                "branch",
                "publishBranch",
                "worktreePath",
            }

            for name, present, domains in (
                ("declared", True, ["drivers", "test/spec_build_driver_test.py"]),
                ("declared-empty", True, []),
                ("omitted", False, None),
            ):
                with self.subTest(case=name):
                    brief = prep_brief(checkout, workspace_root, f"prep-domains-{name}")
                    if present:
                        brief["task"]["conflictDomains"] = domains
                    else:
                        del brief["task"]["conflictDomains"]

                    prepared = run_driver("prep", brief)
                    expected_fields = base_fields | (
                        {"conflictDomains"} if present else set()
                    )
                    self.assertEqual(set(prepared), expected_fields)
                    if present:
                        self.assertEqual(prepared["conflictDomains"], domains)
                    else:
                        self.assertNotIn("conflictDomains", prepared)

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

            prepared = run_driver("prep", brief)
            worktree = Path(prepared["worktreePath"])
            (worktree / "lane-work.txt").write_text("in flight\n", encoding="utf-8")
            git(worktree, "add", "lane-work.txt")
            git(worktree, "commit", "--quiet", "-m", "task-1: in flight")
            in_flight = git(worktree, "rev-parse", "HEAD")

            # The lane is still there: resume adopts it, work and all.
            resumed = run_driver("prep", brief)
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

            rebuilt = run_driver("prep", brief)
            self.assertEqual(rebuilt["branch"], prepared["branch"])
            self.assertEqual(rebuilt["worktreePath"], prepared["worktreePath"])
            self.assertEqual(git(worktree, "rev-parse", "HEAD"), in_flight)
            self.assertEqual(worktree_identity(worktree)["runid"], "killed-pass")
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
                worktree_identity(worktree)["baserev"], rebuilt["baseRev"]
            )

            # The other way a lane directory disappears: removed underneath
            # git, so the lane is still registered and has to be pruned first.
            shutil.rmtree(worktree)
            pruned = run_driver("prep", brief)
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

            prepared = run_driver("prep", brief)
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
            self.assertEqual(worktree_identity(worktree), {})

            healed = run_driver("prep", brief)
            self.assertEqual(healed["branch"], prepared["branch"])
            self.assertEqual(healed["worktreePath"], prepared["worktreePath"])
            self.assertEqual(healed["baseRev"], prepared["baseRev"])
            self.assertEqual(git(worktree, "rev-parse", "HEAD"), in_flight)
            recorded = worktree_identity(worktree)
            self.assertEqual(recorded["baserev"], prepared["baseRev"])
            self.assertEqual(recorded["runid"], "crash-pass")
            # And the healed lane resumes normally from here on.
            self.assertEqual(run_driver("prep", brief), healed)

    def test_lane_identity_is_written_in_one_atomic_act(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            prepared = run_driver("prep",
                prep_brief(checkout, workspace_root, "atomic-pass")
            )
            worktree = Path(prepared["worktreePath"])
            config_path = Path(
                git(
                    worktree,
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-path",
                    "config.worktree",
                )
            )
            self.assertTrue(config_path.is_file())
            original = worktree_identity(worktree)
            self.assertEqual(original["campaign"], "fixture")
            self.assertEqual(original["taskid"], "task-1")

            # A per-worktree key this driver does not own survives the public
            # prep action healing an interrupted identity write.
            command(
                "git", "-C", str(worktree), "config", "--worktree", "other.key", "keep me"
            )
            command(
                "git",
                "-C",
                str(worktree),
                "config",
                "--worktree",
                "--remove-section",
                "tally",
            )
            self.assertEqual(worktree_identity(worktree), {})
            self.assertEqual(run_driver("prep", prep_brief(checkout, workspace_root, "atomic-pass")), prepared)
            self.assertEqual(worktree_identity(worktree), original)
            self.assertEqual(
                command(
                    "git", "-C", str(worktree), "config", "--worktree", "--get", "other.key"
                ).stdout.strip(),
                "keep me",
            )
            # Lane identity is never recorded on the main worktree, where
            # `git config --worktree` means the shared config instead.
            on_main = command(
                "git",
                "-C",
                str(checkout),
                "config",
                "--worktree",
                "--get",
                "tally.campaign",
                check=False,
            )
            self.assertEqual(on_main.returncode, 1)

    def test_a_foreign_lane_at_the_same_path_is_a_conflict(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            prepared = run_driver("prep",
                prep_brief(checkout, workspace_root, "first-pass")
            )
            worktree = Path(prepared["worktreePath"])
            # Another campaign's identity on this campaign's path is not
            # something to clobber.
            set_worktree_identity(worktree, {"campaign": "other"})
            with self.assertRaises(DriverFailure) as raised:
                run_driver("prep", prep_brief(checkout, workspace_root, "first-pass"))
            self.assertIn("different lane identity", str(raised.exception))
            self.assertIn("campaign", str(raised.exception))

    def test_closing_summary_is_a_durable_local_blob(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            _, worklist_brief = install_worklist(
                checkout, [task("task-1"), task("task-2")], max_parallel=2
            )
            receipts = attempt_receipts(root)
            for task_id in ("task-1", "task-2"):
                for attempt in (1, 2):
                    run_driver(
                        "steer",
                        {
                            "campaign": "fixture",
                            "repository": "acme/spec",
                            "repositoryConfig": repository_config(checkout),
                            "issue": issue(),
                            "taskId": task_id,
                            "attempt": attempt,
                            "diagnosis": f"Investigated {task_id} and found the gate stayed red.",
                            "attemptReceipts": receipts,
                        },
                    )
            brief = {
                "campaign": "fixture",
                "campaignIdentity": CAMPAIGN_ID,
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout),
                "issue": issue(),
                **{key: worklist_brief[key] for key in ("worklist", "maxTasks", "maxParallel")},
                "attemptReceipts": receipts,
            }
            first = run_driver("escalate", brief)
            self.assertTrue(first["posted"])
            self.assertTrue(str(first["summary"]).endswith("/summary/quiescent"))
            reference = str(first["summary"]).split("acme/spec/", 1)[1]
            stored = read_remote_blob(checkout, reference)
            self.assertEqual(stored["kind"], "closing-summary")
            self.assertIn("Campaign closed at frontier quiescence", stored["body"])
            self.assertIn("0 of 2 task(s)", stored["body"])
            self.assertIn("the gate stayed red", stored["body"])

            second = run_driver("escalate", brief)
            self.assertFalse(second["posted"])
            self.assertEqual(second["comment"], first["comment"])
            self.assertEqual(read_remote_blob(checkout, reference), stored)

    def test_conflicting_published_head_is_aborted_abandoned_and_rebuilt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            stable_branch = stable_publish_branch()
            initial = git(checkout, "rev-parse", "origin/main")
            run_driver(
                "prep",
                prep_brief(
                    checkout,
                    root / "seed-workspaces",
                    "seed-conflict",
                    source_revision=initial,
                ),
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
                f"refs/heads/{integration_branch()}",
                current_main,
                initial,
            )

            old_brief = prep_brief(checkout, workspace_root, "old-pass")
            old_brief["task"]["conflictDomains"] = ["root.go"]
            prepared = run_driver("prep", old_brief)
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
            with self.assertRaises(DriverFailure) as raised:
                run_driver("rebase", rebase_brief)
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
            run_driver("cleanup",
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
            rebuilt = run_driver("prep", new_brief)
            self.assertEqual(rebuilt["baseRev"], current_main)
            self.assertEqual(git(Path(rebuilt["worktreePath"]), "rev-parse", "HEAD"), current_main)
            run_driver("cleanup",
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
            stable_branch = stable_publish_branch()
            initial = git(checkout, "rev-parse", "origin/main")
            run_driver(
                "prep",
                prep_brief(
                    checkout,
                    root / "seed-workspaces",
                    "seed-domain-failure",
                    source_revision=initial,
                ),
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
                f"refs/heads/{integration_branch()}",
                current_main,
                initial,
            )

            rebase_task = task("task-1")
            rebase_task["conflictDomains"] = ["owned-only.txt"]
            old_brief = {
                **prep_brief(checkout, workspace_root, "old-pass"),
                "task": rebase_task,
            }
            prepared = run_driver("prep", old_brief)
            with self.assertRaises(DriverFailure) as raised:
                run_driver("rebase",
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
                run_driver("sweep",
                    sweep_brief(checkout, workspace_root, "dead-pass", tally.program)
                )
                prepared = run_driver("prep",
                    prep_brief(checkout, workspace_root, "dead-pass")
                )
                worktree = Path(prepared["worktreePath"])
                self.assertTrue(worktree.is_dir())
                # Lane identity lives in git's own per-worktree configuration,
                # and nothing else: no bespoke marker file is written.
                self.assertEqual(
                    worktree_identity(worktree),
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
                enumerated = git(checkout, "worktree", "list", "--porcelain")
                self.assertIn(f"worktree {worktree.resolve()}", enumerated)
                self.assertIn(f"branch refs/heads/{prepared['branch']}", enumerated)
                tally.update(currentFlowRunId=live_flow, flows={dead_flow: []})
                swept = run_driver("sweep",
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
                (
                    workspace_root
                    / ".state"
                    / "passes"
                    / f"{hashlib.sha256(b'dead-pass').hexdigest()[:12]}.json"
                ).exists()
            )
            self.assertTrue(
                (
                    workspace_root
                    / ".state"
                    / "passes"
                    / f"{hashlib.sha256(b'live-pass').hexdigest()[:12]}.json"
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
                run_driver("sweep",
                    sweep_brief(checkout, workspace_root, run_id, tally.program)
                )
                worktree = workspace_root / "spec" / run_hash / "task-1"
                worktree.mkdir(parents=True)
                (worktree / "uncommitted.txt").write_text("stale\n", encoding="utf-8")
                branch = f"tally-work/fixture-{run_hash}/task-1"
                git(checkout, "branch", branch)
                tally.update(currentFlowRunId=live_flow, flows={dead_flow: []})
                swept = run_driver("sweep",
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

    def test_sweep_defers_and_preserves_every_lane_while_an_old_flow_job_is_live(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            workspace_root = root / "workspaces"
            dead_flow = "00000000-0000-4000-8000-000000000915"
            waiting_flow = "00000000-0000-4000-8000-000000000916"
            settled_flow = "00000000-0000-4000-8000-000000000917"
            with FakeTally(root, dead_flow) as tally:
                run_driver("sweep",
                    sweep_brief(checkout, workspace_root, "dead-pass", tally.program)
                )
                prepared = run_driver("prep",
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
                deferred = run_driver("sweep",
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
                swept = run_driver("sweep",
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
                run_driver("sweep",
                    sweep_brief(checkout, workspace_root, "killed-pass", tally.program)
                )
                prepared = run_driver("preflight",
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
                deferred = run_driver("sweep",
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
                swept = run_driver("sweep",
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
                run_driver("sweep",
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
                prepared = run_driver("prep", old_prep)
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
                deferred = run_driver("sweep",
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
            prepared = run_driver("prep",
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
                swept = run_driver("sweep",
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
                run_driver("sweep",
                    sweep_brief(checkout, workspace_root, "dead-pass", tally.program)
                )
                prepared = run_driver("prep",
                    prep_brief(checkout, workspace_root, "dead-pass")
                )
                worktree = Path(prepared["worktreePath"])
                tally.update(
                    currentFlowRunId="00000000-0000-4000-8000-000000000921",
                    failQueries=True,
                )
                with self.assertRaisesRegex(DriverFailure, "injected tally query failure"):
                    run_driver("sweep",
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
            run_driver("cleanup",
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
            run_driver("cleanup",
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


class SubjectAdoptionTests(unittest.TestCase):
    """The squash adopts the lane tip and formats its commit prose."""

    def merged_message(
        self, context: dict[str, Any], merged: dict[str, Any]
    ) -> str:
        return git(
            context["checkout"],
            "show",
            "-s",
            "--format=%B",
            merged["mergeCommit"],
        )

    def test_a_valid_lane_tip_subject_and_body_survive_the_squash(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            context = prepared_lane_context(
                Path(temporary),
                message=(
                    "feat(driver): adopt the lane subject\n\n"
                    "Recorded the lane-authored rationale."
                ),
            )
            _, merged = merge_lane(context)
            message = self.merged_message(context, merged)
            self.assertEqual(message.splitlines()[0], "feat(driver): adopt the lane subject")
            self.assertIn("Recorded the lane-authored rationale.", message)
            self.assertIn(
                "Adopted the lane-tip commit message without repair.",
                message,
            )

    def test_an_invalid_lane_tip_falls_back_to_the_task_template(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            context = prepared_lane_context(
                Path(temporary),
                message="implement task 1",
            )
            _, merged = merge_lane(context)
            message = self.merged_message(context, merged)
            self.assertEqual(message.splitlines()[0], "task-1: Task task-1")
            self.assertIn(
                "Rejected the lane-tip commit message and used the task-id "
                "template instead.",
                message,
            )
            self.assertIn("does not match", message)

    def test_a_lane_tip_cannot_forge_managed_trailers(self) -> None:
        cases = {
            "assisted": ("aSsIsTeD-bY: forged", "Assisted-by trailer"),
            "completion": ("tAlLy-TaSk: forged", "managed completion trailer"),
        }
        for name, (trailer, reason) in cases.items():
            with self.subTest(name), tempfile.TemporaryDirectory() as temporary:
                context = prepared_lane_context(
                    Path(temporary),
                    message=(
                        "feat(driver): attempt trailer injection\n\n"
                        f"Recorded the attempt.\n\n{trailer}"
                    ),
                )
                _, merged = merge_lane(context)
                message = self.merged_message(context, merged)
                self.assertEqual(message.splitlines()[0], "task-1: Task task-1")
                self.assertIn(reason, message)
                self.assertNotIn(trailer, message)

    def test_a_101_column_body_is_folded_and_punctuated(self) -> None:
        wide_body = "Recorded " + ("x" * 92)
        self.assertEqual(len(wide_body), 101)
        with tempfile.TemporaryDirectory() as temporary:
            context = prepared_lane_context(
                Path(temporary),
                message=f"fix(driver): format the lane body\n\n{wide_body}",
            )
            _, merged = merge_lane(context)
            message = self.merged_message(context, merged)
            self.assertEqual(message.splitlines()[0], "fix(driver): format the lane body")
            self.assertIn("\nRecorded\n", message)
            self.assertIn("\n" + ("x" * 92) + ".\n", message)
            self.assertTrue(
                all(len(line) <= 100 for line in message.splitlines()),
                message,
            )
            self.assertIn("after deterministic formatting", message)

    def test_the_steward_catalog_role_remains_bound_but_is_not_invoked(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            invoked = root / "steward-was-invoked"
            executable = root / "unused-steward"
            executable.write_text(
                (
                    f"#!{sys.executable}\n"
                    "from pathlib import Path\n"
                    f"Path({str(invoked)!r}).write_text('invoked', encoding='utf-8')\n"
                    "raise SystemExit(91)\n"
                ),
                encoding="utf-8",
            )
            executable.chmod(0o755)
            context = prepared_lane_context(
                root,
                message="feat(driver): keep the steward catalog seam",
            )
            _, merged = merge_lane(
                context,
                steward_catalog_role([sys.executable, str(executable)]),
            )
            self.assertFalse(invoked.exists())
            self.assertEqual(
                self.merged_message(context, merged).splitlines()[0],
                "feat(driver): keep the steward catalog seam",
            )


class OutcomeFirstGrammarTests(unittest.TestCase):
    """The public steering action enforces the outcome-first prose contract."""

    def steer(
        self,
        root: Path,
        checkout: Path,
        diagnosis: str,
        *,
        task_id: str = "task-1",
        attempt: int = 1,
    ) -> tuple[dict[str, Any], str]:
        result = run_driver(
            "steer",
            {
                "campaign": "fixture",
                "repository": "acme/spec",
                "repositoryConfig": repository_config(checkout),
                "issue": issue(),
                "taskId": task_id,
                "attempt": attempt,
                "diagnosis": diagnosis,
                "attemptReceipts": attempt_receipts(root),
            },
        )
        record = next(
            item
            for item in attempt_records(root)
            if item["kind"] == "diagnosis"
            and item["taskId"] == task_id
            and item["attempt"] == attempt
        )
        return result, str(record["diagnosis"])

    def test_a_compliant_leading_sentence_is_accepted(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            diagnosis = (
                "Fixed the drift the reconciler kept re-reading.\n\n"
                "- detail one\n- detail two"
            )
            _, body = self.steer(root, checkout, diagnosis)
            self.assertEqual(body, diagnosis)

    def test_empty_text_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            with self.assertRaisesRegex(DriverFailure, "non-empty"):
                self.steer(root, checkout, "   ")

    def test_over_length_text_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            diagnosis = "Investigated the failure. " + "x" * MAX_DIAGNOSIS_CHARS
            with self.assertRaisesRegex(DriverFailure, "exceeds 12000 characters"):
                self.steer(root, checkout, diagnosis)

    def test_a_bare_exclamation_mark_in_prose_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            _, body = self.steer(root, checkout, "Fixed the drift!")
            self.assertIn("exclamation mark", body)
            self.assertIn("Validation rejected the proposal", body)

    def test_a_shell_negation_inside_inline_code_is_accepted(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            diagnosis = (
                "Recorded a failing check. Reproduce with "
                "`! grep -n stale test/x.py`."
            )
            _, body = self.steer(root, checkout, diagnosis)
            self.assertEqual(body, diagnosis)

    def test_an_inline_code_span_does_not_hide_a_prose_exclamation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            _, body = self.steer(
                root,
                checkout,
                "Recorded a failing check! Reproduce with `! grep -n stale test/x.py`.",
            )
            self.assertIn("exclamation mark", body)

    def test_an_unclosed_inline_code_span_does_not_hide_an_exclamation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            _, body = self.steer(
                root,
                checkout,
                "Recorded a failing check. Reproduce with `! grep -n stale test/x.py.",
            )
            self.assertIn("exclamation mark", body)

    def test_rejected_excerpt_preserves_code_but_sanitizes_prose_bangs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            _, body = self.steer(
                root,
                checkout,
                "Investigating now! Reproduce with `! grep -n stale test/x.py`.",
            )
            self.assertIn(
                "Investigating now. Reproduce with `! grep -n stale test/x.py`.",
                body,
            )
            self.assertNotIn("Investigating now!", body)

    def test_a_list_opening_the_text_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            _, body = self.steer(root, checkout, "- detail one\n- detail two")
            self.assertIn("not a list", body)

    def test_a_leading_line_with_no_terminating_period_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            _, body = self.steer(root, checkout, "Fixed the drift")
            self.assertIn("end with a period", body)

    def test_a_present_tense_or_non_verb_opening_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            for index, opening in enumerate(
                ("Fixing the drift.", "The drift is fixed.", "Fix the drift."), 1
            ):
                with self.subTest(opening):
                    _, body = self.steer(
                        root, checkout, opening, task_id=f"task-{index}"
                    )
                    self.assertIn("past-tense verb", body)

    def test_an_irregular_past_tense_opening_is_accepted_case_insensitively(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            for task_id, diagnosis in (("task-1", "read the log."), ("task-2", "Read the log.")):
                _, body = self.steer(root, checkout, diagnosis, task_id=task_id)
                self.assertEqual(body, diagnosis)

    def test_the_closing_summary_leads_with_an_outcome_first_sentence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            _, worklist_brief = install_worklist(checkout, [task("task-1")])
            self.steer(root, checkout, "Investigated the first failure.", attempt=1)
            self.steer(root, checkout, "Investigated the second failure.", attempt=2)
            result = run_driver(
                "escalate",
                {
                    "campaign": "fixture",
                    "campaignIdentity": CAMPAIGN_ID,
                    "repository": "acme/spec",
                    "repositoryConfig": repository_config(checkout),
                    "issue": issue(),
                    **{key: worklist_brief[key] for key in ("worklist", "maxTasks", "maxParallel")},
                    "attemptReceipts": attempt_receipts(root),
                },
            )
            reference = str(result["summary"]).split("acme/spec/", 1)[1]
            summary = read_remote_blob(checkout, reference)["body"]
            lines = [line for line in summary.split("\n") if line]
            prose = next(line for line in lines if not line.startswith(("<", "#", "-")))
            self.assertTrue(prose.startswith("Settled "))


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

            result = run_driver("steeringRecheck",
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

            result = run_driver("steeringRecheck", self.brief(source, [before]))

            self.assertEqual(result["authorizedComments"], [after["comment"]])
            self.assertEqual(result["receipt"]["lateRecheckCommentIds"], [1])

    def test_partial_local_record_is_refused_instead_of_silently_skipped(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            source = self.source(Path(temporary), 0)
            Path(str(source["logPath"])).write_text("{", encoding="utf-8")

            with self.assertRaisesRegex(
                DriverFailure, "incomplete final record"
            ):
                run_driver("steeringRecheck", self.brief(source, []))

    def test_local_record_embargo_must_be_one_second_after_creation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            source = self.source(Path(temporary), 0)
            record = local_steering_record(1, "Keep the embargo.", None)
            record["doNotDispatchBefore"] = "2026-08-13T00:00:09Z"
            self.write_records(source, [record])

            with self.assertRaisesRegex(
                DriverFailure, "inconsistent append-only timestamps"
            ):
                run_driver("steeringRecheck", self.brief(source, []))

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
                DriverFailure, "does not advance doNotDispatchBefore"
            ):
                run_driver("steeringRecheck", self.brief(source, []))


class SteeringGrammarTests(unittest.TestCase):
    """#385: the public prose contract also applies to steering notes."""

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
        records = attempt_records(Path(str(config["checkout"])).parent)
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
            steered = run_driver("steer",
                self.brief(root, checkout, diagnosis=rejected)
            )
            self.assertTrue(steered["posted"])
            self.assertEqual(steered["kind"], "diagnosis")
            blob = self.receipt_record(config, steered)
            self.assertEqual(blob["kind"], "diagnosis")
            body = str(blob["diagnosis"])
            self.assertIn("grammar-rejected", body)
            self.assertIn("must end with a period", body)
            self.assertIn("Redacted proposal excerpt:", body)
            self.assertIn("[redacted-token]", body)
            self.assertNotIn(secret, body)
            accepted = run_driver(
                "steer",
                self.brief(root, checkout, taskId="task-2", diagnosis=body),
            )
            self.assertTrue(accepted["posted"])
            self.assertEqual(
                self.receipt_record(config, accepted)["diagnosis"], body
            )
            self.assertEqual(
                [record["kind"] for record in attempt_records(root)],
                ["diagnosis", "diagnosis"],
            )

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
            omitted = run_driver("steer",
                self.brief(
                    root,
                    checkout,
                    diagnosis="Investigated the failing gate carefully.",
                    gateEvidence={"id": "gate:forbid-secrets", "detail": detail},
                )
            )
            body = self.receipt_body(config, omitted)
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
            steered = run_driver("steer",
                self.brief(
                    root,
                    checkout,
                    diagnosis=diagnosis,
                    gateEvidence={"id": "gate:forbid-secrets", "detail": detail},
                )
            )
            body = self.receipt_body(config, steered)
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
        records = attempt_records(Path(str(config["checkout"])).parent)
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
            config = repository_config(checkout, "local")

            steered = run_driver("steer", self.brief(checkout))

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
            self.assertEqual(
                [
                    (item["taskId"], item["attempt"])
                    for item in attempt_records(root)
                    if item["kind"] == "diagnosis"
                ],
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
            config = repository_config(checkout, "local")

            run_driver("steer", self.brief(checkout))

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
            config = repository_config(checkout, "local")

            steered = run_driver("steer",
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
            with self.assertRaisesRegex(DriverFailure, "abortReason"):
                run_driver("steer", self.brief(checkout, abortReason="whatever"))

    def test_a_breach_without_an_abort_reason_keeps_its_own_sentence(self) -> None:
        """The #386 caller sent no `abortReason` and still must not change."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            config = repository_config(checkout, "local")

            run_driver("steer", self.brief(checkout))

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
            config = repository_config(checkout, "local")

            steered = run_driver("steer", self.brief(checkout, diagnosis=bad))

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
        prose = lead + ("x" * (MAX_DIAGNOSIS_CHARS - len(lead)))
        self.assertEqual(len(prose), MAX_DIAGNOSIS_CHARS)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, _ = initialize_repository(root, remote=True)
            config = repository_config(checkout, "local")

            run_driver("steer", self.brief(checkout, diagnosis=prose))

            body = self.blob(config, 1)["diagnosis"]
            self.assertLessEqual(len(body), MAX_DIAGNOSIS_CHARS)
            # The load-bearing halves survived the squeeze.
            self.assertIn("Aborted the lane", body)
            self.assertIn("secrets/leak.pem", body)


class SquashMergeTests(unittest.TestCase):
    """Squash integration, and the proofs that replace head ancestry."""

    def campaign(self, root: Path, *, commits: int = 2) -> dict[str, Any]:
        checkout, _ = initialize_repository(root, remote=True)
        admitted, worklist_brief = install_worklist(checkout, [task("task-1")])
        campaign_task = admitted["tasks"][0]
        workspace_root = root / "workspaces"
        run_id = "merge-pass"
        brief = prep_brief(
            checkout,
            workspace_root,
            run_id,
            source_revision=admitted["source"]["revision"],
        )
        brief["task"] = campaign_task
        workspace = run_driver("prep", brief)
        for index in range(commits):
            commit_lane(
                workspace,
                content="".join(f"line {line}\n" for line in range(index + 1)),
                message=f"wip: task step {index + 1}",
            )
        publication = run_driver(
            "publish",
            publication_brief(
                checkout,
                workspace_root,
                run_id,
                campaign_task,
                workspace,
            ),
        )
        integration = run_driver(
            "rebase",
            rebase_brief(
                checkout,
                workspace_root,
                run_id,
                campaign_task,
                workspace,
                publication,
            ),
        )
        return {
            "checkout": checkout,
            "workspaceRoot": workspace_root,
            "runId": run_id,
            "task": campaign_task,
            "workspace": workspace,
            "publication": publication,
            "integration": integration,
            "worklistBrief": worklist_brief,
            "root": root,
        }

    def reconcile(self, context: dict[str, Any]) -> dict[str, Any]:
        worklist = context["worklistBrief"]
        return run_driver(
            "reconcile",
            {
                "campaign": "fixture",
                "campaignIdentity": CAMPAIGN_ID,
                "repository": "acme/spec",
                "repositoryConfig": repository_config(context["checkout"]),
                "issue": issue(),
                **{key: worklist[key] for key in ("worklist", "maxTasks", "maxParallel")},
                "attemptReceipts": attempt_receipts(context["root"]),
            },
        )

    def merge(self, context: dict[str, Any], method: str = "squash") -> dict[str, Any]:
        return run_driver(
            "merge",
            merge_brief(
                context["checkout"],
                context["workspaceRoot"],
                context["runId"],
                context["task"],
                context["workspace"],
                context["integration"],
                method=method,
            ),
        )

    def test_local_squash_lands_one_commit_and_a_readable_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            context = self.campaign(Path(directory))
            checkout = context["checkout"]
            revision = context["task"]["revision"]
            head = context["integration"]["head"]
            merged = self.merge(context)
            merge_commit = merged["mergeCommit"]
            base = git(checkout, "rev-parse", integration_branch())
            self.assertEqual(base, merge_commit)
            self.assertEqual(len(git(checkout, "log", "-1", "--format=%P", base).split()), 1)
            message = git(checkout, "log", "-1", "--format=%B", base)
            self.assertTrue(
                message.endswith(
                    f"Tally-Task: task-1\nTally-Revision: {revision}"
                ),
                message,
            )
            self.assertNotEqual(
                command(
                    "git", "-C", str(checkout), "merge-base", "--is-ancestor", head, base,
                    check=False,
                ).returncode,
                0,
            )
            receipt = merge_receipt_ref("task-1", revision)
            self.assertEqual(local_refs(checkout, receipt).get(receipt), merge_commit)
            facts = self.reconcile(context)["merged"]
            self.assertEqual([fact["mergeCommit"] for fact in facts], [merge_commit])

            git(checkout, "update-ref", receipt, git(checkout, "rev-parse", f"{merge_commit}^"))
            self.assertEqual(self.reconcile(context)["merged"], facts)

    def test_a_moved_integration_branch_is_refused_and_mergeable_next_pass(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            context = self.campaign(Path(directory))
            checkout = context["checkout"]
            revision = context["task"]["revision"]
            witnessed = context["integration"]
            (checkout / "sibling.txt").write_text("sibling\n", encoding="utf-8")
            git(checkout, "add", "sibling.txt")
            git(checkout, "commit", "--quiet", "-m", "sibling: advance integration")
            sibling = git(checkout, "rev-parse", "HEAD")
            git(
                checkout,
                "update-ref",
                f"refs/heads/{integration_branch()}",
                sibling,
                witnessed["baseRev"],
            )
            with self.assertRaisesRegex(DriverFailure, "integration branch moved"):
                self.merge(context)
            receipt = merge_receipt_ref("task-1", revision)
            self.assertIsNone(local_refs(checkout, receipt).get(receipt))
            self.assertEqual(self.reconcile(context)["merged"], [])

            context["integration"] = run_driver(
                "rebase",
                rebase_brief(
                    checkout,
                    context["workspaceRoot"],
                    context["runId"],
                    context["task"],
                    context["workspace"],
                    context["publication"],
                ),
            )
            merged = self.merge(context)
            self.assertEqual(local_refs(checkout, receipt).get(receipt), merged["mergeCommit"])
            self.assertEqual(git(checkout, "rev-parse", integration_branch()), merged["mergeCommit"])
            self.assertEqual(
                [fact["mergeCommit"] for fact in self.reconcile(context)["merged"]],
                [merged["mergeCommit"]],
            )

    def test_a_moved_published_branch_is_refused_by_the_actual_head_guard(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            context = self.campaign(Path(directory))
            checkout = context["checkout"]
            integration = context["integration"]
            (checkout / "moved.txt").write_text("moved\n", encoding="utf-8")
            git(checkout, "add", "moved.txt")
            git(checkout, "commit", "--quiet", "-m", "move published branch")
            moved = git(checkout, "rev-parse", "HEAD")
            git(
                checkout,
                "update-ref",
                f"refs/heads/{integration['branch']}",
                moved,
                integration["head"],
            )
            with self.assertRaisesRegex(DriverFailure, "published branch moved"):
                self.merge(context)
            self.assertEqual(
                git(checkout, "rev-parse", integration_branch()), integration["baseRev"]
            )

    def test_a_reachable_but_untrailed_commit_does_not_complete_a_task(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout, _ = initialize_repository(root, remote=True)
            admitted, worklist_brief = install_worklist(checkout, [task("task-1")])
            context = {
                "checkout": checkout,
                "worklistBrief": worklist_brief,
                "root": root,
            }
            base = self.reconcile(context)["baseRevision"]
            git(checkout, "commit", "--quiet", "--allow-empty", "-m", "unmarked integration commit")
            unmarked = git(checkout, "rev-parse", "HEAD")
            git(
                checkout,
                "update-ref",
                f"refs/heads/{integration_branch()}",
                unmarked,
                base,
            )
            revision = admitted["tasks"][0]["revision"]
            branch = stable_publish_branch(revision=revision)
            git(checkout, "update-ref", f"refs/heads/{branch}", unmarked)
            git(checkout, "update-ref", merge_receipt_ref("task-1", revision), unmarked)
            self.assertEqual(self.reconcile(context)["merged"], [])

    def test_only_one_contiguous_final_trailer_block_completes_a_task(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout, _ = initialize_repository(root, remote=True)
            admitted, worklist_brief = install_worklist(checkout, [task("task-1")])
            context = {
                "checkout": checkout,
                "worklistBrief": worklist_brief,
                "root": root,
            }
            base = self.reconcile(context)["baseRevision"]
            revision = admitted["tasks"][0]["revision"]
            trailers = f"Tally-Task: task-1\nTally-Revision: {revision}"
            git(
                checkout,
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "feat(fixture): leave trailer-shaped prose",
                "-m",
                trailers,
                "-m",
                "Explained why those lines are only an example.",
            )
            git(
                checkout,
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "feat(fixture): split the trailer block",
                "-m",
                "Tally-Task: task-1",
                "-m",
                f"Tally-Revision: {revision}",
            )
            git(
                checkout,
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "feat(fixture): complete the task",
                "-m",
                trailers,
            )
            completed = git(checkout, "rev-parse", "HEAD")
            git(
                checkout,
                "update-ref",
                f"refs/heads/{integration_branch()}",
                completed,
                base,
            )
            self.assertEqual(
                [fact["mergeCommit"] for fact in self.reconcile(context)["merged"]],
                [completed],
            )

    def test_local_merge_method_still_produces_a_merge_commit_and_no_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            context = self.campaign(Path(directory))
            checkout = context["checkout"]
            head = context["integration"]["head"]
            merged = self.merge(context, "merge")
            merge_commit = merged["mergeCommit"]
            self.assertEqual(
                len(git(checkout, "log", "-1", "--format=%P", merge_commit).split()), 2
            )
            self.assertEqual(
                command(
                    "git",
                    "-C",
                    str(checkout),
                    "merge-base",
                    "--is-ancestor",
                    head,
                    merge_commit,
                    check=False,
                ).returncode,
                0,
            )
            self.assertEqual(
                local_refs(
                    checkout,
                    f"{local_state_prefix('fixture', CAMPAIGN_ID)}/merge/",
                ),
                {},
            )
            self.assertEqual(
                [fact["mergeCommit"] for fact in self.reconcile(context)["merged"]],
                [merge_commit],
            )
            self.assertNotEqual(merge_commit, head)


class AssistedByTrailerTests(unittest.TestCase):
    ASSISTED = {
        "adapter": "codex",
        "model": "provider/model-1",
        "taskUuid": "00000000-0000-4000-8000-000000000311",
        "witnessSeq": 42,
    }

    def test_the_trailer_is_the_published_format(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            valid = prepared_publication(root / "valid")
            merged = run_driver(
                "merge",
                merge_brief(
                    valid["checkout"],
                    valid["workspaceRoot"],
                    valid["runId"],
                    valid["task"],
                    valid["workspace"],
                    valid["integration"],
                    assisted_by=self.ASSISTED,
                ),
            )
            trailer = (
                "Assisted-by: codex:provider/model-1 "
                "(tally:00000000-0000-4000-8000-000000000311 witness:42)"
            )
            self.assertEqual(merged["trailer"], trailer)
            message = git(
                valid["checkout"],
                "log",
                "-1",
                "--format=%B",
                merged["mergeCommit"],
            )
            completion = (
                "Tally-Task: task-1\n"
                f"Tally-Revision: {valid['task']['revision']}"
            )
            self.assertTrue(message.endswith(completion + "\n" + trailer), message)

            absent = prepared_publication(root / "absent")
            without = run_driver(
                "merge",
                merge_brief(
                    absent["checkout"],
                    absent["workspaceRoot"],
                    absent["runId"],
                    absent["task"],
                    absent["workspace"],
                    absent["integration"],
                ),
            )
            self.assertIsNone(without["trailer"])
            self.assertNotIn(
                "Assisted-by:",
                git(absent["checkout"], "log", "-1", "--format=%B", without["mergeCommit"]),
            )

            invalid = prepared_publication(root / "invalid")
            for broken in (
                {**self.ASSISTED, "taskUuid": "not-a-uuid"},
                {**self.ASSISTED, "witnessSeq": 0},
                {**self.ASSISTED, "model": ""},
                {**self.ASSISTED, "model": "provider/model (1)"},
            ):
                with self.subTest(broken=broken), self.assertRaises(DriverFailure):
                    run_driver(
                        "merge",
                        merge_brief(
                            invalid["checkout"],
                            invalid["workspaceRoot"],
                            invalid["runId"],
                            invalid["task"],
                            invalid["workspace"],
                            invalid["integration"],
                            assisted_by=broken,
                        ),
                    )

if __name__ == "__main__":
    unittest.main()
