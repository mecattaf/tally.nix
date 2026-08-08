"""Session launch-cwd recovery seam probes (#440)."""

from __future__ import annotations

from copy import deepcopy
import json
import time
from pathlib import Path
from typing import Any, Callable

from support import SUITE_ROOT, Context, case, copy_executable, make_case_directory, require


def recovery_config(context: Context, root: Path) -> tuple[dict[str, Any], Path]:
    program = root / "cwd-agent"
    copy_executable(SUITE_ROOT / "fixtures/recovery/cwd-agent.py", program)
    log = root / "invocations.jsonl"
    adapter = {
        "argv": [str(program), "fresh", "--"],
        "resume": [str(program), "resume", "%<sessionRef>%", "--"],
        "resumeRequiresLaunchCwd": True,
        "env": {"FINAL_BAR_RECOVERY_LOG": str(log)},
        "scrape": {
            "sessionRef": {
                "stream": "stdout",
                "mode": "jsonPath",
                "pattern": "$..thread_id",
            }
        },
    }
    return (
        {
            "enqueue": {"requireDedupKey": False},
            "pools": {"stock": {"resource": "slot", "capacity": 1}},
            "adapters": {"cwd-keyed": adapter},
        },
        log,
    )


def enqueue_first(daemon: Any, cwd: Path) -> str:
    result = daemon.tally(
        "enqueue",
        "--pool",
        "stock",
        "--adapter",
        "cwd-keyed",
        "--cwd",
        cwd,
        "--wait",
        "--",
        "first",
        timeout=60,
    )
    require(result.returncode == 0, f"fresh cwd session failed: {(result.stderr or result.stdout)[-1800:]}")
    value = result.json("fresh cwd enqueue")
    task = value.get("task_uuid") or value.get("taskUuid")
    require(isinstance(task, str), f"fresh enqueue omitted task UUID: {value!r}")
    return task


def continue_job(daemon: Any, task: str, *workload: str, wait: bool = True) -> Any:
    arguments: list[str] = ["queue", "continue", task]
    if wait:
        arguments.append("--wait")
    arguments.extend(["--", *workload])
    return daemon.tally(*arguments, timeout=60)


def invocations(log: Path) -> list[dict[str, Any]]:
    require(log.is_file(), "cwd-keyed adapter process did not run")
    return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines() if line]


def require_resume(log: Path, cwd: Path) -> None:
    values = invocations(log)
    require(len(values) >= 2, f"expected fresh and resume launches: {values!r}")
    resumed = values[-1]
    require(resumed["cwd"] == str(cwd), f"resume cwd {resumed['cwd']!r} != {str(cwd)!r}")
    require(
        resumed["argv"][1:4] == ["resume", "final-bar-cwd-session", "--"],
        f"recovery did not render the persisted session: {resumed['argv']!r}",
    )


@case(
    "launch-cwd-ordinary-completion",
    (440,),
    "ordinary completion installs sessionRef and launch cwd atomically",
)
def launch_cwd_ordinary_completion(context: Context) -> None:
    root = make_case_directory(context, "launch-cwd-ordinary-completion")
    cwd = root / "checkout"
    cwd.mkdir()
    configuration, log = recovery_config(context, root)
    with context.daemon(root / "daemon", configuration) as daemon:
        task = enqueue_first(daemon, cwd)
        resumed = continue_job(daemon, task, "review")
        require(resumed.returncode == 0, f"same-cwd continuation failed: {(resumed.stderr or resumed.stdout)[-1800:]}")
    require_resume(log, cwd)


@case(
    "launch-cwd-restarted-capture",
    (440,),
    "restart re-derives launch cwd beside a retained session capture",
)
def launch_cwd_restarted_capture(context: Context) -> None:
    root = make_case_directory(context, "launch-cwd-restarted-capture")
    cwd = root / "checkout"
    cwd.mkdir()
    configuration, log = recovery_config(context, root)
    with context.daemon(root / "daemon", configuration) as daemon:
        task = enqueue_first(daemon, cwd)
    with context.daemon(root / "daemon", configuration) as restarted:
        resumed = continue_job(restarted, task, "review")
        require(resumed.returncode == 0, f"post-restart continuation failed: {(resumed.stderr or resumed.stdout)[-2000:]}")
    require_resume(log, cwd)


@case(
    "launch-cwd-recovered-install",
    (440,),
    "a paused continuation recovered at startup keeps the launch cwd",
)
def launch_cwd_recovered_install(context: Context) -> None:
    root = make_case_directory(context, "launch-cwd-recovered-install")
    cwd = root / "checkout"
    cwd.mkdir()
    configuration, log = recovery_config(context, root)
    with context.daemon(root / "daemon", configuration) as daemon:
        task = enqueue_first(daemon, cwd)
        paused = daemon.tally("queue", "pause", "stock")
        require(paused.returncode == 0, f"could not establish queued recovery seam: {paused.stderr}")
        admitted = continue_job(daemon, task, "queued-after-restart", wait=False)
        require(admitted.returncode == 0, f"continuation admission failed: {admitted.stderr}")
        value = admitted.json("queued continuation")
        continued = value.get("task_uuid") or value.get("taskUuid")
        require(isinstance(continued, str), f"continuation omitted task UUID: {value!r}")
    with context.daemon(root / "daemon", configuration) as restarted:
        resumed_pool = restarted.tally("queue", "resume", "stock")
        require(resumed_pool.returncode == 0, f"could not release recovered job: {resumed_pool.stderr}")
        settled = restarted.tally("queue", "await-job", continued, timeout=60)
        require(settled.returncode == 0, f"recovered continuation failed: {(settled.stderr or settled.stdout)[-2000:]}")
    require_resume(log, cwd)


@case(
    "launch-cwd-adopted-metadata",
    (440,),
    "a running continuation adopted after daemon restart keeps the launch cwd",
)
def launch_cwd_adopted_metadata(context: Context) -> None:
    root = make_case_directory(context, "launch-cwd-adopted-metadata")
    cwd = root / "checkout"
    cwd.mkdir()
    configuration, log = recovery_config(context, root)
    with context.daemon(root / "daemon", configuration) as daemon:
        task = enqueue_first(daemon, cwd)
        admitted = continue_job(daemon, task, "hold", wait=False)
        require(admitted.returncode == 0, f"running continuation admission failed: {admitted.stderr}")
        value = admitted.json("running continuation")
        continued = value.get("task_uuid") or value.get("taskUuid")
        require(isinstance(continued, str), f"continuation omitted task UUID: {value!r}")
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline and (not log.exists() or len(invocations(log)) < 2):
            time.sleep(0.05)
        require(log.exists() and len(invocations(log)) >= 2, "continuation never reached process launch")
    with context.daemon(root / "daemon", configuration) as restarted:
        settled = restarted.tally("queue", "await-job", continued, timeout=60)
        require(settled.returncode == 0, f"adopted continuation failed: {(settled.stderr or settled.stdout)[-2400:]}")
    require_resume(log, cwd)


@case(
    "launch-cwd-representation-hydration",
    (440,),
    "startup re-presentation and its public continuation retain the exact launch cwd",
)
def launch_cwd_representation_hydration(context: Context) -> None:
    # The confirmed-pool-loss scenario is the deterministic public-state
    # producer for RecoveryAction::RePresent. It drives a row through the same
    # enqueue/witness/recovery surfaces while controlling the remote liveness
    # boundary; the ordinary public process cases above supply the cwd oracle.
    name = "daemon::tests::confirmed_pool_loss_witnesses_and_return_re_presents_the_same_row"
    require(name in context.core_test_names(), f"target omits re-presentation recovery case {name}")
    result = context.run_core_test(name, timeout=240)
    require(
        result.returncode == 0,
        "confirmed pool return could not re-present its durable row:\n"
        + (result.stdout + result.stderr)[-5000:],
    )
    # Also require the recovery capture binding: together these two process
    # probes catch deleting either re-presentation or cwd hydration.
    restart = "daemon::tests::a_restarted_daemon_re_derives_the_launch_record_beside_the_pointer"
    require(restart in context.core_test_names(), f"target omits launch-cwd recovery case {restart}")
    bound = context.run_core_test(restart, timeout=180)
    require(bound.returncode == 0, (bound.stdout + bound.stderr)[-5000:])
