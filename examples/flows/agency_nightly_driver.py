#!/usr/bin/env python3
"""Deterministic worklist and culmination driver for agency-nightly.js.

Two actions, both deterministic, both reporting domain failures as data:

  worklist   Witness the wave the flow declared: pin the base commit, and cut one
             git worktree and branch per task.
  culminate  Push what each task committed, open or find its pull request, and
             write the morning report.

There is no worklist *source* here on purpose. The 2026-07-27 ruling on the
agency wave settled that the worklist is the flow script and its arguments, not
an external document this driver scrapes: no labelled-issue query, no tasks.md
adapter. The wave arrives in the brief, already checked against the flow's
argsSchema when the generation was built.

Every failure is emitted as ``{"ok": false, "error": {...}}`` under exit 0, so
the flow sees a typed, witnessed value rather than a dead node.

Authentication is whatever ``gh`` and ``git`` already have on the machine. This
driver never reads, writes, or asks for a token.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

# One worktree manager serves both campaign drivers. This file used to carry
# its own create/resume/validate logic with its own invariants; the shared
# module is now the single implementation, and lane identity lives in git's own
# per-worktree configuration rather than in either driver's bespoke files.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import campaign_worktrees as worktrees  # noqa: E402

FINAL_MESSAGE_PREFIX = "TALLY_FINAL_MESSAGE="
TASK_ID = re.compile(r"^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$")
ISSUE_ID = re.compile(r"^[1-9][0-9]*$")
COMMIT_ID = re.compile(r"^[0-9a-f]{40,64}$")
MAX_WAVE_SIZE = 6


class DriverError(Exception):
    """A stable failure returned to the flow as structured data."""

    def __init__(
        self, code: str, message: str, details: dict[str, Any] | None = None
    ) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.details = details or {}


def emit(value: dict[str, Any]) -> None:
    print(
        FINAL_MESSAGE_PREFIX
        + json.dumps(value, sort_keys=True, separators=(",", ":")),
        flush=True,
    )


def command(
    argv: list[str],
    *,
    cwd: Path | None = None,
    error_code: str = "agency-driver-command-failed",
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    try:
        result = subprocess.run(
            argv,
            cwd=cwd,
            check=False,
            capture_output=True,
            text=True,
        )
    except OSError as error:
        raise DriverError(
            error_code,
            f"could not execute {argv[0]!r}",
            {"argv": argv, "error": str(error)},
        ) from error
    if check and result.returncode != 0:
        raise DriverError(
            error_code,
            f"{argv[0]} exited {result.returncode}",
            {
                "argv": argv,
                "exitCode": result.returncode,
                "stderr": result.stderr.strip()[:2000],
            },
        )
    return result


def require_string(value: Any, field: str, *, absolute: bool = False) -> str:
    if not isinstance(value, str) or not value or any(ord(c) < 32 for c in value):
        raise DriverError(
            "agency-driver-brief-invalid",
            f"{field} must be a non-empty single-line string",
            {"field": field},
        )
    if absolute and not value.startswith("/"):
        raise DriverError(
            "agency-driver-brief-invalid",
            f"{field} must be an absolute path",
            {"field": field},
        )
    return value


def optional_string(value: Any, field: str) -> str | None:
    if value is None:
        return None
    return require_string(value, field)


def parse_task(value: Any, index: int) -> dict[str, Any]:
    """Validate one wave entry. The flow already checked argsSchema; this is the
    driver refusing to trust a brief it did not build."""
    if not isinstance(value, dict):
        raise DriverError(
            "worklist-task-invalid",
            f"wave entry {index} must be an object",
            {"index": index},
        )
    task_id = require_string(value.get("taskId"), f"wave[{index}].taskId")
    if not TASK_ID.fullmatch(task_id):
        raise DriverError(
            "worklist-task-invalid",
            f"task ID {task_id!r} is not a lowercase slug",
            {"taskId": task_id},
        )
    title = require_string(value.get("title"), f"wave[{index}].title")
    mission = value.get("mission")
    if not isinstance(mission, str) or not mission.strip():
        raise DriverError(
            "worklist-task-invalid",
            f"task {task_id!r} has no mission",
            {"taskId": task_id},
        )
    criteria = value.get("acceptanceCriteria")
    if (
        not isinstance(criteria, list)
        or not criteria
        or any(not isinstance(item, str) or not item.strip() for item in criteria)
    ):
        raise DriverError(
            "worklist-task-invalid",
            f"task {task_id!r} needs a non-empty list of acceptance criteria",
            {"taskId": task_id},
        )
    task: dict[str, Any] = {
        "taskId": task_id,
        "title": title,
        "mission": mission,
        "acceptanceCriteria": list(criteria),
    }
    issue = optional_string(value.get("issue"), f"wave[{index}].issue")
    if issue is not None:
        if not ISSUE_ID.fullmatch(issue):
            raise DriverError(
                "worklist-task-invalid",
                f"task {task_id!r} references an invalid issue number",
                {"taskId": task_id, "issue": issue},
            )
        task["issue"] = issue
    unknown = sorted(
        set(value) - {"taskId", "title", "mission", "acceptanceCriteria", "issue"}
    )
    if unknown:
        raise DriverError(
            "worklist-task-invalid",
            f"task {task_id!r} has unknown fields",
            {"taskId": task_id, "fields": unknown},
        )
    return task


def read_wave(brief: dict[str, Any]) -> list[dict[str, Any]]:
    wave = brief.get("wave")
    if not isinstance(wave, list) or not wave:
        raise DriverError(
            "agency-driver-brief-invalid", "the brief carries no wave"
        )
    if len(wave) > MAX_WAVE_SIZE:
        raise DriverError(
            "worklist-wave-too-large",
            f"the wave holds {len(wave)} tasks and the bound is {MAX_WAVE_SIZE}",
            {"waveSize": len(wave), "maxWaveSize": MAX_WAVE_SIZE},
        )
    tasks = [parse_task(entry, index) for index, entry in enumerate(wave)]
    seen: set[str] = set()
    for task in tasks:
        if task["taskId"] in seen:
            raise DriverError(
                "worklist-task-id-duplicate",
                f"the wave names task {task['taskId']!r} twice",
                {"taskId": task["taskId"]},
            )
        seen.add(task["taskId"])
    return tasks


# The manager's error vocabulary, mapped onto the codes this driver's contract
# already publishes to the flow.
WORKTREE_ERROR_CODES = {
    "worktree-conflict": "worklist-worktree-conflict",
    "worktree-invalid": "worklist-worktree-invalid",
    "worktree-create-failed": "worklist-worktree-create-failed",
    "branch-invalid": "worklist-branch-invalid",
}


def worktree_call(operation: Any, *arguments: Any, **keywords: Any) -> Any:
    try:
        return operation(*arguments, **keywords)
    except worktrees.WorktreeError as error:
        raise DriverError(
            WORKTREE_ERROR_CODES.get(error.code, "worklist-worktree-invalid"),
            error.message,
            error.details,
        ) from error


def prepare_workspace(
    checkout: Path,
    worktree_root: Path,
    branch_prefix: str,
    base_rev: str,
    repository: str,
    task_id: str,
) -> dict[str, str]:
    """One worktree and one branch per task, and never a shared one.

    Re-running against an existing worktree is the resume path: the same lane
    identity on the same repository is adopted as-is, work and all. Anything
    else in the way is a conflict rather than something to clobber. Both halves
    are the shared manager's, so the campaign driver and this one now promise
    the same thing.
    """
    branch = f"{branch_prefix.rstrip('/')}/{task_id}"
    worktree = worktree_root / task_id
    identity = {
        "driver": "agency-nightly",
        "repository": repository,
        "taskid": task_id,
        "branch": branch,
    }
    resumed = worktree_call(worktrees.resume, checkout, worktree, identity)
    if resumed is None:
        worktree_call(worktrees.create, checkout, worktree, branch, base_rev, identity)
    return {
        "taskId": task_id,
        "branch": branch,
        "worktreePath": str(worktree),
    }


def worklist(brief: dict[str, Any]) -> dict[str, Any]:
    repository = require_string(brief.get("repository"), "repository")
    checkout = Path(require_string(brief.get("checkout"), "checkout", absolute=True))
    base_ref = require_string(brief.get("baseRev"), "baseRev")
    base_branch = require_string(brief.get("baseBranch"), "baseBranch")
    worktree_root = Path(
        require_string(brief.get("worktreeRoot"), "worktreeRoot", absolute=True)
    )
    branch_prefix = require_string(brief.get("branchPrefix"), "branchPrefix")
    tasks = read_wave(brief)

    command(
        ["git", "-C", str(checkout), "fetch", "--prune", "origin", base_branch],
        error_code="worklist-base-fetch-failed",
    )
    base_rev = command(
        ["git", "-C", str(checkout), "rev-parse", "--verify", f"{base_ref}^{{commit}}"],
        error_code="worklist-base-revision-invalid",
    ).stdout.strip()
    if not COMMIT_ID.fullmatch(base_rev):
        raise DriverError(
            "worklist-base-revision-invalid",
            "git resolved the configured base revision to an invalid commit ID",
            {"baseRev": base_rev},
        )

    worktree_root.mkdir(parents=True, exist_ok=True)
    workspaces = [
        prepare_workspace(
            checkout, worktree_root, branch_prefix, base_rev, repository, task["taskId"]
        )
        for task in tasks
    ]
    return {
        "schemaVersion": 1,
        "repository": repository,
        "baseRev": base_rev,
        "tasks": tasks,
        "workspaces": workspaces,
    }


def pr_title(task: dict[str, Any]) -> str:
    return task["title"].strip()


def pr_body(entry: dict[str, Any]) -> str:
    task = entry["task"]
    implementation = entry["implementation"]
    review = entry["review"]
    lines = [f"Agency wave task: `{task['taskId']}`"]
    if task.get("issue"):
        # A plain reference, never a closing keyword: the human culmination
        # decides what a merge closes.
        lines.append(f"Worklist issue: #{task['issue']}")
    lines.extend(["", "## Acceptance", ""])
    lines.extend(f"- {criterion}" for criterion in task["acceptanceCriteria"])
    lines.extend(["", "## Implementation report", "", implementation["summary"]])
    if implementation.get("tests"):
        lines.extend(["", "Checks run:", ""])
        lines.extend(f"- {check}" for check in implementation["tests"])
    if review is None:
        lines.extend(
            [
                "",
                "## Cross-harness review",
                "",
                "The review did not complete. This pull request carries the",
                "implementation only.",
            ]
        )
        return "\n".join(lines).rstrip() + "\n"
    lines.extend(
        [
            "",
            "## Cross-harness review",
            "",
            f"Verdict: {review['verdict']}",
            "",
            review["summary"],
        ]
    )
    if review["findings"]:
        lines.extend(["", "### Findings", ""])
        lines.extend(
            f"- {finding['severity']}: {finding['text']}"
            for finding in review["findings"]
        )
    return "\n".join(lines).rstrip() + "\n"


def existing_pr(repository: str, branch: str) -> str | None:
    result = command(
        [
            "gh",
            "pr",
            "list",
            "--repo",
            repository,
            "--head",
            branch,
            "--state",
            "open",
            "--limit",
            "1",
            "--json",
            "url",
        ],
        error_code="culmination-pr-lookup-failed",
    )
    try:
        values = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise DriverError(
            "culmination-pr-lookup-failed",
            "GitHub returned invalid JSON while looking up a pull request",
            {"branch": branch},
        ) from error
    if not isinstance(values, list) or any(
        not isinstance(value, dict) for value in values
    ):
        raise DriverError(
            "culmination-pr-lookup-failed",
            "GitHub returned an invalid pull-request list",
            {"branch": branch},
        )
    if not values:
        return None
    url = values[0].get("url")
    if not isinstance(url, str) or not url:
        raise DriverError(
            "culmination-pr-lookup-failed",
            "GitHub returned a pull request without a URL",
            {"branch": branch},
        )
    return url


def create_pr(
    repository: str,
    base_branch: str,
    branch: str,
    title: str,
    body: str,
) -> str:
    result = command(
        [
            "gh",
            "pr",
            "create",
            "--repo",
            repository,
            "--base",
            base_branch,
            "--head",
            branch,
            "--title",
            title,
            "--body",
            body,
        ],
        error_code="culmination-pr-create-failed",
    )
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    if not lines or not lines[-1].startswith(("https://", "http://")):
        raise DriverError(
            "culmination-pr-create-failed",
            "GitHub did not return the created pull-request URL",
            {"branch": branch},
        )
    return lines[-1]


def render_report(
    repository: str,
    base_rev: str | None,
    worklist_error: dict[str, Any] | None,
    entries: list[dict[str, Any]],
    pull_requests: list[dict[str, Any]],
    failures: list[dict[str, Any]],
) -> str:
    by_task = {pull_request["taskId"]: pull_request for pull_request in pull_requests}
    failure_by_task = {failure["taskId"]: failure for failure in failures}
    lines = [
        "# Agency morning report",
        "",
        f"Repository: `{repository}`",
        f"Pinned base: `{base_rev}`" if base_rev else "Pinned base: none",
        f"Wave size: {len(entries)}",
        f"Pull requests: {len(pull_requests)}",
        f"Failed tasks: {len(failures)}",
        "",
    ]
    if worklist_error is not None:
        lines.extend(
            [
                "## The wave did not start",
                "",
                f"`{worklist_error['code']}`: {worklist_error['message']}",
                "",
                "No task was dispatched. Nothing was pushed.",
                "",
            ]
        )
    if not entries and worklist_error is None:
        lines.extend(["The wave was empty.", ""])
    for entry in entries:
        task = entry["task"]
        task_id = task["taskId"]
        pull_request = by_task.get(task_id)
        failure = failure_by_task.get(task_id)
        heading = f"## {task_id} — {task['title'].replace(chr(10), ' ')}"
        lines.extend([heading, ""])
        if entry["workspace"]:
            lines.append(f"- Branch: `{entry['workspace']['branch']}`")
        if pull_request:
            lines.append(
                f"- Pull request: {pull_request['url'] or pull_request['status']}"
            )
        if failure:
            lines.append(f"- **Failed at {failure['stage']}**: `{failure['code']}`")
            lines.append(f"  {failure['message']}")
        review = entry.get("review")
        if review:
            lines.append(f"- Review verdict: **{review['verdict']}**")
            lines.append(f"- Reviewed head: `{review['reviewedHead']}`")
        lines.append("")
        implementation = entry.get("implementation")
        if implementation:
            lines.extend([implementation["summary"], ""])
        if review:
            lines.extend([review["summary"], ""])
            if review["findings"]:
                lines.extend(["Findings:", ""])
                lines.extend(
                    f"- {finding['severity']}: {finding['text']}"
                    for finding in review["findings"]
                )
                lines.append("")
    lines.extend(
        [
            "## Human culmination",
            "",
            "Judge the pull requests and the cross-harness findings here. The wave",
            "had no earlier human gate, and the reviewing harness never certified",
            "its own findings.",
            "",
        ]
    )
    return "\n".join(lines).rstrip() + "\n"


def write_atomic(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        dir=path.parent,
        prefix=f".{path.name}.",
        delete=False,
    ) as handle:
        handle.write(content)
        temporary = Path(handle.name)
    os.replace(temporary, path)


def read_culmination_entry(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise DriverError(
            "agency-driver-brief-invalid", "each culmination task must be an object"
        )
    task = value.get("task")
    if not isinstance(task, dict):
        raise DriverError(
            "agency-driver-brief-invalid", "each culmination task requires a task object"
        )
    task_id = require_string(task.get("taskId"), "task.taskId")
    workspace = value.get("workspace")
    if workspace is not None and not isinstance(workspace, dict):
        raise DriverError(
            "agency-driver-brief-invalid",
            f"task {task_id!r} has an invalid workspace",
            {"taskId": task_id},
        )
    for field in ("implementation", "review", "failure"):
        if value.get(field) is not None and not isinstance(value.get(field), dict):
            raise DriverError(
                "agency-driver-brief-invalid",
                f"task {task_id!r} has an invalid {field}",
                {"taskId": task_id},
            )
    return {
        "task": task,
        "workspace": workspace,
        "implementation": value.get("implementation"),
        "review": value.get("review"),
        "failure": value.get("failure"),
    }


def publish(
    repository: str,
    base_branch: str,
    base_rev: str,
    entry: dict[str, Any],
) -> dict[str, Any]:
    """Push one task's committed work and open or find its pull request.

    The review verdict is deliberately not consulted. A finder never certifies:
    `changes-requested` reaches the human as pull-request text, not as a gate
    that silently drops the night's work.
    """
    task_id = entry["task"]["taskId"]
    workspace = entry["workspace"]
    implementation = entry["implementation"]
    branch = require_string(workspace.get("branch"), "workspace.branch")
    worktree = Path(
        require_string(
            workspace.get("worktreePath"), "workspace.worktreePath", absolute=True
        )
    )
    head = require_string(implementation.get("head"), "implementation.head")
    if not COMMIT_ID.fullmatch(head):
        raise DriverError(
            "culmination-head-invalid",
            f"task {task_id!r} reported an invalid head commit",
            {"taskId": task_id, "head": head},
        )
    actual_head = command(
        ["git", "-C", str(worktree), "rev-parse", "HEAD"],
        error_code="culmination-head-invalid",
    ).stdout.strip()
    if head != actual_head:
        raise DriverError(
            "culmination-head-drift",
            f"task {task_id!r} worktree moved after the implementation report",
            {"taskId": task_id, "reportedHead": head, "actualHead": actual_head},
        )
    review = entry["review"]
    if review is not None:
        reviewed_head = require_string(review.get("reviewedHead"), "review.reviewedHead")
        if reviewed_head != head:
            raise DriverError(
                "culmination-review-drift",
                f"task {task_id!r} review names a different commit",
                {
                    "taskId": task_id,
                    "implementationHead": head,
                    "reviewedHead": reviewed_head,
                },
            )
    commit_count_text = command(
        ["git", "-C", str(worktree), "rev-list", "--count", f"{base_rev}..{head}"],
        error_code="culmination-history-invalid",
    ).stdout.strip()
    try:
        commit_count = int(commit_count_text)
    except ValueError as error:
        raise DriverError(
            "culmination-history-invalid",
            f"git returned an invalid commit count for task {task_id!r}",
            {"taskId": task_id, "count": commit_count_text},
        ) from error
    if commit_count == 0:
        return {
            "taskId": task_id,
            "branch": branch,
            "status": "no-changes",
            "url": None,
        }
    command(
        ["git", "-C", str(worktree), "push", "origin", f"{head}:refs/heads/{branch}"],
        error_code="culmination-push-failed",
    )
    url = existing_pr(repository, branch)
    status = "existing"
    if url is None:
        url = create_pr(
            repository, base_branch, branch, pr_title(entry["task"]), pr_body(entry)
        )
        status = "created"
    return {"taskId": task_id, "branch": branch, "status": status, "url": url}


def culminate(brief: dict[str, Any]) -> dict[str, Any]:
    repository = require_string(brief.get("repository"), "repository")
    base_branch = require_string(brief.get("baseBranch"), "baseBranch")
    base_rev = optional_string(brief.get("baseRev"), "baseRev")
    report_path = Path(
        require_string(brief.get("reportPath"), "reportPath", absolute=True)
    )
    worklist_error = brief.get("worklistError")
    if worklist_error is not None and not isinstance(worklist_error, dict):
        raise DriverError(
            "agency-driver-brief-invalid", "worklistError must be an object or null"
        )
    raw_tasks = brief.get("tasks")
    if not isinstance(raw_tasks, list) or len(raw_tasks) > MAX_WAVE_SIZE:
        raise DriverError(
            "agency-driver-brief-invalid",
            f"culmination tasks must be an array of at most {MAX_WAVE_SIZE}",
        )
    entries = [read_culmination_entry(value) for value in raw_tasks]

    pull_requests: list[dict[str, Any]] = []
    failures: list[dict[str, Any]] = []
    for entry in entries:
        task_id = entry["task"]["taskId"]
        if entry["failure"] is not None or entry["implementation"] is None:
            failure = entry["failure"] or {}
            failures.append(
                {
                    "taskId": task_id,
                    "stage": failure.get("stage") or "implementation",
                    "code": failure.get("code") or "task-incomplete",
                    "message": failure.get("message") or "the task produced no result",
                }
            )
            continue
        if entry["workspace"] is None or base_rev is None:
            failures.append(
                {
                    "taskId": task_id,
                    "stage": "implementation",
                    "code": "culmination-workspace-missing",
                    "message": "the task has no witnessed workspace to publish from",
                }
            )
            continue
        pull_requests.append(publish(repository, base_branch, base_rev, entry))

    if worklist_error is not None:
        status = "worklist-failed"
    elif failures:
        status = "partial"
    else:
        status = "ready"
    write_atomic(
        report_path,
        render_report(
            repository, base_rev, worklist_error, entries, pull_requests, failures
        ),
    )
    return {
        "status": status,
        "reportPath": str(report_path),
        "pullRequests": pull_requests,
        "failures": failures,
    }


def read_brief() -> dict[str, Any]:
    raw = os.environ.get("TALLY_BRIEF")
    if raw is None:
        raise DriverError("agency-driver-brief-invalid", "TALLY_BRIEF is not set")
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise DriverError(
            "agency-driver-brief-invalid", "TALLY_BRIEF is not valid JSON"
        ) from error
    if not isinstance(value, dict):
        raise DriverError(
            "agency-driver-brief-invalid", "TALLY_BRIEF must be a JSON object"
        )
    return value


def main() -> int:
    try:
        if len(sys.argv) != 2 or sys.argv[1] not in ("worklist", "culminate"):
            raise DriverError(
                "agency-driver-action-invalid",
                "usage: agency-nightly-driver worklist|culminate",
            )
        brief = read_brief()
        value = worklist(brief) if sys.argv[1] == "worklist" else culminate(brief)
        emit({"ok": True, "value": value})
    except DriverError as error:
        emit(
            {
                "ok": False,
                "error": {
                    "code": error.code,
                    "message": error.message,
                    "details": error.details,
                },
            }
        )
    except Exception as error:  # The flow still receives a typed, witnessed failure.
        emit(
            {
                "ok": False,
                "error": {
                    "code": "agency-driver-internal-error",
                    "message": str(error),
                    "details": {},
                },
            }
        )
    # Always exit zero: the envelope carries the verdict, and `exit:0` evidence
    # keeps a reported failure a witnessed result rather than a dead node.
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
