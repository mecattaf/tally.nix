#!/usr/bin/env python3
"""Deterministic policy driver for the shipped spec-build tally flow."""

from __future__ import annotations

import argparse
import glob
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from typing import Any


TASK_ID = re.compile(r"^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$")
COMPONENT = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_.-]*$")
REPOSITORY = re.compile(r"^[^/ \t]+/[^/ \t]+$")


class DriverError(RuntimeError):
    pass


def fail(message: str) -> None:
    raise DriverError(message)


def object_exact(value: Any, fields: set[str], context: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        fail(f"{context} must be an object")
    unknown = sorted(set(value) - fields)
    if unknown:
        fail(f"{context} has unknown fields: {', '.join(unknown)}")
    return value


def required_string(value: Any, context: str, maximum: int | None = None) -> str:
    if not isinstance(value, str) or not value or any(ord(char) < 32 for char in value):
        fail(f"{context} must be a non-empty string without control characters")
    if maximum is not None and len(value) > maximum:
        fail(f"{context} exceeds {maximum} characters")
    return value


def string_list(value: Any, context: str, *, nonempty: bool = False) -> list[str]:
    if not isinstance(value, list) or (nonempty and not value):
        fail(f"{context} must be {'a non-empty' if nonempty else 'an'} array")
    return [required_string(item, f"{context}[{index}]") for index, item in enumerate(value)]


def argv(value: Any, context: str) -> list[str]:
    result = string_list(value, context, nonempty=True)
    if not result[0]:
        fail(f"{context} requires a non-empty executable")
    return result


def load_brief() -> dict[str, Any]:
    path_text = os.environ.get("TALLY_BRIEF")
    if not path_text:
        fail("TALLY_BRIEF is required")
    path = Path(path_text)
    if not path.is_absolute() or not path.is_file():
        fail("TALLY_BRIEF must name an absolute regular file")
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read TALLY_BRIEF: {error}")
    if not isinstance(value, dict):
        fail("TALLY_BRIEF must contain an object")
    return value


def emit(value: dict[str, Any]) -> None:
    print("TALLY_FINAL_MESSAGE=" + json.dumps(value, sort_keys=True, separators=(",", ":")))


def run(
    command: list[str],
    *,
    cwd: Path | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as error:
        fail(f"cannot execute {command[0]!r}: {error}")
    if check and result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no output"
        fail(f"command {command!r} exited {result.returncode}: {detail}")
    return result


def git(checkout: Path, *arguments: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return run(["git", "-C", str(checkout), *arguments], check=check)


def repo_config(value: Any) -> dict[str, Any]:
    config = object_exact(value, {"checkout", "baseBranch", "remote", "forge"}, "repositoryConfig")
    checkout = Path(required_string(config.get("checkout"), "repositoryConfig.checkout"))
    if not checkout.is_absolute() or not checkout.is_dir():
        fail("repositoryConfig.checkout must be an absolute directory")
    base_branch = required_string(config.get("baseBranch"), "repositoryConfig.baseBranch")
    remote = required_string(config.get("remote"), "repositoryConfig.remote")
    forge = config.get("forge")
    if forge not in {"github", "local"}:
        fail("repositoryConfig.forge must be github or local")
    git(checkout, "rev-parse", "--git-dir")
    return {
        "checkout": checkout.resolve(),
        "baseBranch": base_branch,
        "remote": remote,
        "forge": forge,
    }


def normalize_acceptance(value: Any, context: str) -> list[dict[str, Any]]:
    if not isinstance(value, list) or not value:
        fail(f"{context} must be a non-empty array")
    result: list[dict[str, Any]] = []
    seen: set[str] = set()
    for index, candidate in enumerate(value):
        item = object_exact(candidate, {"id", "description", "argv"}, f"{context}[{index}]")
        identifier = required_string(item.get("id"), f"{context}[{index}].id", 80)
        if not COMPONENT.fullmatch(identifier):
            fail(f"{context}[{index}].id is not a safe component")
        if identifier in seen:
            fail(f"{context} repeats id {identifier!r}")
        seen.add(identifier)
        result.append(
            {
                "id": identifier,
                "description": required_string(
                    item.get("description"), f"{context}[{index}].description", 4000
                ),
                "argv": argv(item.get("argv"), f"{context}[{index}].argv"),
            }
        )
    return result


def normalize_task(value: Any, index: int, prior_ids: set[str]) -> dict[str, Any]:
    context = f"tasks[{index}]"
    task = object_exact(
        value,
        {
            "id",
            "title",
            "goal",
            "deliveredBehaviors",
            "readFirst",
            "acceptanceCriteria",
            "dependencies",
        },
        context,
    )
    identifier = required_string(task.get("id"), f"{context}.id", 80)
    if not TASK_ID.fullmatch(identifier):
        fail(f"{context}.id must match {TASK_ID.pattern}")
    read_first = object_exact(
        task.get("readFirst"), {"specSections", "styleReferences"}, f"{context}.readFirst"
    )
    dependencies = string_list(task.get("dependencies"), f"{context}.dependencies")
    if len(dependencies) != len(set(dependencies)):
        fail(f"{context}.dependencies contains duplicates")
    missing = [dependency for dependency in dependencies if dependency not in prior_ids]
    if missing:
        fail(
            f"{context}.dependencies must reference earlier tasks; unavailable: {', '.join(missing)}"
        )
    return {
        "id": identifier,
        "title": required_string(task.get("title"), f"{context}.title", 300),
        "goal": required_string(task.get("goal"), f"{context}.goal", 12000),
        "deliveredBehaviors": string_list(
            task.get("deliveredBehaviors"), f"{context}.deliveredBehaviors", nonempty=True
        ),
        "readFirst": {
            "specSections": string_list(
                read_first.get("specSections"), f"{context}.readFirst.specSections", nonempty=True
            ),
            "styleReferences": string_list(
                read_first.get("styleReferences"), f"{context}.readFirst.styleReferences"
            ),
        },
        "acceptanceCriteria": normalize_acceptance(
            task.get("acceptanceCriteria"), f"{context}.acceptanceCriteria"
        ),
        "dependencies": dependencies,
    }


def action_worklist(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief, {"repository", "repositoryConfig", "worklist", "maxTasks"}, "worklist brief"
    )
    repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(repository):
        fail("repository must use owner/name form")
    config = repo_config(data.get("repositoryConfig"))
    pattern = required_string(data.get("worklist"), "worklist")
    pattern_path = Path(pattern)
    if pattern_path.is_absolute() or ".." in pattern_path.parts:
        fail("worklist must be a relative pattern without '..'")
    max_tasks = data.get("maxTasks")
    if not isinstance(max_tasks, int) or isinstance(max_tasks, bool) or not 1 <= max_tasks <= 128:
        fail("maxTasks must be an integer from 1 through 128")

    checkout = config["checkout"]
    matches = sorted(Path(path) for path in glob.glob(str(checkout / pattern), recursive=False))
    matches = [path for path in matches if path.is_file()]
    if len(matches) != 1:
        fail(f"worklist pattern {pattern!r} matched {len(matches)} regular files; expected exactly one")
    source = matches[0].resolve()
    try:
        source.relative_to(checkout)
    except ValueError:
        fail("worklist match resolves outside the configured checkout")
    raw = source.read_bytes()
    try:
        document = json.loads(raw)
    except json.JSONDecodeError as error:
        fail(f"worklist is not valid JSON: {error}")
    document = object_exact(document, {"schemaVersion", "tasks"}, "worklist")
    if document.get("schemaVersion") != 1:
        fail("worklist.schemaVersion must equal 1")
    candidates = document.get("tasks")
    if not isinstance(candidates, list) or not candidates:
        fail("worklist.tasks must be a non-empty array")
    if len(candidates) > max_tasks:
        fail(f"worklist has {len(candidates)} tasks, exceeding maxTasks {max_tasks}")
    tasks: list[dict[str, Any]] = []
    prior_ids: set[str] = set()
    for index, candidate in enumerate(candidates):
        task = normalize_task(candidate, index, prior_ids)
        if task["id"] in prior_ids:
            fail(f"worklist repeats task id {task['id']!r}")
        tasks.append(task)
        prior_ids.add(task["id"])
    return {
        "schemaVersion": 1,
        "repository": repository,
        "source": {
            "path": str(source.relative_to(checkout)),
            "sha256": "sha256:" + hashlib.sha256(raw).hexdigest(),
        },
        "tasks": tasks,
    }


def safe_slug(value: str, maximum: int) -> str:
    slug = re.sub(r"[^A-Za-z0-9._-]+", "-", value).strip(".-") or "campaign"
    return slug[:maximum]


def prep_identity(brief: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    data = object_exact(
        brief,
        {
            "campaign",
            "repository",
            "repositoryConfig",
            "issue",
            "runId",
            "workspaceRoot",
            "task",
        },
        "prep brief",
    )
    task = data.get("task")
    if not isinstance(task, dict):
        fail("task must be an object")
    task_id = required_string(task.get("id"), "task.id")
    if not TASK_ID.fullmatch(task_id):
        fail("task.id is not safe")
    campaign = required_string(data.get("campaign"), "campaign")
    repository = required_string(data.get("repository"), "repository")
    run_id = required_string(data.get("runId"), "runId", 512)
    workspace_root = Path(required_string(data.get("workspaceRoot"), "workspaceRoot"))
    if not workspace_root.is_absolute():
        fail("workspaceRoot must be absolute")
    config = repo_config(data.get("repositoryConfig"))
    identity = {
        "campaign": campaign,
        "repository": repository,
        "runId": run_id,
        "taskId": task_id,
        "workspaceRoot": workspace_root,
    }
    return data, config, identity


def marker_path(identity: dict[str, Any]) -> Path:
    run_hash = hashlib.sha256(identity["runId"].encode()).hexdigest()[:16]
    return identity["workspaceRoot"] / ".state" / run_hash / f"{identity['taskId']}.json"


def write_atomic(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + f".tmp-{os.getpid()}")
    temporary.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")), encoding="utf-8")
    os.replace(temporary, path)


def action_prep(brief: dict[str, Any]) -> dict[str, Any]:
    _, config, identity = prep_identity(brief)
    checkout: Path = config["checkout"]
    remote = config["remote"]
    base_branch = config["baseBranch"]
    run_hash = hashlib.sha256(identity["runId"].encode()).hexdigest()[:12]
    campaign_slug = safe_slug(identity["campaign"], 24)
    repository_slug = safe_slug(identity["repository"].split("/", 1)[1], 40)
    branch = f"tally/{campaign_slug}-{run_hash}/{identity['taskId']}"
    worktree = (
        identity["workspaceRoot"] / repository_slug / run_hash / identity["taskId"]
    ).resolve()
    marker = marker_path(identity)

    if marker.exists():
        saved = json.loads(marker.read_text(encoding="utf-8"))
        expected = {
            "campaign": identity["campaign"],
            "repository": identity["repository"],
            "runId": identity["runId"],
            "taskId": identity["taskId"],
            "branch": branch,
            "worktreePath": str(worktree),
        }
        if any(saved.get(key) != value for key, value in expected.items()):
            fail(f"existing prep marker {marker} does not match this task")
        if not worktree.is_dir():
            fail(f"prepared worktree {worktree} is missing")
        return {
            "taskId": identity["taskId"],
            "baseRev": required_string(saved.get("baseRev"), "prep marker baseRev"),
            "branch": branch,
            "worktreePath": str(worktree),
        }

    git(checkout, "fetch", "--prune", remote, base_branch)
    base_ref = f"{remote}/{base_branch}"
    base_rev = git(checkout, "rev-parse", "--verify", f"{base_ref}^{{commit}}").stdout.strip()
    if git(checkout, "show-ref", "--verify", "--quiet", f"refs/heads/{branch}", check=False).returncode == 0:
        fail(f"branch {branch!r} exists without its prep marker")
    if worktree.exists():
        fail(f"worktree path already exists without its prep marker: {worktree}")
    worktree.parent.mkdir(parents=True, exist_ok=True)
    git(checkout, "worktree", "add", "--detach", str(worktree), base_rev)
    git(worktree, "switch", "-c", branch)
    saved = {
        "campaign": identity["campaign"],
        "repository": identity["repository"],
        "runId": identity["runId"],
        "taskId": identity["taskId"],
        "baseRev": base_rev,
        "branch": branch,
        "worktreePath": str(worktree),
    }
    write_atomic(marker, saved)
    return {
        "taskId": identity["taskId"],
        "baseRev": base_rev,
        "branch": branch,
        "worktreePath": str(worktree),
    }


def publication_identity(brief: dict[str, Any], action: str) -> tuple[dict[str, Any], dict[str, Any], Path]:
    allowed = {
        "campaign",
        "repository",
        "repositoryConfig",
        "issue",
        "runId",
        "workspaceRoot",
        "task",
        "workspace",
    }
    if action == "merge":
        allowed.add("publication")
    data = object_exact(brief, allowed, f"{action} brief")
    config = repo_config(data.get("repositoryConfig"))
    workspace = object_exact(
        data.get("workspace"), {"taskId", "baseRev", "branch", "worktreePath"}, "workspace"
    )
    worktree = Path(required_string(workspace.get("worktreePath"), "workspace.worktreePath"))
    if not worktree.is_absolute():
        fail("workspace.worktreePath must be absolute")
    if action == "publish" and not worktree.is_dir():
        fail("workspace.worktreePath must be an existing directory for publication")
    if worktree.exists():
        if not worktree.is_dir():
            fail("workspace.worktreePath exists but is not a directory")
        git(worktree, "rev-parse", "--git-dir")
    return data, config, worktree


def github_pull_request(data: dict[str, Any], config: dict[str, Any], worktree: Path, head: str) -> str:
    repository = required_string(data.get("repository"), "repository")
    workspace = data["workspace"]
    branch = workspace["branch"]
    existing = run(
        [
            "gh",
            "pr",
            "list",
            "--repo",
            repository,
            "--head",
            branch,
            "--state",
            "all",
            "--json",
            "url,headRefName,state",
        ]
    )
    candidates = json.loads(existing.stdout)
    if candidates:
        return required_string(candidates[0].get("url"), "existing pull request URL")
    task = data["task"]
    issue = data["issue"]
    body = (
        f"Spec-build campaign progress for {repository}#{issue['number']}.\n\n"
        f"Task `{task['id']}`: {task['title']}\n\n"
        f"Witnessed gates are the merge criterion. Campaign issue: {issue['url']}\n"
        f"Head: `{head}`"
    )
    created = run(
        [
            "gh",
            "pr",
            "create",
            "--repo",
            repository,
            "--base",
            config["baseBranch"],
            "--head",
            branch,
            "--title",
            f"{task['id']}: {task['title']}",
            "--body",
            body,
        ],
        cwd=worktree,
    )
    url = created.stdout.strip().splitlines()[-1] if created.stdout.strip() else ""
    return required_string(url, "created pull request URL")


def action_publish(brief: dict[str, Any]) -> dict[str, Any]:
    data, config, worktree = publication_identity(brief, "publish")
    workspace = data["workspace"]
    task_id = required_string(workspace.get("taskId"), "workspace.taskId")
    branch = required_string(workspace.get("branch"), "workspace.branch")
    base_rev = required_string(workspace.get("baseRev"), "workspace.baseRev")
    actual_branch = git(worktree, "branch", "--show-current").stdout.strip()
    if actual_branch != branch:
        fail(f"worktree is on branch {actual_branch!r}, expected {branch!r}")
    status = git(worktree, "status", "--porcelain").stdout
    if status:
        fail("agent left uncommitted changes; commit the task before publication")
    head = git(worktree, "rev-parse", "HEAD").stdout.strip()
    if head == base_rev:
        fail("agent produced no commit relative to the prepared base")
    if git(worktree, "merge-base", "--is-ancestor", base_rev, head, check=False).returncode != 0:
        fail("task head is not descended from its prepared base revision")
    git(worktree, "push", "--set-upstream", config["remote"], f"HEAD:refs/heads/{branch}")
    if config["forge"] == "github":
        pull_request = github_pull_request(data, config, worktree, head)
    else:
        pull_request = f"local://{data['repository']}/{branch}"
    return {
        "taskId": task_id,
        "branch": branch,
        "head": head,
        "pullRequest": pull_request,
    }


def cleanup_worktree(checkout: Path, worktree: Path, branch: str) -> None:
    git(checkout, "worktree", "remove", "--force", str(worktree), check=False)
    git(checkout, "branch", "-D", branch, check=False)


def merge_local(
    data: dict[str, Any], config: dict[str, Any], worktree: Path, publication: dict[str, Any]
) -> str:
    checkout: Path = config["checkout"]
    remote_url = git(checkout, "remote", "get-url", config["remote"]).stdout.strip()
    workspace_root = Path(data["workspaceRoot"])
    workspace_root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="merge-", dir=workspace_root) as temporary:
        integration = Path(temporary) / "repository"
        run(["git", "clone", "--quiet", remote_url, str(integration)])
        git(integration, "config", "user.name", "tally spec-build")
        git(integration, "config", "user.email", "tally-spec-build@invalid")
        git(integration, "fetch", "origin", config["baseBranch"], publication["branch"])
        git(integration, "switch", "-C", config["baseBranch"], f"origin/{config['baseBranch']}")
        git(
            integration,
            "merge",
            "--no-ff",
            "--no-edit",
            f"origin/{publication['branch']}",
        )
        merge_commit = git(integration, "rev-parse", "HEAD").stdout.strip()
        git(integration, "push", "origin", f"HEAD:refs/heads/{config['baseBranch']}")
    cleanup_worktree(checkout, worktree, publication["branch"])
    return merge_commit


def merge_github(
    data: dict[str, Any], config: dict[str, Any], worktree: Path, publication: dict[str, Any]
) -> str:
    repository = required_string(data.get("repository"), "repository")
    url = required_string(publication.get("pullRequest"), "publication.pullRequest")
    viewed = run(["gh", "pr", "view", url, "--repo", repository, "--json", "state,mergeCommit"])
    state = json.loads(viewed.stdout)
    if state.get("state") != "MERGED":
        run(["gh", "pr", "merge", url, "--repo", repository, "--merge"])
        viewed = run(
            ["gh", "pr", "view", url, "--repo", repository, "--json", "state,mergeCommit"]
        )
        state = json.loads(viewed.stdout)
    if state.get("state") != "MERGED":
        fail(f"pull request {url} did not reach MERGED")
    merge_commit = (state.get("mergeCommit") or {}).get("oid")
    merge_commit = required_string(merge_commit, "pull request merge commit")
    checkout: Path = config["checkout"]
    git(checkout, "fetch", "--prune", config["remote"], config["baseBranch"])
    if (
        git(
            checkout,
            "merge-base",
            "--is-ancestor",
            publication["head"],
            f"{config['remote']}/{config['baseBranch']}",
            check=False,
        ).returncode
        != 0
    ):
        fail("current remote base does not contain the merged task head")
    github_progress_comment(data, publication, merge_commit)
    cleanup_worktree(checkout, worktree, publication["branch"])
    return merge_commit


def github_progress_comment(
    data: dict[str, Any], publication: dict[str, Any], merge_commit: str
) -> None:
    repository = required_string(data.get("repository"), "repository")
    campaign = required_string(data.get("campaign"), "campaign")
    run_id = required_string(data.get("runId"), "runId", 512)
    issue = object_exact(data.get("issue"), {"number", "url"}, "issue")
    issue_number = required_string(issue.get("number"), "issue.number")
    task = data.get("task")
    if not isinstance(task, dict):
        fail("task must be an object")
    task_id = required_string(task.get("id"), "task.id")
    task_title = required_string(task.get("title"), "task.title")
    run_hash = hashlib.sha256(run_id.encode()).hexdigest()[:16]
    marker = f"<!-- tally:spec-build:{campaign}:{run_hash}:{task_id}:merged -->"
    comments = run(
        [
            "gh",
            "api",
            "--paginate",
            f"repos/{repository}/issues/{issue_number}/comments?per_page=100",
            "--jq",
            ".[].body",
        ]
    ).stdout
    if marker in comments:
        return
    body = (
        f"{marker}\n"
        f"Campaign task `{task_id}` ({task_title}) merged via "
        f"{publication['pullRequest']}.\n\nMerge commit: `{merge_commit}`"
    )
    run(
        [
            "gh",
            "issue",
            "comment",
            issue_number,
            "--repo",
            repository,
            "--body",
            body,
        ]
    )


def action_merge(brief: dict[str, Any]) -> dict[str, Any]:
    data, config, worktree = publication_identity(brief, "merge")
    publication = object_exact(
        data.get("publication"), {"taskId", "branch", "head", "pullRequest"}, "publication"
    )
    task_id = required_string(publication.get("taskId"), "publication.taskId")
    branch = required_string(publication.get("branch"), "publication.branch")
    head = required_string(publication.get("head"), "publication.head")
    pull_request = required_string(publication.get("pullRequest"), "publication.pullRequest")
    if config["forge"] == "github":
        merge_commit = merge_github(data, config, worktree, publication)
    else:
        merge_commit = merge_local(data, config, worktree, publication)
    return {
        "taskId": task_id,
        "head": head,
        "mergeCommit": merge_commit,
        "pullRequest": pull_request,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("worklist", "prep", "publish", "merge"))
    arguments = parser.parse_args()
    brief = load_brief()
    actions = {
        "worklist": action_worklist,
        "prep": action_prep,
        "publish": action_publish,
        "merge": action_merge,
    }
    emit(actions[arguments.action](brief))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except DriverError as error:
        print(f"spec-build-driver: {error}", file=sys.stderr)
        raise SystemExit(1)
