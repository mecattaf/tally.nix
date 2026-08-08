"""Configuration-locator contract and hermetic full campaign pipeline (#442 plus E2E)."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shutil
from typing import Any, Iterator

from cases.manifest import ISSUE_URL, base_manifest, host_config, issue_state, repository
from support import SUITE_ROOT, Context, case, copy_executable, make_case_directory, require


def fixture_environment(context: Context, root: Path, forge: Path) -> tuple[Path, dict[str, str]]:
    fake_bin = root / "bin"
    fake_bin.mkdir()
    copy_executable(SUITE_ROOT / "fixtures/pipeline/fake-gh.py", fake_bin / "gh")
    environment = context.environment(
        TALLY_GH_PROGRAM=fake_bin / "gh",
        FINAL_BAR_FORGE_STATE=forge,
        PATH=f"{fake_bin}:{os.environ.get('PATH', '')}",
    )
    return fake_bin, environment


def campaign_configuration(
    context: Context,
    root: Path,
    fake_bin: Path,
    forge: Path,
    *,
    git_ai: bool,
) -> dict[str, Any]:
    path = root / "source-config.json"
    host_config(context, path)
    value = json.loads(path.read_text(encoding="utf-8"))
    value["maxFrameBytes"] = 13_107_200
    value["pools"]["flow"]["capacity"] = 3
    child_env = {
        "PATH": f"{fake_bin}:{os.environ.get('PATH', '')}",
        "FINAL_BAR_FORGE_STATE": str(forge),
        "XDG_CONFIG_HOME": str(root / "xdg"),
    }
    value["adapters"]["shell"].setdefault("env", {}).update(child_env)
    value["adapters"]["spec-build-driver"].setdefault("env", {}).update(child_env)
    if git_ai:
        value["gitAi"] = {
            "enable": True,
            "mode": "advisory",
            "awaitTimeoutSec": 3,
        }
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
                campaignForge = {{
                  enable = true;
                  login = "final-bar";
                  tokenFile = "/run/secrets/final-bar";
                }};
              }};
            }}
          ];
        }};
      in evaluated.config.systemd.services.tally-campaign-poll.serviceConfig.ExecStart
    '''
    result = context.command("nix", "eval", "--impure", "--raw", "--expr", expression, timeout=600)
    require(result.returncode == 0, f"could not evaluate NixOS campaign command: {result.stderr[-5000:]}")
    script = Path(result.stdout.strip())
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
    manifest = base_manifest(checkout)
    forge = root / "forge.json"
    issue_state(forge, manifest)
    fake_bin, environment = fixture_environment(context, root, forge)
    configuration = campaign_configuration(context, root, fake_bin, forge, git_ai=False)
    failures: list[str] = []
    script = nixos_poll_script(context)
    if "--config /etc/tally/config.json" not in script:
        failures.append("rendered NixOS poll child omits --config /etc/tally/config.json")
    with context.daemon(root / "daemon", configuration, extra_env=environment) as daemon:
        paused = daemon.tally("queue", "pause", "flow")
        require(paused.returncode == 0, f"could not hold initial child for inspection: {paused.stderr}")
        armed = daemon.tally(
            "campaign",
            "arm",
            ISSUE_URL,
            "--allow-test-local-forge",
            "--flow",
            context.tally.parent.parent / "share/tally/flows/spec-build.js",
            "--driver",
            context.driver,
            "--state-dir",
            daemon.state,
            "--workspace-root",
            root / "workspaces",
            env=environment,
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
    value["runtimeMaxSec"] = 900
    value["driverRuntimeMaxSec"] = 180
    value["agent"] = {
        "adapter": "shell",
        "argv": ["python3", str(agent)],
        "priority": "low",
        "runtimeMaxSec": 90,
        "approvalPolicy": None,
        "sandboxPolicy": None,
        "diagnosisSandboxPolicy": None,
        "model": None,
    }
    value["gates"] = [
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
    (439, 441, 442, 444, 446, 448),
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
    manifest = pipeline_manifest(checkout, agent)
    forge = root / "forge.json"
    issue_state(forge, manifest)
    fake_bin, environment = fixture_environment(context, root, forge)
    proof = root / "agent-proof.json"
    configuration = campaign_configuration(context, root, fake_bin, forge, git_ai=True)
    for adapter in ("shell", "spec-build-driver"):
        configuration["adapters"][adapter].setdefault("env", {})[
            "FINAL_BAR_PIPELINE_PROOF"
        ] = str(proof)
    configuration["adapters"]["spec-build-driver"]["env"]["FINAL_BAR_DRIVER_SCRIPT"] = str(
        context.driver_script
    )
    # Keep a fallback copy only so a non-conforming current initial child can
    # continue far enough to illuminate downstream edges. The argv assertion
    # still requires the exact explicit locator and therefore catches #442.
    fallback = root / "xdg/tally/config.json"
    fallback.parent.mkdir(parents=True)
    fallback.write_text(json.dumps(configuration), encoding="utf-8")
    failures: list[str] = []
    with context.daemon(root / "daemon", configuration, extra_env=environment) as daemon:
        arm = daemon.tally(
            "campaign",
            "arm",
            ISSUE_URL,
            "--allow-test-local-forge",
            "--flow",
            context.tally.parent.parent / "share/tally/flows/spec-build.js",
            "--driver",
            driver_launcher,
            "--state-dir",
            daemon.state,
            "--workspace-root",
            root / "workspaces",
            "--wait",
            env=environment,
            timeout=300,
        )
        if arm.returncode != 0:
            failures.append(f"arm/first pass failed: {(arm.stderr or arm.stdout)[-3000:]}")
        for _ in range(8):
            forge_value = json.loads(forge.read_text(encoding="utf-8"))
            if forge_value["master"]["state"].lower() == "closed":
                break
            polled = daemon.tally(
                "campaign",
                "poll",
                "--once",
                "--wait",
                "--state-dir",
                daemon.state,
                env=environment,
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
            # A successfully closed campaign has already pruned its authority;
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
            failures.append("implementation task never executed with git-ai enabled")
        else:
            proof_value = json.loads(proof.read_text(encoding="utf-8"))
            attributes = proof_value.get("attributes", {})
            if not attributes.get("taskRef", "").endswith("/task-one") or len(attributes) != 7:
                failures.append(f"task-ref git-ai attributes drifted: {attributes!r}")

    fetched = context.command("git", "-C", checkout, "fetch", "--quiet", "origin", "main")
    if fetched.returncode != 0:
        failures.append(f"could not fetch integrated result: {fetched.stderr[-1200:]}")
    shown = context.command("git", "-C", checkout, "show", "origin/main:result.txt")
    if shown.returncode != 0 or shown.stdout != "final-bar\n":
        failures.append(f"execute/merge did not land result.txt: {shown.stderr or shown.stdout!r}")
    forge_value = json.loads(forge.read_text(encoding="utf-8"))
    if forge_value["master"]["state"].lower() != "closed":
        failures.append("terminal digest did not close the campaign issue")
    comments = [item.get("body", "") for item in forge_value.get("postedComments", [])]
    closing = [body for body in comments if "tally:campaign-complete:v1" in body]
    if len(closing) != 1 or not re.search(r"sha256:[0-9a-f]{64}", closing[0] if closing else ""):
        failures.append(f"expected one final campaign digest comment, got {closing!r}")
    require(not failures, "full campaign pipeline failures:\n- " + "\n- ".join(failures))
