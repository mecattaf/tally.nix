"""Git-ai producer/validator boundary and terminal failure projection (#441)."""

from __future__ import annotations

import json
import os
from typing import Any

from support import Context, case, make_case_directory, require


@case(
    "git-ai-task-ref-contract",
    (441,),
    "a public task-ref execution receives exactly the seven-key bounded correlation set",
)
def git_ai_task_ref_contract(context: Context) -> None:
    root = make_case_directory(context, "git-ai-task-ref-contract")
    attributes = root / "attributes.json"
    flow = "00000000-0000-4000-8000-000000000441"
    configuration: dict[str, Any] = {
        "enqueue": {"requireDedupKey": False},
        "pools": {"stock": {"resource": "slot", "capacity": 1}},
        "adapters": {"shell": context.presets["shell"]},
        "gitAi": {"enable": True, "mode": "advisory", "awaitTimeoutSec": 3},
    }
    with context.daemon(root / "daemon", configuration) as daemon:
        orchestration = json.dumps(
            {
                "flowName": "spec-build",
                "flowRunId": flow,
                "nodeOrdinal": 2,
                "nodeLabel": "payload",
                "taskRef": "crm/t07",
            },
            separators=(",", ":"),
        )
        result = daemon.tally(
            "enqueue",
            "--pool",
            "stock",
            "--adapter",
            "shell",
            "--orchestration",
            orchestration,
            "--evidence",
            "exit:0",
            "--wait",
            "--",
            "/bin/sh",
            "-c",
            f"printf '%s' \"$GIT_AI_CUSTOM_ATTRIBUTES\" > {attributes}",
            timeout=60,
        )
        require(result.returncode == 0, f"task-ref payload did not pass: {(result.stderr or result.stdout)[-2400:]}")
        require(attributes.is_file(), "git-ai task payload did not receive correlation attributes")
        try:
            value = json.loads(attributes.read_text(encoding="utf-8"))
        except json.JSONDecodeError as error:
            require(False, f"GIT_AI_CUSTOM_ATTRIBUTES is not JSON: {error}")
        expected_keys = {
            "taskUuid",
            "attempt",
            "leaseEpoch",
            "adapter",
            "flowRunId",
            "nodeOrdinal",
            "taskRef",
        }
        require(set(value) == expected_keys, f"correlation keys {set(value)!r} != {expected_keys!r}")
        require(value["flowRunId"] == flow, f"flowRunId drifted: {value!r}")
        require(value["nodeOrdinal"] == "2", f"nodeOrdinal drifted: {value!r}")
        require(value["taskRef"] == "crm/t07", f"taskRef drifted: {value!r}")


@case(
    "git-ai-validation-terminal-cause",
    (441,),
    "executor validation is an immediate structured witness/query/flow cause",
)
def git_ai_validation_terminal_cause(context: Context) -> None:
    names = context.core_test_names()
    witness_test = (
        "daemon::tests::executor_validation_failure_is_witnessed_and_projected_as_the_terminal_cause"
    )
    require(witness_test in names, f"target omits structured public projection probe {witness_test}")
    witness = context.run_core_test(witness_test, timeout=180)
    require(
        witness.returncode == 0,
        "executor rejection did not retain one structured terminal cause:\n"
        + (witness.stdout + witness.stderr)[-5000:],
    )
    env = context.environment(CARGO_TARGET_DIR=context.work / "cargo-target")
    flow_test = "flow_live::tests::executor_validation_failure_skips_the_advisory_projection_wait"
    projected = context.command(
        "nix",
        "develop",
        context.flake,
        "-c",
        "cargo",
        "test",
        "--manifest-path",
        context.target / "Cargo.toml",
        "-p",
        "tally",
        "--bin",
        "tally",
        flow_test,
        "--",
        "--exact",
        "--nocapture",
        env=env,
        timeout=600,
    )
    require(
        projected.returncode == 0,
        "flow converted executor-validation-failed into a projection wait/schema error:\n"
        + (projected.stdout + projected.stderr)[-5000:],
    )
