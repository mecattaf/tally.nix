#!/usr/bin/env python3
"""Deterministic GitHub worklist and culmination driver for agency-nightly.js."""

from __future__ import annotations

import heapq
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path, PurePosixPath
from typing import Any, Callable

FINAL_MESSAGE_PREFIX = "TALLY_FINAL_MESSAGE="
PARALLEL_TITLE = re.compile(r"^\[P\](?:\s+|$)")
ACCEPTANCE_HEADING = re.compile(r"^##\s+Acceptance\s*$")
LEVEL_TWO_HEADING = re.compile(r"^##(?:\s+|$)")
TASK_LIST_ITEM = re.compile(r"^\s*[-*+]\s+\[([ xX])\]\s+(.+?)\s*$")
FILES_TRAILER = re.compile(r"^Files:\s*(.*?)\s*$")
DEPENDS_TRAILER = re.compile(r"^Depends-on:\s*(.*?)\s*$")
DEPENDENCY_REFERENCE = re.compile(r"#([1-9][0-9]*)")
COMMIT_ID = re.compile(r"^[0-9a-f]{40,64}$")


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
            f"command {argv[0]!r} exited {result.returncode}",
            {
                "argv": argv,
                "exitCode": result.returncode,
                "stderr": result.stderr[-4000:],
            },
        )
    return result


def require_string(value: Any, field: str, *, absolute: bool = False) -> str:
    if not isinstance(value, str) or not value or any(ord(char) < 32 for char in value):
        raise DriverError(
            "agency-driver-brief-invalid",
            f"{field} must be a non-empty string without control characters",
            {"field": field},
        )
    if absolute and not Path(value).is_absolute():
        raise DriverError(
            "agency-driver-brief-invalid",
            f"{field} must be an absolute path",
            {"field": field},
        )
    return value


def require_int(value: Any, field: str, minimum: int, maximum: int) -> int:
    if (
        not isinstance(value, int)
        or isinstance(value, bool)
        or value < minimum
        or value > maximum
    ):
        raise DriverError(
            "agency-driver-brief-invalid",
            f"{field} must be an integer from {minimum} through {maximum}",
            {"field": field},
        )
    return value


def parse_acceptance(body: str, task_id: str) -> list[dict[str, Any]]:
    lines = body.splitlines()
    headings = [
        index for index, line in enumerate(lines) if ACCEPTANCE_HEADING.fullmatch(line)
    ]
    if len(headings) != 1:
        raise DriverError(
            "worklist-acceptance-missing",
            f"issue #{task_id} must contain exactly one ## Acceptance heading",
            {"taskId": task_id, "headingCount": len(headings)},
        )
    start = headings[0] + 1
    end = next(
        (
            index
            for index in range(start, len(lines))
            if LEVEL_TWO_HEADING.match(lines[index])
        ),
        len(lines),
    )
    criteria = []
    for line in lines[start:end]:
        match = TASK_LIST_ITEM.match(line)
        if match:
            criteria.append(
                {
                    "text": match.group(2),
                    "checked": match.group(1).lower() == "x",
                }
            )
    if not criteria:
        raise DriverError(
            "worklist-acceptance-missing",
            f"issue #{task_id} has no task-list checklist under ## Acceptance",
            {"taskId": task_id},
        )
    return criteria


def parse_files(body: str, task_id: str) -> list[str]:
    files: list[str] = []
    for line in body.splitlines():
        match = FILES_TRAILER.fullmatch(line)
        if not match:
            continue
        values = [value.strip() for value in match.group(1).split(",")]
        if not values or any(not value for value in values):
            raise DriverError(
                "worklist-files-invalid",
                f"issue #{task_id} has an empty Files: trailer",
                {"taskId": task_id},
            )
        for value in values:
            path = PurePosixPath(value)
            if (
                path.is_absolute()
                or value.startswith("./")
                or "\\" in value
                or any(part in ("", ".", "..") for part in path.parts)
                or any(ord(char) < 32 for char in value)
            ):
                raise DriverError(
                    "worklist-files-invalid",
                    f"issue #{task_id} has an invalid repository-relative file path",
                    {"taskId": task_id, "path": value},
                )
            if value not in files:
                files.append(value)
    return files


def parse_dependencies(body: str, task_id: str) -> list[str]:
    dependencies: list[str] = []
    for line in body.splitlines():
        match = DEPENDS_TRAILER.fullmatch(line)
        if not match:
            continue
        value = match.group(1)
        references = DEPENDENCY_REFERENCE.findall(value)
        residual = DEPENDENCY_REFERENCE.sub("", value)
        if not references or residual.strip(" ,\t"):
            raise DriverError(
                "worklist-dependency-invalid",
                f"issue #{task_id} has an invalid Depends-on: trailer",
                {"taskId": task_id, "trailer": value},
            )
        for dependency in references:
            if dependency not in dependencies:
                dependencies.append(dependency)
    return dependencies


def parse_issue(issue: dict[str, Any]) -> dict[str, Any]:
    number = issue.get("number")
    title = issue.get("title")
    body = issue.get("body")
    if (
        not isinstance(number, int)
        or number < 1
        or not isinstance(title, str)
        or not title
        or not isinstance(body, str)
    ):
        raise DriverError(
            "worklist-issue-invalid",
            "GitHub returned an issue without a positive number, title, or body",
            {"issue": issue},
        )
    task_id = str(number)
    entry: dict[str, Any] = {
        "taskId": task_id,
        "title": title,
        "acceptanceCriteria": parse_acceptance(body, task_id),
        "parallelism": "parallel"
        if PARALLEL_TITLE.match(title)
        else "sequential",
    }
    files = parse_files(body, task_id)
    dependencies = parse_dependencies(body, task_id)
    if files:
        entry["files"] = files
    if dependencies:
        entry["dependsOn"] = dependencies
    return entry


def topological_order(entries: list[dict[str, Any]]) -> list[dict[str, Any]]:
    by_id = {entry["taskId"]: entry for entry in entries}
    if len(by_id) != len(entries):
        raise DriverError(
            "worklist-task-id-duplicate",
            "the GitHub worklist contains a duplicate task ID",
        )
    indegree = {task_id: 0 for task_id in by_id}
    dependents = {task_id: [] for task_id in by_id}
    for task_id, entry in by_id.items():
        for dependency in entry.get("dependsOn", []):
            if dependency in by_id:
                indegree[task_id] += 1
                dependents[dependency].append(task_id)
    ready = [int(task_id) for task_id, count in indegree.items() if count == 0]
    heapq.heapify(ready)
    ordered: list[dict[str, Any]] = []
    while ready:
        task_id = str(heapq.heappop(ready))
        ordered.append(by_id[task_id])
        for dependent in sorted(dependents[task_id], key=int):
            indegree[dependent] -= 1
            if indegree[dependent] == 0:
                heapq.heappush(ready, int(dependent))
    if len(ordered) != len(entries):
        cycle = sorted(
            (task_id for task_id, count in indegree.items() if count > 0), key=int
        )
        raise DriverError(
            "worklist-dependency-cycle",
            "the labeled GitHub worklist contains a dependency cycle",
            {"taskIds": cycle},
        )
    return ordered


def dependency_states(
    entries: list[dict[str, Any]],
    repository: str,
    lookup: Callable[[str], str],
) -> dict[str, str]:
    states = {entry["taskId"]: "OPEN" for entry in entries}
    for entry in entries:
        for dependency in entry.get("dependsOn", []):
            if dependency in states:
                continue
            state = lookup(dependency).upper()
            if state not in ("OPEN", "CLOSED"):
                raise DriverError(
                    "worklist-dependency-lookup",
                    f"dependency #{dependency} in {repository} has an unknown state",
                    {"taskId": entry["taskId"], "dependency": dependency, "state": state},
                )
            states[dependency] = state
    return states


def select_wave(
    ordered: list[dict[str, Any]], states: dict[str, str], max_wave_size: int
) -> list[str]:
    ready = [
        entry
        for entry in ordered
        if all(states[dependency] == "CLOSED" for dependency in entry.get("dependsOn", []))
    ]
    if not ready:
        return []
    if ready[0]["parallelism"] == "sequential":
        return [ready[0]["taskId"]]
    wave = []
    for entry in ready:
        if entry["parallelism"] != "parallel" or len(wave) == max_wave_size:
            break
        wave.append(entry["taskId"])
    return wave


def gh_issue_state(repository: str, task_id: str) -> str:
    result = command(
        [
            "gh",
            "issue",
            "view",
            task_id,
            "--repo",
            repository,
            "--json",
            "state",
        ],
        error_code="worklist-dependency-lookup",
    )
    try:
        value = json.loads(result.stdout)
        state = value["state"]
    except (json.JSONDecodeError, KeyError, TypeError) as error:
        raise DriverError(
            "worklist-dependency-lookup",
            f"GitHub returned an invalid state for dependency #{task_id}",
            {"taskId": task_id},
        ) from error
    if not isinstance(state, str):
        raise DriverError(
            "worklist-dependency-lookup",
            f"GitHub returned a non-string state for dependency #{task_id}",
            {"taskId": task_id},
        )
    return state


def same_repository(checkout: Path, worktree: Path) -> bool:
    checkout_common = command(
        ["git", "-C", str(checkout), "rev-parse", "--git-common-dir"],
        error_code="worklist-worktree-invalid",
    ).stdout.strip()
    worktree_common = command(
        ["git", "-C", str(worktree), "rev-parse", "--git-common-dir"],
        error_code="worklist-worktree-invalid",
    ).stdout.strip()
    checkout_path = (checkout / checkout_common).resolve()
    worktree_path = (worktree / worktree_common).resolve()
    return checkout_path == worktree_path


def prepare_workspace(
    checkout: Path,
    worktree_root: Path,
    branch_prefix: str,
    base_rev: str,
    task_id: str,
) -> dict[str, str]:
    branch = f"{branch_prefix.rstrip('/')}/issue-{task_id}"
    command(
        ["git", "check-ref-format", "--branch", branch],
        error_code="worklist-branch-invalid",
    )
    worktree = worktree_root / f"issue-{task_id}"
    if worktree.exists():
        if not worktree.is_dir() or not same_repository(checkout, worktree):
            raise DriverError(
                "worklist-worktree-conflict",
                f"existing path for task #{task_id} is not a worktree of the configured checkout",
                {"taskId": task_id, "worktreePath": str(worktree)},
            )
        actual_branch = command(
            ["git", "-C", str(worktree), "symbolic-ref", "--short", "HEAD"],
            error_code="worklist-worktree-invalid",
        ).stdout.strip()
        if actual_branch != branch:
            raise DriverError(
                "worklist-worktree-conflict",
                f"existing worktree for task #{task_id} is on a different branch",
                {
                    "taskId": task_id,
                    "worktreePath": str(worktree),
                    "expectedBranch": branch,
                    "actualBranch": actual_branch,
                },
            )
    else:
        branch_exists = (
            command(
                [
                    "git",
                    "-C",
                    str(checkout),
                    "show-ref",
                    "--verify",
                    "--quiet",
                    f"refs/heads/{branch}",
                ],
                check=False,
            ).returncode
            == 0
        )
        argv = ["git", "-C", str(checkout), "worktree", "add"]
        if not branch_exists:
            argv.extend(["-b", branch])
        argv.append(str(worktree))
        argv.append(branch if branch_exists else base_rev)
        command(argv, error_code="worklist-worktree-create-failed")
    return {
        "taskId": task_id,
        "branch": branch,
        "worktreePath": str(worktree),
    }


def read_worklist(brief: dict[str, Any]) -> dict[str, Any]:
    source = brief.get("source")
    if not isinstance(source, dict):
        raise DriverError(
            "agency-driver-brief-invalid", "worklist source must be an object"
        )
    if source.get("kind") != "github-issues" or source.get("state") != "open":
        raise DriverError(
            "agency-driver-brief-invalid",
            "worklist source must select open GitHub issues",
        )
    repository = require_string(source.get("repository"), "source.repository")
    label = require_string(source.get("label"), "source.label")
    if label != "tally:worklist":
        raise DriverError(
            "worklist-label-invalid",
            "the canonical GitHub adapter requires the tally:worklist label",
            {"label": label},
        )
    checkout = Path(require_string(brief.get("checkout"), "checkout", absolute=True))
    base_ref = require_string(brief.get("baseRev"), "baseRev")
    base_branch = require_string(brief.get("baseBranch"), "baseBranch")
    worktree_root = Path(
        require_string(brief.get("worktreeRoot"), "worktreeRoot", absolute=True)
    )
    branch_prefix = require_string(brief.get("branchPrefix"), "branchPrefix")
    max_wave_size = require_int(brief.get("maxWaveSize"), "maxWaveSize", 1, 6)

    result = command(
        [
            "gh",
            "issue",
            "list",
            "--repo",
            repository,
            "--state",
            "open",
            "--label",
            label,
            "--limit",
            "1000",
            "--json",
            "number,title,body",
        ],
        error_code="worklist-source-failed",
    )
    try:
        raw_issues = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise DriverError(
            "worklist-source-failed",
            "GitHub returned invalid JSON for the labeled worklist",
        ) from error
    if not isinstance(raw_issues, list):
        raise DriverError(
            "worklist-source-failed",
            "GitHub returned a non-array labeled worklist",
        )

    entries = [parse_issue(issue) for issue in raw_issues]
    ordered = topological_order(entries)
    states = dependency_states(
        entries,
        repository,
        lambda task_id: gh_issue_state(repository, task_id),
    )
    wave = select_wave(ordered, states, max_wave_size)

    command(
        [
            "git",
            "-C",
            str(checkout),
            "fetch",
            "--prune",
            "origin",
            base_branch,
        ],
        error_code="worklist-base-fetch-failed",
    )
    base_rev = command(
        [
            "git",
            "-C",
            str(checkout),
            "rev-parse",
            "--verify",
            f"{base_ref}^{{commit}}",
        ],
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
            checkout, worktree_root, branch_prefix, base_rev, task_id
        )
        for task_id in wave
    ]
    return {
        "schemaVersion": 1,
        "source": {
            "kind": "github-issues",
            "repository": repository,
            "label": label,
        },
        "baseRev": base_rev,
        "entries": ordered,
        "wave": wave,
        "workspaces": workspaces,
    }


def compact_title(title: str) -> str:
    return PARALLEL_TITLE.sub("", title, count=1).strip()


def pr_body(task: dict[str, Any]) -> str:
    entry = task["entry"]
    implementation = task["implementation"]
    review = task["review"]
    lines = [
        f"Worklist issue: #{entry['taskId']}",
        "",
        "## Acceptance",
        "",
    ]
    for criterion in entry["acceptanceCriteria"]:
        mark = "x" if criterion["checked"] else " "
        lines.append(f"- [{mark}] {criterion['text']}")
    lines.extend(
        [
            "",
            "## Implementation report",
            "",
            implementation["summary"],
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
        for finding in review["findings"]:
            lines.append(f"- {finding['severity']}: {finding['text']}")
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
    base_rev: str,
    tasks: list[dict[str, Any]],
    pull_requests: list[dict[str, Any]],
) -> str:
    lines = [
        "# Agency morning report",
        "",
        f"Repository: `{repository}`",
        f"Pinned base: `{base_rev}`",
        f"Wave size: {len(tasks)}",
        "",
    ]
    if not tasks:
        lines.append("No ready `tally:worklist` tasks were available.")
        return "\n".join(lines).rstrip() + "\n"
    for task, pull_request in zip(tasks, pull_requests, strict=True):
        entry = task["entry"]
        implementation = task["implementation"]
        review = task["review"]
        lines.extend(
            [
                f"## #{entry['taskId']} — {entry['title'].replace(chr(10), ' ')}",
                "",
                f"- Branch: `{pull_request['branch']}`",
                f"- Pull request: {pull_request['url'] or pull_request['status']}",
                f"- Review verdict: **{review['verdict']}**",
                f"- Reviewed head: `{review['reviewedHead']}`",
                "",
                implementation["summary"],
                "",
                review["summary"],
                "",
            ]
        )
        if review["findings"]:
            lines.append("Findings:")
            lines.append("")
            for finding in review["findings"]:
                lines.append(f"- {finding['severity']}: {finding['text']}")
            lines.append("")
    lines.extend(
        [
            "## Human culmination",
            "",
            "Judge the pull requests and cross-harness findings here. The flow has no earlier human gate.",
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


def culminate(brief: dict[str, Any]) -> dict[str, Any]:
    repository = require_string(brief.get("repository"), "repository")
    base_rev = require_string(brief.get("baseRev"), "baseRev")
    base_branch = require_string(brief.get("baseBranch"), "baseBranch")
    report_path = Path(
        require_string(brief.get("reportPath"), "reportPath", absolute=True)
    )
    tasks = brief.get("tasks")
    if not isinstance(tasks, list) or len(tasks) > 6:
        raise DriverError(
            "agency-driver-brief-invalid", "culmination tasks must be an array of at most six"
        )

    pull_requests = []
    for task in tasks:
        if not isinstance(task, dict):
            raise DriverError(
                "agency-driver-brief-invalid",
                "each culmination task must be an object",
            )
        entry = task.get("entry")
        workspace = task.get("workspace")
        implementation = task.get("implementation")
        review = task.get("review")
        if not all(
            isinstance(value, dict)
            for value in (entry, workspace, implementation, review)
        ):
            raise DriverError(
                "agency-driver-brief-invalid",
                "each culmination task requires entry, workspace, implementation, and review objects",
            )
        task_id = require_string(entry.get("taskId"), "entry.taskId")
        branch = require_string(workspace.get("branch"), "workspace.branch")
        worktree = Path(
            require_string(
                workspace.get("worktreePath"), "workspace.worktreePath", absolute=True
            )
        )
        head = require_string(implementation.get("head"), "implementation.head")
        reviewed_head = require_string(review.get("reviewedHead"), "review.reviewedHead")
        actual_head = command(
            ["git", "-C", str(worktree), "rev-parse", "HEAD"],
            error_code="culmination-head-invalid",
        ).stdout.strip()
        if head != actual_head:
            raise DriverError(
                "culmination-head-drift",
                f"task #{task_id} worktree moved after the implementation report",
                {
                    "taskId": task_id,
                    "reportedHead": head,
                    "actualHead": actual_head,
                },
            )
        if reviewed_head != head:
            raise DriverError(
                "culmination-review-drift",
                f"task #{task_id} review names a different commit",
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
                f"git returned an invalid commit count for task #{task_id}",
                {"taskId": task_id, "count": commit_count_text},
            ) from error
        if commit_count == 0:
            pull_requests.append(
                {
                    "taskId": task_id,
                    "branch": branch,
                    "status": "no-changes",
                    "url": None,
                }
            )
            continue
        command(
            [
                "git",
                "-C",
                str(worktree),
                "push",
                "origin",
                f"{head}:refs/heads/{branch}",
            ],
            error_code="culmination-push-failed",
        )
        url = existing_pr(repository, branch)
        status = "existing"
        if url is None:
            url = create_pr(
                repository,
                base_branch,
                branch,
                compact_title(require_string(entry.get("title"), "entry.title")),
                pr_body(task),
            )
            status = "created"
        pull_requests.append(
            {
                "taskId": task_id,
                "branch": branch,
                "status": status,
                "url": url,
            }
        )

    status = "ready" if tasks else "empty"
    write_atomic(
        report_path,
        render_report(repository, base_rev, tasks, pull_requests),
    )
    return {
        "status": status,
        "reportPath": str(report_path),
        "pullRequests": pull_requests,
    }


def read_brief() -> dict[str, Any]:
    raw = os.environ.get("TALLY_BRIEF")
    if raw is None:
        raise DriverError(
            "agency-driver-brief-invalid", "TALLY_BRIEF is not set"
        )
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


def load_github_credential() -> None:
    if os.environ.get("GH_TOKEN"):
        return
    directory = os.environ.get("CREDENTIALS_DIRECTORY")
    if not directory:
        return
    token_path = Path(directory) / "GH_TOKEN"
    if not token_path.is_file():
        return
    token = token_path.read_text(encoding="utf-8").strip()
    if not token:
        raise DriverError(
            "agency-driver-credential-invalid",
            "the GH_TOKEN systemd credential is empty",
        )
    os.environ["GH_TOKEN"] = token


def main() -> int:
    try:
        if len(sys.argv) != 2 or sys.argv[1] not in ("worklist", "culminate"):
            raise DriverError(
                "agency-driver-action-invalid",
                "usage: agency-nightly-driver worklist|culminate",
            )
        load_github_credential()
        brief = read_brief()
        value = read_worklist(brief) if sys.argv[1] == "worklist" else culminate(brief)
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
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
