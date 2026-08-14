"""Committed local-worklist arm/driver shared-language corpus (#439/#444/#446)."""

from __future__ import annotations

from copy import deepcopy
import hashlib
import json
from pathlib import Path
from typing import Any

from support import (
    SUITE_ROOT,
    Context,
    canonical_json,
    case,
    make_case_directory,
    require,
)


CODE_REPOSITORY = "acme/spec"
WORKLIST_PATTERN = "specs/final-bar.json"
ISSUE_URL = f"local://{CODE_REPOSITORY}/{WORKLIST_PATTERN}"
DEFAULT_FINAL_PATTERN = r"^TALLY_FINAL_MESSAGE=(.*)$"


def git(context: Context, checkout: Path, *arguments: str) -> str:
    result = context.command("git", "-C", checkout, *arguments)
    require(
        result.returncode == 0,
        f"git {' '.join(arguments)} failed: {(result.stderr or result.stdout)[-3000:]}",
    )
    return result.stdout.strip()


def repository(context: Context, root: Path) -> tuple[Path, Path]:
    checkout = root / "checkout"
    remote = root / "remote.git"
    initialized = context.command("git", "init", "--quiet", "--initial-branch=main", checkout)
    require(initialized.returncode == 0, initialized.stderr)
    git(context, checkout, "config", "user.name", "Final Bar")
    git(context, checkout, "config", "user.email", "final-bar@invalid")
    (checkout / "README.md").write_text("fixture\n", encoding="utf-8")
    git(context, checkout, "add", "README.md")
    git(context, checkout, "commit", "--quiet", "-m", "fixture: initialize")
    created = context.command("git", "init", "--bare", "--quiet", "--initial-branch=main", remote)
    require(created.returncode == 0, created.stderr)
    git(context, checkout, "remote", "add", "origin", str(remote))
    git(context, checkout, "push", "--quiet", "--set-upstream", "origin", "main")
    return checkout.resolve(), remote


def implementation_task(
    identifier: str = "task-one",
    *,
    title: str = "Task one",
    goal: str = "Create result.txt.",
    dependencies: list[str] | None = None,
    conflict_domains: list[str] | None = None,
) -> dict[str, Any]:
    task: dict[str, Any] = {
        "id": identifier,
        "kind": "implementation",
        "title": title,
        "goal": goal,
        "deliveredBehaviors": ["The requested result is committed."],
        "readFirst": {"specSections": ["Fixture specification"], "styleReferences": []},
        "acceptanceCriteria": [
            {
                "id": "result-green",
                "description": "The result exists.",
                "argv": ["test", "-f", "result.txt"],
            }
        ],
        "dependencies": dependencies or [],
    }
    if conflict_domains is not None:
        task["conflictDomains"] = conflict_domains
    return task


def base_manifest(_checkout: Path | None = None) -> dict[str, Any]:
    """The complete local worklist document committed before every arm."""
    return {
        "schemaVersion": 1,
        "campaign": {
            "name": "final-bar",
            "maxTasks": 1,
            "maxParallel": 1,
            "mergeMethod": "squash",
            "driverRuntimeMaxSec": 120,
            "runtimeMaxSec": 600,
            "agent": {
                "adapter": "shell",
                "argv": ["/bin/sh", "-c", "true"],
                "priority": "low",
                "runtimeMaxSec": 60,
                "approvalPolicy": None,
                "sandboxPolicy": None,
                "diagnosisSandboxPolicy": None,
                "model": None,
            },
            "steward": None,
            "gates": [
                {
                    "kind": "command",
                    "id": "clean",
                    "preflightArgv": ["/bin/sh", "-c", "true"],
                    "argv": ["/bin/sh", "-c", "true"],
                    "runtimeMaxSec": 30,
                }
            ],
        },
        "tasks": [implementation_task(conflict_domains=["result.txt"])],
    }


def commit_worklist(
    context: Context,
    checkout: Path,
    document: dict[str, Any],
    *,
    message: str = "fixture: commit local worklist",
) -> Path:
    path = checkout / WORKLIST_PATTERN
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(document, ensure_ascii=False) + "\n", encoding="utf-8")
    git(context, checkout, "add", WORKLIST_PATTERN)
    git(context, checkout, "commit", "--quiet", "-m", message)
    git(context, checkout, "push", "--quiet", "origin", "main")
    return path


def apply_ops(value: dict[str, Any], operations: list[dict[str, Any]]) -> None:
    for operation in operations:
        path = operation["path"]
        parent: Any = value
        for component in path[:-1]:
            parent = parent[component]
        key = path[-1]
        if operation["op"] == "set":
            if isinstance(parent, list) and key == len(parent):
                parent.append(deepcopy(operation["value"]))
            else:
                parent[key] = deepcopy(operation["value"])
        elif operation["op"] == "delete":
            del parent[key]
        else:
            raise ValueError(f"unknown corpus operation: {operation!r}")


def digest(value: Any) -> str:
    return "sha256:" + hashlib.sha256(canonical_json(value)).hexdigest()


def host_config(context: Context, path: Path) -> None:
    presets = context.presets
    value = {
        "enqueue": {"fanoutCap": 64},
        "pools": {
            "flow": {"resource": "cpu-slot", "capacity": 8},
            "campaign-agent": {"resource": "slot", "capacity": 4},
            "campaign-control": {"resource": "cpu-slot", "capacity": 4},
            f"campaign/{CODE_REPOSITORY}": {"resource": "mutex", "capacity": 1},
        },
        "adapters": {
            "shell": presets["shell"],
            "spec-build-driver": {
                "argv": [],
                "scrape": {
                    "finalMessage": {
                        "stream": "stdout",
                        "mode": "regex",
                        "pattern": DEFAULT_FINAL_PATTERN,
                    }
                },
            },
            "narrator": {"argv": ["/bin/sh", "-c", "true"]},
        },
    }
    path.write_text(json.dumps(value), encoding="utf-8")


def parse_driver(stdout: str) -> Any:
    lines = [line for line in stdout.splitlines() if line.startswith("TALLY_FINAL_MESSAGE=")]
    require(len(lines) == 1, f"packaged driver emitted no single final-message record: {stdout!r}")
    return json.loads(lines[0].split("=", 1)[1])


def approved_graph(state: Path) -> dict[str, Any]:
    paths = sorted((state / "campaigns/approved-graphs").rglob("*.graph-v1.json"))
    require(len(paths) == 1, f"expected one approved graph snapshot, found {paths!r}")
    snapshot = json.loads(paths[0].read_text(encoding="utf-8"))
    graph = snapshot.get("graph")
    require(isinstance(graph, dict), f"approved graph snapshot omitted graph: {snapshot!r}")
    return graph


def driver_worklist_brief(checkout: Path, document: dict[str, Any]) -> dict[str, Any]:
    campaign = document.get("campaign")
    campaign = campaign if isinstance(campaign, dict) else {}
    max_tasks = campaign.get("maxTasks", 64)
    max_parallel = campaign.get("maxParallel", 1)
    return {
        "repository": CODE_REPOSITORY,
        "repositoryConfig": {
            "checkout": str(checkout),
            "baseBranch": "main",
            "remote": "origin",
            "forge": "local",
        },
        "worklist": WORKLIST_PATTERN,
        "maxTasks": max_tasks,
        "maxParallel": max_parallel,
    }


@case(
    "campaign-manifest-corpus",
    (439, 444, 446),
    "Rust arm and packaged driver accept, normalize, hash, or reject one local-worklist corpus",
)
def campaign_manifest_corpus(context: Context) -> None:
    root = make_case_directory(context, "campaign-manifest-corpus")
    config = root / "config.json"
    host_config(context, config)
    flow = context.tally.parent.parent / "share/tally/flows/spec-build.js"
    require(flow.is_file(), f"packaged tally has no spec-build flow: {flow}")

    corpus = json.loads(
        (SUITE_ROOT / "fixtures/manifest/cases.json").read_text(encoding="utf-8")
    )
    failures: list[str] = []
    for fixture in corpus["cases"]:
        name = fixture["name"]
        case_root = root / name
        case_root.mkdir()
        checkout, _ = repository(context, case_root)
        document = base_manifest(checkout)
        if "modelLength" in fixture:
            document["campaign"]["agent"]["model"] = "m" * fixture["modelLength"]
        apply_ops(document, fixture.get("ops", []))
        commit_worklist(context, checkout, document)

        arm_checkout = checkout
        if fixture.get("checkout") == "symlink":
            arm_checkout = case_root / "checkout-link"
            arm_checkout.symlink_to(checkout, target_is_directory=True)
        elif fixture.get("checkout") == "dotdot":
            nested = case_root / "spelling/child"
            nested.mkdir(parents=True)
            arm_checkout = nested / ".." / ".." / "checkout"

        state = case_root / "registry"
        arm = context.command(
            context.tally,
            "--config",
            config,
            "--socket",
            case_root / "unused.sock",
            "campaign",
            "arm",
            CODE_REPOSITORY,
            WORKLIST_PATTERN,
            "--checkout",
            arm_checkout,
            "--no-enqueue",
            "--flow",
            flow,
            "--driver",
            context.driver,
            "--state-dir",
            state,
            "--workspace-root",
            case_root / "workspaces",
            timeout=60,
        )
        driver_brief = case_root / "driver-brief.json"
        driver_brief.write_text(
            json.dumps(driver_worklist_brief(arm_checkout, document)), encoding="utf-8"
        )
        driven = context.command(
            context.driver,
            "worklist",
            env=context.environment(TALLY_BRIEF=driver_brief),
            timeout=60,
        )
        expected_accept = fixture["expect"] == "accept"
        registrations = list((state / "campaigns/armed").glob("*.json"))
        if not expected_accept:
            if arm.returncode == 0 or driven.returncode == 0 or registrations:
                failures.append(
                    f"{name}: expected both rejection/no authority; arm={arm.returncode}, "
                    f"driver={driven.returncode}, registrations={registrations}, "
                    f"arm-detail={(arm.stderr or arm.stdout)[-600:]!r}, "
                    f"driver-detail={(driven.stderr or driven.stdout)[-600:]!r}"
                )
            continue

        if arm.returncode != 0 or driven.returncode != 0:
            failures.append(
                f"{name}: valid local worklist rejected; "
                f"arm={(arm.stderr or arm.stdout)[-1200:]!r}, "
                f"driver={(driven.stderr or driven.stdout)[-1200:]!r}"
            )
            continue
        armed = arm.json(f"{name} arm")
        graph = approved_graph(state)
        calculated = digest({"manifest": graph["manifest"], "tasks": graph["tasks"]})
        if armed.get("graphDigest") != calculated or graph.get("executableDigest") != calculated:
            failures.append(
                f"{name}: arm/snapshot digest disagreement: arm={armed.get('graphDigest')!r}, "
                f"graph={graph.get('executableDigest')!r}, calculated={calculated}"
            )
            continue
        manifest = graph["manifest"]
        if manifest["repository"]["checkout"] != str(checkout):
            failures.append(
                f"{name}: checkout did not canonicalize to {checkout}: "
                f"{manifest['repository']['checkout']!r}"
            )
        if manifest.get("pool") != f"campaign/{CODE_REPOSITORY}":
            failures.append(f"{name}: local runner pool drifted: {manifest.get('pool')!r}")
        output = parse_driver(driven.stdout)
        expected_ids = [task["id"] for task in document["tasks"]]
        observed_ids = [task.get("id") for task in output.get("tasks", [])]
        if observed_ids != expected_ids:
            failures.append(f"{name}: driver task order {observed_ids!r} != {expected_ids!r}")
        expected_domains = document["tasks"][0].get("conflictDomains", "<absent>")
        observed_domains = output["tasks"][0].get("conflictDomains", "<absent>")
        if observed_domains != expected_domains:
            failures.append(
                f"{name}: driver conflictDomains {observed_domains!r} != {expected_domains!r}"
            )
        if document["tasks"][0]["goal"] not in graph["tasks"][0]["body"]:
            failures.append(f"{name}: Rust graph body omitted the committed task goal")

    require(not failures, "local-worklist corpus disagreements:\n- " + "\n- ".join(failures))
