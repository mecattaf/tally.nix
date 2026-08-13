"""Campaign manifest arm/driver shared-language corpus (#439/#444/#446)."""

from __future__ import annotations

from copy import deepcopy
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
from typing import Any

from support import (
    SUITE_ROOT,
    Context,
    canonical_json,
    case,
    make_case_directory,
    require,
    require_equal,
)


ISSUE_URL = "https://github.com/acme/spec/issues/7"
BEGIN = "<!-- tally:campaign:v1 -->"
END = "<!-- tally:campaign:v1:end -->"
WORKLIST_BEGIN = "<!-- tally:campaign-worklist:v1 -->"
WORKLIST_END = "<!-- tally:campaign-worklist:v1:end -->"
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


def base_manifest(checkout: Path) -> dict[str, Any]:
    return {
        "schemaVersion": 1,
        "name": "final-bar",
        "repository": {
            "checkout": str(checkout),
            "baseBranch": "main",
            "remote": "origin",
            "forge": "local",
        },
        "maxTasks": 1,
        "maxParallel": 1,
        "driverRuntimeMaxSec": 120,
        "runtimeMaxSec": 600,
        "pool": "campaign",
        "mergeMethod": "squash",
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
        "tasks": [
            {
                "id": "task-one",
                "kind": "implementation",
                "issue": 8,
                "dependencies": [],
                "conflictDomains": ["result.txt"],
            }
        ],
    }


def apply_ops(value: dict[str, Any], operations: list[dict[str, Any]]) -> None:
    for operation in operations:
        path = operation["path"]
        parent: Any = value
        for component in path[:-1]:
            parent = parent[component]
        key = path[-1]
        if operation["op"] == "set":
            parent[key] = deepcopy(operation["value"])
        elif operation["op"] == "delete":
            del parent[key]
        else:
            raise ValueError(f"unknown corpus operation: {operation!r}")


def normalized_manifest(raw: dict[str, Any]) -> dict[str, Any]:
    """Normative accepted-value oracle; called only for accept fixtures."""
    manifest = deepcopy(raw)
    manifest["repository"]["checkout"] = str(Path(manifest["repository"]["checkout"]).resolve())
    steward = manifest.get("steward")
    if steward is not None:
        steward.setdefault("env", {})
        steward.setdefault("finalMessagePattern", DEFAULT_FINAL_PATTERN)
        steward.setdefault("runtimeMaxSec", 120)
    for task in manifest["tasks"]:
        if task["kind"] == "implementation" and "conflictDomains" not in task:
            # Absence is a value in the serial grammar, not an empty list.
            task.pop("conflictDomains", None)
        task.setdefault("argv", None)
        task.setdefault("runtimeMaxSec", None)
    return manifest


def graph_value(manifest: dict[str, Any]) -> dict[str, Any]:
    records = {
        8: {"number": 8, "title": "Task one", "body": "Create result.txt."},
        9: {"number": 9, "title": "Task two", "body": "Create second.txt."},
    }
    return {
        "manifest": manifest,
        "tasks": [records[int(task["issue"])] for task in manifest["tasks"]],
    }


def digest(value: Any) -> str:
    return "sha256:" + hashlib.sha256(canonical_json(value)).hexdigest()


def issue_state(path: Path, manifest: dict[str, Any]) -> None:
    worklist = "".join(
        f"- [ ] <!-- tally:campaign-task:v1 id={task['id']} --> "
        f"#{int(task['issue'])} — {'Task one' if int(task['issue']) == 8 else 'Task two'}\n"
        for task in manifest["tasks"]
    )
    body = (
        f"Campaign fixture\n\n{BEGIN}\n```json\n{json.dumps(manifest, ensure_ascii=False)}\n"
        f"```\n{END}\n\n{WORKLIST_BEGIN}\n{worklist}{WORKLIST_END}\n"
    )
    value = {
        "actor": "operator",
        "master": {
            "number": 7,
            "title": "Final bar campaign",
            "body": body,
            "state": "open",
            "html_url": ISSUE_URL,
            "updated_at": "2026-08-08T00:00:00Z",
            "user": {"login": "operator"},
        },
        "subissues": [
            {
                "number": number,
                "title": "Task one" if number == 8 else "Task two",
                "body": "Create result.txt." if number == 8 else "Create second.txt.",
                "state": "open",
                "html_url": f"https://github.com/acme/spec/issues/{number}",
                "updated_at": "2026-08-08T00:00:00Z",
                "user": {"login": "operator"},
            }
            for number in [int(task["issue"]) for task in manifest["tasks"]]
        ],
        "masterComments": [],
        "threadComments": {},
    }
    path.write_text(json.dumps(value), encoding="utf-8")


def host_config(context: Context, path: Path) -> None:
    presets = context.presets
    value = {
        "enqueue": {"fanoutCap": 64},
        "pools": {
            "flow": {"resource": "cpu-slot", "capacity": 8},
            "campaign-agent": {"resource": "slot", "capacity": 4},
            "campaign-control": {"resource": "cpu-slot", "capacity": 4},
            "campaign": {"resource": "mutex", "capacity": 1},
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


@case(
    "campaign-manifest-corpus",
    (439, 444, 446),
    "Rust arm and packaged driver accept, normalize, hash, or reject one shared corpus",
)
def campaign_manifest_corpus(context: Context) -> None:
    root = make_case_directory(context, "campaign-manifest-corpus")
    checkout, _ = repository(context, root)
    symlink = root / "checkout-link"
    symlink.symlink_to(checkout, target_is_directory=True)
    nested = root / "spelling" / "child"
    nested.mkdir(parents=True)
    dotdot = nested / ".." / ".." / "checkout"
    config = root / "config.json"
    host_config(context, config)
    fake_gh = SUITE_ROOT / "fixtures/pipeline/fake-gh.py"
    fake_bin = root / "bin"
    fake_bin.mkdir()
    shutil.copy2(fake_gh, fake_bin / "gh")
    (fake_bin / "gh").chmod(0o755)
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
        raw = base_manifest(checkout)
        if fixture.get("checkout") == "symlink":
            raw["repository"]["checkout"] = str(symlink)
        elif fixture.get("checkout") == "dotdot":
            raw["repository"]["checkout"] = str(dotdot)
        if "modelLength" in fixture:
            raw["agent"]["model"] = "m" * fixture["modelLength"]
        apply_ops(raw, fixture.get("ops", []))
        forge_state = case_root / "forge.json"
        issue_state(forge_state, raw)
        registry = case_root / "registry"
        workspace = case_root / "workspaces"
        env = context.environment(
            TALLY_GH_PROGRAM=fake_bin / "gh",
            FINAL_BAR_FORGE_STATE=forge_state,
            PATH=f"{fake_bin}:{os.environ.get('PATH', '')}",
        )
        arm = context.command(
            context.tally,
            "--config",
            config,
            "--socket",
            case_root / "unused.sock",
            "campaign",
            "arm",
            ISSUE_URL,
            "--no-enqueue",
            "--allow-test-local-forge",
            "--flow",
            flow,
            "--driver",
            context.driver,
            "--state-dir",
            registry,
            "--workspace-root",
            workspace,
            env=env,
            timeout=60,
        )
        expected_accept = fixture["expect"] == "accept"
        registrations = list((registry / "campaigns/armed").glob("*.json"))
        if not expected_accept:
            driver_brief = case_root / "driver-brief.json"
            driver_brief.write_text(
                json.dumps(
                    {
                        "repository": "acme/spec",
                        "issue": {"number": "7", "url": ISSUE_URL},
                        "worklist": {"kind": "github-issue", "graphDigest": "sha256:" + "0" * 64},
                        "armedManifest": raw,
                    }
                ),
                encoding="utf-8",
            )
            driver_env = env | {"TALLY_BRIEF": str(driver_brief)}
            driven = context.command(
                "python3", context.driver_script, "reconcile", env=driver_env, timeout=60
            )
            if arm.returncode == 0 or driven.returncode == 0 or registrations:
                failures.append(
                    f"{name}: expected both rejection/no side effects; arm={arm.returncode}, "
                    f"driver={driven.returncode}, registrations={registrations}, "
                    f"arm-detail={(arm.stderr or arm.stdout)[-500:]!r}, "
                    f"driver-detail={(driven.stderr or driven.stdout)[-500:]!r}"
                )
            continue

        if arm.returncode != 0:
            failures.append(f"{name}: arm rejected valid fixture: {(arm.stderr or arm.stdout)[-1200:]}")
            continue
        try:
            armed = json.loads(arm.stdout)
        except json.JSONDecodeError as error:
            failures.append(f"{name}: arm output is not JSON: {error}: {arm.stdout!r}")
            continue
        normalized = normalized_manifest(raw)
        expected_digest = digest(graph_value(normalized))
        if armed.get("graphDigest") != expected_digest:
            failures.append(
                f"{name}: arm digest {armed.get('graphDigest')!r} != normative {expected_digest}; "
                f"canonical={canonical_json(normalized).decode()}"
            )
            continue
        if len(registrations) != 1:
            failures.append(f"{name}: accepted arm wrote {len(registrations)} registrations")
            continue
        driver_brief = case_root / "driver-brief.json"
        driver_brief.write_text(
            json.dumps(
                {
                    "repository": "acme/spec",
                    "issue": {"number": "7", "url": ISSUE_URL},
                    "worklist": {"kind": "github-issue", "graphDigest": expected_digest},
                    "armedManifest": normalized,
                }
            ),
            encoding="utf-8",
        )
        driven = context.command(
            "python3",
            context.driver_script,
            "reconcile",
            env=env | {"TALLY_BRIEF": str(driver_brief)},
            timeout=60,
        )
        if driven.returncode != 0:
            failures.append(
                f"{name}: packaged driver rejected arm's canonical value: "
                f"{(driven.stderr or driven.stdout)[-1600:]}"
            )
            continue
        output = parse_driver(driven.stdout)
        if output["config"]["repositoryConfig"]["checkout"] != normalized["repository"]["checkout"]:
            failures.append(f"{name}: driver returned a different canonical checkout")
        if output["config"].get("steward") != normalized.get("steward"):
            failures.append(
                f"{name}: driver steward normalization differs: {output['config'].get('steward')!r}"
            )
        expected_domains = normalized["tasks"][0].get("conflictDomains", "<absent>")
        observed_task = output["tasks"][0]
        observed_domains = observed_task.get("conflictDomains", "<absent>")
        if observed_domains != expected_domains:
            failures.append(
                f"{name}: driver conflictDomains {observed_domains!r} != {expected_domains!r}"
            )

    require(not failures, "manifest corpus disagreements:\n- " + "\n- ".join(failures))
