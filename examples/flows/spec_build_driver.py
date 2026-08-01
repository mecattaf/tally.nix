#!/usr/bin/env python3
"""Deterministic policy driver for the shipped spec-build tally flow."""

from __future__ import annotations

import argparse
import fnmatch
import glob
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
from typing import Any


TASK_ID = re.compile(r"^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$")
COMPONENT = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_.-]*$")
REPOSITORY = re.compile(r"^[^/ \t]+/[^/ \t]+$")
GIT_OID = re.compile(r"^[0-9a-f]{40,64}$")


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


def positive_integer(value: Any, context: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 1:
        fail(f"{context} must be a positive integer")
    return value


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


def normalize_conflict_domains(value: Any, context: str, *, required: bool) -> list[str]:
    if value is None and not required:
        return []
    domains = string_list(value, context, nonempty=required)
    if len(domains) != len(set(domains)):
        fail(f"{context} contains duplicates")
    normalized: list[str] = []
    for index, domain in enumerate(domains):
        path = Path(domain)
        if path.is_absolute() or ".." in path.parts or domain.endswith("/"):
            fail(f"{context}[{index}] must be a normalized relative path without '..'")
        rendered = path.as_posix()
        if rendered in {"", "."} or rendered != domain:
            fail(f"{context}[{index}] must be a normalized relative path")
        normalized.append(rendered)
    return normalized


def normalize_dependencies(value: Any, context: str, prior_ids: set[str]) -> list[str]:
    dependencies = string_list(value, f"{context}.dependencies")
    if len(dependencies) != len(set(dependencies)):
        fail(f"{context}.dependencies contains duplicates")
    missing = [dependency for dependency in dependencies if dependency not in prior_ids]
    if missing:
        fail(
            f"{context}.dependencies must reference earlier tasks; unavailable: {', '.join(missing)}"
        )
    return dependencies


def normalize_task(
    value: Any, index: int, prior_ids: set[str], *, require_conflict_domains: bool
) -> dict[str, Any]:
    context = f"tasks[{index}]"
    if not isinstance(value, dict):
        fail(f"{context} must be an object")
    kind = value.get("kind")
    if kind == "checkpoint":
        task = object_exact(
            value,
            {"id", "kind", "title", "argv", "runtimeMaxSec", "dependencies"},
            context,
        )
        identifier = required_string(task.get("id"), f"{context}.id", 80)
        if not TASK_ID.fullmatch(identifier):
            fail(f"{context}.id must match {TASK_ID.pattern}")
        return {
            "id": identifier,
            "kind": "checkpoint",
            "title": required_string(task.get("title"), f"{context}.title", 300),
            "argv": argv(task.get("argv"), f"{context}.argv"),
            "runtimeMaxSec": positive_integer(
                task.get("runtimeMaxSec"), f"{context}.runtimeMaxSec"
            ),
            "dependencies": normalize_dependencies(task.get("dependencies"), context, prior_ids),
        }
    if kind != "implementation":
        fail(f"{context}.kind must equal implementation or checkpoint")
    task = object_exact(
        value,
        {
            "id",
            "kind",
            "title",
            "goal",
            "deliveredBehaviors",
            "readFirst",
            "acceptanceCriteria",
            "dependencies",
            "conflictDomains",
        },
        context,
    )
    identifier = required_string(task.get("id"), f"{context}.id", 80)
    if not TASK_ID.fullmatch(identifier):
        fail(f"{context}.id must match {TASK_ID.pattern}")
    read_first = object_exact(
        task.get("readFirst"), {"specSections", "styleReferences"}, f"{context}.readFirst"
    )
    return {
        "id": identifier,
        "kind": "implementation",
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
        "dependencies": normalize_dependencies(task.get("dependencies"), context, prior_ids),
        "conflictDomains": normalize_conflict_domains(
            task.get("conflictDomains"),
            f"{context}.conflictDomains",
            required=require_conflict_domains,
        ),
    }


def action_worklist(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief,
        {"repository", "repositoryConfig", "worklist", "maxTasks", "maxParallel"},
        "worklist brief",
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
    max_parallel = data.get("maxParallel", 1)
    if (
        not isinstance(max_parallel, int)
        or isinstance(max_parallel, bool)
        or not 1 <= max_parallel <= 128
    ):
        fail("maxParallel must be an integer from 1 through 128")
    if max_parallel > max_tasks:
        fail("maxParallel must not exceed maxTasks")

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
        task = normalize_task(
            candidate,
            index,
            prior_ids,
            require_conflict_domains=max_parallel > 1,
        )
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


def campaign_issue(value: Any) -> dict[str, str]:
    issue = object_exact(value, {"number", "url"}, "issue")
    number = required_string(issue.get("number"), "issue.number")
    if not number.isdigit() or number.startswith("0"):
        fail("issue.number must be a positive decimal string")
    url = required_string(issue.get("url"), "issue.url")
    return {"number": number, "url": url}


def pull_request_marker(campaign: str, issue_number: str, task_id: str) -> str:
    return (
        "<!-- tally:spec-build:v1 "
        f"campaign={campaign} issue={issue_number} task={task_id} -->"
    )


def checkpoint_ref(
    campaign: str, issue_number: str, task_id: str, source_sha256: str
) -> str:
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", source_sha256):
        fail("worklist source digest is not a lowercase SHA-256 identity")
    digest = source_sha256.removeprefix("sha256:")
    readable = re.sub(r"[^a-z0-9]+", "-", campaign.casefold()).strip("-")
    readable = (readable or "campaign")[:24].rstrip("-") or "campaign"
    campaign_identity = hashlib.sha256(campaign.encode()).hexdigest()[:12]
    return (
        "refs/tags/tally/spec-build/v1/"
        f"{readable}-{campaign_identity}-issue-{issue_number}/{task_id}-{digest}"
    )


def remote_ref_oid(checkout: Path, remote: str, reference: str) -> str | None:
    listed = git(checkout, "ls-remote", "--refs", remote, reference)
    lines = [line for line in listed.stdout.splitlines() if line]
    if not lines:
        return None
    if len(lines) != 1:
        fail(f"remote ref lookup for {reference!r} returned {len(lines)} rows")
    fields = lines[0].split("\t")
    if len(fields) != 2 or fields[1] != reference or not re.fullmatch(
        r"[0-9a-f]{40,64}", fields[0]
    ):
        fail(f"remote ref lookup for {reference!r} returned malformed output")
    return fields[0]


def merged_github_tasks(
    repository: str,
    campaign: str,
    issue_number: str,
    base_branch: str,
    tasks: list[dict[str, Any]],
) -> tuple[list[dict[str, str]], list[str]]:
    fields = "url,body,baseRefName,headRefName,mergeCommit"
    viewed = run(
        [
            "gh",
            "pr",
            "list",
            "--repo",
            repository,
            "--state",
            "merged",
            "--limit",
            "1000",
            "--json",
            fields,
        ]
    )
    try:
        candidates = json.loads(viewed.stdout)
    except json.JSONDecodeError as error:
        fail(f"gh pr list returned invalid JSON: {error}")
    if not isinstance(candidates, list):
        fail("gh pr list must return an array")
    facts: list[dict[str, str]] = []
    warnings: list[str] = []
    claimed_urls: set[str] = set()
    markers = {
        task["id"]: pull_request_marker(campaign, issue_number, task["id"])
        for task in tasks
        if task["kind"] == "implementation"
    }
    campaign_marker_prefix = (
        "<!-- tally:spec-build:v1 "
        f"campaign={campaign} issue={issue_number} task="
    )
    for candidate in candidates:
        if not isinstance(candidate, dict) or not isinstance(candidate.get("body"), str):
            continue
        body = candidate["body"]
        if campaign_marker_prefix in body and not any(marker in body for marker in markers.values()):
            url = candidate.get("url")
            identity = url if isinstance(url, str) and url else "an unidentifiable pull request"
            warnings.append(
                f"ignored {identity}: its campaign marker names no task in the witnessed worklist"
            )

    def candidate_key(candidate: dict[str, Any]) -> str:
        url = candidate.get("url")
        if isinstance(url, str) and url:
            return f"url:{url}"
        return "json:" + json.dumps(candidate, sort_keys=True, default=str)

    def validated_candidate(
        candidate: dict[str, Any], task_id: str, branch: str
    ) -> tuple[dict[str, str] | None, str | None]:
        url_value = candidate.get("url")
        url = url_value if isinstance(url_value, str) and url_value else "unidentifiable PR"
        problems: list[str] = []
        if not isinstance(url_value, str) or not url_value or any(ord(char) < 32 for char in url_value):
            problems.append("has no valid URL")
        if candidate.get("baseRefName") != base_branch:
            problems.append(
                f"targets {candidate.get('baseRefName')!r}, not base {base_branch!r}"
            )
        if candidate.get("headRefName") != branch:
            problems.append(
                f"uses head {candidate.get('headRefName')!r}, not stable branch {branch!r}"
            )
        commit = candidate.get("mergeCommit")
        oid = commit.get("oid") if isinstance(commit, dict) else None
        if not isinstance(oid, str) or not GIT_OID.fullmatch(oid):
            problems.append("has no valid merge commit")
        if problems:
            return None, f"ignored {url} for task {task_id!r}: {'; '.join(problems)}"
        return {
            "taskId": task_id,
            "pullRequest": url_value,
            "mergeCommit": oid,
        }, None

    for task in tasks:
        if task["kind"] != "implementation":
            continue
        marker = markers[task["id"]]
        branch = stable_publish_branch(campaign, issue_number, task["id"])
        matching = [
            candidate
            for candidate in candidates
            if isinstance(candidate, dict)
            and isinstance(candidate.get("body"), str)
            and marker in candidate["body"]
        ]

        valid: list[dict[str, str]] = []
        seen_candidates: set[str] = set()
        for candidate in matching:
            key = candidate_key(candidate)
            if key in seen_candidates:
                continue
            seen_candidates.add(key)
            fact, warning = validated_candidate(candidate, task["id"], branch)
            if warning is not None:
                warnings.append(warning)
            if fact is not None:
                valid.append(fact)

        # The stable head ref remains a durable lookup key even after this
        # campaign's PR ages out of the forge-wide recent-PR window. Query it
        # when the broad list supplied no usable proof, including when a quoted
        # or retargeted marker was ignored above.
        if not valid:
            by_branch = run(
                [
                    "gh",
                    "pr",
                    "list",
                    "--repo",
                    repository,
                    "--head",
                    branch,
                    "--state",
                    "merged",
                    "--limit",
                    "2",
                    "--json",
                    fields,
                ]
            )
            try:
                branch_candidates = json.loads(by_branch.stdout)
            except json.JSONDecodeError as error:
                fail(f"gh pr list for {branch!r} returned invalid JSON: {error}")
            if not isinstance(branch_candidates, list):
                fail(f"gh pr list for {branch!r} must return an array")
            branch_matching = [
                candidate
                for candidate in branch_candidates
                if isinstance(candidate, dict)
                and isinstance(candidate.get("body"), str)
                and marker in candidate["body"]
            ]
            for candidate in branch_matching:
                key = candidate_key(candidate)
                if key in seen_candidates:
                    continue
                seen_candidates.add(key)
                fact, warning = validated_candidate(candidate, task["id"], branch)
                if warning is not None:
                    warnings.append(warning)
                if fact is not None:
                    valid.append(fact)
        if len(valid) > 1:
            fail(f"multiple merged pull requests claim campaign task {task['id']!r}")
        if not valid:
            continue
        fact = valid[0]
        url = fact["pullRequest"]
        if url in claimed_urls:
            fail(f"merged pull request {url} claims more than one campaign task")
        claimed_urls.add(url)
        facts.append(fact)
    return facts, list(dict.fromkeys(warnings))


def stable_publish_branch(campaign: str, issue_number: str, task_id: str) -> str:
    campaign_slug = safe_slug(campaign, 32)
    return f"tally/{campaign_slug}-issue-{issue_number}/{task_id}"


def merged_local_tasks(
    repository: str,
    config: dict[str, Any],
    campaign: str,
    issue_number: str,
    tasks: list[dict[str, Any]],
) -> list[dict[str, str]]:
    checkout: Path = config["checkout"]
    remote = config["remote"]
    base_branch = config["baseBranch"]
    git(checkout, "fetch", "--prune", remote)
    base_ref = f"{remote}/{base_branch}"
    base_rev = git(checkout, "rev-parse", "--verify", f"{base_ref}^{{commit}}").stdout.strip()
    facts: list[dict[str, str]] = []
    for task in tasks:
        if task["kind"] != "implementation":
            continue
        branch = stable_publish_branch(campaign, issue_number, task["id"])
        remote_ref = f"refs/remotes/{remote}/{branch}"
        if git(checkout, "show-ref", "--verify", "--quiet", remote_ref, check=False).returncode:
            continue
        head = git(checkout, "rev-parse", "--verify", f"{remote_ref}^{{commit}}").stdout.strip()
        if git(checkout, "merge-base", "--is-ancestor", head, base_rev, check=False).returncode:
            continue
        facts.append(
            {
                "taskId": task["id"],
                "pullRequest": f"local://{repository}/{branch}",
                "mergeCommit": base_rev,
            }
        )
    return facts


def completed_checkpoint_tasks(
    config: dict[str, Any],
    campaign: str,
    issue_number: str,
    tasks: list[dict[str, Any]],
    source: dict[str, str],
    merged_ids: set[str],
) -> list[dict[str, str]]:
    checkpoints = [task for task in tasks if task["kind"] == "checkpoint"]
    if not checkpoints:
        return []
    checkout: Path = config["checkout"]
    remote = config["remote"]
    git(checkout, "fetch", "--prune", "--no-tags", remote)
    base_ref = f"{remote}/{config['baseBranch']}"
    base_rev = git(checkout, "rev-parse", "--verify", f"{base_ref}^{{commit}}").stdout.strip()
    facts: list[dict[str, str]] = []
    completed_ids = set(merged_ids)
    for task in checkpoints:
        if not all(dependency in completed_ids for dependency in task["dependencies"]):
            continue
        reference = checkpoint_ref(
            campaign, issue_number, task["id"], source["sha256"]
        )
        target = remote_ref_oid(checkout, remote, reference)
        if target is None:
            continue
        git(checkout, "fetch", "--no-tags", remote, reference)
        fetched = git(checkout, "rev-parse", "--verify", "FETCH_HEAD^{commit}").stdout.strip()
        if fetched != target or git(checkout, "cat-file", "-t", target).stdout.strip() != "commit":
            fail(f"checkpoint ref {reference!r} must point directly to a commit")
        if git(checkout, "merge-base", "--is-ancestor", target, base_rev, check=False).returncode:
            continue
        facts.append({"taskId": task["id"], "ref": reference, "revision": target})
        completed_ids.add(task["id"])
    return facts


def domains_overlap(left: str, right: str) -> bool:
    left_parts = Path(left).parts
    right_parts = Path(right).parts
    width = min(len(left_parts), len(right_parts))
    return left_parts[:width] == right_parts[:width]


def task_conflicts(task: dict[str, Any], selected: list[dict[str, Any]]) -> bool:
    return any(
        domains_overlap(left, right)
        for other in selected
        for left in task.get("conflictDomains", [])
        for right in other.get("conflictDomains", [])
    )


def action_reconcile(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief,
        {
            "campaign",
            "repository",
            "repositoryConfig",
            "issue",
            "worklist",
            "maxTasks",
            "maxParallel",
        },
        "reconcile brief",
    )
    campaign = required_string(data.get("campaign"), "campaign")
    if not COMPONENT.fullmatch(campaign):
        fail("campaign is not a safe component")
    issue = campaign_issue(data.get("issue"))
    worklist = action_worklist(
        {
            "repository": data.get("repository"),
            "repositoryConfig": data.get("repositoryConfig"),
            "worklist": data.get("worklist"),
            "maxTasks": data.get("maxTasks"),
            "maxParallel": data.get("maxParallel"),
        }
    )
    repository = worklist["repository"]
    config = repo_config(data.get("repositoryConfig"))
    if config["forge"] == "github":
        merged, warnings = merged_github_tasks(
            repository,
            campaign,
            issue["number"],
            config["baseBranch"],
            worklist["tasks"],
        )
    else:
        merged = merged_local_tasks(
            repository,
            config,
            campaign,
            issue["number"],
            worklist["tasks"],
        )
        warnings = []
    checkpoints = completed_checkpoint_tasks(
        config,
        campaign,
        issue["number"],
        worklist["tasks"],
        worklist["source"],
        {fact["taskId"] for fact in merged},
    )
    completed_ids = {fact["taskId"] for fact in merged + checkpoints}
    remaining = [task for task in worklist["tasks"] if task["id"] not in completed_ids]
    ready = [
        task
        for task in remaining
        if all(dependency in completed_ids for dependency in task["dependencies"])
    ]
    max_parallel = data["maxParallel"]
    frontier: list[dict[str, Any]] = []
    for task in ready:
        if len(frontier) == max_parallel:
            break
        if not task_conflicts(task, frontier):
            frontier.append(task)
    return {
        "schemaVersion": 1,
        "repository": repository,
        "source": worklist["source"],
        "tasks": worklist["tasks"],
        "merged": merged,
        "checkpoints": checkpoints,
        "remaining": [task["id"] for task in remaining],
        "frontier": frontier,
        "complete": not remaining,
        "warnings": warnings,
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
    task_kind = task.get("kind")
    if task_kind not in {"implementation", "checkpoint"}:
        fail("task.kind must equal implementation or checkpoint")
    campaign = required_string(data.get("campaign"), "campaign")
    repository = required_string(data.get("repository"), "repository")
    issue = campaign_issue(data.get("issue"))
    run_id = required_string(data.get("runId"), "runId", 512)
    workspace_root = Path(required_string(data.get("workspaceRoot"), "workspaceRoot"))
    if not workspace_root.is_absolute():
        fail("workspaceRoot must be absolute")
    config = repo_config(data.get("repositoryConfig"))
    identity = {
        "campaign": campaign,
        "repository": repository,
        "issueNumber": issue["number"],
        "runId": run_id,
        "taskId": task_id,
        "taskKind": task_kind,
        "workspaceRoot": workspace_root,
    }
    return data, config, identity


def marker_path(identity: dict[str, Any]) -> Path:
    run_hash = hashlib.sha256(identity["runId"].encode()).hexdigest()[:16]
    return identity["workspaceRoot"] / ".state" / run_hash / f"{identity['taskId']}.json"


def prune_empty_ancestors(path: Path, stop: Path) -> None:
    current = path
    while current != stop:
        try:
            current.rmdir()
        except OSError:
            return
        current = current.parent


def remove_prep_markers(
    workspace_root: Path,
    campaign: str,
    repository: str,
    run_id: str,
    task_id: str,
    worktree: Path,
) -> None:
    state_root = workspace_root / ".state"
    if not state_root.is_dir():
        return
    for marker in state_root.glob("*/*.json"):
        try:
            saved = json.loads(marker.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if not isinstance(saved, dict):
            continue
        if (
            saved.get("campaign") == campaign
            and saved.get("repository") == repository
            and saved.get("runId") == run_id
            and saved.get("taskId") == task_id
            and saved.get("worktreePath") == str(worktree)
        ):
            marker.unlink(missing_ok=True)
            prune_empty_ancestors(marker.parent, state_root)


def parse_worktrees(checkout: Path) -> list[dict[str, str]]:
    records: list[dict[str, str]] = []
    current: dict[str, str] = {}
    for line in git(checkout, "worktree", "list", "--porcelain").stdout.splitlines():
        if not line:
            if current:
                records.append(current)
                current = {}
            continue
        key, _, value = line.partition(" ")
        current[key] = value
    if current:
        records.append(current)
    return records


def action_sweep(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief,
        {"campaign", "repository", "repositoryConfig", "runId", "workspaceRoot"},
        "sweep brief",
    )
    campaign = required_string(data.get("campaign"), "campaign")
    if not COMPONENT.fullmatch(campaign):
        fail("campaign is not a safe component")
    repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(repository):
        fail("repository must use owner/name form")
    run_id = required_string(data.get("runId"), "runId", 512)
    workspace_root = Path(required_string(data.get("workspaceRoot"), "workspaceRoot"))
    if not workspace_root.is_absolute():
        fail("workspaceRoot must be absolute")
    config = repo_config(data.get("repositoryConfig"))
    checkout: Path = config["checkout"]
    campaign_slug = safe_slug(campaign, 24)
    repository_slug = safe_slug(repository.split("/", 1)[1], 40)
    repository_root = (workspace_root / repository_slug).resolve()
    current_hash = hashlib.sha256(run_id.encode()).hexdigest()[:12]
    branch_pattern = re.compile(
        rf"^tally-work/{re.escape(campaign_slug)}-([0-9a-f]{{12}})/"
        r"(_campaign-preflight|[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)$"
    )
    cleaned: list[str] = []
    warnings: list[str] = []

    for record in parse_worktrees(checkout):
        raw_path = record.get("worktree")
        if not raw_path:
            continue
        worktree = Path(raw_path).resolve()
        try:
            relative = worktree.relative_to(repository_root)
        except ValueError:
            continue
        if (
            len(relative.parts) != 2
            or not re.fullmatch(r"[0-9a-f]{12}", relative.parts[0])
            or (
                relative.parts[1] != "_campaign-preflight"
                and not TASK_ID.fullmatch(relative.parts[1])
            )
        ):
            warnings.append(f"left unexpected campaign worktree path untouched: {worktree}")
            continue
        lane_hash = relative.parts[0]
        if lane_hash == current_hash:
            continue
        branch_ref = record.get("branch", "")
        branch = branch_ref.removeprefix("refs/heads/")
        matched_branch = branch_pattern.fullmatch(branch) if branch else None
        if branch and (
            matched_branch is None
            or matched_branch.group(1) != lane_hash
            or matched_branch.group(2) != relative.parts[1]
        ):
            warnings.append(f"left campaign worktree with unexpected branch untouched: {worktree}")
            continue
        removed = git(checkout, "worktree", "remove", "--force", str(worktree), check=False)
        if removed.returncode != 0 and worktree.exists():
            detail = removed.stderr.strip() or removed.stdout.strip() or "no output"
            warnings.append(f"could not sweep worktree {worktree}: {detail}")
            continue
        if branch:
            git(checkout, "branch", "-D", branch, check=False)
        cleaned.append(f"worktree:{worktree}")

    git(checkout, "worktree", "prune", check=False)
    listed = git(
        checkout,
        "for-each-ref",
        "--format=%(refname:short)",
        f"refs/heads/tally-work/{campaign_slug}-",
    ).stdout.splitlines()
    for branch in listed:
        matched = branch_pattern.fullmatch(branch)
        if matched is None or matched.group(1) == current_hash:
            continue
        deleted = git(checkout, "branch", "-D", branch, check=False)
        if deleted.returncode == 0:
            cleaned.append(f"branch:{branch}")
        else:
            detail = deleted.stderr.strip() or deleted.stdout.strip() or "no output"
            warnings.append(f"could not sweep branch {branch!r}: {detail}")

    state_root = workspace_root / ".state"
    if state_root.is_dir():
        for marker in state_root.glob("*/*.json"):
            try:
                saved = json.loads(marker.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError) as error:
                warnings.append(f"left unreadable campaign state marker untouched: {marker}: {error}")
                continue
            if not isinstance(saved, dict):
                warnings.append(f"left non-object campaign state marker untouched: {marker}")
                continue
            if saved.get("campaign") != campaign or saved.get("repository") != repository:
                continue
            saved_run_id = saved.get("runId")
            if saved_run_id == run_id:
                continue
            if not isinstance(saved_run_id, str) or not saved_run_id:
                warnings.append(f"left identity-less campaign state marker untouched: {marker}")
                continue
            saved_hash = hashlib.sha256(saved_run_id.encode()).hexdigest()[:12]
            saved_worktree_value = saved.get("worktreePath")
            saved_branch = saved.get("branch")
            if not isinstance(saved_worktree_value, str) or not isinstance(saved_branch, str):
                warnings.append(f"left incomplete campaign state marker untouched: {marker}")
                continue
            saved_worktree = Path(saved_worktree_value).resolve()
            try:
                relative = saved_worktree.relative_to(repository_root)
            except ValueError:
                warnings.append(f"left out-of-root campaign state marker untouched: {marker}")
                continue
            if len(relative.parts) != 2 or relative.parts[0] != saved_hash:
                warnings.append(f"left mismatched campaign state marker untouched: {marker}")
                continue
            saved_task_id = saved.get("taskId")
            saved_lane = (
                "_campaign-preflight"
                if saved_task_id == "campaign-preflight"
                else saved_task_id
            )
            if not isinstance(saved_lane, str) or relative.parts[1] != saved_lane:
                warnings.append(f"left mismatched campaign task marker untouched: {marker}")
                continue
            matched = branch_pattern.fullmatch(saved_branch)
            if (
                matched is None
                or matched.group(1) != saved_hash
                or matched.group(2) != relative.parts[1]
            ):
                warnings.append(f"left mismatched campaign branch marker untouched: {marker}")
                continue
            if saved_worktree.exists():
                if not saved_worktree.is_dir():
                    warnings.append(
                        f"left non-directory campaign worktree path untouched: {saved_worktree}"
                    )
                    continue
                try:
                    shutil.rmtree(saved_worktree)
                except OSError as error:
                    warnings.append(
                        f"could not sweep unregistered campaign worktree "
                        f"{saved_worktree}: {error}"
                    )
                    continue
                cleaned.append(f"worktree:{saved_worktree}")
            git(checkout, "branch", "-D", saved_branch, check=False)
            marker.unlink(missing_ok=True)
            prune_empty_ancestors(marker.parent, state_root)
            cleaned.append(f"marker:{marker}")

    if repository_root.is_dir():
        for child in repository_root.iterdir():
            if child.is_dir():
                prune_empty_ancestors(child, repository_root)
    return {
        "currentRunHash": current_hash,
        "cleaned": sorted(set(cleaned)),
        "warnings": list(dict.fromkeys(warnings)),
    }


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
    branch = f"tally-work/{campaign_slug}-{run_hash}/{identity['taskId']}"
    publish_branch = stable_publish_branch(
        identity["campaign"], identity["issueNumber"], identity["taskId"]
    )
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
            "taskKind": identity["taskKind"],
            "branch": branch,
            "publishBranch": publish_branch,
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
            "publishBranch": publish_branch,
            "worktreePath": str(worktree),
        }

    git(checkout, "fetch", "--prune", remote)
    base_ref = f"{remote}/{base_branch}"
    base_rev = git(checkout, "rev-parse", "--verify", f"{base_ref}^{{commit}}").stdout.strip()
    publish_ref = f"refs/remotes/{remote}/{publish_branch}"
    published = identity["taskKind"] == "implementation" and git(
        checkout,
        "show-ref",
        "--verify",
        "--quiet",
        publish_ref,
        check=False,
    ).returncode == 0
    if published:
        start_rev = git(
            checkout, "rev-parse", "--verify", f"{publish_ref}^{{commit}}"
        ).stdout.strip()
        base_rev = git(checkout, "merge-base", start_rev, base_rev).stdout.strip()
    else:
        start_rev = base_rev
    if git(checkout, "show-ref", "--verify", "--quiet", f"refs/heads/{branch}", check=False).returncode == 0:
        fail(f"branch {branch!r} exists without its prep marker")
    if worktree.exists():
        fail(f"worktree path already exists without its prep marker: {worktree}")
    worktree.parent.mkdir(parents=True, exist_ok=True)
    git(checkout, "worktree", "add", "--detach", str(worktree), start_rev)
    git(worktree, "switch", "-c", branch)
    saved = {
        "campaign": identity["campaign"],
        "repository": identity["repository"],
        "runId": identity["runId"],
        "taskId": identity["taskId"],
        "taskKind": identity["taskKind"],
        "baseRev": base_rev,
        "branch": branch,
        "publishBranch": publish_branch,
        "worktreePath": str(worktree),
    }
    write_atomic(marker, saved)
    return {
        "taskId": identity["taskId"],
        "baseRev": base_rev,
        "branch": branch,
        "publishBranch": publish_branch,
        "worktreePath": str(worktree),
    }


def action_preflight(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief,
        {
            "campaign",
            "repository",
            "repositoryConfig",
            "issue",
            "runId",
            "workspaceRoot",
        },
        "preflight brief",
    )
    campaign = required_string(data.get("campaign"), "campaign")
    if not COMPONENT.fullmatch(campaign):
        fail("campaign is not a safe component")
    repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(repository):
        fail("repository must use owner/name form")
    campaign_issue(data.get("issue"))
    run_id = required_string(data.get("runId"), "runId", 512)
    workspace_root = Path(required_string(data.get("workspaceRoot"), "workspaceRoot"))
    if not workspace_root.is_absolute():
        fail("workspaceRoot must be absolute")
    config = repo_config(data.get("repositoryConfig"))
    checkout: Path = config["checkout"]
    run_hash = hashlib.sha256(run_id.encode()).hexdigest()[:12]
    campaign_slug = safe_slug(campaign, 24)
    repository_slug = safe_slug(repository.split("/", 1)[1], 40)
    task_id = "campaign-preflight"
    branch = f"tally-work/{campaign_slug}-{run_hash}/_campaign-preflight"
    worktree = (
        workspace_root / repository_slug / run_hash / "_campaign-preflight"
    ).resolve()
    marker = workspace_root / ".state" / run_hash / "_campaign-preflight.json"

    if marker.exists():
        saved = json.loads(marker.read_text(encoding="utf-8"))
        expected = {
            "campaign": campaign,
            "repository": repository,
            "runId": run_id,
            "taskId": task_id,
            "branch": branch,
            "publishBranch": branch,
            "worktreePath": str(worktree),
        }
        if any(saved.get(key) != value for key, value in expected.items()):
            fail(f"existing preflight marker {marker} does not match this pass")
        if not worktree.is_dir():
            fail(f"prepared preflight worktree {worktree} is missing")
        return {
            "taskId": task_id,
            "baseRev": required_string(saved.get("baseRev"), "preflight marker baseRev"),
            "branch": branch,
            "publishBranch": branch,
            "worktreePath": str(worktree),
        }

    git(checkout, "fetch", "--prune", config["remote"])
    base_ref = f"{config['remote']}/{config['baseBranch']}"
    base_rev = git(checkout, "rev-parse", "--verify", f"{base_ref}^{{commit}}").stdout.strip()
    if git(
        checkout,
        "show-ref",
        "--verify",
        "--quiet",
        f"refs/heads/{branch}",
        check=False,
    ).returncode == 0:
        fail(f"preflight branch {branch!r} exists without its marker")
    if worktree.exists():
        fail(f"preflight worktree exists without its marker: {worktree}")
    worktree.parent.mkdir(parents=True, exist_ok=True)
    git(checkout, "worktree", "add", "--detach", str(worktree), base_rev)
    git(worktree, "switch", "-c", branch)
    saved = {
        "campaign": campaign,
        "repository": repository,
        "runId": run_id,
        "taskId": task_id,
        "baseRev": base_rev,
        "branch": branch,
        "publishBranch": branch,
        "worktreePath": str(worktree),
    }
    write_atomic(marker, saved)
    return {
        "taskId": task_id,
        "baseRev": base_rev,
        "branch": branch,
        "publishBranch": branch,
        "worktreePath": str(worktree),
    }


def path_glob_matches(path: str, pattern: str) -> bool:
    """Match one repository-relative path with the campaign glob contract."""
    path_parts = path.casefold().split("/")
    folded_pattern = pattern.casefold()
    pattern_parts = folded_pattern.split("/")
    if len(pattern_parts) == 1:
        return fnmatch.fnmatchcase(path_parts[-1], folded_pattern)

    memo: dict[tuple[int, int], bool] = {}

    def match(path_index: int, pattern_index: int) -> bool:
        key = (path_index, pattern_index)
        if key in memo:
            return memo[key]
        if pattern_index == len(pattern_parts):
            result = path_index == len(path_parts)
        elif pattern_parts[pattern_index] == "**":
            result = match(path_index, pattern_index + 1) or (
                path_index < len(path_parts) and match(path_index + 1, pattern_index)
            )
        else:
            result = (
                path_index < len(path_parts)
                and fnmatch.fnmatchcase(path_parts[path_index], pattern_parts[pattern_index])
                and match(path_index + 1, pattern_index + 1)
            )
        memo[key] = result
        return result

    return match(0, 0)


def normalize_forbid_paths_gate(value: Any, context: str) -> dict[str, Any]:
    gate = object_exact(value, {"kind", "id", "forbidPaths", "runtimeMaxSec"}, context)
    if gate.get("kind") != "forbidPaths":
        fail(f"{context}.kind must equal forbidPaths")
    gate_id = required_string(gate.get("id"), f"{context}.id", 80)
    if not COMPONENT.fullmatch(gate_id):
        fail(f"{context}.id is not a safe component")
    patterns = string_list(gate.get("forbidPaths"), f"{context}.forbidPaths", nonempty=True)
    if len(patterns) > 128:
        fail(f"{context}.forbidPaths exceeds 128 patterns")
    if len(patterns) != len(set(patterns)):
        fail(f"{context}.forbidPaths contains duplicates")
    for index, pattern in enumerate(patterns):
        components = pattern.split("/")
        if len(pattern) > 1024:
            fail(f"{context}.forbidPaths[{index}] exceeds 1024 characters")
        if (
            pattern.startswith("/")
            or ".." in components
            or any("**" in component and component != "**" for component in components)
        ):
            fail(
                f"{context}.forbidPaths[{index}] must be a repository-relative glob "
                "without '..' and may use '**' only as a complete path component"
            )
    positive_integer(gate.get("runtimeMaxSec"), f"{context}.runtimeMaxSec")
    return {"kind": "forbidPaths", "id": gate_id, "forbidPaths": patterns}


def changed_paths_in_history(worktree: Path, base_rev: str, head: str) -> list[str]:
    changed = git(
        worktree,
        "log",
        "-m",
        "--format=",
        "--name-only",
        "--no-renames",
        "--diff-filter=ACMRTUXB",
        "-z",
        f"{base_rev}..{head}",
        "--",
    ).stdout
    return sorted({path for path in changed.split("\0") if path})


def changed_paths_in_diff(worktree: Path, base_rev: str, head: str) -> list[str]:
    """Return every path affected by the committed base-to-head task diff."""
    changed = git(
        worktree,
        "diff",
        "--name-only",
        "--no-renames",
        "--diff-filter=ACDMRTUXB",
        "-z",
        base_rev,
        head,
        "--",
    ).stdout
    return sorted({path for path in changed.split("\0") if path})


def enforce_conflict_domains(
    worktree: Path,
    base_rev: str,
    head: str,
    task: Any,
    expected_task_id: str,
) -> int:
    if not isinstance(task, dict):
        fail("task must be an object")
    task_id = required_string(task.get("id"), "task.id")
    if not TASK_ID.fullmatch(task_id):
        fail("task.id is not safe")
    if task_id != expected_task_id:
        fail("task.id does not match workspace.taskId")
    domains = normalize_conflict_domains(
        task.get("conflictDomains"), "task.conflictDomains", required=False
    )
    # Serial campaigns may omit conflictDomains. A non-empty declaration is an
    # ownership contract, and parallel worklists are guaranteed to have one.
    if not domains:
        return 0

    changed_paths = changed_paths_in_diff(worktree, base_rev, head)
    outside = [
        path
        for path in changed_paths
        if not any(domains_overlap(path, domain) for domain in domains)
    ]
    if outside:
        preview = ", ".join(json.dumps(path) for path in outside[:20])
        if len(outside) > 20:
            preview += f", and {len(outside) - 20} more"
        declared = ", ".join(json.dumps(domain) for domain in domains)
        fail(
            f"task {task_id!r} changed {len(outside)} path(s) outside its declared "
            f"conflictDomains: {preview}; declared domains: {declared}"
        )
    return len(changed_paths)


def evaluate_forbid_paths(
    worktree: Path, base_rev: str, head: str, gate_id: str, patterns: list[str]
) -> int:
    changed_paths = changed_paths_in_history(worktree, base_rev, head)
    violations: list[tuple[str, list[str]]] = []
    for path in changed_paths:
        matched = [pattern for pattern in patterns if path_glob_matches(path, pattern)]
        if matched:
            violations.append((path, matched))
    if violations:
        preview = "; ".join(
            f"{json.dumps(path)} (matched {', '.join(json.dumps(pattern) for pattern in matched)})"
            for path, matched in violations[:20]
        )
        if len(violations) > 20:
            preview += f"; and {len(violations) - 20} more"
        fail(
            f"forbidPaths gate {gate_id!r} rejected {len(violations)} changed path(s): {preview}"
        )
    return len(changed_paths)


def action_constraint(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(brief, {"gate", "workspace"}, "constraint brief")
    gate = normalize_forbid_paths_gate(data.get("gate"), "constraint gate")
    gate_id = gate["id"]
    patterns = gate["forbidPaths"]

    workspace = object_exact(
        data.get("workspace"), {"taskId", "baseRev", "branch", "worktreePath"}, "workspace"
    )
    task_id = required_string(workspace.get("taskId"), "workspace.taskId")
    if not TASK_ID.fullmatch(task_id):
        fail("workspace.taskId is not safe")
    base_rev = required_string(workspace.get("baseRev"), "workspace.baseRev")
    if not re.fullmatch(r"[0-9a-f]{40,64}", base_rev):
        fail("workspace.baseRev must be a full Git object ID")
    worktree = Path(required_string(workspace.get("worktreePath"), "workspace.worktreePath"))
    if not worktree.is_absolute() or not worktree.is_dir():
        fail("workspace.worktreePath must be an absolute existing directory")
    git(worktree, "rev-parse", "--git-dir")
    git(worktree, "rev-parse", "--verify", f"{base_rev}^{{commit}}")
    head = git(worktree, "rev-parse", "--verify", "HEAD^{commit}").stdout.strip()
    if git(worktree, "merge-base", "--is-ancestor", base_rev, head, check=False).returncode != 0:
        fail("task head is not descended from its prepared base revision")

    checked_paths = evaluate_forbid_paths(worktree, base_rev, head, gate_id, patterns)
    return {
        "gateId": gate_id,
        "kind": "forbidPaths",
        "patterns": patterns,
        "checkedPaths": checked_paths,
        "baseRev": base_rev,
        "head": head,
    }


def normalize_constraint_results(value: Any, context: str) -> list[dict[str, Any]]:
    if not isinstance(value, list):
        fail(f"{context} must be an array")
    results: list[dict[str, Any]] = []
    seen: set[str] = set()
    for index, candidate in enumerate(value):
        receipt_context = f"{context}[{index}]"
        receipt = object_exact(
            candidate,
            {"gateId", "kind", "patterns", "checkedPaths", "baseRev", "head"},
            receipt_context,
        )
        gate = normalize_forbid_paths_gate(
            {
                "kind": receipt.get("kind"),
                "id": receipt.get("gateId"),
                "forbidPaths": receipt.get("patterns"),
                "runtimeMaxSec": 1,
            },
            receipt_context,
        )
        if gate["id"] in seen:
            fail(f"{context} repeats gateId {gate['id']!r}")
        seen.add(gate["id"])
        checked_paths = receipt.get("checkedPaths")
        if (
            not isinstance(checked_paths, int)
            or isinstance(checked_paths, bool)
            or checked_paths < 0
        ):
            fail(f"{receipt_context}.checkedPaths must be a non-negative integer")
        base_rev = required_string(receipt.get("baseRev"), f"{receipt_context}.baseRev")
        head = required_string(receipt.get("head"), f"{receipt_context}.head")
        if not re.fullmatch(r"[0-9a-f]{40,64}", base_rev):
            fail(f"{receipt_context}.baseRev must be a full Git object ID")
        if not re.fullmatch(r"[0-9a-f]{40,64}", head):
            fail(f"{receipt_context}.head must be a full Git object ID")
        results.append(
            {
                "gateId": gate["id"],
                "kind": gate["kind"],
                "patterns": gate["forbidPaths"],
                "checkedPaths": checked_paths,
                "baseRev": base_rev,
                "head": head,
            }
        )
    return results


def enforce_constraint_results(
    worktree: Path,
    base_rev: str,
    head: str,
    constraints: list[dict[str, Any]],
) -> None:
    for constraint in constraints:
        if constraint["baseRev"] != base_rev:
            fail(
                f"forbidPaths gate {constraint['gateId']!r} was witnessed against base "
                f"{constraint['baseRev']}, expected {base_rev}"
            )
        checked_paths = evaluate_forbid_paths(
            worktree,
            base_rev,
            head,
            constraint["gateId"],
            constraint["patterns"],
        )
        if constraint["head"] == head and constraint["checkedPaths"] != checked_paths:
            fail(
                f"forbidPaths gate {constraint['gateId']!r} receipt counted "
                f"{constraint['checkedPaths']} paths at {head}, publication counted {checked_paths}"
            )


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
    if action == "rebase":
        allowed.update({"publication", "constraints"})
    if action == "publish":
        allowed.add("constraints")
    if action == "merge":
        allowed.add("integration")
    data = object_exact(brief, allowed, f"{action} brief")
    config = repo_config(data.get("repositoryConfig"))
    workspace = object_exact(
        data.get("workspace"),
        {"taskId", "baseRev", "branch", "publishBranch", "worktreePath"},
        "workspace",
    )
    worktree = Path(required_string(workspace.get("worktreePath"), "workspace.worktreePath"))
    if not worktree.is_absolute():
        fail("workspace.worktreePath must be absolute")
    if action in {"publish", "rebase"} and not worktree.is_dir():
        fail(f"workspace.worktreePath must be an existing directory for {action}")
    if worktree.exists():
        if not worktree.is_dir():
            fail("workspace.worktreePath exists but is not a directory")
        git(worktree, "rev-parse", "--git-dir")
    return data, config, worktree


def github_pull_request(data: dict[str, Any], config: dict[str, Any], worktree: Path, head: str) -> str:
    repository = required_string(data.get("repository"), "repository")
    workspace = data["workspace"]
    branch = workspace["publishBranch"]
    task = data["task"]
    issue = campaign_issue(data.get("issue"))
    marker = pull_request_marker(
        required_string(data.get("campaign"), "campaign"), issue["number"], task["id"]
    )
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
            "url,body,baseRefName,headRefName,headRefOid,state",
        ]
    )
    try:
        candidates = json.loads(existing.stdout)
    except json.JSONDecodeError as error:
        fail(f"gh pr list returned invalid JSON: {error}")
    if not isinstance(candidates, list):
        fail("gh pr list must return an array")
    if len(candidates) > 1:
        fail(f"multiple pull requests use stable task branch {branch!r}")
    if candidates:
        candidate = candidates[0]
        url = required_string(candidate.get("url"), "existing pull request URL")
        if candidate.get("headRefName") != branch:
            fail(f"pull request {url} does not use expected head branch {branch!r}")
        if candidate.get("baseRefName") != config["baseBranch"]:
            fail(
                f"pull request {url} does not target expected base "
                f"{config['baseBranch']!r}"
            )
        if candidate.get("headRefOid") != head:
            fail(f"pull request {url} does not expose the just-published head")
        body = candidate.get("body")
        if not isinstance(body, str) or marker not in body:
            fail(f"pull request {url} lacks this campaign task's identity marker")
        state = candidate.get("state")
        if state == "CLOSED":
            run(["gh", "pr", "reopen", url, "--repo", repository])
        elif state != "OPEN":
            fail(f"pull request {url} is unexpectedly {state!r}")
        return url
    campaign = required_string(data.get("campaign"), "campaign")
    task_ref = f"{campaign}/{task['id']}"
    body = (
        f"{marker}\n"
        f"Spec-build campaign progress for {repository}#{issue['number']}.\n\n"
        f"Task `{task['id']}`: {task['title']}\n\n"
        f"Task ref: `{task_ref}`\n\n"
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
    constraints = normalize_constraint_results(data.get("constraints"), "publish constraints")
    workspace = data["workspace"]
    task_id = required_string(workspace.get("taskId"), "workspace.taskId")
    local_branch = required_string(workspace.get("branch"), "workspace.branch")
    publish_branch = required_string(
        workspace.get("publishBranch"), "workspace.publishBranch"
    )
    base_rev = required_string(workspace.get("baseRev"), "workspace.baseRev")
    actual_branch = git(worktree, "branch", "--show-current").stdout.strip()
    if actual_branch != local_branch:
        fail(f"worktree is on branch {actual_branch!r}, expected {local_branch!r}")
    status = git(worktree, "status", "--porcelain").stdout
    if status:
        fail("agent left uncommitted changes; commit the task before publication")
    head = git(worktree, "rev-parse", "HEAD").stdout.strip()
    if head == base_rev:
        fail("agent produced no commit relative to the prepared base")
    if git(worktree, "merge-base", "--is-ancestor", base_rev, head, check=False).returncode != 0:
        fail("task head is not descended from its prepared base revision")
    enforce_conflict_domains(worktree, base_rev, head, data.get("task"), task_id)
    enforce_constraint_results(worktree, base_rev, head, constraints)
    git(worktree, "push", config["remote"], f"HEAD:refs/heads/{publish_branch}")
    if config["forge"] == "github":
        pull_request = github_pull_request(data, config, worktree, head)
    else:
        pull_request = f"local://{data['repository']}/{publish_branch}"
    return {
        "taskId": task_id,
        "branch": publish_branch,
        "head": head,
        "pullRequest": pull_request,
    }


def publication(value: Any, context: str = "publication") -> dict[str, str]:
    item = object_exact(value, {"taskId", "branch", "head", "pullRequest"}, context)
    task_id = required_string(item.get("taskId"), f"{context}.taskId")
    if not TASK_ID.fullmatch(task_id):
        fail(f"{context}.taskId is not safe")
    return {
        "taskId": task_id,
        "branch": required_string(item.get("branch"), f"{context}.branch"),
        "head": required_string(item.get("head"), f"{context}.head"),
        "pullRequest": required_string(
            item.get("pullRequest"), f"{context}.pullRequest"
        ),
    }


def action_rebase(brief: dict[str, Any]) -> dict[str, Any]:
    data, config, worktree = publication_identity(brief, "rebase")
    published = publication(data.get("publication"))
    constraints = normalize_constraint_results(data.get("constraints"), "rebase constraints")
    workspace = data["workspace"]
    if published["taskId"] != workspace["taskId"]:
        fail("publication.taskId does not match workspace.taskId")
    if published["branch"] != workspace["publishBranch"]:
        fail("publication.branch does not match workspace.publishBranch")
    local_head = git(worktree, "rev-parse", "HEAD").stdout.strip()
    if local_head != published["head"]:
        fail("worktree head changed after publication")
    for constraint in constraints:
        if constraint["baseRev"] != workspace["baseRev"]:
            fail(
                f"forbidPaths gate {constraint['gateId']!r} was witnessed against base "
                f"{constraint['baseRev']}, expected prepared base {workspace['baseRev']}"
            )

    checkout: Path = config["checkout"]
    remote = config["remote"]
    git(checkout, "fetch", "--prune", remote)
    base_ref = f"{remote}/{config['baseBranch']}"
    branch_ref = f"{remote}/{published['branch']}"
    base_rev = git(checkout, "rev-parse", "--verify", f"{base_ref}^{{commit}}").stdout.strip()
    remote_head = git(
        checkout, "rev-parse", "--verify", f"{branch_ref}^{{commit}}"
    ).stdout.strip()
    if remote_head != published["head"]:
        fail("published branch moved before integration")

    if git(worktree, "merge-base", "--is-ancestor", base_rev, local_head, check=False).returncode == 0:
        enforce_conflict_domains(
            worktree, base_rev, local_head, data.get("task"), published["taskId"]
        )
        return {
            "taskId": published["taskId"],
            "baseRev": base_rev,
            "branch": published["branch"],
            "head": local_head,
            "pullRequest": published["pullRequest"],
            "regate": False,
        }

    rebased = git(worktree, "rebase", base_rev, check=False)
    if rebased.returncode != 0:
        detail = rebased.stderr.strip() or rebased.stdout.strip() or "no output"
        aborted = git(worktree, "rebase", "--abort", check=False)
        abandoned = git(
            worktree,
            "push",
            f"--force-with-lease=refs/heads/{published['branch']}:{published['head']}",
            remote,
            f":refs/heads/{published['branch']}",
            check=False,
        )
        if abandoned.returncode != 0:
            abandon_detail = (
                abandoned.stderr.strip() or abandoned.stdout.strip() or "no output"
            )
            fail(
                f"cannot rebase task onto current base {base_rev}: {detail}; "
                f"rebase abort exited {aborted.returncode}, and the exact published head "
                f"could not be abandoned: {abandon_detail}"
            )
        if aborted.returncode != 0:
            abort_detail = aborted.stderr.strip() or aborted.stdout.strip() or "no output"
            fail(
                f"cannot rebase task onto current base {base_rev}: {detail}; "
                f"the published branch was abandoned, but rebase abort failed: {abort_detail}"
            )
        fail(
            f"cannot rebase task onto current base {base_rev}: {detail}; "
            "rebase was aborted and the exact published branch was abandoned so a fresh "
            "pass can rebuild the task from current base"
        )
    rebased_head = git(worktree, "rev-parse", "HEAD").stdout.strip()
    enforce_conflict_domains(
        worktree, base_rev, rebased_head, data.get("task"), published["taskId"]
    )
    for constraint in constraints:
        evaluate_forbid_paths(
            worktree,
            base_rev,
            rebased_head,
            constraint["gateId"],
            constraint["patterns"],
        )
    git(
        worktree,
        "push",
        f"--force-with-lease=refs/heads/{published['branch']}:{published['head']}",
        remote,
        f"HEAD:refs/heads/{published['branch']}",
    )
    return {
        "taskId": published["taskId"],
        "baseRev": base_rev,
        "branch": published["branch"],
        "head": rebased_head,
        "pullRequest": published["pullRequest"],
        "regate": True,
    }


def cleanup_worktree(checkout: Path, worktree: Path, branch: str) -> None:
    removed = git(checkout, "worktree", "remove", "--force", str(worktree), check=False)
    if removed.returncode != 0 and worktree.exists():
        detail = removed.stderr.strip() or removed.stdout.strip() or "no output"
        fail(f"cannot remove campaign worktree {worktree}: {detail}")
    deleted = git(checkout, "branch", "-D", branch, check=False)
    if (
        deleted.returncode != 0
        and git(
            checkout,
            "show-ref",
            "--verify",
            "--quiet",
            f"refs/heads/{branch}",
            check=False,
        ).returncode
        == 0
    ):
        detail = deleted.stderr.strip() or deleted.stdout.strip() or "no output"
        fail(f"cannot remove campaign branch {branch!r}: {detail}")


def action_cleanup(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief,
        {
            "campaign",
            "repository",
            "repositoryConfig",
            "runId",
            "taskId",
            "workspaceRoot",
            "workspace",
        },
        "cleanup brief",
    )
    campaign = required_string(data.get("campaign"), "campaign")
    if not COMPONENT.fullmatch(campaign):
        fail("campaign is not a safe component")
    repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(repository):
        fail("repository must use owner/name form")
    run_id = required_string(data.get("runId"), "runId", 512)
    workspace_root = Path(required_string(data.get("workspaceRoot"), "workspaceRoot"))
    if not workspace_root.is_absolute():
        fail("workspaceRoot must be absolute")
    config = repo_config(data.get("repositoryConfig"))
    task_id = required_string(data.get("taskId"), "taskId")
    if not TASK_ID.fullmatch(task_id):
        fail("taskId is not safe")
    run_hash = hashlib.sha256(run_id.encode()).hexdigest()[:12]
    campaign_slug = safe_slug(campaign, 24)
    repository_slug = safe_slug(repository.split("/", 1)[1], 40)
    lane_name = "_campaign-preflight" if task_id == "campaign-preflight" else task_id
    repository_root = (workspace_root / repository_slug).resolve()
    run_root = repository_root / run_hash
    expected_worktree = run_root / lane_name
    if run_root.is_symlink() or expected_worktree.is_symlink():
        fail("cleanup campaign lane must not traverse a symlink")
    expected_resolved = expected_worktree.resolve()
    expected_branch = f"tally-work/{campaign_slug}-{run_hash}/{lane_name}"

    workspace_value = data.get("workspace")
    if workspace_value is None:
        worktree = expected_worktree
        branch = expected_branch
    else:
        workspace = object_exact(
            workspace_value,
            {"taskId", "baseRev", "branch", "publishBranch", "worktreePath"},
            "workspace",
        )
        if required_string(workspace.get("taskId"), "workspace.taskId") != task_id:
            fail("cleanup workspace.taskId does not match taskId")
        branch = required_string(workspace.get("branch"), "workspace.branch")
        required_string(workspace.get("baseRev"), "workspace.baseRev")
        required_string(workspace.get("publishBranch"), "workspace.publishBranch")
        worktree = Path(
            required_string(workspace.get("worktreePath"), "workspace.worktreePath")
        )
        if not worktree.is_absolute():
            fail("workspace.worktreePath must be absolute")
    if worktree.resolve() != expected_resolved:
        fail(f"cleanup worktree {worktree} is outside this campaign lane")
    worktree = expected_worktree
    if branch != expected_branch:
        fail(f"cleanup branch {branch!r} does not match this campaign lane")
    if worktree.exists():
        if not worktree.is_dir():
            fail("workspace.worktreePath exists but is not a directory")
        registered = any(
            record.get("worktree")
            and Path(record["worktree"]).resolve() == expected_resolved
            for record in parse_worktrees(config["checkout"])
        )
        if registered:
            git(worktree, "rev-parse", "--git-dir")
            actual_branch = git(worktree, "branch", "--show-current").stdout.strip()
            if actual_branch not in ("", branch):
                fail(
                    f"cleanup worktree is on branch {actual_branch!r}, "
                    f"expected {branch!r} or a detached prep head"
                )
        else:
            try:
                shutil.rmtree(worktree)
            except OSError as error:
                fail(f"cannot remove partial campaign worktree {worktree}: {error}")
    cleanup_worktree(config["checkout"], worktree, branch)
    remove_prep_markers(
        workspace_root,
        campaign,
        repository,
        run_id,
        task_id,
        expected_worktree,
    )
    prune_empty_ancestors(expected_worktree.parent, repository_root)
    return {"taskId": task_id, "cleaned": True}


def merge_local(data: dict[str, Any], config: dict[str, Any], integration: dict[str, Any]) -> str:
    checkout: Path = config["checkout"]
    remote_url = git(checkout, "remote", "get-url", config["remote"]).stdout.strip()
    workspace_root = Path(data["workspaceRoot"])
    workspace_root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="merge-", dir=workspace_root) as temporary:
        integration_checkout = Path(temporary) / "repository"
        run(["git", "clone", "--quiet", remote_url, str(integration_checkout)])
        git(integration_checkout, "config", "user.name", "tally spec-build")
        git(integration_checkout, "config", "user.email", "tally-spec-build@invalid")
        git(
            integration_checkout,
            "fetch",
            "origin",
            config["baseBranch"],
            integration["branch"],
        )
        actual_base = git(
            integration_checkout, "rev-parse", f"origin/{config['baseBranch']}"
        ).stdout.strip()
        actual_head = git(
            integration_checkout, "rev-parse", f"origin/{integration['branch']}"
        ).stdout.strip()
        if actual_base != integration["baseRev"]:
            fail("remote base moved after the rebased head was gated")
        if actual_head != integration["head"]:
            fail("published branch moved after the rebased head was gated")
        git(integration_checkout, "switch", "-C", config["baseBranch"], actual_base)
        git(
            integration_checkout,
            "merge",
            "--no-ff",
            "--no-edit",
            f"origin/{integration['branch']}",
        )
        merge_commit = git(integration_checkout, "rev-parse", "HEAD").stdout.strip()
        git(
            integration_checkout,
            "push",
            "origin",
            f"HEAD:refs/heads/{config['baseBranch']}",
        )
    return merge_commit


def merge_github(data: dict[str, Any], config: dict[str, Any], integration: dict[str, Any]) -> str:
    repository = required_string(data.get("repository"), "repository")
    url = required_string(integration.get("pullRequest"), "integration.pullRequest")
    checkout: Path = config["checkout"]
    git(checkout, "fetch", "--prune", config["remote"])
    base_ref = f"{config['remote']}/{config['baseBranch']}"
    branch_ref = f"{config['remote']}/{integration['branch']}"
    current_base = git(checkout, "rev-parse", f"{base_ref}^{{commit}}").stdout.strip()
    current_head = git(checkout, "rev-parse", f"{branch_ref}^{{commit}}").stdout.strip()
    if current_base != integration["baseRev"]:
        fail("remote base moved after the rebased head was gated")
    if current_head != integration["head"]:
        fail("published branch moved after the rebased head was gated")
    viewed = run(
        [
            "gh",
            "pr",
            "view",
            url,
            "--repo",
            repository,
            "--json",
            "state,mergeCommit,baseRefName,headRefName,headRefOid",
        ]
    )
    state = json.loads(viewed.stdout)
    if state.get("baseRefName") != config["baseBranch"]:
        fail(f"pull request {url} changed base branch before merge")
    if state.get("headRefName") != integration["branch"]:
        fail(f"pull request {url} changed head branch before merge")
    if state.get("headRefOid") != integration["head"]:
        fail(f"pull request {url} changed head commit before merge")
    if state.get("state") != "MERGED":
        run(
            [
                "gh",
                "pr",
                "merge",
                url,
                "--repo",
                repository,
                "--merge",
                "--match-head-commit",
                integration["head"],
            ]
        )
        viewed = run(
            ["gh", "pr", "view", url, "--repo", repository, "--json", "state,mergeCommit"]
        )
        state = json.loads(viewed.stdout)
    if state.get("state") != "MERGED":
        fail(f"pull request {url} did not reach MERGED")
    merge_commit = (state.get("mergeCommit") or {}).get("oid")
    merge_commit = required_string(merge_commit, "pull request merge commit")
    git(checkout, "fetch", "--prune", config["remote"])
    if (
        git(
            checkout,
            "merge-base",
            "--is-ancestor",
            integration["head"],
            f"{config['remote']}/{config['baseBranch']}",
            check=False,
        ).returncode
        != 0
    ):
        fail("current remote base does not contain the merged task head")
    github_progress_comment(data, integration, merge_commit)
    return merge_commit


def github_checkpoint_progress_comment(
    data: dict[str, Any], reference: str, revision: str, source_sha256: str
) -> None:
    repository = required_string(data.get("repository"), "repository")
    campaign = required_string(data.get("campaign"), "campaign")
    issue = campaign_issue(data.get("issue"))
    task = data["task"]
    task_id = task["id"]
    marker = (
        "<!-- tally:spec-build:v1 "
        f"campaign={campaign} issue={issue['number']} checkpoint={task_id} "
        f"source={source_sha256} passed -->"
    )
    comments = run(
        [
            "gh",
            "api",
            "--paginate",
            f"repos/{repository}/issues/{issue['number']}/comments?per_page=100",
            "--jq",
            ".[].body",
        ]
    ).stdout
    if marker not in comments:
        body = (
            f"{marker}\n"
            f"Automated checkpoint `{task_id}` ({task['title']}) passed at `{revision}`.\n\n"
            f"Task ref: `{campaign}/{task_id}`\n\n"
            f"Completion ref: `{reference}`"
        )
        run(
            [
                "gh",
                "issue",
                "comment",
                issue["number"],
                "--repo",
                repository,
                "--body",
                body,
            ]
        )
    reconcile_command = required_string(
        data.get("reconcileCommand"), "reconcileCommand", 300
    )
    if not reconcile_command.startswith("/"):
        fail("reconcileCommand must be an explicit slash command")
    run(
        [
            "gh",
            "issue",
            "comment",
            issue["number"],
            "--repo",
            repository,
            "--body",
            reconcile_command,
        ]
    )


def action_checkpoint(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief,
        {
            "campaign",
            "repository",
            "repositoryConfig",
            "issue",
            "reconcileCommand",
            "task",
            "source",
            "workspace",
        },
        "checkpoint brief",
    )
    campaign = required_string(data.get("campaign"), "campaign")
    if not COMPONENT.fullmatch(campaign):
        fail("campaign is not a safe component")
    repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(repository):
        fail("repository must use owner/name form")
    issue = campaign_issue(data.get("issue"))
    config = repo_config(data.get("repositoryConfig"))
    task = object_exact(
        data.get("task"),
        {"id", "kind", "title", "argv", "runtimeMaxSec", "dependencies"},
        "checkpoint task",
    )
    task_id = required_string(task.get("id"), "checkpoint task.id", 80)
    if not TASK_ID.fullmatch(task_id) or task.get("kind") != "checkpoint":
        fail("checkpoint task must carry a safe id and kind checkpoint")
    required_string(task.get("title"), "checkpoint task.title", 300)
    argv(task.get("argv"), "checkpoint task.argv")
    positive_integer(task.get("runtimeMaxSec"), "checkpoint task.runtimeMaxSec")
    string_list(task.get("dependencies"), "checkpoint task.dependencies")
    source = object_exact(data.get("source"), {"path", "sha256"}, "source")
    required_string(source.get("path"), "source.path")
    source_sha256 = required_string(source.get("sha256"), "source.sha256")
    workspace = object_exact(
        data.get("workspace"),
        {"taskId", "baseRev", "branch", "publishBranch", "worktreePath"},
        "workspace",
    )
    if workspace.get("taskId") != task_id:
        fail("checkpoint task.id does not match workspace.taskId")
    base_rev = required_string(workspace.get("baseRev"), "workspace.baseRev")
    if not re.fullmatch(r"[0-9a-f]{40,64}", base_rev):
        fail("workspace.baseRev must be a full Git object ID")
    branch = required_string(workspace.get("branch"), "workspace.branch")
    required_string(workspace.get("publishBranch"), "workspace.publishBranch")
    worktree = Path(required_string(workspace.get("worktreePath"), "workspace.worktreePath"))
    if not worktree.is_absolute() or not worktree.is_dir():
        fail("workspace.worktreePath must be an absolute existing directory")
    if git(worktree, "branch", "--show-current").stdout.strip() != branch:
        fail("checkpoint worktree changed branches during validation")
    if git(worktree, "rev-parse", "HEAD^{commit}").stdout.strip() != base_rev:
        fail("checkpoint command changed HEAD instead of validating the prepared base")
    if git(worktree, "status", "--porcelain", "--untracked-files=no").stdout:
        fail("checkpoint command changed tracked files instead of validating the prepared base")

    checkout: Path = config["checkout"]
    remote = config["remote"]
    git(checkout, "fetch", "--prune", "--no-tags", remote)
    current_base = git(
        checkout,
        "rev-parse",
        "--verify",
        f"{remote}/{config['baseBranch']}^{{commit}}",
    ).stdout.strip()
    if current_base != base_rev:
        fail("remote base moved after the checkpoint command was witnessed")
    reference = checkpoint_ref(campaign, issue["number"], task_id, source_sha256)
    existing = remote_ref_oid(worktree, remote, reference)
    if existing != base_rev:
        push_arguments = ["push"]
        if existing is not None:
            push_arguments.append(f"--force-with-lease={reference}:{existing}")
        push_arguments.extend([remote, f"{base_rev}:{reference}"])
        git(worktree, *push_arguments)
    if remote_ref_oid(worktree, remote, reference) != base_rev:
        fail("checkpoint completion ref did not expose the witnessed base revision")
    if config["forge"] == "github":
        github_checkpoint_progress_comment(data, reference, base_rev, source_sha256)
    return {"taskId": task_id, "ref": reference, "revision": base_rev}


def github_progress_comment(
    data: dict[str, Any], integration: dict[str, Any], merge_commit: str
) -> None:
    repository = required_string(data.get("repository"), "repository")
    campaign = required_string(data.get("campaign"), "campaign")
    issue = campaign_issue(data.get("issue"))
    issue_number = issue["number"]
    task = data.get("task")
    if not isinstance(task, dict):
        fail("task must be an object")
    task_id = required_string(task.get("id"), "task.id")
    task_title = required_string(task.get("title"), "task.title")
    marker = (
        "<!-- tally:spec-build:v1 "
        f"campaign={campaign} issue={issue_number} task={task_id} merged -->"
    )
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
    if marker not in comments:
        body = (
            f"{marker}\n"
            f"Campaign task `{task_id}` ({task_title}) merged via "
            f"{integration['pullRequest']}.\n\n"
            f"Task ref: `{campaign}/{task_id}`\n\n"
            f"Merge commit: `{merge_commit}`"
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


def action_continue(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief,
        {
            "campaign",
            "repository",
            "repositoryConfig",
            "issue",
            "runId",
            "reconcileCommand",
        },
        "continue brief",
    )
    required_string(data.get("campaign"), "campaign")
    repository = required_string(data.get("repository"), "repository")
    config = repo_config(data.get("repositoryConfig"))
    if config["forge"] != "github":
        fail("continue is only valid for a GitHub-backed campaign")
    issue = campaign_issue(data.get("issue"))
    required_string(data.get("runId"), "runId", 512)
    reconcile_command = required_string(data.get("reconcileCommand"), "reconcileCommand", 300)
    if not reconcile_command.startswith("/"):
        fail("reconcileCommand must be an explicit slash command")
    run(
        [
            "gh",
            "issue",
            "comment",
            issue["number"],
            "--repo",
            repository,
            "--body",
            reconcile_command,
        ]
    )
    return {"command": reconcile_command, "posted": True}


def action_merge(brief: dict[str, Any]) -> dict[str, Any]:
    data, config, worktree = publication_identity(brief, "merge")
    integration = object_exact(
        data.get("integration"),
        {"taskId", "baseRev", "branch", "head", "pullRequest", "regate"},
        "integration",
    )
    task_id = required_string(integration.get("taskId"), "integration.taskId")
    required_string(integration.get("baseRev"), "integration.baseRev")
    required_string(integration.get("branch"), "integration.branch")
    head = required_string(integration.get("head"), "integration.head")
    pull_request = required_string(integration.get("pullRequest"), "integration.pullRequest")
    if not isinstance(integration.get("regate"), bool):
        fail("integration.regate must be boolean")
    if task_id != data["workspace"]["taskId"]:
        fail("integration.taskId does not match workspace.taskId")
    if integration["branch"] != data["workspace"]["publishBranch"]:
        fail("integration.branch does not match workspace.publishBranch")
    if config["forge"] == "github":
        merge_commit = merge_github(data, config, integration)
    else:
        merge_commit = merge_local(data, config, integration)
    return {
        "taskId": task_id,
        "head": head,
        "mergeCommit": merge_commit,
        "pullRequest": pull_request,
        "regated": integration["regate"],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "action",
        choices=(
            "worklist",
            "sweep",
            "reconcile",
            "preflight",
            "prep",
            "cleanup",
            "constraint",
            "checkpoint",
            "publish",
            "rebase",
            "merge",
            "continue",
        ),
    )
    arguments = parser.parse_args()
    brief = load_brief()
    actions = {
        "worklist": action_worklist,
        "sweep": action_sweep,
        "reconcile": action_reconcile,
        "preflight": action_preflight,
        "prep": action_prep,
        "constraint": action_constraint,
        "checkpoint": action_checkpoint,
        "cleanup": action_cleanup,
        "publish": action_publish,
        "rebase": action_rebase,
        "merge": action_merge,
        "continue": action_continue,
    }
    emit(actions[arguments.action](brief))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except DriverError as error:
        print(f"spec-build-driver: {error}", file=sys.stderr)
        raise SystemExit(1)
