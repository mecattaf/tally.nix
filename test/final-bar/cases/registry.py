"""Campaign registry rollback and asset-ownership contracts (#447/#448)."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
from typing import Any

from cases.manifest import (
    ISSUE_URL,
    base_manifest,
    host_config,
    issue_state,
    repository,
)
from support import SUITE_ROOT, Context, case, copy_executable, make_case_directory, require


def arm_fixture(
    context: Context,
    root: Path,
    *,
    flow: Path | None = None,
    driver: Path | None = None,
    projection_wait_ms: int | None = None,
    extra_env: dict[str, str] | None = None,
) -> tuple[Path, Path, dict[str, str]]:
    checkout, _ = repository(context, root)
    manifest = base_manifest(checkout)
    forge = root / "forge.json"
    issue_state(forge, manifest)
    config = root / "config.json"
    host_config(context, config)
    fake_bin = root / "bin"
    fake_bin.mkdir()
    copy_executable(SUITE_ROOT / "fixtures/pipeline/fake-gh.py", fake_bin / "gh")
    state = root / "registry"
    chosen_flow = flow or context.tally.parent.parent / "share/tally/flows/spec-build.js"
    chosen_driver = driver or context.driver
    environment = context.environment(
        TALLY_GH_PROGRAM=fake_bin / "gh",
        FINAL_BAR_FORGE_STATE=forge,
        PATH=f"{fake_bin}:{os.environ.get('PATH', '')}",
    )
    if extra_env:
        environment.update(extra_env)
    command: list[os.PathLike[str] | str] = [
        context.tally,
        "--config",
        config,
        "--socket",
        root / "unused.sock",
        "campaign",
        "arm",
        ISSUE_URL,
        "--no-enqueue",
        "--allow-test-local-forge",
        "--flow",
        chosen_flow,
        "--driver",
        chosen_driver,
        "--state-dir",
        state,
        "--workspace-root",
        root / "workspaces",
    ]
    if projection_wait_ms is not None:
        command.extend(["--projection-wait-ms", str(projection_wait_ms)])
    armed = context.command(*command, env=environment, timeout=90)
    require(armed.returncode == 0, f"campaign arm fixture failed: {(armed.stderr or armed.stdout)[-3000:]}")
    return state, forge, environment


def authority_file(state: Path) -> Path:
    files = sorted((state / "campaigns/armed").glob("*.json"))
    require(len(files) == 1, f"expected one authority file, found {files!r}")
    return files[0]


def list_with(context: Context, executable: Path, state: Path) -> tuple[int, Any, str]:
    result = context.command(executable, "campaign", "list", "--state-dir", state, timeout=60)
    value: Any = None
    if result.returncode == 0:
        try:
            value = json.loads(result.stdout)
        except json.JSONDecodeError:
            pass
    return result.returncode, value, result.stderr or result.stdout


@case(
    "campaign-registry-n-minus-one",
    (447,),
    "current default/override authorities decode in actual N-1 and tuning stays in an ignored sidecar",
    long=True,
)
def campaign_registry_n_minus_one(context: Context) -> None:
    root = make_case_directory(context, "campaign-registry-n-minus-one")
    failures: list[str] = []
    for label, wait in (("default", None), ("explicit", 240_000)):
        lane = root / label
        lane.mkdir()
        state, _, _ = arm_fixture(context, lane, projection_wait_ms=wait)
        authority_path = authority_file(state)
        authority = json.loads(authority_path.read_text(encoding="utf-8"))
        if authority.get("schemaVersion") != 2:
            failures.append(f"{label}: authority schema is {authority.get('schemaVersion')!r}, expected 2")
        if "projectionWaitMs" in authority:
            failures.append(f"{label}: host tuning leaked into closed v2 authority JSON")
        tuning_files = sorted((state / "campaigns/host-tuning").glob("*.json"))
        if wait is None:
            if tuning_files:
                value = json.loads(tuning_files[0].read_text(encoding="utf-8"))
                if value.get("projectionWaitMs") not in (None, 10_000):
                    failures.append(f"default: sidecar does not mean 10s default: {value!r}")
        else:
            if len(tuning_files) != 1:
                failures.append(f"explicit: expected one tuning sidecar, got {tuning_files!r}")
            else:
                tuning = json.loads(tuning_files[0].read_text(encoding="utf-8"))
                if not isinstance(tuning.get("schemaVersion"), int) or tuning.get("projectionWaitMs") != wait:
                    failures.append(f"explicit: malformed tuning sidecar: {tuning!r}")
        code, values, detail = list_with(context, context.n_minus_one_tally, state)
        if code != 0 or not isinstance(values, list) or len(values) != 1:
            failures.append(f"{label}: actual N-1 could not decode authority: rc={code} {detail[-1600:]}")
        else:
            n_minus = values[0]
            for field in (
                "registrationId",
                "issueUrl",
                "repository",
                "issueNumber",
                "approvedGraphDigest",
                "authenticatedActor",
                "allowedActors",
                "flow",
                "driver",
                "workspaceRoot",
            ):
                if n_minus.get(field) != authority.get(field):
                    failures.append(f"{label}: rollback drifted authority field {field}")
    require(not failures, "N/N-1 registry failures:\n- " + "\n- ".join(failures))


@case(
    "campaign-registry-forward-read",
    (447,),
    "current reader accepts literal N-1 v2 authority and supplies default host tuning without rewriting it",
)
def campaign_registry_forward_read(context: Context) -> None:
    root = make_case_directory(context, "campaign-registry-forward-read")
    state = root / "registry"
    armed = state / "campaigns/armed"
    armed.mkdir(parents=True)
    flow = context.tally.parent.parent / "share/tally/flows/spec-build.js"
    original = {
        "schemaVersion": 2,
        "registrationId": "019f1000-0000-7000-8000-000000000447",
        "issueUrl": ISSUE_URL,
        "repository": "acme/spec",
        "issueNumber": 7,
        "armedAt": "2026-08-08T00:00:00Z",
        "armSerial": 1,
        "approvedGraphDigest": "sha256:" + "a" * 64,
        "authenticatedActor": "operator",
        "allowedActors": ["operator"],
        "allowTestLocalForge": True,
        "subIssueWalk": False,
        "lastObservation": None,
        "lastForgeObservation": None,
        "flow": str(flow),
        "driver": str(context.driver),
        "workspaceRoot": str(root / "workspaces"),
    }
    path = armed / "n-minus-one.json"
    encoded = json.dumps(original, sort_keys=True).encode()
    path.write_bytes(encoded)
    code, values, detail = list_with(context, context.tally, state)
    require(code == 0, f"current reader rejected literal N-1 v2: {detail[-2000:]}")
    require(isinstance(values, list) and len(values) == 1, f"current list shape invalid: {values!r}")
    current = values[0]
    for field, expected in original.items():
        require(current.get(field) == expected, f"forward read drifted {field}: {current.get(field)!r} != {expected!r}")
    require(path.read_bytes() == encoded, "forward read rewrote authority bytes")
    effective = current.get("projectionWaitMs", 10_000)
    require(effective == 10_000, f"absent N-1 tuning did not supply 10s default: {effective!r}")


def store_object(path: Path) -> str:
    parts = path.resolve().parts
    require(len(parts) >= 4 and parts[1:3] == ("nix", "store"), f"not a store asset: {path}")
    return "/" + "/".join(parts[1:4])


def root_calls(log: Path) -> list[tuple[Path, Path]]:
    if not log.is_file():
        return []
    calls = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines() if line]
    roots: list[tuple[Path, Path]] = []
    for arguments in calls:
        if "--add-root" in arguments and "--realise" in arguments:
            roots.append(
                (
                    Path(arguments[arguments.index("--add-root") + 1]),
                    Path(arguments[arguments.index("--realise") + 1]),
                )
            )
    return roots


@case(
    "campaign-store-asset-ownership",
    (448,),
    "flow and driver get independent indirect roots before visibility and cleanup on disarm/prune",
)
def campaign_store_asset_ownership(context: Context) -> None:
    root = make_case_directory(context, "campaign-store-asset-ownership")
    fake = root / "fake-nix-store"
    copy_executable(SUITE_ROOT / "fixtures/registry/fake-nix-store.py", fake)
    log = root / "nix-store.jsonl"
    environment = {
        "TALLY_NIX_STORE_PROGRAM": str(fake),
        "FINAL_BAR_NIX_STORE_LOG": str(log),
    }
    state, forge, arm_env = arm_fixture(context, root / "disarm", extra_env=environment)
    authority = json.loads(authority_file(state).read_text(encoding="utf-8"))
    calls = root_calls(log)
    expected_targets = {store_object(Path(authority["flow"])), store_object(Path(authority["driver"]))}
    actual_targets = {str(target) for _, target in calls}
    failures: list[str] = []
    if len(calls) != 2 or actual_targets != expected_targets:
        failures.append(f"independent root calls {calls!r} do not own {expected_targets!r}")
    if any(not link.is_symlink() or str(link.resolve()) != str(target) for link, target in calls):
        failures.append(f"one or more durable indirect roots are missing: {calls!r}")
    disarmed = context.command(
        context.tally,
        "campaign",
        "disarm",
        ISSUE_URL,
        "--state-dir",
        state,
        env=arm_env,
    )
    if disarmed.returncode != 0:
        failures.append(f"disarm failed: {disarmed.stderr[-1200:]}")
    if any(link.exists() or link.is_symlink() for link, _ in calls):
        failures.append("disarm left an asset GC root")

    prune_root = root / "prune"
    prune_log = prune_root / "nix-store.jsonl"
    prune_fake = prune_root / "fake-nix-store"
    prune_root.mkdir()
    copy_executable(SUITE_ROOT / "fixtures/registry/fake-nix-store.py", prune_fake)
    state2, forge2, env2 = arm_fixture(
        context,
        prune_root / "arm",
        extra_env={
            "TALLY_NIX_STORE_PROGRAM": str(prune_fake),
            "FINAL_BAR_NIX_STORE_LOG": str(prune_log),
        },
    )
    prune_calls = root_calls(prune_log)
    forge_value = json.loads(forge2.read_text(encoding="utf-8"))
    forge_value["master"]["state"] = "closed"
    forge2.write_text(json.dumps(forge_value), encoding="utf-8")
    polled = context.command(
        context.tally,
        "--config",
        prune_root / "arm/config.json",
        "--socket",
        prune_root / "unused.sock",
        "campaign",
        "poll",
        "--once",
        "--state-dir",
        state2,
        env=env2,
        timeout=60,
    )
    if polled.returncode != 0:
        failures.append(f"closed-issue prune failed: {(polled.stderr or polled.stdout)[-1200:]}")
    if list((state2 / "campaigns/armed").glob("*.json")):
        failures.append("closed-issue poll did not prune authority")
    if any(link.exists() or link.is_symlink() for link, _ in prune_calls):
        failures.append("closed-issue prune left an asset GC root")
    require(not failures, "store ownership failures:\n- " + "\n- ".join(failures))


@case(
    "campaign-nonstore-snapshots",
    (448,),
    "non-store override files become immutable registration-owned executable snapshots",
)
def campaign_nonstore_snapshots(context: Context) -> None:
    root = make_case_directory(context, "campaign-nonstore-snapshots")
    source = root / "overrides"
    source.mkdir()
    flow_source = source / "flow.js"
    driver_source = source / "driver"
    shutil.copy2(context.tally.parent.parent / "share/tally/flows/spec-build.js", flow_source)
    shutil.copy2(context.driver, driver_source)
    driver_source.chmod(driver_source.stat().st_mode | 0o111)
    expected = {"flow": flow_source.read_bytes(), "driver": driver_source.read_bytes()}
    state, _, environment = arm_fixture(
        context,
        root / "arm",
        flow=flow_source,
        driver=driver_source,
    )
    registration = json.loads(authority_file(state).read_text(encoding="utf-8"))
    flow_registered = Path(registration["flow"])
    driver_registered = Path(registration["driver"])
    failures: list[str] = []
    if flow_registered == flow_source or driver_registered == driver_source:
        failures.append("authority still points at mutable non-store override")
    for label, path in (("flow", flow_registered), ("driver", driver_registered)):
        if not path.is_file() or path.read_bytes() != expected[label]:
            failures.append(f"{label} registration snapshot missing or changed: {path}")
        if not str(path).startswith(str(state / "campaigns")):
            failures.append(f"{label} snapshot is not registration-owned: {path}")
    if not os.access(driver_registered, os.X_OK):
        failures.append("driver snapshot lost executable mode")
    flow_source.unlink()
    driver_source.unlink()
    code, values, detail = list_with(context, context.tally, state)
    if code != 0 or not values:
        failures.append(f"registration became unreadable after source deletion: {detail[-1200:]}")
    disarmed = context.command(
        context.tally, "campaign", "disarm", ISSUE_URL, "--state-dir", state, env=environment
    )
    if disarmed.returncode != 0:
        failures.append(f"snapshot disarm failed: {disarmed.stderr[-1200:]}")
    if flow_registered.exists() or driver_registered.exists():
        failures.append("disarm left unreferenced snapshot content")
    require(not failures, "non-store snapshot failures:\n- " + "\n- ".join(failures))
