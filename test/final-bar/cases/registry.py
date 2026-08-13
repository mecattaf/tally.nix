"""Campaign registry rollback and asset-ownership contracts (#447/#448)."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
from typing import Any

from cases.manifest import (
    CODE_REPOSITORY,
    WORKLIST_PATTERN,
    base_manifest,
    commit_worklist,
    host_config,
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
    document = base_manifest(checkout)
    commit_worklist(context, checkout, document)
    config = root / "config.json"
    host_config(context, config)
    state = root / "registry"
    chosen_flow = flow or context.tally.parent.parent / "share/tally/flows/spec-build.js"
    chosen_driver = driver or context.driver
    environment = context.environment()
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
        CODE_REPOSITORY,
        WORKLIST_PATTERN,
        "--checkout",
        checkout,
        "--no-enqueue",
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
    return state, checkout, environment


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
    "schema-4 authority is self-contained, tuning stays in a sidecar, and actual N-1 refuses it",
    long=True,
)
def campaign_registry_n_minus_one(context: Context) -> None:
    root = make_case_directory(context, "campaign-registry-n-minus-one")
    failures: list[str] = []
    for label, wait in (("default", None), ("explicit", 240_000)):
        lane = root / label
        lane.mkdir()
        state, checkout, _ = arm_fixture(context, lane, projection_wait_ms=wait)
        authority_path = authority_file(state)
        encoded = authority_path.read_bytes()
        authority = json.loads(authority_path.read_text(encoding="utf-8"))
        if authority.get("schemaVersion") != 4:
            failures.append(f"{label}: authority schema is {authority.get('schemaVersion')!r}, expected 4")
        for field, expected in (
            ("codeRepository", CODE_REPOSITORY),
            ("worklistPattern", WORKLIST_PATTERN),
            ("checkout", str(checkout)),
            ("baseBranch", "main"),
            ("remote", "origin"),
        ):
            if authority.get(field) != expected:
                failures.append(f"{label}: self-contained authority {field}={authority.get(field)!r}, expected {expected!r}")
        if "projectionWaitMs" in authority:
            failures.append(f"{label}: host tuning leaked into closed v4 authority JSON")
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
        current_code, current_values, current_detail = list_with(context, context.tally, state)
        if current_code != 0 or not isinstance(current_values, list) or len(current_values) != 1:
            failures.append(f"{label}: current reader rejected v4 authority: {current_detail[-1600:]}")
        code, _, detail = list_with(context, context.n_minus_one_tally, state)
        if code == 0:
            failures.append(f"{label}: actual N-1 silently accepted the newer v4 authority")
        if authority_path.read_bytes() != encoded:
            failures.append(f"{label}: N-1 refusal rewrote current authority bytes")
    require(not failures, "N/N-1 registry failures:\n- " + "\n- ".join(failures))


@case(
    "campaign-registry-forward-read",
    (447,),
    "current reader refuses literal schema-3 authority with the disarm/re-arm remedy and no rewrite",
)
def campaign_registry_forward_read(context: Context) -> None:
    root = make_case_directory(context, "campaign-registry-forward-read")
    state = root / "registry"
    armed = state / "campaigns/armed"
    armed.mkdir(parents=True)
    flow = context.tally.parent.parent / "share/tally/flows/spec-build.js"
    original = {
        "schemaVersion": 3,
        "registrationId": "019f1000-0000-7000-8000-000000000447",
        "worklistPattern": WORKLIST_PATTERN,
        "codeRepository": CODE_REPOSITORY,
        "armedAt": "2026-08-08T00:00:00Z",
        "armSerial": 1,
        "approvedGraphDigest": "sha256:" + "a" * 64,
        "localActor": "uid:1000",
        "allowedActors": ["local"],
        "lastObservation": None,
        "flow": str(flow),
        "driver": str(context.driver),
        "workspaceRoot": str(root / "workspaces"),
    }
    path = armed / "n-minus-one.json"
    encoded = json.dumps(original, sort_keys=True).encode()
    path.write_bytes(encoded)
    code, values, detail = list_with(context, context.tally, state)
    require(code != 0, "current reader silently accepted pre-self-contained schema 3")
    lowered = detail.lower()
    for text in ("schema 3", "schema 4", "disarm and re-arm"):
        require(text in lowered, f"schema refusal omitted {text!r}: {detail[-2000:]}")
    require(values is None, f"refused schema unexpectedly produced a list: {values!r}")
    require(path.read_bytes() == encoded, "schema refusal rewrote authority bytes")


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
    "flow and driver get independent indirect roots before visibility and cleanup on disarm",
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
    state, _, arm_env = arm_fixture(context, root / "disarm", extra_env=environment)
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
        CODE_REPOSITORY,
        WORKLIST_PATTERN,
        "--state-dir",
        state,
        env=arm_env,
    )
    if disarmed.returncode != 0:
        failures.append(f"disarm failed: {disarmed.stderr[-1200:]}")
    if any(link.exists() or link.is_symlink() for link, _ in calls):
        failures.append("disarm left an asset GC root")

    repeat_root = root / "repeat-disarm"
    repeat_log = repeat_root / "nix-store.jsonl"
    repeat_fake = repeat_root / "fake-nix-store"
    repeat_root.mkdir()
    copy_executable(SUITE_ROOT / "fixtures/registry/fake-nix-store.py", repeat_fake)
    state2, _, env2 = arm_fixture(
        context,
        repeat_root / "arm",
        extra_env={
            "TALLY_NIX_STORE_PROGRAM": str(repeat_fake),
            "FINAL_BAR_NIX_STORE_LOG": str(repeat_log),
        },
    )
    repeat_calls = root_calls(repeat_log)
    disarmed2 = context.command(
        context.tally,
        "campaign",
        "disarm",
        CODE_REPOSITORY,
        WORKLIST_PATTERN,
        "--state-dir",
        state2,
        env=env2,
        timeout=60,
    )
    if disarmed2.returncode != 0:
        failures.append(f"second local disarm failed: {(disarmed2.stderr or disarmed2.stdout)[-1200:]}")
    if list((state2 / "campaigns/armed").glob("*.json")):
        failures.append("second local disarm did not remove authority")
    if any(link.exists() or link.is_symlink() for link, _ in repeat_calls):
        failures.append("second local disarm left an asset GC root")
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
        context.tally,
        "campaign",
        "disarm",
        CODE_REPOSITORY,
        WORKLIST_PATTERN,
        "--state-dir",
        state,
        env=environment,
    )
    if disarmed.returncode != 0:
        failures.append(f"snapshot disarm failed: {disarmed.stderr[-1200:]}")
    if flow_registered.exists() or driver_registered.exists():
        failures.append("disarm left unreferenced snapshot content")
    require(not failures, "non-store snapshot failures:\n- " + "\n- ".join(failures))
