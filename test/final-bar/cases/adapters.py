"""Evaluated Nix preset / Rust argv conformance corpus (#443/#445)."""

from __future__ import annotations

from copy import deepcopy
import hashlib
import json
import os
from pathlib import Path
import shutil
from typing import Any

from support import SUITE_ROOT, Context, case, make_case_directory, require


def fixture() -> dict[str, Any]:
    return json.loads((SUITE_ROOT / "fixtures/adapters/cases.json").read_text(encoding="utf-8"))


def checked_stream(context: Context, declaration: dict[str, Any]) -> Path:
    path = context.target / declaration["path"]
    require(path.is_file(), f"recorded provider stream is missing: {path}")
    actual = hashlib.sha256(path.read_bytes()).hexdigest()
    require(
        actual == declaration["sha256"],
        f"recorded provider stream {declaration['path']} changed: {actual} != {declaration['sha256']}",
    )
    return path


def render(
    context: Context,
    config: Path,
    adapter: str,
    workload: str,
    *,
    cwd: Path,
    stream: Path | None = None,
) -> Any:
    empty = config.parent / "empty.stderr"
    empty.touch()
    command: list[os.PathLike[str] | str] = [
        context.tally,
        "--config",
        config,
        "__adapter-render",
        adapter,
        "--cwd",
        cwd,
    ]
    if stream is not None:
        command.extend(["--scrape-stdout", stream, "--scrape-stderr", empty])
    command.extend(["--", workload])
    result = context.command(*command)
    return result


@case(
    "adapter-argv-corpus",
    (443, 445),
    "evaluated stock presets and Rust renderer agree with recorded Codex/Pi grammar",
)
def adapter_argv_corpus(context: Context) -> None:
    root = make_case_directory(context, "adapter-argv-corpus")
    data = fixture()
    codex_stream = checked_stream(context, data["streams"]["codexNoModel"])
    pi_stream = checked_stream(context, data["streams"]["piNormal"])
    presets = context.presets
    failures: list[str] = []

    codex = presets.get("codex", {})
    expected_resume = [
        "codex",
        "-C",
        "%<cwd>%",
        "exec",
        "resume",
        "--json",
        "%<sessionRef>%",
        "--",
    ]
    if codex.get("resume") != expected_resume:
        failures.append(
            "codex preset requires a provider-emitted model on resume; "
            f"expected {expected_resume!r}, got {codex.get('resume')!r}"
        )
    scope = codex.get("scrape", {}).get("usage", {}).get("counterScope")
    if scope != "session-cumulative":
        failures.append(
            "codex usage capture must declare counterScope=session-cumulative; "
            f"got {scope!r}"
        )
    pi = presets.get("pi", {})
    policy = pi.get("launch", {}).get("rejectOptionLikeWorkloadHead")
    if policy is not True:
        failures.append(
            "Pi preset must declare launch.rejectOptionLikeWorkloadHead=true; "
            f"got {policy!r}"
        )

    config = context.adapter_config(root)
    codex_render = render(
        context,
        config,
        "codex",
        "continue",
        cwd=root,
        stream=codex_stream,
    )
    if codex_render.returncode != 0:
        failures.append(
            "real no-model Codex stream was not resumable: "
            + (codex_render.stderr or codex_render.stdout)[-1400:]
        )
    else:
        value = codex_render.json("Codex adapter render")
        expected_argv = [
            "codex",
            "-C",
            str(root),
            "exec",
            "resume",
            "--json",
            data["streams"]["codexNoModel"]["sessionRef"],
            "--",
            "continue",
        ]
        if value.get("argv") != expected_argv:
            failures.append(f"no-model Codex resume argv {value.get('argv')!r} != {expected_argv!r}")
        captures = value.get("captures", {})
        for key in ("sessionRef", "finalMessage", "usage"):
            expected = data["streams"]["codexNoModel"][key]
            if captures.get(key) != expected:
                failures.append(f"Codex capture {key} {captures.get(key)!r} != {expected!r}")
        if "model" in captures:
            failures.append(f"Codex real stream acquired a synthetic model capture: {captures['model']!r}")

    pi_render = render(context, config, "pi", "work", cwd=root, stream=pi_stream)
    if pi_render.returncode != 0:
        failures.append(
            "recorded normal Pi stream no longer round-trips: "
            + (pi_render.stderr or pi_render.stdout)[-1400:]
        )
    else:
        value = pi_render.json("Pi adapter render")
        expected = data["streams"]["piNormal"]
        expected_argv = [
            "pi",
            "--mode",
            "json",
            "--session",
            expected["sessionRef"],
            "--model",
            expected["model"],
            "work",
        ]
        if value.get("argv") != expected_argv:
            failures.append(f"normal Pi resume argv {value.get('argv')!r} != {expected_argv!r}")
        captures = value.get("captures", {})
        for key in ("sessionRef", "model", "finalMessage"):
            if captures.get(key) != expected[key]:
                failures.append(f"Pi capture {key} {captures.get(key)!r} != {expected[key]!r}")

    for workload in data["refusedPiWorkloads"]:
        refused = render(context, config, "pi", workload, cwd=root)
        detail = (refused.stderr + "\n" + refused.stdout).lower()
        if refused.returncode == 0:
            failures.append(f"Pi option-like workload {workload!r} rendered instead of refusing")
        for needle in ("option-like-workload-head", "pi", "index 0", workload.lower()):
            if needle not in detail:
                failures.append(
                    f"Pi refusal for {workload!r} omitted {needle!r}: {detail[-800:]!r}"
                )

    require(not failures, "adapter corpus disagreements:\n- " + "\n- ".join(failures))


def daemon_config(adapter: dict[str, Any]) -> dict[str, Any]:
    return {
        "enqueue": {"requireDedupKey": False},
        "pools": {"stock": {"resource": "slot", "capacity": 1}},
        "adapters": {"probe": adapter},
    }


def fake_adapter(context: Context, root: Path, preset: dict[str, Any], shape: str) -> tuple[dict[str, Any], Path]:
    program = SUITE_ROOT / "fixtures/adapters/fake-agent.py"
    copied = root / "fake-agent"
    shutil.copy2(program, copied)
    copied.chmod(0o755)
    log = root / "invocations.jsonl"
    adapter = deepcopy(preset)
    adapter["argv"][0] = str(copied)
    if adapter.get("resume"):
        adapter["resume"][0] = str(copied)
    adapter.setdefault("env", {})["FINAL_BAR_ADAPTER_LOG"] = str(log)
    adapter["env"]["FINAL_BAR_ADAPTER_SHAPE"] = shape
    return adapter, log


@case(
    "pi-prelaunch-refusal",
    (445,),
    "Pi option-looking workloads are refused before enqueue/process creation",
)
def pi_prelaunch_refusal(context: Context) -> None:
    root = make_case_directory(context, "pi-prelaunch-refusal")
    adapter, log = fake_adapter(context, root, context.presets["pi"], "pi")
    failures: list[str] = []
    with context.daemon(root / "daemon", daemon_config(adapter)) as daemon:
        for workload in fixture()["refusedPiWorkloads"]:
            result = daemon.tally(
                "adapter",
                "smoke",
                "probe",
                f"--prompt={workload}",
                "--pool",
                "stock",
                timeout=60,
            )
            detail = (result.stderr + "\n" + result.stdout).lower()
            if result.returncode == 0 or "option-like-workload-head" not in detail:
                failures.append(
                    f"{workload!r} was not a typed pre-launch refusal: rc={result.returncode}, "
                    f"detail={detail[-1000:]!r}"
                )
    if log.exists() and log.read_text(encoding="utf-8").strip():
        failures.append("Pi process-spawn sentinel was touched for a refused workload")
    require(not failures, "Pi pre-launch contract failures:\n- " + "\n- ".join(failures))


def parse_task_uuid(result: Any) -> str:
    value = result.json("enqueue result")
    task = value.get("task_uuid") or value.get("taskUuid")
    require(isinstance(task, str) and task, f"enqueue response omitted task UUID: {value!r}")
    return task


def normalized_invocations(log: Path, program: Path) -> list[list[str]]:
    require(log.is_file(), "fake Codex process never launched")
    values = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines() if line]
    for value in values:
        if value and value[0] == str(program):
            value[0] = "codex"
    return values


@case(
    "codex-model-recovery",
    (443,),
    "default-model continuation omits --model while an admitted explicit model stays pinned",
)
def codex_model_recovery(context: Context) -> None:
    root = make_case_directory(context, "codex-model-recovery")
    failures: list[str] = []
    for explicit in (None, "pinned-model"):
        label = "default" if explicit is None else "explicit"
        lane = root / label
        lane.mkdir()
        adapter, log = fake_adapter(context, lane, context.presets["codex"], "codex")
        # The explicit-model half exercises the public job-option mechanism.
        # Empty allowedValues means any non-empty exact value is authorized.
        adapter.setdefault("launch", {})["model"] = {
            "argv": ["--model", "%<value>%"],
            "allowedValues": [],
        }
        program = Path(adapter["argv"][0])
        with context.daemon(lane / "daemon", daemon_config(adapter)) as daemon:
            command: list[str] = [
                "enqueue",
                "--pool",
                "stock",
                "--adapter",
                "probe",
                "--cwd",
                str(lane),
            ]
            if explicit is not None:
                command.extend(["--model", explicit])
            command.extend(["--wait", "--", "first"])
            fresh = daemon.tally(*command, timeout=60)
            if fresh.returncode != 0:
                failures.append(f"{label}: fresh attempt failed: {(fresh.stderr or fresh.stdout)[-1000:]}")
                continue
            task = parse_task_uuid(fresh)
            resumed = daemon.tally("queue", "continue", task, "--wait", "--", "second", timeout=60)
            if resumed.returncode != 0:
                failures.append(
                    f"{label}: continuation failed: {(resumed.stderr or resumed.stdout)[-1200:]}"
                )
                continue
        invocations = normalized_invocations(log, program)
        if len(invocations) != 2:
            failures.append(f"{label}: expected two process launches, got {invocations!r}")
            continue
        resume = invocations[1]
        expected = [
            "codex",
            "-C",
            str(lane),
            "exec",
            "resume",
            "--json",
        ]
        if explicit is not None:
            expected.extend(["--model", explicit])
        expected.extend(["codex-final-bar-thread", "--", "second"])
        if resume != expected:
            failures.append(f"{label}: resume argv {resume!r} != {expected!r}")
        if explicit is None and "--model" in resume:
            failures.append("default-model continuation fabricated a model flag")
        if explicit is not None and resume.count("--model") != 1:
            failures.append("explicit-model continuation did not carry exactly one model flag")
    require(not failures, "Codex recovery failures:\n- " + "\n- ".join(failures))
