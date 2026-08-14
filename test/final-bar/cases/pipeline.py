"""Configuration-locator contract and hermetic full campaign pipeline (#442 plus E2E)."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
from typing import Any, Iterator

from cases.manifest import (
    CODE_REPOSITORY,
    WORKLIST_PATTERN,
    base_manifest,
    commit_worklist,
    host_config,
    repository,
)
from support import SUITE_ROOT, Context, case, copy_executable, make_case_directory, require


def campaign_configuration(
    context: Context,
    root: Path,
) -> dict[str, Any]:
    path = root / "source-config.json"
    host_config(context, path)
    value = json.loads(path.read_text(encoding="utf-8"))
    value["maxFrameBytes"] = 13_107_200
    value["pools"]["flow"]["capacity"] = 3
    child_env = {
        "PATH": os.environ.get("PATH", ""),
        "XDG_CONFIG_HOME": str(root / "xdg"),
    }
    value["adapters"]["shell"].setdefault("env", {}).update(child_env)
    value["adapters"]["spec-build-driver"].setdefault("env", {}).update(child_env)
    return value


def json_values(root: Path) -> Iterator[Any]:
    if not root.exists():
        return
    for path in root.rglob("*"):
        if not path.is_file() or path.stat().st_size > 8 * 1024 * 1024:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        try:
            yield json.loads(text)
            continue
        except json.JSONDecodeError:
            pass
        for line in text.splitlines():
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                continue


def lists(value: Any) -> Iterator[list[Any]]:
    if isinstance(value, list):
        yield value
        for item in value:
            yield from lists(item)
    elif isinstance(value, dict):
        for item in value.values():
            yield from lists(item)


def command_vectors(*roots: Path) -> list[list[str]]:
    vectors: list[list[str]] = []
    for root in roots:
        for value in json_values(root):
            for item in lists(value):
                if item and all(isinstance(part, str) for part in item):
                    vectors.append(item)
    return vectors


def nixos_poll_script(context: Context) -> str:
    target = json.dumps(str(context.target))
    expression = f'''
      let
        f = builtins.getFlake ("path:" + {target});
        system = builtins.currentSystem;
        evaluated = f.inputs.nixpkgs.lib.nixosSystem {{
          inherit system;
          modules = [
            f.nixosModules.tally
            {{
              system.stateVersion = "26.11";
              boot.loader.grub.enable = false;
              fileSystems."/" = {{ device = "none"; fsType = "tmpfs"; }};
              services.tally = {{
                enable = true;
                campaignPoll.enable = true;
              }};
            }}
          ];
        }};
      in evaluated.config.systemd.services.tally-campaign-poll.serviceConfig.ExecStart
    '''
    result = context.command("nix", "eval", "--impure", "--raw", "--expr", expression, timeout=600)
    require(result.returncode == 0, f"could not evaluate NixOS campaign command: {result.stderr[-5000:]}")
    script = Path(result.stdout.strip())
    derivation = context.command(
        "nix",
        "eval",
        "--impure",
        "--json",
        "--expr",
        f"builtins.getContext ({expression})",
        timeout=600,
    )
    require(
        derivation.returncode == 0,
        f"could not resolve NixOS campaign command derivation: {derivation.stderr[-5000:]}",
    )
    context_value = derivation.json("campaign poll string context")
    derivers = [path for path in context_value if path.endswith(".drv")]
    require(len(derivers) == 1, f"campaign poll command has unexpected Nix context: {context_value!r}")
    realised = context.command("nix-store", "--realise", derivers[0], timeout=600)
    require(realised.returncode == 0, f"could not realise NixOS campaign command: {realised.stderr[-5000:]}")
    require(script.is_file(), f"evaluated campaign poll program is missing: {script}")
    return script.read_text(encoding="utf-8")


@case(
    "campaign-config-locator",
    (442,),
    "NixOS, initial flow, and continuation children all serialize their exact config locator",
)
def campaign_config_locator(context: Context) -> None:
    root = make_case_directory(context, "campaign-config-locator")
    checkout, _ = repository(context, root)
    document = base_manifest(checkout)
    commit_worklist(context, checkout, document)
    configuration = campaign_configuration(context, root)
    failures: list[str] = []
    script = nixos_poll_script(context)
    if "--config /etc/tally/config.json" not in script:
        failures.append("rendered NixOS poll child omits --config /etc/tally/config.json")
    with context.daemon(root / "daemon", configuration) as daemon:
        paused = daemon.tally("queue", "pause", "flow")
        require(paused.returncode == 0, f"could not hold initial child for inspection: {paused.stderr}")
        armed = daemon.tally(
            "campaign",
            "arm",
            CODE_REPOSITORY,
            WORKLIST_PATTERN,
            "--checkout",
            checkout,
            "--flow",
            context.tally.parent.parent / "share/tally/flows/spec-build.js",
            "--driver",
            context.driver,
            "--state-dir",
            daemon.state,
            "--workspace-root",
            root / "workspaces",
            timeout=60,
        )
        require(armed.returncode == 0, f"config-locator arm failed: {(armed.stderr or armed.stdout)[-2400:]}")
        vectors = command_vectors(daemon.state, daemon.data)
        initial = next((argv for argv in vectors if "flow" in argv and "run" in argv), None)
        continuation = next(
            (argv for argv in vectors if "campaign" in argv and "poll" in argv and "--once" in argv),
            None,
        )
        expected_locator = ["--config", str(daemon.config)]
        if initial is None or initial[1:3] != expected_locator:
            failures.append(f"initial child config locator {None if initial is None else initial[:5]!r} != {expected_locator!r}")
        if continuation is None or continuation[1:3] != expected_locator:
            failures.append(
                f"continuation config locator {None if continuation is None else continuation[:5]!r} != {expected_locator!r}"
            )
        if initial is not None and continuation is not None and initial[0] != continuation[0]:
            failures.append(f"initial/continuation executable identity drifted: {initial[0]!r} != {continuation[0]!r}")
        # The only config has non-default values; successful admission proves
        # both producer and consumer parsed it before a node was launched.
        checked = context.command(context.tally, "--mode", "check-config", "--config", daemon.config)
        if checked.returncode != 0:
            failures.append(f"exact child config is not consumable: {checked.stderr[-1000:]}")
    require(not failures, "campaign config-locator failures:\n- " + "\n- ".join(failures))


def pipeline_manifest(checkout: Path, agent: Path) -> dict[str, Any]:
    value = base_manifest(checkout)
    # This is the serial omission form from #439.  A conforming driver must
    # distinguish it from an explicitly empty declaration and derive the
    # owned-paths fallback from the task's committed diff.
    value["tasks"][0].pop("conflictDomains")
    value["campaign"]["runtimeMaxSec"] = 900
    value["campaign"]["driverRuntimeMaxSec"] = 180
    value["campaign"]["agent"] = {
        "adapter": "shell",
        "argv": ["python3", str(agent)],
        "priority": "low",
        "runtimeMaxSec": 90,
        "approvalPolicy": None,
        "sandboxPolicy": None,
        "diagnosisSandboxPolicy": None,
        "model": None,
    }
    value["campaign"]["gates"] = [
        {
            "kind": "command",
            "id": "clean-diff",
            "preflightArgv": ["git", "rev-parse", "HEAD"],
            "argv": ["git", "diff", "--check", "origin/main...HEAD"],
            "runtimeMaxSec": 45,
        }
    ]
    return value


@case(
    "campaign-full-pipeline",
    (439, 442, 444, 446, 448),
    "arm -> poll -> reconcile -> dispatch -> execute -> sweep -> digest using packaged artifacts",
    long=True,
)
def campaign_full_pipeline(context: Context) -> None:
    root = make_case_directory(context, "campaign-full-pipeline")
    checkout, _ = repository(context, root)
    agent = root / "pipeline-agent.py"
    copy_executable(SUITE_ROOT / "fixtures/pipeline/agent.py", agent)
    driver_launcher = root / "packaged-driver-launch"
    copy_executable(SUITE_ROOT / "fixtures/pipeline/driver-launch.py", driver_launcher)
    document = pipeline_manifest(checkout, agent)
    commit_worklist(context, checkout, document)
    proof = root / "agent-proof.json"
    configuration = campaign_configuration(context, root)
    for adapter in ("shell", "spec-build-driver"):
        configuration["adapters"][adapter].setdefault("env", {})[
            "FINAL_BAR_PIPELINE_PROOF"
        ] = str(proof)
    configuration["adapters"]["spec-build-driver"]["env"]["FINAL_BAR_DRIVER"] = str(
        context.driver
    )
    # Keep a fallback copy only so a non-conforming current initial child can
    # continue far enough to illuminate downstream edges. The argv assertion
    # still requires the exact explicit locator and therefore catches #442.
    fallback = root / "xdg/tally/config.json"
    fallback.parent.mkdir(parents=True)
    fallback.write_text(json.dumps(configuration), encoding="utf-8")
    failures: list[str] = []
    completion_refs: dict[str, str] = {}
    integration_ref = ""
    with context.daemon(root / "daemon", configuration) as daemon:
        arm = daemon.tally(
            "campaign",
            "arm",
            CODE_REPOSITORY,
            WORKLIST_PATTERN,
            "--checkout",
            checkout,
            "--flow",
            context.tally.parent.parent / "share/tally/flows/spec-build.js",
            "--driver",
            driver_launcher,
            "--state-dir",
            daemon.state,
            "--workspace-root",
            root / "workspaces",
            "--wait",
            timeout=300,
        )
        if arm.returncode != 0:
            failures.append(f"arm/first pass failed: {(arm.stderr or arm.stdout)[-3000:]}")
        for _ in range(8):
            listed = context.command(
                "git", "-C", checkout, "ls-remote", "--refs", "origin", "refs/tally/spec-build/v1/*"
            )
            completion_refs = {
                name: target
                for target, name in (
                    line.split("\t", 1) for line in listed.stdout.splitlines() if "\t" in line
                )
                if name.endswith("/summary/complete")
            }
            if completion_refs:
                break
            polled = daemon.tally(
                "campaign",
                "poll",
                "--once",
                "--wait",
                "--state-dir",
                daemon.state,
                timeout=300,
            )
            if polled.returncode != 0:
                failures.append(f"poll pass failed: {(polled.stderr or polled.stdout)[-2400:]}")
                break

        authorities = list((daemon.state / "campaigns/armed").glob("*.json"))
        graph_digest = None
        if authorities:
            authority = json.loads(authorities[0].read_text(encoding="utf-8"))
            graph_digest = authority.get("approvedGraphDigest")
        else:
            # A completed campaign may already have removed its authority;
            # recover the admitted digest from durable enqueue evidence.
            text = "\n".join(json.dumps(value) for value in json_values(daemon.state))
            match = re.search(r"sha256:[0-9a-f]{64}", text)
            graph_digest = match.group(0) if match else None
        if not isinstance(graph_digest, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", graph_digest):
            failures.append(f"arm boundary omitted canonical graph digest: {graph_digest!r}")

        vectors = command_vectors(daemon.state, daemon.data)
        initial = next((argv for argv in vectors if "flow" in argv and "run" in argv), None)
        continuation = next(
            (argv for argv in vectors if "campaign" in argv and "poll" in argv and "--once" in argv),
            None,
        )
        expected_locator = ["--config", str(daemon.config)]
        if initial is None or initial[1:3] != expected_locator:
            failures.append(f"initial packaged child omitted exact config: {initial!r}")
        if continuation is None or continuation[1:3] != expected_locator:
            failures.append(f"continuation child omitted exact config: {continuation!r}")

        durable_text = "\n".join(
            path.read_text(encoding="utf-8", errors="replace")
            for base in (daemon.state, daemon.data)
            for path in base.rglob("*")
            if path.is_file() and path.stat().st_size < 8 * 1024 * 1024
        )
        for stage in (
            "reconcile",
            "sweep",
            "ownership-task-one",
            "tree-delta-task-one",
            "owned-paths-fallback",
        ):
            if stage not in durable_text:
                failures.append(f"durable flow/witness surfaces never recorded {stage!r}")
        if "\"verdict\":\"pass\"" not in durable_text.replace(" ", ""):
            failures.append("pipeline emitted no terminal passing witness")

        if not proof.is_file():
            failures.append("implementation task never executed")

        refs = context.command(
            "git",
            "-C",
            checkout,
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads/tally/",
        )
        integration_refs = [
            line
            for line in refs.stdout.splitlines()
            if line.startswith("refs/heads/tally/final-bar-campaign-")
            and line.endswith("/integration")
        ]
        if len(integration_refs) != 1:
            failures.append(f"expected one local integration ref, got {integration_refs!r}")
        else:
            integration_ref = integration_refs[0]

        disarmed = daemon.tally(
            "campaign",
            "disarm",
            CODE_REPOSITORY,
            WORKLIST_PATTERN,
            "--state-dir",
            daemon.state,
        )
        if disarmed.returncode != 0:
            failures.append(f"completed local campaign could not disarm: {disarmed.stderr[-1200:]}")
        quiescent = daemon.tally("campaign", "quiescent", "--state-dir", daemon.state)
        if quiescent.returncode != 0:
            failures.append(f"disarmed local campaign is not quiescent: {quiescent.stderr[-1200:]}")

    if integration_ref:
        shown = context.command("git", "-C", checkout, "show", f"{integration_ref}:result.txt")
        if shown.returncode != 0 or shown.stdout != "final-bar\n":
            failures.append(
                f"execute/merge did not land result.txt on integration: "
                f"{shown.stderr or shown.stdout!r}"
            )
    if len(completion_refs) != 1:
        failures.append(f"expected one durable local completion summary, got {completion_refs!r}")
    else:
        summary_ref = next(iter(completion_refs))
        # The ref targets a JSON blob, not a commit; cat-file is the public Git
        # boundary for reading the exact idempotent local receipt.
        summary = context.command("git", "-C", checkout, "cat-file", "blob", completion_refs[summary_ref])
        if summary.returncode != 0 or "tally:campaign-complete:v1" not in summary.stdout:
            failures.append(f"local completion summary is missing its digest marker: {summary.stderr or summary.stdout!r}")
    require(not failures, "full campaign pipeline failures:\n- " + "\n- ".join(failures))
