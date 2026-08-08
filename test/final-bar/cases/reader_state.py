"""Reader-state identity and view-local aggregate contract (#415)."""

from __future__ import annotations

import json
import uuid
from typing import Any

from support import SUITE_ROOT, Context, case, make_case_directory, require


def config(context: Context) -> dict[str, Any]:
    return {
        "enqueue": {"requireDedupKey": False},
        "pools": {"stock": {"resource": "vram", "capacity": 1}},
        "adapters": {"shell": context.presets["shell"]},
    }


def object_output(result: Any, label: str) -> dict[str, Any]:
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        require(False, f"{label} did not emit JSON: {error}; {result.stdout!r} {result.stderr!r}")
    require(isinstance(value, dict), f"{label} output is not an object: {value!r}")
    return value


def archived_fixture(context: Context, case_id: str) -> tuple[Any, str, str]:
    root = make_case_directory(context, case_id)
    flow = str(uuid.uuid5(uuid.NAMESPACE_URL, f"tally-final-bar:{case_id}"))
    reused_flow = flow
    manager = context.daemon(root / "daemon", config(context))
    daemon = manager.__enter__()
    script = SUITE_ROOT / "fixtures/reader-state/one.js"
    first = daemon.tally(
        "flow", "run", script, "--flow-run-id", flow, "--max-nodes", "1", timeout=60
    )
    if first.returncode != 0:
        manager.__exit__(None, None, None)
        require(False, f"reader-state seed failed: {(first.stderr or first.stdout)[-1800:]}")
    # Attach the same durable row to a second run. That is a real reuse
    # disposition and gives the aggregate probe a non-zero value to hide.
    reused = daemon.tally(
        "flow", "run", script, "--flow-run-id", reused_flow, "--max-nodes", "1",
        timeout=60,
    )
    if reused.returncode != 0:
        manager.__exit__(None, None, None)
        require(False, f"reader-state reuse failed: {(reused.stderr or reused.stdout)[-1800:]}")
    memberships = [
        json.loads(line)
        for line in (daemon.data / "flow-membership.jsonl").read_text(encoding="utf-8").splitlines()
        if line
    ]
    primary = [item for item in memberships if item.get("flowRunId") == flow]
    require(primary, f"flow did not persist primary membership: {memberships!r}")
    task = primary[0].get("taskUuid")
    require(isinstance(task, str), f"flow membership omitted task UUID: {primary[0]!r}")
    archived = context.command(
        context.tally,
        "reader-state",
        "archive",
        flow,
        "--data-dir",
        daemon.data,
    )
    if archived.returncode != 0:
        manager.__exit__(None, None, None)
        require(False, f"reader-state archive failed: {(archived.stderr or archived.stdout)[-1800:]}")
    archived_reuse = context.command(
        context.tally,
        "reader-state",
        "archive",
        reused_flow,
        "--data-dir",
        daemon.data,
    )
    if archived_reuse.returncode != 0:
        manager.__exit__(None, None, None)
        require(False, f"reader-state reuse archive failed: {archived_reuse.stderr[-1800:]}")
    # Keep the context manager reachable for the caller's finally block.
    daemon._final_bar_manager = manager
    return daemon, flow, task


def close_fixture(daemon: Any) -> None:
    daemon._final_bar_manager.__exit__(None, None, None)


@case(
    "reader-state-explicit-identity",
    (415,),
    "explicit archived run/job lookup returns durable members and contradictory filtering is refused",
)
def reader_state_explicit_identity(context: Context) -> None:
    daemon, flow, task = archived_fixture(context, "reader-state-explicit-identity")
    try:
        run_result = daemon.tally("query", "run", flow, "--json")
        require(run_result.returncode == 0, f"explicit query run failed: {run_result.stderr}")
        run = object_output(run_result, "query run")
        failures: list[str] = []
        if run.get("archived") is not True:
            failures.append(f"explicit run omitted archived=true: {run!r}")
        tasks = run.get("tasks", run.get("items", []))
        if not any(item.get("taskUuid") == task for item in tasks if isinstance(item, dict)):
            failures.append(f"explicit run omitted archived task {task}: {tasks!r}")

        jobs_result = daemon.tally("query", "jobs", "--flow-run", flow, "--json")
        require(jobs_result.returncode == 0, f"explicit query jobs failed: {jobs_result.stderr}")
        jobs = object_output(jobs_result, "query jobs")
        if jobs.get("flowRunTasks") != 1:
            failures.append(f"flowRunTasks={jobs.get('flowRunTasks')!r}, expected 1")
        items = jobs.get("items", [])
        if not any(item.get("taskUuid") == task for item in items if isinstance(item, dict)):
            failures.append(f"explicit query jobs withheld archived member: {items!r}")
        contradictory = daemon.tally(
            "query", "jobs", "--flow-run", flow, "--no-archived", "--json"
        )
        if contradictory.returncode == 0:
            failures.append("explicit flow identity plus --no-archived was silently accepted")
        require(not failures, "reader-state identity failures:\n- " + "\n- ".join(failures))
    finally:
        close_fixture(daemon)


@case(
    "reader-state-view-aggregates",
    (415,),
    "archived-only default standup has hidden counts but zero visible aggregates",
)
def reader_state_view_aggregates(context: Context) -> None:
    daemon, flow, _ = archived_fixture(context, "reader-state-view-aggregates")
    try:
        default_result = daemon.tally("query", "standup")
        require(default_result.returncode == 0, f"default standup failed: {default_result.stderr}")
        default = object_output(default_result, "default standup")
        included_result = daemon.tally("query", "standup", "--archived")
        require(included_result.returncode == 0, f"included standup failed: {included_result.stderr}")
        included = object_output(included_result, "included standup")
        failures: list[str] = []
        if default.get("archivedHidden", 0) < 1:
            failures.append(f"default did not count hidden archived task: {default!r}")
        if default.get("archivedRunsHidden", 0) < 1:
            failures.append(f"default did not count hidden archived run: {default!r}")
        for field in ("completed", "gateFails", "cancelled", "inFlight", "runs"):
            if default.get(field) not in ([], None):
                failures.append(f"default visible {field} is not empty: {default.get(field)!r}")
        if default.get("reused") != 0:
            failures.append(f"hidden reuse leaked into default aggregate: {default.get('reused')!r}")
        if default.get("canonicalGpuSeconds") not in (0, 0.0, None):
            failures.append(
                f"hidden GPU time leaked into default aggregate: {default.get('canonicalGpuSeconds')!r}"
            )
        included_runs = included.get("runs", [])
        if not any(item.get("flowRunId") == flow for item in included_runs if isinstance(item, dict)):
            failures.append(f"--archived did not recompute over archived run: {included_runs!r}")
        if not isinstance(included.get("canonicalGpuSeconds"), (int, float)) or included.get(
            "canonicalGpuSeconds", 0
        ) <= 0:
            failures.append(f"control fixture did not create visible GPU cost: {included!r}")
        require(not failures, "reader-state aggregate failures:\n- " + "\n- ".join(failures))
    finally:
        close_fixture(daemon)
