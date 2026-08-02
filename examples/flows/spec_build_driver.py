#!/usr/bin/env python3
"""Deterministic policy driver for the shipped spec-build tally flow."""

from __future__ import annotations

import argparse
import fcntl
import fnmatch
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tempfile
from typing import Any
import uuid

# The unified worktree manager ships beside this driver, so both campaign
# drivers create, resume, validate, and clean lanes through one implementation.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import campaign_worktrees as worktrees  # noqa: E402


TASK_ID = re.compile(r"^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$")
COMPONENT = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_.-]*$")
REPOSITORY = re.compile(r"^[^/ \t]+/[^/ \t]+$")
GIT_OID = re.compile(r"^[0-9a-f]{40,64}$")
CAMPAIGN_BEGIN = "<!-- tally:campaign:v1 -->"
CAMPAIGN_END = "<!-- tally:campaign:v1:end -->"
WORKLIST_BEGIN = "<!-- tally:campaign-worklist:v1 -->"
WORKLIST_END = "<!-- tally:campaign-worklist:v1:end -->"
TASK_MARKER_PREFIX = "<!-- tally:campaign-task:v1 id="
BRIEF_SENTINEL = (
    "Read the file whose path is in the TALLY_BRIEF environment variable and execute "
    "the mission it contains. That brief is your complete instruction set."
)
DIAGNOSIS_MARKER = re.compile(
    r"^<!-- tally:spec-build:diagnosis:v1 "
    r"campaign=([A-Za-z0-9_][A-Za-z0-9_.-]*) "
    r"issue=([1-9][0-9]*) "
    r"task=([a-z0-9](?:[a-z0-9-]*[a-z0-9])?) "
    r"attempt=([12]) -->$"
)
RETRY_MARKER = re.compile(
    r"^<!-- tally:spec-build:retry:v1 "
    r"campaign=([A-Za-z0-9_][A-Za-z0-9_.-]*) "
    r"issue=([1-9][0-9]*) "
    r"task=([a-z0-9](?:[a-z0-9-]*[a-z0-9])?) "
    r"attempt=([12]) -->$"
)
# A campaign machinery fault is not evidence that the task's work is wrong, so
# it buys a retry instead of a steering attempt. The budget is bounded and read
# back from the forge: past it, the fault is treated as a failed attempt.
MAX_MACHINE_RETRIES = 2
MAX_RETRY_CHARS = 2_000
# The closing summary is a bounded rendering of a bounded digest: past this
# many rows a section says how many it dropped instead of growing without end.
MAX_SUMMARY_ROWS = 40
MAX_DIAGNOSIS_CHARS = 12_000
MAX_DIFF_CHARS = 128 * 1024
# The daemon rejects an ingress file above 1 MiB. Refuse to write one instead
# of leaving an unclaimable event in the drain directory.
MAX_CONTINUATION_EVENT_BYTES = 1024 * 1024
PUBLIC_REDACTION = "conservative-v2"
# Receipts written by an earlier redactor stay readable: a redaction identity
# this driver no longer writes must never brick a running campaign.
PUBLIC_REDACTIONS = frozenset({"conservative-v1", PUBLIC_REDACTION})
PUBLIC_DIAGNOSIS_TRUNCATION = "\n[... diagnosis truncated after redaction ...]"
LIVE_JOB_STATES = {"paused", "queued", "running"}
PASS_RECORD_SCHEMA_VERSION = 2
# How a campaign integrates a task. `squash` is the campaign default: §3's
# target footprint is one conventional commit per task, and a merge commit
# carrying a template message is not that.
MERGE_METHODS = frozenset({"merge", "squash"})
# The narrate slot of §2's steward duty roster. The model proposes text; this
# commitlint-shaped grammar is what decides whether the text is used, and the
# node — never the model — runs git.
DEFAULT_NARRATION_PATTERN = r"^TALLY_FINAL_MESSAGE=(.*)$"
# Environment the publish node owns and a steward adapter may not redefine.
# TALLY_BRIEF is stripped outright rather than overridden: the narrator is
# handed its request on stdin and has no business reading the driver's own
# brief, which names the campaign's checkout, agent argv, and adapter table.
ENVIRONMENT_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
RESERVED_STEWARD_ENVIRONMENT = frozenset({"TALLY_BRIEF"})
NARRATION_TYPES = frozenset(
    {
        "build",
        "chore",
        "ci",
        "docs",
        "feat",
        "fix",
        "perf",
        "refactor",
        "revert",
        "style",
        "test",
    }
)
NARRATION_SCOPE = re.compile(r"^[a-z0-9][a-z0-9._/-]{0,31}$")
NARRATION_HEADER_MAX = 72
NARRATION_SUBJECT_MAX = 200
NARRATION_BODY_MAX = 4_000
NARRATION_BODY_LINE_MAX = 100
NARRATION_REASON_MAX = 200
NARRATION_ATTEMPTS = 2
NARRATION_DEFAULT_RUNTIME_MAX_SEC = 120
# The fourth proof axis (AUGUST-01-DESIGN.md §7). `off` is the shipped state.
GIT_AI_BINDINGS = frozenset({"off", "advisory", "required"})
GIT_AI_PROGRAM = "git-ai"
GIT_AI_NOTE_REF = "refs/notes/ai"
# Scratch refs the publication step owns. The remote's notes ref is fetched
# into the first; the exact tree that will be pushed is assembled in the
# second. Both are deleted afterwards and neither is ever a campaign artifact.
GIT_AI_REMOTE_REF = "refs/notes/tally-spec-build-remote-ai"
GIT_AI_PUBLISH_REF = "refs/notes/tally-spec-build-publish-ai"
# The settlement barrier the binding waits on, when the campaign names no
# budget of its own. git-ai mints a note from its background service after
# `git commit` returns, so a read taken before the service settles observes
# nothing and would report a false missing-note. The barrier runs inside the
# merge node, so a campaign whose node deadline is shorter than this would be
# killed mid-await on every task; the module refuses that pairing rather than
# leaving the two numbers unrelated.
GIT_AI_AWAIT_SEC = 60
# `Assisted-by: <adapter>:<model> (tally:<taskUuid> witness:<seq>)`. The exact
# formatting the gh producer already publishes; the trailer is a pointer into
# the witness, never the proof (§7). Reject a narrator that proposes one: the
# provenance line is the node's authority, not the model's.
ASSISTED_BY_PREFIX = "Assisted-by:"
# Git matches trailer keys case-insensitively, so `assisted-by:` is the same
# trailer to every git-native reader. The refusal below matches the way git
# reads the line, not the way the node happens to spell it -- the same reason
# NARRATION_CLOSING_KEYWORD is compiled `(?i)`.
NARRATION_ASSISTED_BY = re.compile(
    r"(?im)^" + re.escape(ASSISTED_BY_PREFIX)
)
ASSISTED_BY_MAX = 200
UUID_TEXT = re.compile(
    r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
)
# Every managed campaign marker starts here. A narrator that proposes one is
# proposing to forge campaign state, so its proposal is refused outright — the
# same line the worklist reader holds when a body repeats a managed marker.
MANAGED_MARKER_PREFIX = "<!-- tally:"
# A pull-request body is executable on GitHub: a closing keyword in a merged
# body, or in a commit message that lands on the default branch, closes the
# issue it names, and an @mention notifies a person or a whole team. The
# narration is spliced into both surfaces, so a proposal carrying either is
# proposing to mutate public forge state the campaign never named. The node
# appends its own `Closes #<sub-issue>` at `github_pull_request`; that
# authority belongs to the node, not to the model. A bare `#<n>` cross
# reference stays allowed: it backlinks and notifies nobody.
NARRATION_CLOSING_KEYWORD = re.compile(
    r"(?i)\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\b\s*:?\s*"
    r"(?:\#\d+|GH-\d+|[\w.-]+/[\w.-]+\#\d+|https?://\S+/(?:issues|pull)/\d+)"
)
NARRATION_MENTION = re.compile(r"(?<![0-9A-Za-z._-])@[0-9A-Za-z][0-9A-Za-z-]*")


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


def required_body(value: Any, context: str, maximum: int) -> str:
    if not isinstance(value, str) or not value.strip() or "\0" in value:
        fail(f"{context} must be non-empty text without NUL bytes")
    if len(value) > maximum:
        fail(f"{context} exceeds {maximum} characters")
    return value


def required_bool(value: Any, context: str) -> bool:
    if not isinstance(value, bool):
        fail(f"{context} must be boolean")
    return value


def full_git_oid(value: Any, context: str) -> str:
    oid = required_string(value, context)
    if not GIT_OID.fullmatch(oid):
        fail(f"{context} must be a full Git object ID")
    return oid


def required_text(value: Any, context: str, maximum: int) -> str:
    if not isinstance(value, str) or not value.strip():
        fail(f"{context} must be non-empty text")
    if len(value) > maximum:
        fail(f"{context} exceeds {maximum} characters")
    if any(ord(char) < 32 and char not in "\n\t\r" for char in value):
        fail(f"{context} contains unsupported control characters")
    return value.replace("\r\n", "\n").replace("\r", "\n").strip()


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


def canonical_sha256(value: Any) -> str:
    encoded = json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode("utf-8")
    return "sha256:" + hashlib.sha256(encoded).hexdigest()


def run(
    command: list[str],
    *,
    cwd: Path | None = None,
    check: bool = True,
    input_text: str | None = None,
    timeout: int | None = None,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            check=False,
            text=True,
            input=input_text,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            env=env,
        )
    except subprocess.TimeoutExpired:
        # A deadline is only ever set on an advisory subprocess whose caller
        # handles failure, so a timeout is reported as an exit status rather
        # than as a driver fault.
        result = subprocess.CompletedProcess(command, 124, "", "timed out")
    except OSError as error:
        fail(f"cannot execute {command[0]!r}: {error}")
    if check and result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no output"
        fail(f"command {command!r} exited {result.returncode}: {detail}")
    return result


def run_bytes(
    command: list[str],
    *,
    cwd: Path | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[bytes]:
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as error:
        fail(f"cannot execute {command[0]!r}: {error}")
    if check and result.returncode != 0:
        detail = (
            (result.stderr or result.stdout).decode(errors="replace").strip()
            or "no output"
        )
        fail(f"command {command!r} exited {result.returncode}: {detail}")
    return result


def git(
    checkout: Path,
    *arguments: str,
    check: bool = True,
    input_text: str | None = None,
) -> subprocess.CompletedProcess[str]:
    return run(
        ["git", "-C", str(checkout), *arguments],
        check=check,
        input_text=input_text,
    )


def git_bytes(
    checkout: Path, *arguments: str, check: bool = True
) -> subprocess.CompletedProcess[bytes]:
    return run_bytes(["git", "-C", str(checkout), *arguments], check=check)


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


def merge_method(value: Any, context: str) -> str:
    """How the merge node integrates a task, defaulting to the campaign default.

    An absent value is `squash`, so a campaign brief minted before the option
    existed integrates the way every campaign now does rather than silently
    keeping the old merge-commit footprint.
    """
    if value is None:
        return "squash"
    method = required_string(value, context)
    if method not in MERGE_METHODS:
        fail(f"{context} must be merge or squash")
    return method


def git_ai_binding(value: Any, context: str) -> str:
    """Whether the merge node binds Git AI authorship on what it integrated.

    An absent value is `off`, so a campaign brief minted before the option
    existed integrates exactly the way it did before: no binding, no notes
    pushed, no receipt.
    """
    if value is None:
        return "off"
    binding = required_string(value, context)
    if binding not in GIT_AI_BINDINGS:
        fail(f"{context} must be off, advisory, or required")
    return binding


def git_ai_await_sec(value: Any, context: str) -> int:
    """How long the merge node may wait on git-ai's settlement barrier.

    Absent is the shipped default. The module derives it from the campaign's
    own node deadline and refuses a pairing that would kill the node mid-wait.
    """
    if value is None:
        return GIT_AI_AWAIT_SEC
    return positive_integer(value, context)


def assisted_by_record(value: Any, context: str) -> dict[str, Any] | None:
    """The assisting session the merge node may point at from the commit.

    Null is ordinary: a checkpoint task has no agent, and an estate that never
    named a model leaves one component of the trailer unknowable. The node
    refuses to guess it -- an `Assisted-by:` line naming a model nobody
    executed is a provenance claim, and a wrong one is worse than none.
    """
    if value is None:
        return None
    record = object_exact(value, {"adapter", "model", "taskUuid", "witnessSeq"}, context)
    adapter = required_string(record.get("adapter"), f"{context}.adapter", 128)
    model = required_string(record.get("model"), f"{context}.model", 128)
    task_uuid = required_string(record.get("taskUuid"), f"{context}.taskUuid", 36)
    if not UUID_TEXT.fullmatch(task_uuid):
        fail(f"{context}.taskUuid must be a UUID")
    witness_seq = positive_integer(record.get("witnessSeq"), f"{context}.witnessSeq")
    for name, item in (("adapter", adapter), ("model", model)):
        if "(" in item or ")" in item or "\n" in item:
            fail(f"{context}.{name} must not contain trailer punctuation")
    return {
        "adapter": adapter,
        "model": model,
        "taskUuid": task_uuid,
        "witnessSeq": witness_seq,
    }


def assisted_by_trailer(record: dict[str, Any] | None) -> str | None:
    """The exact trailer `gh_intake` publishes, or nothing."""
    if record is None:
        return None
    trailer = (
        f"{ASSISTED_BY_PREFIX} {record['adapter']}:{record['model']} "
        f"(tally:{record['taskUuid']} witness:{record['witnessSeq']})"
    )
    if len(trailer) > ASSISTED_BY_MAX:
        fail("assistedBy renders a trailer over the published cap")
    return trailer


def steward_role(value: Any, context: str = "campaign steward") -> dict[str, Any] | None:
    """The §2 steward bound as a catalog role, not as a script choice.

    `adapter` names the entry in the estate's adapter table; that entry's argv
    and env are what decide which model answers, at which endpoint, with which
    credentials. `finalMessagePattern` is the adapter's own declared
    final-message capture. Null is the shipped state: no steward, template
    narration, no model call.
    """
    if value is None:
        return None
    role = object_exact(
        value,
        {"adapter", "argv", "env", "finalMessagePattern", "runtimeMaxSec"},
        context,
    )
    adapter = required_string(role.get("adapter"), f"{context}.adapter", 128)
    if not COMPONENT.fullmatch(adapter):
        fail(f"{context}.adapter is not a safe component")
    arguments = argv(role.get("argv"), f"{context}.argv")
    environment = role.get("env", {})
    if not isinstance(environment, dict):
        fail(f"{context}.env must be an object")
    for key, item in environment.items():
        if not isinstance(key, str) or not ENVIRONMENT_NAME.fullmatch(key):
            fail(f"{context}.env names must be environment identifiers")
        if key in RESERVED_STEWARD_ENVIRONMENT:
            fail(f"{context}.env must not set reserved variable {key}")
        required_string(item, f"{context}.env.{key}", 4096)
    pattern = role.get("finalMessagePattern", DEFAULT_NARRATION_PATTERN)
    pattern = required_string(pattern, f"{context}.finalMessagePattern", 1024)
    try:
        compiled = re.compile(pattern)
    except re.error as error:
        fail(f"{context}.finalMessagePattern is not a valid regular expression: {error}")
    if compiled.groups != 1:
        fail(f"{context}.finalMessagePattern must declare exactly one capture group")
    runtime = role.get("runtimeMaxSec", NARRATION_DEFAULT_RUNTIME_MAX_SEC)
    if runtime is not None:
        runtime = positive_integer(runtime, f"{context}.runtimeMaxSec")
    return {
        "adapter": adapter,
        "argv": arguments,
        "env": {key: str(item) for key, item in environment.items()},
        "finalMessagePattern": pattern,
        "runtimeMaxSec": runtime,
    }


def narration_record(value: Any, context: str) -> dict[str, Any]:
    """A validated narration carried forward from the publish node."""
    record = object_exact(value, {"source", "subject", "body"}, context)
    source = required_string(record.get("source"), f"{context}.source")
    if source not in {"steward", "template"}:
        fail(f"{context}.source must be steward or template")
    subject = required_string(record.get("subject"), f"{context}.subject", NARRATION_SUBJECT_MAX)
    body = record.get("body", "")
    if not isinstance(body, str) or len(body) > NARRATION_BODY_MAX:
        fail(f"{context}.body must be a string of at most {NARRATION_BODY_MAX} characters")
    if "\0" in body:
        fail(f"{context}.body must not contain NUL bytes")
    return {"source": source, "subject": subject, "body": body}


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
    if len(domains) != len({domain.casefold() for domain in domains}):
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


def normalize_owned_paths(value: Any, context: str) -> list[str]:
    paths = string_list(value, context)
    if len(paths) != len(set(paths)):
        fail(f"{context} contains duplicates")
    normalized: list[str] = []
    for index, candidate in enumerate(paths):
        path = Path(candidate)
        if path.is_absolute() or ".." in path.parts or candidate.endswith("/"):
            fail(f"{context}[{index}] must be a normalized relative path without '..'")
        rendered = path.as_posix()
        if rendered in {"", "."} or rendered != candidate:
            fail(f"{context}[{index}] must be a normalized relative path")
        normalized.append(rendered)
    return normalized


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

    checkout: Path = config["checkout"]
    remote = config["remote"]
    git(checkout, "fetch", "--prune", "--no-tags", remote)
    base_ref = f"{remote}/{config['baseBranch']}"
    base_rev = git(checkout, "rev-parse", "--verify", f"{base_ref}^{{commit}}").stdout.strip()
    pattern_parts = PurePosixPath(pattern).parts
    literal_prefix: list[str] = []
    for part in pattern_parts:
        if any(character in part for character in "*?["):
            break
        literal_prefix.append(part)
    tree_arguments = ["ls-tree", "-r", "-z", "--full-tree", base_rev]
    if literal_prefix:
        tree_arguments.extend(["--", "/".join(literal_prefix)])
    tree = git_bytes(checkout, *tree_arguments).stdout
    matches: list[tuple[str, str]] = []
    for raw_entry in tree.split(b"\0"):
        if not raw_entry:
            continue
        try:
            metadata, raw_path = raw_entry.split(b"\t", 1)
            mode, object_type, object_id = metadata.decode("ascii").split(" ")
            path = raw_path.decode("utf-8")
        except (UnicodeDecodeError, ValueError):
            fail("remote base tree contains a malformed worklist candidate")
        path_parts = PurePosixPath(path).parts
        if len(path_parts) != len(pattern_parts) or not all(
            (not part.startswith(".") or candidate.startswith("."))
            and fnmatch.fnmatchcase(part, candidate)
            for part, candidate in zip(path_parts, pattern_parts, strict=True)
        ):
            continue
        if object_type == "blob" and mode in {"100644", "100755"}:
            matches.append((path, object_id))
    if len(matches) != 1:
        fail(f"worklist pattern {pattern!r} matched {len(matches)} regular files; expected exactly one")
    source_path, source_object = matches[0]
    raw = git_bytes(checkout, "cat-file", "blob", source_object).stdout
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
            "path": source_path,
            "sha256": "sha256:" + hashlib.sha256(raw).hexdigest(),
            "revision": base_rev,
        },
        "tasks": tasks,
    }


def github_json(arguments: list[str], context: str) -> Any:
    viewed = run(["gh", *arguments])
    try:
        return json.loads(viewed.stdout)
    except json.JSONDecodeError as error:
        fail(f"{context} returned invalid JSON: {error}")


def managed_section(body: str, start: str, end: str, context: str) -> str:
    start_index = body.find(start)
    if start_index < 0:
        fail(f"{context} is missing {start}")
    content_start = start_index + len(start)
    end_index = body.find(end, content_start)
    if end_index < 0:
        fail(f"{context} is missing {end}")
    if body.find(start, content_start) >= 0 or body.find(end, end_index + len(end)) >= 0:
        fail(f"{context} repeats a managed campaign marker")
    return body[content_start:end_index].strip()


def forge_agent(value: Any) -> dict[str, Any]:
    agent = object_exact(
        value,
        {
            "adapter",
            "argv",
            "priority",
            "runtimeMaxSec",
            "approvalPolicy",
            "sandboxPolicy",
            "diagnosisSandboxPolicy",
            "model",
        },
        "campaign agent",
    )
    adapter = agent.get("adapter", "codex")
    adapter = required_string(adapter, "campaign agent.adapter")
    arguments = agent.get("argv", [BRIEF_SENTINEL])
    arguments = argv(arguments, "campaign agent.argv")
    priority = agent.get("priority", "low")
    if priority not in {"interrupt", "high", "medium", "low"}:
        fail("campaign agent.priority is invalid")
    runtime = agent.get("runtimeMaxSec", 14_400)
    if runtime is not None:
        runtime = positive_integer(runtime, "campaign agent.runtimeMaxSec")
    # A campaign node is unattended, so it never asks for an escalation, and its
    # sandbox must reach git metadata or the implementation node cannot commit.
    approval = agent.get("approvalPolicy", "never")
    if approval is not None:
        approval = required_string(approval, "campaign agent.approvalPolicy")
    sandbox = agent.get("sandboxPolicy", "danger-full-access")
    if sandbox is not None:
        sandbox = required_string(sandbox, "campaign agent.sandboxPolicy")
    # The diagnosis brief prohibits mutation, so its node does not inherit the
    # implementation node's writable sandbox.
    diagnosis_sandbox = agent.get("diagnosisSandboxPolicy", "read-only")
    if diagnosis_sandbox is not None:
        diagnosis_sandbox = required_string(
            diagnosis_sandbox, "campaign agent.diagnosisSandboxPolicy"
        )
    # The model this campaign dispatches with. It is what the daemon records
    # as the job's canonical model and therefore what the merge node may
    # honestly name in an `Assisted-by:` trailer; null leaves both alone.
    model = agent.get("model")
    if model is not None:
        model = required_string(model, "campaign agent.model", 128)
    return {
        "adapter": adapter,
        "argv": arguments,
        "priority": priority,
        "runtimeMaxSec": runtime,
        "approvalPolicy": approval,
        "sandboxPolicy": sandbox,
        "diagnosisSandboxPolicy": diagnosis_sandbox,
        "model": model,
    }


def validated_narration(value: Any) -> tuple[dict[str, str] | None, str | None]:
    """The commitlint-shaped grammar check on one steward proposal.

    Deterministic and total: it returns either the composed conventional
    message or exactly one reason the proposal was refused. Nothing here
    consults a model, and no proposal reaches git without passing.
    """
    if not isinstance(value, dict):
        return None, "proposal is not a JSON object"
    unknown = sorted(set(value) - {"type", "scope", "subject", "body"})
    if unknown:
        return None, f"proposal has unknown fields: {', '.join(unknown)}"
    kind = value.get("type")
    if not isinstance(kind, str) or kind not in NARRATION_TYPES:
        return None, "type must be one of " + ", ".join(sorted(NARRATION_TYPES))
    scope = value.get("scope")
    if scope is not None and (not isinstance(scope, str) or not NARRATION_SCOPE.fullmatch(scope)):
        return None, "scope must be null or a short lowercase identifier"
    subject = value.get("subject")
    if not isinstance(subject, str) or not subject.strip():
        return None, "subject must be non-empty text"
    subject = subject.strip()
    if any(ord(char) < 32 or ord(char) == 127 for char in subject):
        return None, "subject contains control characters"
    if subject.endswith("."):
        return None, "subject must not end with a period"
    if subject[:1].isupper():
        return None, "subject must not start with a capital letter"
    header = f"{kind}({scope}): {subject}" if scope else f"{kind}: {subject}"
    if len(header) > NARRATION_HEADER_MAX:
        return None, (
            f"header is {len(header)} characters, over the {NARRATION_HEADER_MAX} cap"
        )
    body = value.get("body")
    if body is None:
        body = ""
    if not isinstance(body, str):
        return None, "body must be null or a string"
    body = body.replace("\r\n", "\n").replace("\r", "\n").strip()
    if len(body) > NARRATION_BODY_MAX:
        return None, f"body is over the {NARRATION_BODY_MAX} character cap"
    if any(ord(char) < 32 and char != "\n" for char in body) or "\x7f" in body:
        return None, "body contains control characters"
    for line in body.split("\n"):
        if len(line) > NARRATION_BODY_LINE_MAX:
            return None, f"body wraps past {NARRATION_BODY_LINE_MAX} columns"
    for text in (header, body):
        if MANAGED_MARKER_PREFIX in text:
            return None, "proposal contains a managed campaign marker"
        if NARRATION_ASSISTED_BY.search(text):
            # The merge node appends the real trailer from the witnessed
            # attempt. A model-authored one would be a provenance claim nothing
            # verified, spliced into a commit that lands on the default branch.
            return None, "proposal contains an Assisted-by trailer"
        if NARRATION_CLOSING_KEYWORD.search(text):
            return None, "proposal contains a GitHub closing keyword"
        if NARRATION_MENTION.search(text):
            return None, "proposal contains an @mention"
    return {"subject": header, "body": body}, None


def template_narration(task: dict[str, Any]) -> dict[str, Any]:
    """The brief-derived fallback: exactly the pre-steward publication text."""
    return {
        "source": "template",
        "subject": f"{task['id']}: {task['title']}",
        "body": "",
    }


def narrate(
    role: dict[str, Any] | None, task: dict[str, Any], request: dict[str, Any]
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    """§2's narrate slot: model proposes, validator enforces, node executes.

    The steward is a plain direct argv — the estate's adapter table supplies
    that argv and the environment it runs in, so which model answers and how it
    is reached is an adapter change — and its output is read from the capture
    that adapter declares, defaulting to the `spec-build-driver` adapter's own
    final-message contract. Two validation failures spend the slot; the third
    message is the template, and the campaign proceeds either way. The steward
    never runs git.

    The adapter's per-job launch policies, hardening preset, and writable paths
    are deliberately not applied: this is a subprocess of the publish node, not
    a tally job, which is what keeps the seam free of flow nodes. The module
    refuses a steward adapter that declares any of them rather than letting the
    estate believe they took effect.
    """
    if role is None:
        return template_narration(task), []
    capture_pattern = re.compile(role["finalMessagePattern"])
    environment = {
        key: value
        for key, value in os.environ.items()
        if key not in RESERVED_STEWARD_ENVIRONMENT
    }
    environment.update(role["env"])
    transcript: list[dict[str, Any]] = []

    def reject(attempt: int, status: str, reason: str) -> None:
        transcript.append(
            {
                "attempt": attempt,
                "status": status,
                "reason": reason[:NARRATION_REASON_MAX],
            }
        )

    for attempt in range(1, NARRATION_ATTEMPTS + 1):
        payload = dict(request)
        payload["attempt"] = attempt
        if transcript:
            payload["previousRejection"] = transcript[-1]["reason"]
        invoked = run(
            role["argv"],
            check=False,
            input_text=json.dumps(payload, sort_keys=True, ensure_ascii=False),
            timeout=role["runtimeMaxSec"],
            env=environment,
        )
        if invoked.returncode != 0:
            # The narrator's own stderr is deliberately not echoed: it can
            # carry the estate's endpoint and credentials, and this transcript
            # is quotable in a public failure receipt.
            reject(attempt, "failed", f"steward exited {invoked.returncode}")
            continue
        captured: str | None = None
        for line in invoked.stdout.splitlines():
            matched = capture_pattern.match(line)
            if matched:
                captured = matched.group(1)
        if captured is None:
            reject(attempt, "failed", "steward produced no final-message line")
            continue
        try:
            proposal = json.loads(captured)
        except json.JSONDecodeError:
            reject(attempt, "rejected", "final message is not valid JSON")
            continue
        narration, reason = validated_narration(proposal)
        if narration is None:
            reject(attempt, "rejected", reason or "proposal is invalid")
            continue
        transcript.append({"attempt": attempt, "status": "accepted", "reason": None})
        return {"source": "steward", **narration}, transcript
    return template_narration(task), transcript


def forge_gates(value: Any) -> list[dict[str, Any]]:
    if not isinstance(value, list) or not 1 <= len(value) <= 16:
        fail("campaign gates must contain 1..=16 entries")
    result: list[dict[str, Any]] = []
    identifiers: set[str] = set()
    for index, candidate in enumerate(value):
        context = f"campaign gates[{index}]"
        if not isinstance(candidate, dict):
            fail(f"{context} must be an object")
        kind = candidate.get("kind")
        if kind == "command":
            gate = object_exact(
                candidate,
                {"kind", "id", "preflightArgv", "argv", "runtimeMaxSec"},
                context,
            )
            normalized = {
                "kind": "command",
                "id": required_string(gate.get("id"), f"{context}.id", 80),
                "preflightArgv": argv(gate.get("preflightArgv"), f"{context}.preflightArgv"),
                "argv": argv(gate.get("argv"), f"{context}.argv"),
                "runtimeMaxSec": positive_integer(
                    gate.get("runtimeMaxSec", 900), f"{context}.runtimeMaxSec"
                ),
            }
        elif kind == "forbidPaths":
            gate = object_exact(
                candidate,
                {"kind", "id", "forbidPaths", "runtimeMaxSec"},
                context,
            )
            identifier = required_string(gate.get("id"), f"{context}.id", 80)
            patterns = string_list(gate.get("forbidPaths"), f"{context}.forbidPaths", nonempty=True)
            if len(patterns) > 128 or len(patterns) != len(set(patterns)):
                fail(f"{context}.forbidPaths must contain 1..=128 unique patterns")
            for pattern in patterns:
                path = Path(pattern)
                if path.is_absolute() or ".." in path.parts or pattern.endswith("/"):
                    fail(f"{context}.forbidPaths contains an unsafe pattern")
            normalized = {
                "kind": "forbidPaths",
                "id": identifier,
                "forbidPaths": patterns,
                "runtimeMaxSec": positive_integer(
                    gate.get("runtimeMaxSec", 900), f"{context}.runtimeMaxSec"
                ),
            }
        else:
            fail(f"{context}.kind must be command or forbidPaths")
        identifier = normalized["id"]
        if not COMPONENT.fullmatch(identifier) or identifier in identifiers:
            fail("campaign gate ids must be safe and unique")
        identifiers.add(identifier)
        result.append(normalized)
    return result


def forge_manifest(
    value: Any,
) -> tuple[dict[str, Any], list[dict[str, Any]], dict[str, Any]]:
    manifest = object_exact(
        value,
        {
            "schemaVersion",
            "name",
            "repository",
            "maxTasks",
            "maxParallel",
            "driverRuntimeMaxSec",
            "runtimeMaxSec",
            "pool",
            "mergeMethod",
            "gitAiBinding",
            "gitAiAwaitSec",
            "agent",
            "steward",
            "gates",
            "tasks",
        },
        "campaign manifest",
    )
    if manifest.get("schemaVersion") != 1:
        fail("campaign manifest schemaVersion must equal 1")
    campaign = required_string(manifest.get("name"), "campaign manifest.name", 80)
    if not COMPONENT.fullmatch(campaign):
        fail("campaign manifest.name is not a safe component")
    repository_config = repo_config(manifest.get("repository"))
    max_tasks = manifest.get("maxTasks", 64)
    if not isinstance(max_tasks, int) or isinstance(max_tasks, bool) or not 1 <= max_tasks <= 100:
        fail("campaign manifest.maxTasks must be in 1..=100")
    max_parallel = manifest.get("maxParallel", 1)
    if (
        not isinstance(max_parallel, int)
        or isinstance(max_parallel, bool)
        or not 1 <= max_parallel <= max_tasks
    ):
        fail("campaign manifest.maxParallel must be positive and not exceed maxTasks")
    driver_runtime = positive_integer(
        manifest.get("driverRuntimeMaxSec", 900),
        "campaign manifest.driverRuntimeMaxSec",
    )
    runtime = manifest.get("runtimeMaxSec", 86_400)
    if runtime is not None:
        positive_integer(runtime, "campaign manifest.runtimeMaxSec")
    pool = manifest.get("pool", "campaign")
    if not isinstance(pool, str) or not COMPONENT.fullmatch(pool):
        fail("campaign manifest.pool is not a safe component")
    method = merge_method(manifest.get("mergeMethod"), "campaign manifest.mergeMethod")
    binding = git_ai_binding(manifest.get("gitAiBinding"), "campaign manifest.gitAiBinding")
    await_sec = git_ai_await_sec(
        manifest.get("gitAiAwaitSec"), "campaign manifest.gitAiAwaitSec"
    )
    agent = forge_agent(manifest.get("agent"))
    steward = steward_role(manifest.get("steward"), "campaign manifest.steward")
    gates = forge_gates(manifest.get("gates"))
    candidates = manifest.get("tasks")
    if not isinstance(candidates, list) or not 1 <= len(candidates) <= max_tasks:
        fail(f"campaign manifest.tasks must contain 1..={max_tasks} entries")
    references: list[dict[str, Any]] = []
    prior: set[str] = set()
    issues: set[int] = set()
    for index, candidate in enumerate(candidates):
        context = f"campaign manifest.tasks[{index}]"
        if not isinstance(candidate, dict):
            fail(f"{context} must be an object")
        kind = candidate.get("kind")
        if kind == "implementation":
            task = object_exact(
                candidate,
                {"id", "kind", "issue", "dependencies", "conflictDomains"},
                context,
            )
        elif kind == "checkpoint":
            task = object_exact(
                candidate,
                {"id", "kind", "issue", "dependencies", "argv", "runtimeMaxSec"},
                context,
            )
        else:
            fail(f"{context}.kind must be implementation or checkpoint")
        identifier = required_string(task.get("id"), f"{context}.id", 80)
        if not TASK_ID.fullmatch(identifier) or identifier in prior:
            fail(f"{context}.id is invalid or duplicated")
        number = task.get("issue")
        if not isinstance(number, int) or isinstance(number, bool) or number < 1 or number in issues:
            fail(f"{context}.issue must be a unique positive integer")
        dependencies = string_list(task.get("dependencies", []), f"{context}.dependencies")
        if len(dependencies) != len(set(dependencies)) or any(item not in prior for item in dependencies):
            fail(f"{context}.dependencies must be unique earlier task ids")
        conflicts = (
            normalize_conflict_domains(
                task.get("conflictDomains"),
                f"{context}.conflictDomains",
                required=max_parallel > 1,
            )
            if kind == "implementation"
            else []
        )
        checkpoint_argv = (
            argv(task.get("argv"), f"{context}.argv") if kind == "checkpoint" else None
        )
        checkpoint_runtime = (
            positive_integer(task.get("runtimeMaxSec"), f"{context}.runtimeMaxSec")
            if kind == "checkpoint"
            else None
        )
        references.append(
            {
                "id": identifier,
                "kind": kind,
                "issue": number,
                "dependencies": dependencies,
                "conflictDomains": conflicts,
                "argv": checkpoint_argv,
                "runtimeMaxSec": checkpoint_runtime,
            }
        )
        prior.add(identifier)
        issues.add(number)
    config = {
        "campaign": campaign,
        "repositoryConfig": {
            "checkout": str(repository_config["checkout"]),
            "baseBranch": repository_config["baseBranch"],
            "remote": repository_config["remote"],
            "forge": repository_config["forge"],
        },
        "maxParallel": max_parallel,
        "mergeMethod": method,
        "gitAiBinding": binding,
        "gitAiAwaitSec": await_sec,
        "agent": agent,
        "steward": steward,
        "gates": gates,
    }
    normalized_manifest = {
        "schemaVersion": 1,
        "name": campaign,
        "repository": {
            "checkout": str(repository_config["checkout"]),
            "baseBranch": repository_config["baseBranch"],
            "remote": repository_config["remote"],
            "forge": repository_config["forge"],
        },
        "maxTasks": max_tasks,
        "maxParallel": max_parallel,
        "driverRuntimeMaxSec": driver_runtime,
        "runtimeMaxSec": runtime,
        "pool": pool,
        "mergeMethod": method,
        "gitAiBinding": binding,
        "gitAiAwaitSec": await_sec,
        "agent": agent,
        "steward": steward,
        "gates": gates,
        "tasks": references,
    }
    return config, references, normalized_manifest


def issue_graph_worklist(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(brief, {"repository", "issue", "worklist"}, "reconcile brief")
    repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(repository):
        fail("repository must use owner/name form")
    issue = campaign_issue(data.get("issue"))
    expected_url = f"https://github.com/{repository}/issues/{issue['number']}"
    if issue["url"] != expected_url:
        fail("campaign issue URL does not match repository and issue number")
    selector = object_exact(data.get("worklist"), {"kind", "graphDigest"}, "worklist")
    if selector.get("kind") != "github-issue":
        fail("forge-native worklist.kind must equal github-issue")
    admitted_digest = required_string(selector.get("graphDigest"), "worklist.graphDigest")
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", admitted_digest):
        fail("worklist.graphDigest must be a lowercase SHA-256 identity")
    master = github_json(
        ["api", f"repos/{repository}/issues/{issue['number']}"],
        "campaign master issue",
    )
    if not isinstance(master, dict) or master.get("pull_request") is not None:
        fail("campaign master locator did not resolve to an issue")
    if master.get("state") != "open" or master.get("html_url") != expected_url:
        fail("campaign master issue must be open and canonical")
    body = master.get("body")
    if not isinstance(body, str):
        fail("campaign master issue has no body")
    section = managed_section(body, CAMPAIGN_BEGIN, CAMPAIGN_END, "campaign master issue")
    if not section.startswith("```json") or not section.endswith("```"):
        fail("campaign manifest must be one fenced JSON object")
    try:
        manifest_value = json.loads(section[len("```json") : -3].strip())
    except json.JSONDecodeError as error:
        fail(f"campaign manifest is invalid JSON: {error}")
    config, references, normalized_manifest = forge_manifest(manifest_value)
    subissues = github_json(
        [
            "api",
            f"repos/{repository}/issues/{issue['number']}/sub_issues?per_page=100",
        ],
        "campaign sub-issues",
    )
    if not isinstance(subissues, list):
        fail("campaign sub-issues response must be an array")
    by_number: dict[int, dict[str, Any]] = {}
    for index, candidate in enumerate(subissues):
        if not isinstance(candidate, dict):
            fail(f"campaign sub-issues[{index}] must be an object")
        number = candidate.get("number")
        if (
            not isinstance(number, int)
            or isinstance(number, bool)
            or number < 1
            or number in by_number
        ):
            fail("campaign sub-issues must have unique positive issue numbers")
        if candidate.get("pull_request") is not None:
            fail(f"campaign sub-issue #{number} is a pull request")
        if candidate.get("state") not in {"open", "closed"}:
            fail(f"campaign sub-issue #{number} has an unknown state")
        by_number[number] = candidate
    expected_numbers = {reference["issue"] for reference in references}
    if set(by_number) != expected_numbers:
        fail("campaign manifest task issues and native sub-issues differ")
    tasks: list[dict[str, Any]] = []
    for reference in references:
        candidate = by_number[reference["issue"]]
        title = required_string(candidate.get("title"), f"task {reference['id']} title", 300)
        task_body = required_body(candidate.get("body"), f"task {reference['id']} body", 64_000)
        task_url = required_string(candidate.get("html_url"), f"task {reference['id']} URL")
        expected_task_url = f"https://github.com/{repository}/issues/{reference['issue']}"
        if task_url != expected_task_url:
            fail(f"task {reference['id']} issue URL is not canonical")
        common = {
            "id": reference["id"],
            "kind": reference["kind"],
            "title": title,
            "brief": {
                "issue": {"number": str(reference["issue"]), "url": task_url},
                "body": task_body,
            },
            "dependencies": reference["dependencies"],
        }
        if reference["kind"] == "implementation":
            common["conflictDomains"] = reference["conflictDomains"]
        else:
            common["argv"] = reference["argv"]
            common["runtimeMaxSec"] = reference["runtimeMaxSec"]
        tasks.append(common)
    source_value = {
        "manifest": normalized_manifest,
        "tasks": [
            {
                "number": reference["issue"],
                "title": by_number[reference["issue"]].get("title"),
                "body": by_number[reference["issue"]].get("body"),
            }
            for reference in references
        ],
    }
    digest = canonical_sha256(source_value)
    if digest != admitted_digest:
        fail(
            "live issue executable graph does not match the armed digest; "
            "inspect it and explicitly re-arm"
        )
    for task in tasks:
        task["revision"] = canonical_sha256(
            {"source": admitted_digest, "task": task["id"]}
        )
    repository_config = repo_config(config["repositoryConfig"])
    checkout = repository_config["checkout"]
    remote = repository_config["remote"]
    git(checkout, "fetch", "--prune", "--no-tags", remote)
    source_revision = git(
        checkout,
        "rev-parse",
        "--verify",
        f"{remote}/{repository_config['baseBranch']}^{{commit}}",
    ).stdout.strip()
    return {
        "schemaVersion": 1,
        "repository": repository,
        "source": {
            "kind": "github-issue",
            "url": expected_url,
            "sha256": admitted_digest,
            "revision": source_revision,
        },
        "tasks": tasks,
        "config": config,
        "masterBody": body,
    }


def campaign_issue(value: Any) -> dict[str, str]:
    issue = object_exact(value, {"number", "url"}, "issue")
    number = required_string(issue.get("number"), "issue.number")
    if not number.isdigit() or number.startswith("0"):
        fail("issue.number must be a positive decimal string")
    url = required_string(issue.get("url"), "issue.url")
    return {"number": number, "url": url}


def subissue_number(task: dict[str, Any]) -> int:
    """The native sub-issue that carries this task's identity and thread."""
    brief = task.get("brief")
    issue = brief.get("issue") if isinstance(brief, dict) else None
    number = issue.get("number") if isinstance(issue, dict) else None
    if not isinstance(number, str) or not number.isdigit() or number.startswith("0"):
        fail(f"task {task.get('id')!r} carries no native sub-issue number")
    return int(number)


def campaign_capabilities(value: Any) -> dict[str, bool]:
    capabilities = object_exact(value, {"subIssueWalk"}, "capabilities")
    return {
        "subIssueWalk": required_bool(
            capabilities.get("subIssueWalk"), "capabilities.subIssueWalk"
        )
    }


def take_capabilities(brief: dict[str, Any]) -> tuple[dict[str, Any], dict[str, bool]]:
    """Split the arm-time capability record off an action brief.

    Every action validates its brief against an exact key set, and a campaign
    armed before the sub-issue probe existed carries no record at all. Absent
    means degraded, which is the conservative direction: the checkbox
    projection and the per-branch pull-request lookup, exactly as before native
    sub-issues. Re-arming is what turns the native walk on.
    """
    if not isinstance(brief, dict) or "capabilities" not in brief:
        return brief, {"subIssueWalk": False}
    rest = {key: value for key, value in brief.items() if key != "capabilities"}
    return rest, campaign_capabilities(brief["capabilities"])


def diagnosis_marker(campaign: str, issue_number: str, task_id: str, attempt: int) -> str:
    return (
        "<!-- tally:spec-build:diagnosis:v1 "
        f"campaign={campaign} issue={issue_number} task={task_id} attempt={attempt} -->"
    )


def diagnosis_heading(task_id: str, attempt: int) -> str:
    return f"### Machine steering for `{task_id}` (attempt {attempt}/2)"


def retry_marker(campaign: str, issue_number: str, task_id: str, attempt: int) -> str:
    return (
        "<!-- tally:spec-build:retry:v1 "
        f"campaign={campaign} issue={issue_number} task={task_id} attempt={attempt} -->"
    )


def retry_heading(task_id: str, attempt: int) -> str:
    return (
        f"### Machine retry for `{task_id}` "
        f"(campaign fault {attempt}/{MAX_MACHINE_RETRIES})"
    )


def escalation_marker(campaign: str, issue_number: str) -> str:
    return (
        "<!-- tally:spec-build:escalation:v1 "
        f"campaign={campaign} issue={issue_number} -->"
    )


SECRET_TOKEN_PREFIXES = (
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "sk-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxr-",
)
SENSITIVE_LINE_MARKERS = (
    "authorization",
    "bearer",
    "token",
    "secret",
    "password",
    "passwd",
    "credential",
    "credentials",
    "api_key",
    "api-key",
    "apikey",
    "private key",
    "access key",
    "access_key",
    "secret_key",
    "client_secret",
    "client key",
    "cookie",
    "dsn",
    "session_id",
    "sessionid",
)
GIT_OBJECT_ID_LENGTHS = frozenset({40, 64})


def public_token_is_sensitive(value: str) -> bool:
    token = value.strip("'\"`()[]{},;")
    lower = token.lower()
    # Prefixes identify a secret only at the start of a token. Substring
    # matching redacted ordinary campaign words: "task-1" contains "sk-".
    if (
        any(lower.startswith(prefix) for prefix in SECRET_TOKEN_PREFIXES)
        or ((token.startswith("AKIA") or token.startswith("ASIA")) and len(token) >= 16)
        or ("://" in token and ("@" in token or "?" in token))
    ):
        return True
    jwt_parts = token.split(".")
    if len(jwt_parts) == 3 and all(len(part) >= 8 for part in jwt_parts):
        return True
    if len(token) < 32 or not token.isascii():
        return False
    has_lower = any(char.islower() for char in token)
    has_upper = any(char.isupper() for char in token)
    has_digit = any(char.isdigit() for char in token)
    if all(char in "0123456789abcdefABCDEF" for char in token):
        # A bare lowercase 40 or 64 character hex word is a git object id, which
        # steering must be able to name. A hex secret carried by a labelled line
        # ("GITHUB_TOKEN=<40 hex>") is still caught by the line rule below.
        if token == lower and len(token) in GIT_OBJECT_ID_LENGTHS:
            return False
        return True
    token_like = all(char.isalnum() or char in "+/_-=" for char in token)
    return token_like and has_digit and ((has_lower and has_upper) or len(token) >= 40)


def public_line_is_sensitive(lower: str) -> bool:
    # A marker only hides a whole line when it stands in key position, so that
    # "token=<secret>" is redacted while a diagnosis about a token bug survives.
    for marker in SENSITIVE_LINE_MARKERS:
        start = lower.find(marker)
        while start != -1:
            index = start + len(marker)
            while index < len(lower) and lower[index] in "'\"` \t":
                index += 1
            if index < len(lower) and lower[index] in ":=":
                return True
            start = lower.find(marker, start + 1)
    return False


def redact_public_text(value: str) -> tuple[str, bool]:
    output: list[str] = []
    redacted = False
    private_key_block = False
    for line in value.splitlines(keepends=True):
        lower = line.lower()
        if "-----begin " in lower and "private key-----" in lower:
            private_key_block = True
        if private_key_block or public_line_is_sensitive(lower):
            output.append("[redacted sensitive diagnosis line]")
            if line.endswith("\n"):
                output.append("\n")
            redacted = True
        else:
            for chunk in re.split(r"(\s+)", line):
                if chunk and not chunk.isspace() and public_token_is_sensitive(chunk):
                    output.append("[redacted-token]")
                    redacted = True
                else:
                    output.append(chunk)
        if "-----end " in lower and "private key-----" in lower:
            private_key_block = False
    return "".join(output).strip(), redacted


def bound_public_diagnosis(value: str) -> str:
    if len(value) <= MAX_DIAGNOSIS_CHARS:
        return value
    width = MAX_DIAGNOSIS_CHARS - len(PUBLIC_DIAGNOSIS_TRUNCATION)
    return value[:width].rstrip() + PUBLIC_DIAGNOSIS_TRUNCATION


def github_actor() -> str:
    return required_string(
        run(["gh", "api", "user", "--jq", ".login"]).stdout.strip(),
        "authenticated GitHub actor",
    )


def machine_authored(comments: list[Any], actor: str) -> list[dict[str, Any]]:
    return [
        candidate
        for candidate in comments
        if isinstance(candidate, dict)
        and isinstance(candidate.get("user"), dict)
        and isinstance(candidate["user"].get("login"), str)
        and candidate["user"]["login"].casefold() == actor.casefold()
    ]


def github_machine_comments(repository: str, issue_number: str) -> list[dict[str, Any]]:
    actor = github_actor()
    viewed = run(
        [
            "gh",
            "api",
            "--paginate",
            "--slurp",
            f"repos/{repository}/issues/{issue_number}/comments?per_page=100",
        ]
    )
    try:
        pages = json.loads(viewed.stdout)
    except json.JSONDecodeError as error:
        fail(f"gh issue comments returned invalid JSON: {error}")
    if not isinstance(pages, list) or any(not isinstance(page, list) for page in pages):
        fail("gh issue comments pagination must return arrays")
    return machine_authored([candidate for page in pages for candidate in page], actor)


def state_scope(campaign: str, issue_number: str) -> str:
    return hashlib.sha256(f"{campaign}\0{issue_number}".encode()).hexdigest()[:24]


def local_state_prefix(campaign: str, issue_number: str) -> str:
    return f"refs/tally/spec-build/v1/{state_scope(campaign, issue_number)}"


def local_remote_refs(config: dict[str, Any], pattern: str) -> dict[str, str]:
    checkout: Path = config["checkout"]
    viewed = git(checkout, "ls-remote", config["remote"], pattern)
    refs: dict[str, str] = {}
    for line in viewed.stdout.splitlines():
        fields = line.split("\t", 1)
        if len(fields) != 2 or not re.fullmatch(r"[0-9a-f]{40,64}", fields[0]):
            fail("local forge returned a malformed state ref")
        refs[fields[1]] = fields[0]
    return refs


def read_local_blob(config: dict[str, Any], ref: str) -> dict[str, Any]:
    checkout: Path = config["checkout"]
    git(checkout, "fetch", "--quiet", config["remote"], ref)
    content = git(checkout, "cat-file", "blob", "FETCH_HEAD").stdout
    try:
        value = json.loads(content)
    except json.JSONDecodeError as error:
        fail(f"local forge state {ref!r} is invalid JSON: {error}")
    if not isinstance(value, dict):
        fail(f"local forge state {ref!r} must contain an object")
    return value


def write_local_blob(
    config: dict[str, Any], ref: str, value: dict[str, Any]
) -> tuple[bool, dict[str, Any]]:
    existing = local_remote_refs(config, ref)
    if ref in existing:
        return False, read_local_blob(config, ref)
    rendered = json.dumps(value, sort_keys=True, separators=(",", ":"))
    checkout: Path = config["checkout"]
    object_id = required_string(
        git(checkout, "hash-object", "-w", "--stdin", input_text=rendered).stdout.strip(),
        "local forge state object",
    )
    git(checkout, "push", "--quiet", config["remote"], f"{object_id}:{ref}")
    return True, value


def contiguous_receipts(
    receipts: list[dict[str, Any]], kind: str, warnings: list[str]
) -> list[dict[str, Any]]:
    """Keep the attempt-1-anchored run of receipts and witness what it drops.

    An operator who deletes a machine comment between passes leaves a gap. The
    campaign reports the gap and reasons from the receipts that remain instead
    of refusing to reconcile at all.
    """
    kept: list[dict[str, Any]] = []
    for task_id in sorted({receipt["taskId"] for receipt in receipts}):
        owned = sorted(
            (receipt for receipt in receipts if receipt["taskId"] == task_id),
            key=lambda receipt: receipt["attempt"],
        )
        expected = 1
        for receipt in owned:
            if receipt["attempt"] != expected:
                warnings.append(
                    f"dropped machine {kind} for {task_id!r} attempt "
                    f"{receipt['attempt']}: no attempt {expected} receipt precedes it"
                )
                continue
            kept.append(receipt)
            expected += 1
    return kept


def forge_campaign_state(
    repository: str,
    config: dict[str, Any],
    campaign: str,
    issue_number: str,
    task_ids: set[str] | None = None,
    threads: dict[str, list[dict[str, Any]]] | None = None,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], str | None, list[str]]:
    diagnoses: list[dict[str, Any]] = []
    retries: list[dict[str, Any]] = []
    escalations: list[str] = []
    warnings: list[str] = []
    threaded = set(threads or {})

    def accept(kind: str, task_id: str) -> bool:
        # A worklist edit that renames or drops a task leaves receipts naming a
        # task the campaign no longer has. campaigns.md invites those edits
        # between passes, so an orphan receipt is reported and ignored rather
        # than treated as forgery.
        if task_ids is None or task_id in task_ids:
            return True
        warnings.append(
            f"dropped machine {kind} for {task_id!r}: the worklist no longer names that task"
        )
        return False

    def ingest(
        comments: list[dict[str, Any]],
        *,
        surface: str | None,
    ) -> None:
        """Parse machine receipts from one posting surface.

        `surface` is the task whose sub-issue thread these comments came from,
        or None for the campaign master. Receipt parsing follows the posting
        surface: once a task owns a thread, a master receipt naming it is a
        pre-#307 leftover, reported and ignored rather than double-counted.
        """
        expected_escalation = escalation_marker(campaign, issue_number)
        for comment in comments:
            body = comment.get("body")
            if not isinstance(body, str) or not body:
                continue
            first_line = body.splitlines()[0]
            match = DIAGNOSIS_MARKER.fullmatch(first_line)
            retry_match = None if match is not None else RETRY_MARKER.fullmatch(first_line)
            if match is not None or retry_match is not None:
                kind = "diagnosis" if match is not None else "retry"
                groups = (match or retry_match).groups()
                marker_campaign, marker_issue, task_id, attempt_text = groups
                if marker_campaign != campaign or marker_issue != issue_number:
                    continue
                if surface is None and task_id in threaded:
                    warnings.append(
                        f"ignored a master-thread machine {kind} for {task_id!r}: "
                        "task receipts live on the task sub-issue thread"
                    )
                    continue
                if surface is not None and task_id != surface:
                    warnings.append(
                        f"ignored a machine {kind} for {task_id!r} found on the "
                        f"sub-issue thread of {surface!r}"
                    )
                    continue
                if not accept(kind, task_id):
                    continue
                attempt = int(attempt_text)
                heading = (
                    diagnosis_heading(task_id, attempt)
                    if kind == "diagnosis"
                    else retry_heading(task_id, attempt)
                )
                prefix = f"{first_line}\n\n{heading}\n\n"
                if not body.startswith(prefix):
                    fail(f"machine {kind} for {task_id!r} has malformed content")
                payload = "diagnosis" if kind == "diagnosis" else "reason"
                (diagnoses if kind == "diagnosis" else retries).append(
                    {
                        "taskId": task_id,
                        "attempt": attempt,
                        "comment": required_string(
                            comment.get("html_url"), f"machine {kind} comment URL"
                        ),
                        payload: required_text(
                            body[len(prefix) :],
                            f"machine {kind} for {task_id!r}",
                            MAX_DIAGNOSIS_CHARS if kind == "diagnosis" else MAX_RETRY_CHARS,
                        ),
                    }
                )
            elif surface is None and first_line == expected_escalation:
                # Escalation is campaign-wide and always stays on the master.
                escalations.append(
                    required_string(comment.get("html_url"), "machine escalation comment URL")
                )

    if config["forge"] == "github":
        ingest(github_machine_comments(repository, issue_number), surface=None)
        for task_id in sorted(threaded):
            ingest(threads[task_id], surface=task_id)
    else:
        prefix = local_state_prefix(campaign, issue_number)
        refs = local_remote_refs(config, f"{prefix}/*")
        diagnosis_prefix = f"{prefix}/diagnosis/"
        retry_prefix = f"{prefix}/retry/"
        for ref in sorted(refs):
            is_retry = ref.startswith(retry_prefix)
            if not is_retry and not ref.startswith(diagnosis_prefix):
                continue
            kind = "retry" if is_retry else "diagnosis"
            payload = "reason" if is_retry else "diagnosis"
            receipt = object_exact(
                read_local_blob(config, ref),
                {
                    "schemaVersion",
                    "kind",
                    "campaign",
                    "issueNumber",
                    "taskId",
                    "attempt",
                    payload,
                    "redaction",
                },
                f"local forge {kind} {ref}",
            )
            if (
                receipt.get("schemaVersion") != 1
                or receipt.get("kind") != kind
                or receipt.get("campaign") != campaign
                or receipt.get("issueNumber") != issue_number
                or receipt.get("redaction") not in PUBLIC_REDACTIONS
            ):
                fail(f"local forge {kind} {ref!r} has invalid identity")
            task_id = required_string(receipt.get("taskId"), f"local {kind} taskId")
            if not TASK_ID.fullmatch(task_id):
                fail(f"local forge {kind} {ref!r} has unsafe taskId")
            attempt = receipt.get("attempt")
            if attempt not in {1, 2}:
                fail(f"local forge {kind} {ref!r} has invalid attempt")
            expected_ref = (
                f"{retry_prefix if is_retry else diagnosis_prefix}{task_id}/{attempt}"
            )
            if ref != expected_ref:
                fail(f"local forge {kind} {ref!r} disagrees with its identity")
            if not accept(kind, task_id):
                continue
            record = {
                "taskId": task_id,
                "attempt": attempt,
                "comment": f"local://{repository}/{ref}",
                payload: required_text(
                    receipt.get(payload),
                    f"local {kind} for {task_id!r}",
                    MAX_RETRY_CHARS if is_retry else MAX_DIAGNOSIS_CHARS,
                ),
            }
            (retries if is_retry else diagnoses).append(record)
        escalation_ref = f"{prefix}/escalation"
        if escalation_ref in refs:
            receipt = object_exact(
                read_local_blob(config, escalation_ref),
                {"schemaVersion", "kind", "campaign", "issueNumber", "body"},
                "local forge escalation",
            )
            if (
                receipt.get("schemaVersion") != 1
                or receipt.get("kind") != "escalation"
                or receipt.get("campaign") != campaign
                or receipt.get("issueNumber") != issue_number
            ):
                fail("local forge escalation has invalid identity")
            required_text(receipt.get("body"), "local forge escalation body", 60_000)
            escalations.append(f"local://{repository}/{escalation_ref}")

    if len(escalations) > 1:
        fail("multiple machine escalations claim this campaign")
    for kind, records in (("diagnosis", diagnoses), ("retry", retries)):
        seen: set[tuple[str, int]] = set()
        for record in records:
            identity = (record["taskId"], record["attempt"])
            if identity in seen:
                fail(
                    f"multiple machine {kind} receipts claim task {identity[0]!r} "
                    f"attempt {identity[1]}"
                )
            seen.add(identity)
    order_source = task_ids or {record["taskId"] for record in diagnoses + retries}
    task_order = {task_id: index for index, task_id in enumerate(sorted(order_source))}
    diagnoses = contiguous_receipts(diagnoses, "diagnosis", warnings)
    retries = contiguous_receipts(retries, "retry", warnings)
    for records in (diagnoses, retries):
        records.sort(
            key=lambda item: (
                task_order.get(item["taskId"], len(task_order)),
                item["attempt"],
            )
        )
    return diagnoses, retries, escalations[0] if escalations else None, warnings


def task_revision(task: dict[str, Any]) -> str | None:
    value = task.get("revision")
    if value is None:
        return None
    value = required_string(value, f"task {task.get('id')} revision")
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", value):
        fail("task revision must be a lowercase SHA-256 identity")
    return value


def pull_request_marker(
    campaign: str, issue_number: str, task_id: str, revision: str | None = None
) -> str:
    if revision is None:
        return (
            "<!-- tally:spec-build:v1 "
            f"campaign={campaign} issue={issue_number} task={task_id} -->"
        )
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", revision):
        fail("pull request marker revision must be a lowercase SHA-256 identity")
    return (
        "<!-- tally:spec-build:v2 "
        f"campaign={campaign} issue={issue_number} task={task_id} revision={revision} -->"
    )


def checkpoint_identity(
    campaign: str, task_id: str, source_sha256: str, base_rev: str
) -> str:
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", source_sha256):
        fail("worklist source digest is not a lowercase SHA-256 identity")
    if not re.fullmatch(r"[0-9a-f]{40,64}", base_rev):
        fail("checkpoint base revision must be a full Git object ID")
    return f"{task_id}-{source_sha256.removeprefix('sha256:')}/{base_rev}"


def checkpoint_ref(
    campaign: str,
    issue_number: str,
    task_id: str,
    source_sha256: str,
    base_rev: str,
) -> str:
    """Where a new checkpoint receipt is published.

    Receipts live in the same hidden namespace as the campaign's other durable
    state. The pre-#307 namespace was `refs/tags/`, which every clone of a
    public target repository auto-fetches: a private campaign's checkpoint
    ledger became part of the target's public surface. Hidden refs are served
    on request and cloned by nobody.
    """
    identity = checkpoint_identity(campaign, task_id, source_sha256, base_rev)
    return f"{local_state_prefix(campaign, issue_number)}/checkpoint/{identity}"


def merge_receipt_ref(
    campaign: str, issue_number: str, task_id: str, revision: str | None = None
) -> str:
    """Where a local-forge squash merge records the commit it produced.

    A `--no-ff` merge proves itself: the published task head becomes an
    ancestor of the base branch. A squash mints a new commit and leaves the
    task head unreachable from base, so the local read path needs the same kind
    of witnessed pointer the GitHub path gets for free from the pull request's
    `mergeCommit` oid. The receipt lives in the campaign's existing hidden ref
    namespace, is named by the same task identity as the publish branch, and
    proves nothing on its own: the reader still requires the commit it names to
    be an ancestor of the witnessed base.
    """
    suffix = "" if revision is None else "-" + revision.removeprefix("sha256:")[:16]
    return f"{local_state_prefix(campaign, issue_number)}/merge/{task_id}{suffix}"


def legacy_checkpoint_tag(
    campaign: str,
    issue_number: str,
    task_id: str,
    source_sha256: str,
    base_rev: str,
) -> str:
    """The visible tag receipt published before the namespace moved.

    Read for compatibility so a campaign already carrying tag receipts is not
    re-executed; never written again.
    """
    identity = checkpoint_identity(campaign, task_id, source_sha256, base_rev)
    readable = re.sub(r"[^a-z0-9]+", "-", campaign.casefold()).strip("-")
    readable = (readable or "campaign")[:24].rstrip("-") or "campaign"
    campaign_identity = hashlib.sha256(campaign.encode()).hexdigest()[:12]
    return (
        "refs/tags/tally/spec-build/v1/"
        f"{readable}-{campaign_identity}-issue-{issue_number}/{identity}"
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


SUBISSUE_WALK_QUERY = """
query($owner: String!, $name: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $name) {
    issue(number: $number) {
      subIssues(first: 50, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          number
          state
          url
          closedByPullRequestsReferences(first: 20, includeClosedPrs: true) {
            pageInfo { hasNextPage }
            nodes {
              url
              body
              merged
              baseRefName
              headRefName
              mergeCommit { oid }
            }
          }
          comments(last: 100) {
            nodes { url body author { login } }
          }
        }
      }
    }
  }
}
"""
# The parent's sub-issue ceiling is 100 and `tally campaign project` caps a
# manifest at that number, so two pages of 50 always cover an admitted graph.
# A third page means the parent grew outside the campaign and the pass refuses
# to reason from a partial walk.
MAX_SUBISSUE_WALK_PAGES = 2
# `first:` returns the oldest references, so a truncated page drops the newest
# closing pull request — the one most likely to be the current proof. Silently
# reading past that would let the walk narrow what counts as proof, which is
# the one thing it must never do, so a full page fails the pass instead.
MAX_SUBISSUE_PULL_REQUESTS = 20


def subissue_walk(repository: str, issue_number: str) -> dict[int, dict[str, Any]]:
    """Read every sub-issue of the campaign parent in one bounded query.

    This narrows *where* completion candidates come from — a pull request has
    to be linked to the task's own sub-issue — and never what counts as proof.
    `pullRequest.merged` plus the revision-bound body marker plus the existing
    base/head/merge-commit validation remain the whole oracle.
    """
    owner, _, name = repository.partition("/")
    actor = github_actor()
    nodes: dict[int, dict[str, Any]] = {}
    cursor: str | None = None
    for _ in range(MAX_SUBISSUE_WALK_PAGES):
        arguments = [
            "api",
            "graphql",
            "-f",
            f"query={SUBISSUE_WALK_QUERY}",
            "-F",
            f"owner={owner}",
            "-F",
            f"name={name}",
            "-F",
            f"number={issue_number}",
        ]
        if cursor is not None:
            arguments.extend(["-F", f"cursor={cursor}"])
        payload = github_json(arguments, "campaign sub-issue walk")
        if not isinstance(payload, dict):
            fail("campaign sub-issue walk did not return an object")
        connection = payload.get("data")
        for key in ("repository", "issue", "subIssues"):
            connection = connection.get(key) if isinstance(connection, dict) else None
        if not isinstance(connection, dict):
            fail("campaign sub-issue walk returned no sub-issue connection")
        page = connection.get("nodes")
        if not isinstance(page, list):
            fail("campaign sub-issue walk returned a malformed node list")
        for index, candidate in enumerate(page):
            if not isinstance(candidate, dict):
                fail(f"campaign sub-issue walk node {index} is not an object")
            number = candidate.get("number")
            if not isinstance(number, int) or isinstance(number, bool) or number < 1:
                fail("campaign sub-issue walk returned an invalid sub-issue number")
            if number in nodes:
                fail(f"campaign sub-issue walk repeated sub-issue #{number}")
            state = candidate.get("state")
            if state not in {"OPEN", "CLOSED"}:
                fail(f"campaign sub-issue #{number} has an unknown state")
            pulls = []
            references = candidate.get("closedByPullRequestsReferences")
            reference_nodes = (
                references.get("nodes") if isinstance(references, dict) else None
            )
            if not isinstance(reference_nodes, list):
                fail(f"campaign sub-issue #{number} returned malformed pull requests")
            reference_page = (
                references.get("pageInfo") if isinstance(references, dict) else None
            )
            if not isinstance(reference_page, dict):
                fail(f"campaign sub-issue #{number} returned no reference page information")
            if reference_page.get("hasNextPage") is True:
                fail(
                    f"campaign sub-issue #{number} links more than "
                    f"{MAX_SUBISSUE_PULL_REQUESTS} closing pull requests; the walk refuses "
                    "to read completion from a truncated reference page"
                )
            for reference in reference_nodes:
                if not isinstance(reference, dict):
                    fail(f"campaign sub-issue #{number} returned a malformed reference")
                pulls.append(reference)
            comments = candidate.get("comments")
            comment_nodes = comments.get("nodes") if isinstance(comments, dict) else None
            if not isinstance(comment_nodes, list):
                fail(f"campaign sub-issue #{number} returned malformed comments")
            machine: list[dict[str, Any]] = []
            for comment in comment_nodes:
                if not isinstance(comment, dict):
                    fail(f"campaign sub-issue #{number} returned a malformed comment")
                author = comment.get("author")
                login = author.get("login") if isinstance(author, dict) else None
                machine.append(
                    {
                        "body": comment.get("body"),
                        "html_url": comment.get("url"),
                        "user": {"login": login} if isinstance(login, str) else None,
                    }
                )
            nodes[number] = {
                "number": number,
                "state": "closed" if state == "CLOSED" else "open",
                "url": candidate.get("url"),
                "pullRequests": pulls,
                "comments": machine_authored(machine, actor),
            }
        page_info = connection.get("pageInfo")
        if not isinstance(page_info, dict):
            fail("campaign sub-issue walk returned no page information")
        if page_info.get("hasNextPage") is not True:
            return nodes
        cursor = required_string(page_info.get("endCursor"), "sub-issue walk cursor")
    fail(
        "campaign parent carries more sub-issues than the 100-task cap admits; "
        "the walk refuses to reconcile from a partial page"
    )
    raise AssertionError("unreachable")


def normalized_pull_request(value: Any) -> dict[str, Any] | None:
    """Project a REST pull request onto the field names the walk returns."""
    if not isinstance(value, dict):
        return None
    base = value.get("base")
    head = value.get("head")
    merged = isinstance(value.get("merged_at"), str)
    state = value.get("state")
    return {
        "url": value.get("html_url"),
        "body": value.get("body"),
        "merged": merged,
        "state": "MERGED" if merged else str(state).upper(),
        "baseRefName": base.get("ref") if isinstance(base, dict) else None,
        "headRefName": head.get("ref") if isinstance(head, dict) else None,
        "headRefOid": head.get("sha") if isinstance(head, dict) else None,
        "mergeCommit": {"oid": value.get("merge_commit_sha")},
    }


def pull_requests_by_head(
    repository: str, branch: str, state: str, limit: int
) -> list[dict[str, Any]]:
    """Look a campaign's stable publish branch up directly on the forge.

    The stable head ref is a durable lookup key. Reading through it, rather
    than through a forge-wide recent-pull-request scan, is what keeps campaign
    proof from ageing out of a window nobody controls.
    """
    owner, _, _ = repository.partition("/")
    listed = github_json(
        [
            "api",
            f"repos/{repository}/pulls?head={owner}:{branch}"
            f"&state={state}&per_page={limit}",
        ],
        f"pull requests for {branch!r}",
    )
    if not isinstance(listed, list):
        fail(f"pull request lookup for {branch!r} must return an array")
    candidates = []
    for value in listed:
        candidate = normalized_pull_request(value)
        if candidate is not None:
            candidates.append(candidate)
    return candidates


def campaign_marker_prefixes(campaign: str, issue_number: str) -> tuple[str, ...]:
    """Every pull-request marker this campaign has ever written, revision-blind.

    Matching the prefix identifies a pull request as *this campaign's own work*
    without saying anything about whether it still proves a task. That is a
    different question from completion, and conflating the two is what made an
    ordinary graph edit look like operator error.
    """
    return (
        f"<!-- tally:spec-build:v1 campaign={campaign} issue={issue_number} task=",
        f"<!-- tally:spec-build:v2 campaign={campaign} issue={issue_number} task=",
    )


def merged_github_tasks(
    repository: str,
    config: dict[str, Any],
    campaign: str,
    issue_number: str,
    base_branch: str,
    base_rev: str | None,
    tasks: list[dict[str, Any]],
    walk: dict[int, dict[str, Any]] | None = None,
) -> tuple[list[dict[str, str]], list[str]]:
    facts: list[dict[str, str]] = []
    warnings: list[str] = []
    claimed_urls: set[str] = set()
    markers = {
        task["id"]: pull_request_marker(
            campaign, issue_number, task["id"], task_revision(task)
        )
        for task in tasks
        if task["kind"] == "implementation"
    }
    revisions = {task["id"]: task_revision(task) for task in tasks}
    prefixes = campaign_marker_prefixes(campaign, issue_number)

    def unnamed_marker_warning(candidate: dict[str, Any]) -> str | None:
        # A pull request carrying this campaign's marker for a revision the
        # witnessed worklist no longer names is proof of nothing. Naming it is
        # how a stale pre-edit pull request stays visible without counting.
        body = candidate.get("body")
        if not isinstance(body, str):
            return None
        if not any(prefix in body for prefix in prefixes):
            return None
        if any(marker in body for marker in markers.values()):
            return None
        url = candidate.get("url")
        identity = url if isinstance(url, str) and url else "an unidentifiable pull request"
        return (
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
        elif base_rev is not None and git(
            config["checkout"],
            "merge-base",
            "--is-ancestor",
            oid,
            base_rev,
            check=False,
        ).returncode:
            problems.append(
                f"has merge commit {oid} outside witnessed base {base_rev}"
            )
        if problems:
            return None, f"ignored {url} for task {task_id!r}: {'; '.join(problems)}"
        return {
            "taskId": task_id,
            "pullRequest": url_value,
            "mergeCommit": oid,
            **({"revision": revisions[task_id]} if revisions[task_id] is not None else {}),
        }, None

    for task in tasks:
        if task["kind"] != "implementation":
            continue
        marker = markers[task["id"]]
        branch = stable_publish_branch(
            campaign, issue_number, task["id"], task_revision(task)
        )
        if walk is None:
            candidates = pull_requests_by_head(repository, branch, "closed", 20)
        else:
            node = walk.get(subissue_number(task))
            if node is None:
                fail(
                    f"campaign sub-issue walk returned no sub-issue for task {task['id']!r}"
                )
            candidates = node["pullRequests"]
        # A closed pull request that never merged proves nothing, in either
        # read path: `merged` stays the only completion oracle.
        candidates = [
            candidate for candidate in candidates if candidate.get("merged") is True
        ]

        valid: list[dict[str, str]] = []
        seen_candidates: set[str] = set()
        for candidate in candidates:
            key = candidate_key(candidate)
            if key in seen_candidates:
                continue
            seen_candidates.add(key)
            if not isinstance(candidate.get("body"), str) or marker not in candidate["body"]:
                warning = unnamed_marker_warning(candidate)
                if warning is not None:
                    warnings.append(warning)
                continue
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


def stable_publish_branch(
    campaign: str, issue_number: str, task_id: str, revision: str | None = None
) -> str:
    campaign_slug = safe_slug(campaign, 32)
    suffix = "" if revision is None else "-" + revision.removeprefix("sha256:")[:16]
    return f"tally/{campaign_slug}-issue-{issue_number}/{task_id}{suffix}"


def merged_local_tasks(
    repository: str,
    config: dict[str, Any],
    campaign: str,
    issue_number: str,
    base_rev: str | None,
    tasks: list[dict[str, Any]],
) -> list[dict[str, str]]:
    checkout: Path = config["checkout"]
    remote = config["remote"]
    git(checkout, "fetch", "--prune", "--no-tags", remote)
    if base_rev is None:
        base_rev = git(
            checkout,
            "rev-parse",
            "--verify",
            f"{remote}/{config['baseBranch']}^{{commit}}",
        ).stdout.strip()
    facts: list[dict[str, str]] = []
    implementations = [task for task in tasks if task["kind"] == "implementation"]
    # One listing serves every task. Squash receipts are the only merged-ness
    # proof a squashed task leaves behind, and a campaign that switched
    # mergeMethod between passes has tasks of both shapes at once, so both
    # proofs are read on every pass rather than gated on the current setting.
    receipts = (
        local_remote_refs(config, f"{local_state_prefix(campaign, issue_number)}/merge/*")
        if implementations
        else {}
    )
    for task in implementations:
        revision = task_revision(task)
        branch = stable_publish_branch(campaign, issue_number, task["id"], revision)
        merge_commit: str | None = None
        receipt = receipts.get(merge_receipt_ref(campaign, issue_number, task["id"], revision))
        if receipt is not None and not git(
            checkout, "merge-base", "--is-ancestor", receipt, base_rev, check=False
        ).returncode:
            merge_commit = receipt
        if merge_commit is None:
            remote_ref = f"refs/remotes/{remote}/{branch}"
            if git(
                checkout, "show-ref", "--verify", "--quiet", remote_ref, check=False
            ).returncode:
                continue
            head = git(
                checkout, "rev-parse", "--verify", f"{remote_ref}^{{commit}}"
            ).stdout.strip()
            if git(
                checkout, "merge-base", "--is-ancestor", head, base_rev, check=False
            ).returncode:
                continue
            merge_commit = head
        facts.append(
            {
                "taskId": task["id"],
                "pullRequest": f"local://{repository}/{branch}",
                "mergeCommit": merge_commit,
                **({"revision": revision} if revision is not None else {}),
            }
        )
    return facts


def completed_checkpoint_tasks(
    config: dict[str, Any],
    campaign: str,
    issue_number: str,
    tasks: list[dict[str, Any]],
    source: dict[str, str],
    merged: list[dict[str, str]],
) -> list[dict[str, str]]:
    checkpoints = [task for task in tasks if task["kind"] == "checkpoint"]
    if not checkpoints:
        return []
    checkout: Path = config["checkout"]
    remote = config["remote"]
    git(checkout, "fetch", "--prune", "--no-tags", remote)
    base_rev = required_string(source.get("revision"), "worklist source revision")
    if not re.fullmatch(r"[0-9a-f]{40,64}", base_rev):
        fail("worklist source revision must be a full Git object ID")
    facts: list[dict[str, str]] = []
    completed_revisions = {fact["taskId"]: fact["mergeCommit"] for fact in merged}
    for task in checkpoints:
        if not all(dependency in completed_revisions for dependency in task["dependencies"]):
            continue
        reference = checkpoint_ref(
            campaign, issue_number, task["id"], source["sha256"], base_rev
        )
        target = remote_ref_oid(checkout, remote, reference)
        if target is None:
            # Compatibility: a campaign that already published a visible tag
            # receipt is honored where it stands, so the namespace move never
            # re-executes a checkpoint that already passed.
            reference = legacy_checkpoint_tag(
                campaign, issue_number, task["id"], source["sha256"], base_rev
            )
            target = remote_ref_oid(checkout, remote, reference)
        if target is None:
            continue
        git(checkout, "fetch", "--no-tags", remote, reference)
        fetched = git(checkout, "rev-parse", "--verify", "FETCH_HEAD^{commit}").stdout.strip()
        if fetched != target or git(checkout, "cat-file", "-t", target).stdout.strip() != "commit":
            fail(f"checkpoint ref {reference!r} must point directly to a commit")
        if target != base_rev:
            fail(f"checkpoint ref {reference!r} does not point to its named base revision")
        for dependency in task["dependencies"]:
            dependency_revision = completed_revisions[dependency]
            if git(
                checkout,
                "merge-base",
                "--is-ancestor",
                dependency_revision,
                target,
                check=False,
            ).returncode:
                fail(
                    f"checkpoint ref {reference!r} does not contain dependency "
                    f"{dependency!r} revision {dependency_revision}"
                )
        facts.append({"taskId": task["id"], "ref": reference, "revision": target})
        completed_revisions[task["id"]] = target
    return facts


def domains_overlap(left: str, right: str) -> bool:
    left_parts = tuple(part.casefold() for part in Path(left).parts)
    right_parts = tuple(part.casefold() for part in Path(right).parts)
    width = min(len(left_parts), len(right_parts))
    return left_parts[:width] == right_parts[:width]


def task_conflicts(task: dict[str, Any], selected: list[dict[str, Any]]) -> bool:
    return any(
        domains_overlap(left, right)
        for other in selected
        for left in task.get("conflictDomains", [])
        for right in other.get("conflictDomains", [])
    )


def sync_issue_checkboxes(
    repository: str,
    issue_number: str,
    body: str,
    tasks: list[dict[str, Any]],
    completed_ids: set[str],
) -> None:
    start_index = body.find(WORKLIST_BEGIN)
    if start_index < 0:
        fail(f"campaign master issue is missing {WORKLIST_BEGIN}")
    content_start = start_index + len(WORKLIST_BEGIN)
    end_index = body.find(WORKLIST_END, content_start)
    if end_index < 0:
        fail(f"campaign master issue is missing {WORKLIST_END}")
    if body.find(WORKLIST_BEGIN, content_start) >= 0 or body.find(
        WORKLIST_END, end_index + len(WORKLIST_END)
    ) >= 0:
        fail("campaign master issue repeats a worklist marker")
    lines = [""]
    for task in tasks:
        state = "x" if task["id"] in completed_ids else " "
        title = task["title"].replace("\r", " ").replace("\n", " ")
        lines.append(
            f"- [{state}] {TASK_MARKER_PREFIX}{task['id']} --> "
            f"#{task['brief']['issue']['number']} — {title}"
        )
    lines.append("")
    content = "\n".join(lines)
    updated = body[:content_start] + content + body[end_index:]
    if updated != body:
        run(
            [
                "gh",
                "issue",
                "edit",
                issue_number,
                "--repo",
                repository,
                "--body-file",
                "-",
            ],
            input_text=updated,
        )


def close_completed_issue_campaign(
    repository: str,
    issue_number: str,
    tasks: list[dict[str, Any]],
) -> None:
    """Close every open task sub-issue, then the campaign issue itself.

    The closing summary is published before this runs, so a reader who opens
    the closed issue finds the digest as its last comment.
    """
    for task in tasks:
        task_issue = campaign_issue(task["brief"].get("issue"))
        viewed = github_json(
            ["api", f"repos/{repository}/issues/{task_issue['number']}"],
            f"campaign task issue {task['id']}",
        )
        if not isinstance(viewed, dict):
            fail(f"campaign task issue {task['id']} did not return an object")
        if viewed.get("state") == "open":
            run(
                [
                    "gh",
                    "issue",
                    "close",
                    task_issue["number"],
                    "--repo",
                    repository,
                ]
            )
    run(["gh", "issue", "close", issue_number, "--repo", repository])


def closing_summary_marker(
    campaign: str, issue_number: str, outcome: str, source_sha256: str
) -> str:
    """The idempotence marker one terminal outcome's summary carries.

    Completion keeps the pre-summary `campaign-complete` marker: the summary
    took over that comment rather than adding a second one beside it, so a
    campaign still ends with exactly one machine comment before it closes.
    """
    if outcome == "complete":
        return f"<!-- tally:campaign-complete:v1 source={source_sha256} -->"
    return (
        "<!-- tally:campaign-summary:v1 "
        f"campaign={campaign} issue={issue_number} outcome={outcome} -->"
    )


def campaign_digest(reconciliation: dict[str, Any], outcome: str) -> dict[str, Any]:
    """One run-scoped digest of a campaign's terminal state.

    Every field is a projection of facts this pass already witnessed -- merged
    pull requests, checkpoint receipts, diagnosis and retry receipts read back
    from the forge, and the reconciler's own set arithmetic. Nothing here reads
    or writes a state store the campaign did not already have.
    """
    titles = {task["id"]: task["title"] for task in reconciliation["tasks"]}
    merged_ids = {fact["taskId"] for fact in reconciliation["merged"]}
    checkpoint_ids = {fact["taskId"] for fact in reconciliation["checkpoints"]}
    blocked = {fact["taskId"]: fact["blockedBy"] for fact in reconciliation["blocked"]}
    attempts: dict[str, int] = {}
    for diagnosis in reconciliation["diagnoses"]:
        attempts[diagnosis["taskId"]] = max(
            attempts.get(diagnosis["taskId"], 0), diagnosis["attempt"]
        )
    return {
        "schemaVersion": 1,
        "campaign": reconciliation["campaign"],
        "repository": reconciliation["repository"],
        "outcome": outcome,
        "source": reconciliation["source"],
        "taskCount": len(reconciliation["tasks"]),
        "merged": [
            {
                "taskId": fact["taskId"],
                "title": titles.get(fact["taskId"], fact["taskId"]),
                "pullRequest": fact["pullRequest"],
                "mergeCommit": fact["mergeCommit"],
            }
            for fact in reconciliation["merged"]
        ],
        "checkpoints": [
            {
                "taskId": fact["taskId"],
                "title": titles.get(fact["taskId"], fact["taskId"]),
                "revision": fact["revision"],
            }
            for fact in reconciliation["checkpoints"]
        ],
        "blocked": [
            {
                "taskId": task_id,
                "title": titles.get(task_id, task_id),
                "blockedBy": blocked[task_id],
                "attempts": attempts.get(task_id, 0),
            }
            for task_id in reconciliation["remaining"]
            if task_id in blocked
        ],
        "outstanding": [
            task_id
            for task_id in reconciliation["remaining"]
            if task_id not in blocked and task_id not in merged_ids | checkpoint_ids
        ],
        "steering": [
            {
                "taskId": diagnosis["taskId"],
                "attempt": diagnosis["attempt"],
                "summary": compact_summary(diagnosis["diagnosis"], 160),
            }
            for diagnosis in reconciliation["diagnoses"]
        ],
        "retries": [
            {
                "taskId": retry["taskId"],
                "attempt": retry["attempt"],
                "summary": compact_summary(retry["reason"], 160),
            }
            for retry in reconciliation["retries"]
        ],
        "deferrals": [deferral["taskId"] for deferral in reconciliation["deferrals"]],
        "anomalies": [anomaly["detail"] for anomaly in reconciliation["anomalies"]],
        "warnings": list(reconciliation["warnings"]),
    }


def summary_rows(rows: list[Any], render: Any, limit: int = MAX_SUMMARY_ROWS) -> list[str]:
    """Render a bounded list and say plainly when it was truncated."""
    lines = [render(row) for row in rows[:limit]]
    if len(rows) > limit:
        lines.append(f"- …and {len(rows) - limit} more")
    return lines


def render_campaign_summary(digest: dict[str, Any]) -> str:
    """The closing summary a reader of the campaign issue actually gets."""
    complete = digest["outcome"] == "complete"
    heading = (
        "### Campaign complete"
        if complete
        else "### Campaign closed at frontier quiescence"
    )
    settled = len(digest["merged"]) + len(digest["checkpoints"])
    lines = [
        heading,
        "",
        f"Worklist `{digest['source']['sha256']}` at `{digest['source']['revision']}`.",
        f"{settled} of {digest['taskCount']} task(s) are bound to durable "
        "merge/checkpoint facts.",
        "",
    ]
    if not complete:
        lines.extend(
            [
                f"Blocked: {len(digest['blocked'])} · "
                f"Outstanding: {len(digest['outstanding'])} · "
                f"Steering notes issued: {len(digest['steering'])} · "
                f"Machinery retries: {len(digest['retries'])}",
                "",
            ]
        )
    if digest["merged"]:
        lines.extend(["#### Merged", ""])
        lines.extend(
            summary_rows(
                digest["merged"],
                lambda fact: (
                    f"- `{fact['taskId']}` — {compact_summary(fact['title'], 80)} "
                    f"({fact['pullRequest']})"
                ),
            )
        )
        lines.append("")
    if digest["checkpoints"]:
        lines.extend(["#### Checkpoints passed", ""])
        lines.extend(
            summary_rows(
                digest["checkpoints"],
                lambda fact: (
                    f"- `{fact['taskId']}` — {compact_summary(fact['title'], 80)} "
                    f"at `{fact['revision']}`"
                ),
            )
        )
        lines.append("")
    if digest["blocked"]:
        lines.extend(["#### Blocked", ""])
        lines.extend(
            summary_rows(
                digest["blocked"],
                lambda fact: (
                    f"- `{fact['taskId']}` — {compact_summary(fact['title'], 80)}; "
                    f"blocked by {', '.join(f'`{item}`' for item in fact['blockedBy'])}; "
                    f"{fact['attempts']} steered attempt(s)"
                ),
            )
        )
        lines.append("")
    if digest["outstanding"]:
        lines.extend(["#### Not attempted", ""])
        lines.extend(
            summary_rows(digest["outstanding"], lambda task_id: f"- `{task_id}`")
        )
        lines.append("")
    if digest["deferrals"]:
        lines.extend(["#### Checkpoints deferred by outstanding work", ""])
        lines.extend(
            summary_rows(digest["deferrals"], lambda task_id: f"- `{task_id}`")
        )
        lines.append("")
    if digest["steering"]:
        lines.extend(["#### Steering notes issued", ""])
        lines.extend(
            summary_rows(
                digest["steering"],
                lambda note: (
                    f"- `{note['taskId']}` attempt {note['attempt']}: {note['summary']}"
                ),
            )
        )
        lines.append("")
    if digest["retries"]:
        lines.extend(["#### Campaign machinery faults", ""])
        lines.extend(
            summary_rows(
                digest["retries"],
                lambda retry: (
                    f"- `{retry['taskId']}` fault {retry['attempt']}: {retry['summary']}"
                ),
            )
        )
        lines.append("")
    if digest["anomalies"]:
        lines.extend(["#### Anomalies", ""])
        lines.extend(summary_rows(digest["anomalies"], lambda detail: f"- {detail}"))
        lines.append("")
    if digest["warnings"]:
        lines.extend(["#### Reconciler warnings", ""])
        lines.extend(
            summary_rows(
                digest["warnings"], lambda warning: f"- {compact_summary(warning, 200)}", 12
            )
        )
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def publish_closing_summary(
    repository: str,
    config: dict[str, Any],
    campaign: str,
    issue_number: str,
    digest: dict[str, Any],
) -> str:
    """Post one closing summary on the terminal path that produced this digest.

    Always a fresh comment, never an upsert: a summary the operator does not
    get notified about is not a summary. The marker makes a repeated terminal
    pass idempotent rather than chatty.
    """
    outcome = digest["outcome"]
    marker = closing_summary_marker(
        campaign, issue_number, outcome, digest["source"]["sha256"]
    )
    body = f"{marker}\n\n{render_campaign_summary(digest)}"
    if len(body) > 60_000:
        fail("campaign closing summary exceeds the bounded GitHub comment size")
    if config["forge"] == "github":
        comments = run(
            [
                "gh",
                "api",
                "--paginate",
                f"repos/{repository}/issues/{issue_number}/comments?per_page=100",
                "--jq",
                ".[] | .body",
            ]
        ).stdout
        if marker in comments:
            return f"https://github.com/{repository}/issues/{issue_number}"
        posted = run(
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
        return required_string(
            posted.stdout.strip().splitlines()[-1] if posted.stdout.strip() else "",
            "campaign closing summary comment URL",
        )
    ref = f"{local_state_prefix(campaign, issue_number)}/summary/{outcome}"
    write_local_blob(
        config,
        ref,
        {
            "schemaVersion": 1,
            "kind": "closing-summary",
            "campaign": campaign,
            "issueNumber": issue_number,
            "outcome": outcome,
            "body": body,
        },
    )
    return f"local://{repository}/{ref}"


def parallelism_warnings(
    ready: list[dict[str, Any]], frontier: list[dict[str, Any]], max_parallel: int
) -> list[str]:
    """Explain a ready frontier that conflict domains, rather than demand, underfill."""
    if len(ready) < max_parallel or len(frontier) >= max_parallel:
        return []
    selected_ids = {task["id"] for task in frontier}
    blocked = [task for task in ready if task["id"] not in selected_ids]
    examples: list[str] = []
    for task in blocked:
        found = False
        for other in frontier:
            for left in task.get("conflictDomains", []):
                for right in other.get("conflictDomains", []):
                    if domains_overlap(left, right):
                        examples.append(
                            f"{task['id']}:{json.dumps(left)} overlaps "
                            f"{other['id']}:{json.dumps(right)}"
                        )
                        found = True
                        break
                if found:
                    break
            if found:
                break
        if len(examples) == 8:
            break
    blocked_ids = ", ".join(task["id"] for task in blocked[:12])
    if len(blocked) > 12:
        blocked_ids += f", and {len(blocked) - 12} more"
    detail = "; ".join(examples) if examples else "no overlap example available"
    return [
        f"conflictDomains limited this ready frontier to {len(frontier)} of requested "
        f"maxParallel {max_parallel}; blocked tasks: {blocked_ids}; overlaps: {detail}"
    ]


def related_tasks(tasks: list[dict[str, Any]], task_id: str) -> set[str]:
    """Every task the named task depends on, plus every task that depends on it."""
    dependencies = {task["id"]: set(task["dependencies"]) for task in tasks}
    related = {task_id}
    frontier = [task_id]
    while frontier:
        current = frontier.pop()
        for candidate in dependencies.get(current, set()):
            if candidate not in related:
                related.add(candidate)
                frontier.append(candidate)
    frontier = [task_id]
    while frontier:
        current = frontier.pop()
        for task in tasks:
            if current in dependencies[task["id"]] and task["id"] not in related:
                related.add(task["id"])
                frontier.append(task["id"])
    return related


def checkpoint_deferrals(
    tasks: list[dict[str, Any]],
    remaining: list[dict[str, Any]],
    completed_ids: set[str],
    blocked_ids: set[str],
) -> list[dict[str, Any]]:
    """Name the checkpoints whose verdict outstanding unrelated work can still change.

    A checkpoint reads the accumulated tree, so a red verdict while an unrelated
    implementation task is still live says nothing about the checkpoint itself.
    Such a run is a deferral, not a failed attempt. Tasks that are blocked, or
    that sit on either side of the checkpoint's dependency chain, cannot change
    the verdict and so never defer it: the campaign still reaches quiescence.
    """
    deferrals: list[dict[str, Any]] = []
    for task in tasks:
        if task["kind"] != "checkpoint" or task["id"] in completed_ids:
            continue
        related = related_tasks(tasks, task["id"])
        waiting = [
            candidate["id"]
            for candidate in remaining
            if candidate["kind"] != "checkpoint"
            and candidate["id"] not in related
            and candidate["id"] not in blocked_ids
        ]
        if waiting:
            deferrals.append({"taskId": task["id"], "waitingOn": waiting})
    return deferrals


def closed_by_this_campaign(node: dict[str, Any], prefixes: tuple[str, ...]) -> bool:
    """Did this campaign's own merged pull request close the sub-issue?

    A task PR carries `Closes #<sub-issue>`, so the campaign closes its own
    sub-issues as it merges. When a later graph edit rotates every task
    revision, those merged pull requests stop proving anything — but the
    closure they caused is still the campaign's, not a human's. The marker
    prefix is revision-blind on purpose: it answers "did we do this?", which is
    exactly the question the anomaly must not get wrong.
    """
    for candidate in node["pullRequests"]:
        body = candidate.get("body")
        if candidate.get("merged") is True and isinstance(body, str):
            if any(prefix in body for prefix in prefixes):
                return True
    return False


def closed_subissue_anomalies(
    walk: dict[int, dict[str, Any]],
    tasks: list[dict[str, Any]],
    completed_ids: set[str],
    prefixes: tuple[str, ...],
) -> list[dict[str, Any]]:
    """A sub-issue closed *by hand* with no merged proof is a loud anomaly.

    A sub-issue is human-clickable, so closure carries no authority at all;
    `pullRequest.merged` (or the checkpoint completion ref) stays the only
    oracle. The task remains incomplete and the closure is surfaced rather than
    filed as a reconciler warning nobody reads.

    A closure the campaign caused itself is not that signal. Editing one task
    brief and re-arming rotates every task's revision, so every already-merged
    task simultaneously loses its proof and keeps a sub-issue the campaign
    closed. Reporting those as operator error would fire the loudest surface in
    the status verb on the campaign's own documented workflow, once per merged
    task, exactly when the board most needs to stay readable. Those pull
    requests are already named in the reconciler's ignored-marker warnings.
    """
    anomalies: list[dict[str, Any]] = []
    for task in tasks:
        if task["id"] in completed_ids:
            continue
        node = walk.get(subissue_number(task))
        if node is None or node["state"] != "closed":
            continue
        if closed_by_this_campaign(node, prefixes):
            continue
        anomalies.append(
            {
                "kind": "closed-without-merged-proof",
                "taskId": task["id"],
                "issue": str(node["number"]),
                "url": task["brief"]["issue"]["url"],
                "detail": (
                    f"sub-issue #{node['number']} is closed but task {task['id']!r} "
                    "holds no revision-valid merged pull request; closing a sub-issue "
                    "by hand does not complete a task"
                ),
            }
        )
    return anomalies


def action_reconcile(brief: dict[str, Any]) -> dict[str, Any]:
    brief, capabilities = take_capabilities(brief)
    forge_native = isinstance(brief.get("worklist"), dict)
    if forge_native:
        data = object_exact(brief, {"repository", "issue", "worklist"}, "reconcile brief")
        worklist = issue_graph_worklist(data)
        campaign = worklist["config"]["campaign"]
        issue = campaign_issue(data.get("issue"))
        repository = worklist["repository"]
        config = repo_config(worklist["config"]["repositoryConfig"])
        max_parallel = worklist["config"]["maxParallel"]
    else:
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
        max_parallel = data["maxParallel"]
    base_rev = required_string(
        worklist["source"].get("revision"), "worklist source revision"
    )
    # One bounded walk per pass feeds both halves of the forge read: which
    # pull requests may be considered, and which machine receipts each task
    # thread carries.
    walk = (
        subissue_walk(repository, issue["number"])
        if forge_native and config["forge"] == "github" and capabilities["subIssueWalk"]
        else None
    )
    if config["forge"] == "github":
        merged, warnings = merged_github_tasks(
            repository,
            config,
            campaign,
            issue["number"],
            config["baseBranch"],
            base_rev,
            worklist["tasks"],
            walk,
        )
    else:
        merged = merged_local_tasks(
            repository,
            config,
            campaign,
            issue["number"],
            base_rev,
            worklist["tasks"],
        )
        warnings = []
    checkpoints = completed_checkpoint_tasks(
        config,
        campaign,
        issue["number"],
        worklist["tasks"],
        worklist["source"],
        merged,
    )
    merged_ids = {fact["taskId"] for fact in merged}
    completed_ids = {fact["taskId"] for fact in merged + checkpoints}
    task_ids = {task["id"] for task in worklist["tasks"]}
    anomalies = (
        closed_subissue_anomalies(
            walk,
            worklist["tasks"],
            completed_ids,
            campaign_marker_prefixes(campaign, issue["number"]),
        )
        if walk is not None
        else []
    )
    threads = (
        {
            task["id"]: walk[subissue_number(task)]["comments"]
            for task in worklist["tasks"]
            if subissue_number(task) in walk
        }
        if walk is not None
        else None
    )
    diagnoses, retries, escalation, state_warnings = forge_campaign_state(
        repository,
        config,
        campaign,
        issue["number"],
        task_ids,
        threads,
    )
    warnings.extend(state_warnings)
    order = {task["id"]: index for index, task in enumerate(worklist["tasks"])}
    diagnoses.sort(key=lambda item: (order[item["taskId"]], item["attempt"]))
    retries.sort(key=lambda item: (order[item["taskId"]], item["attempt"]))
    remaining = [task for task in worklist["tasks"] if task["id"] not in completed_ids]
    direct_blocked = {
        diagnosis["taskId"]
        for diagnosis in diagnoses
        if diagnosis["attempt"] == 2 and diagnosis["taskId"] not in completed_ids
    }
    blocked_by: dict[str, set[str]] = {}
    blocked: list[dict[str, Any]] = []
    for task in worklist["tasks"]:
        roots = {task["id"]} if task["id"] in direct_blocked else set()
        for dependency in task["dependencies"]:
            roots.update(blocked_by.get(dependency, set()))
        blocked_by[task["id"]] = roots
        if task["id"] not in completed_ids and roots:
            blocked.append({"taskId": task["id"], "blockedBy": sorted(roots, key=order.get)})
    blocked_ids = {fact["taskId"] for fact in blocked}
    ready = [
        task
        for task in remaining
        if task["id"] not in blocked_ids
        and all(dependency in completed_ids for dependency in task["dependencies"])
    ]
    deferrals = checkpoint_deferrals(worklist["tasks"], remaining, completed_ids, blocked_ids)
    deferred_ids = {deferral["taskId"] for deferral in deferrals}
    # A checkpoint whose verdict can still be changed by unrelated outstanding
    # work never displaces that work from a bounded frontier.
    ready.sort(key=lambda task: task["id"] in deferred_ids)
    frontier: list[dict[str, Any]] = []
    for task in ready:
        if len(frontier) == max_parallel:
            break
        if not task_conflicts(task, frontier):
            frontier.append(task)
    if forge_native and walk is None:
        # Native sub-issues make the parent's own progress bar the projection,
        # so tally stops writing one. Without that capability the recomputed
        # checkbox list is still the only progress a reader gets.
        sync_issue_checkboxes(
            repository,
            issue["number"],
            worklist["masterBody"],
            worklist["tasks"],
            completed_ids,
        )
    warnings.extend(parallelism_warnings(ready, frontier, max_parallel))
    result: dict[str, Any] = {
        "schemaVersion": 1,
        "campaign": campaign,
        "repository": repository,
        "source": worklist["source"],
        "tasks": worklist["tasks"],
        "merged": merged,
        "checkpoints": checkpoints,
        "remaining": [task["id"] for task in remaining],
        "frontier": frontier,
        "diagnoses": diagnoses,
        "retries": retries,
        "deferrals": deferrals,
        "blocked": blocked,
        "quiescent": bool(remaining) and not frontier,
        "escalation": escalation,
        "complete": not remaining,
        "anomalies": anomalies,
        "warnings": warnings,
        "closingSummary": None,
    }
    if forge_native:
        result["config"] = worklist["config"]
    if not remaining:
        # Completion is one of the campaign's two terminal outcomes, so the
        # digest is rendered here, inside the node that already owned closing
        # the issue. No new flow node exists for it.
        result["closingSummary"] = publish_closing_summary(
            repository,
            config,
            campaign,
            issue["number"],
            campaign_digest(result, "complete"),
        )
        if forge_native:
            close_completed_issue_campaign(
                repository,
                issue["number"],
                worklist["tasks"],
            )
    return result


def action_diff(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(brief, {"repositoryConfig", "workspace"}, "diff brief")
    repo_config(data.get("repositoryConfig"))
    workspace = object_exact(
        data.get("workspace"),
        {"taskId", "baseRev", "branch", "publishBranch", "worktreePath"},
        "workspace",
    )
    task_id = required_string(workspace.get("taskId"), "workspace.taskId")
    if not TASK_ID.fullmatch(task_id):
        fail("workspace.taskId is not safe")
    base_rev = required_string(workspace.get("baseRev"), "workspace.baseRev")
    if not re.fullmatch(r"[0-9a-f]{40,64}", base_rev):
        fail("workspace.baseRev must be a full Git object ID")
    branch = required_string(workspace.get("branch"), "workspace.branch")
    required_string(workspace.get("publishBranch"), "workspace.publishBranch")
    worktree = Path(required_string(workspace.get("worktreePath"), "workspace.worktreePath"))
    if not worktree.is_absolute():
        fail("workspace.worktreePath must be absolute")
    if not worktree.is_dir():
        return {
            "taskId": task_id,
            "available": False,
            "baseRev": base_rev,
            "head": None,
            "status": "",
            "patch": "",
            "truncated": False,
            "reason": "prepared worktree is no longer available",
        }
    git(worktree, "rev-parse", "--git-dir")
    actual_branch = git(worktree, "branch", "--show-current").stdout.strip()
    if actual_branch != branch:
        fail(f"diff worktree is on branch {actual_branch!r}, expected {branch!r}")
    head = git(worktree, "rev-parse", "HEAD").stdout.strip()
    status = git(worktree, "status", "--short", "--untracked-files=all").stdout
    chunks = [git(worktree, "diff", "--binary", "--no-ext-diff", base_rev, "--").stdout]
    untracked = git(
        worktree, "ls-files", "--others", "--exclude-standard", "-z"
    ).stdout.split("\0")
    for relative in (path for path in untracked if path):
        untracked_diff = git(
            worktree,
            "diff",
            "--no-index",
            "--binary",
            "--",
            "/dev/null",
            f"./{relative}",
            check=False,
        )
        if untracked_diff.returncode not in {0, 1}:
            detail = untracked_diff.stderr.strip() or "no output"
            fail(f"cannot capture untracked diff for {relative!r}: {detail}")
        chunks.append(untracked_diff.stdout)
    patch = "".join(chunks)
    truncated = len(patch) > MAX_DIFF_CHARS
    if truncated:
        patch = patch[:MAX_DIFF_CHARS] + "\n[... diff truncated ...]\n"
    if len(status) > 16_000:
        status = status[:16_000] + "\n[... status truncated ...]\n"
    return {
        "taskId": task_id,
        "available": True,
        "baseRev": base_rev,
        "head": head,
        "status": status,
        "patch": patch,
        "truncated": truncated,
        "reason": None,
    }


def steering_thread(
    repository: str,
    config: dict[str, Any],
    data: dict[str, Any],
    capabilities: dict[str, bool],
    task_id: str,
) -> tuple[str, dict[str, list[dict[str, Any]]] | None]:
    """Where this task's machine receipts are posted and read back.

    A native campaign gives every task its own sub-issue thread: the diagnosis
    for task T lands on T's sub-issue and T's retry brief reads it back from
    there. The master stays the campaign-wide channel.
    """
    issue = campaign_issue(data.get("issue"))
    task_issue = data.get("taskIssue")
    if not capabilities["subIssueWalk"] or task_issue is None:
        return issue["number"], None
    thread = campaign_issue(task_issue)
    if config["forge"] != "github":
        return thread["number"], None
    return thread["number"], {
        task_id: github_machine_comments(repository, thread["number"])
    }


def action_steer(brief: dict[str, Any]) -> dict[str, Any]:
    brief, capabilities = take_capabilities(brief)
    fields = {
        "campaign",
        "repository",
        "repositoryConfig",
        "issue",
        "taskId",
        "attempt",
        "diagnosis",
    }
    if "taskIssue" in brief:
        fields.add("taskIssue")
    data = object_exact(brief, fields, "steer brief")
    campaign = required_string(data.get("campaign"), "campaign")
    if not COMPONENT.fullmatch(campaign):
        fail("campaign is not a safe component")
    repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(repository):
        fail("repository must use owner/name form")
    config = repo_config(data.get("repositoryConfig"))
    issue = campaign_issue(data.get("issue"))
    task_id = required_string(data.get("taskId"), "taskId")
    if not TASK_ID.fullmatch(task_id):
        fail("taskId is not safe")
    attempt = data.get("attempt")
    if attempt not in {1, 2}:
        fail("attempt must equal 1 or 2")
    thread_number, threads = steering_thread(
        repository, config, data, capabilities, task_id
    )
    existing, _, _, _ = forge_campaign_state(
        repository, config, campaign, issue["number"], None, threads
    )
    task_receipts = [receipt for receipt in existing if receipt["taskId"] == task_id]
    if any(receipt["attempt"] == attempt for receipt in task_receipts):
        receipt = next(
            receipt for receipt in task_receipts if receipt["attempt"] == attempt
        )
        return {
            "taskId": task_id,
            "attempt": attempt,
            "comment": receipt["comment"],
            "blocked": attempt == 2,
            "posted": False,
            "redacted": False,
        }
    expected_attempt = len(task_receipts) + 1
    if attempt != expected_attempt:
        fail(
            f"task {task_id!r} diagnosis attempt {attempt} is not next after "
            f"{len(task_receipts)} forge receipts"
        )
    diagnosis = required_text(
        data.get("diagnosis"), "diagnosis", MAX_DIAGNOSIS_CHARS
    )
    diagnosis, redacted = redact_public_text(diagnosis)
    diagnosis = bound_public_diagnosis(diagnosis)
    marker = diagnosis_marker(campaign, issue["number"], task_id, attempt)
    body = f"{marker}\n\n{diagnosis_heading(task_id, attempt)}\n\n{diagnosis}"
    if config["forge"] == "github":
        posted = run(
            [
                "gh",
                "issue",
                "comment",
                thread_number,
                "--repo",
                repository,
                "--body",
                body,
            ]
        )
        comment = required_string(
            posted.stdout.strip().splitlines()[-1] if posted.stdout.strip() else "",
            "machine diagnosis comment URL",
        )
    else:
        ref = (
            f"{local_state_prefix(campaign, issue['number'])}/diagnosis/"
            f"{task_id}/{attempt}"
        )
        created, _ = write_local_blob(
            config,
            ref,
            {
                "schemaVersion": 1,
                "kind": "diagnosis",
                "campaign": campaign,
                "issueNumber": issue["number"],
                "taskId": task_id,
                "attempt": attempt,
                "diagnosis": diagnosis,
                "redaction": PUBLIC_REDACTION,
            },
        )
        if not created:
            fail(f"local forge diagnosis {ref!r} appeared concurrently")
        comment = f"local://{repository}/{ref}"
    return {
        "taskId": task_id,
        "attempt": attempt,
        "comment": comment,
        "blocked": attempt == 2,
        "posted": True,
        "redacted": redacted,
    }


def action_retry(brief: dict[str, Any]) -> dict[str, Any]:
    """Record a campaign machinery fault without spending a steering attempt.

    Prep, lane, rebase, merge and publish faults say nothing about whether the
    task's work is right, so they buy a bounded retry instead. The budget is
    read back from the forge like every other campaign fact; once it is spent
    the caller must treat the next fault as a failed attempt.
    """
    brief, capabilities = take_capabilities(brief)
    fields = {
        "campaign",
        "repository",
        "repositoryConfig",
        "issue",
        "taskId",
        "stage",
        "detail",
    }
    if "taskIssue" in brief:
        fields.add("taskIssue")
    data = object_exact(brief, fields, "retry brief")
    campaign = required_string(data.get("campaign"), "campaign")
    if not COMPONENT.fullmatch(campaign):
        fail("campaign is not a safe component")
    repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(repository):
        fail("repository must use owner/name form")
    config = repo_config(data.get("repositoryConfig"))
    issue = campaign_issue(data.get("issue"))
    task_id = required_string(data.get("taskId"), "taskId")
    if not TASK_ID.fullmatch(task_id):
        fail("taskId is not safe")
    stage = required_string(data.get("stage"), "stage")
    if not re.fullmatch(r"[a-z][a-z0-9:._-]{0,63}", stage):
        fail("stage is not a safe campaign stage name")
    detail = required_text(data.get("detail"), "detail", MAX_RETRY_CHARS)
    thread_number, threads = steering_thread(
        repository, config, data, capabilities, task_id
    )
    _, existing, _, _ = forge_campaign_state(
        repository, config, campaign, issue["number"], None, threads
    )
    spent = len([receipt for receipt in existing if receipt["taskId"] == task_id])
    if spent >= MAX_MACHINE_RETRIES:
        return {
            "taskId": task_id,
            "attempt": spent,
            "comment": None,
            "exhausted": True,
            "posted": False,
            "redacted": False,
        }
    attempt = spent + 1
    reason, redacted = redact_public_text(f"Stage `{stage}` faulted.\n\n{detail}")
    if len(reason) > MAX_RETRY_CHARS:
        reason = reason[: MAX_RETRY_CHARS - 3].rstrip() + "..."
    reason = required_text(reason, "retry reason", MAX_RETRY_CHARS)
    marker = retry_marker(campaign, issue["number"], task_id, attempt)
    body = f"{marker}\n\n{retry_heading(task_id, attempt)}\n\n{reason}"
    if config["forge"] == "github":
        posted = run(
            [
                "gh",
                "issue",
                "comment",
                thread_number,
                "--repo",
                repository,
                "--body",
                body,
            ]
        )
        comment = required_string(
            posted.stdout.strip().splitlines()[-1] if posted.stdout.strip() else "",
            "machine retry comment URL",
        )
    else:
        ref = (
            f"{local_state_prefix(campaign, issue['number'])}/retry/"
            f"{task_id}/{attempt}"
        )
        created, _ = write_local_blob(
            config,
            ref,
            {
                "schemaVersion": 1,
                "kind": "retry",
                "campaign": campaign,
                "issueNumber": issue["number"],
                "taskId": task_id,
                "attempt": attempt,
                "reason": reason,
                "redaction": PUBLIC_REDACTION,
            },
        )
        if not created:
            fail(f"local forge retry {ref!r} appeared concurrently")
        comment = f"local://{repository}/{ref}"
    return {
        "taskId": task_id,
        "attempt": attempt,
        "comment": comment,
        "exhausted": attempt == MAX_MACHINE_RETRIES,
        "posted": True,
        "redacted": redacted,
    }


def compact_summary(value: str, maximum: int = 64) -> str:
    compact = re.sub(r"\s+", " ", value).strip()
    return compact if len(compact) <= maximum else compact[: maximum - 3] + "..."


def action_escalate(brief: dict[str, Any]) -> dict[str, Any]:
    capability_brief = brief
    brief, _ = take_capabilities(brief)
    forge_native = isinstance(brief.get("worklist"), dict)
    if forge_native:
        data = object_exact(
            brief,
            {"repository", "issue", "worklist"},
            "escalate brief",
        )
    else:
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
            "escalate brief",
        )
    reconciliation = action_reconcile(capability_brief)
    if reconciliation["complete"] or not reconciliation["quiescent"]:
        fail("campaign escalation requires an incomplete empty frontier")
    if reconciliation["escalation"] is not None:
        return {
            "posted": False,
            "comment": reconciliation["escalation"],
            "summary": None,
            "diagnosisCount": len(reconciliation["diagnoses"]),
            "retryCount": len(reconciliation["retries"]),
        }
    campaign = required_string(reconciliation.get("campaign"), "campaign")
    repository = reconciliation["repository"]
    issue = campaign_issue(data.get("issue"))
    config_value = (
        reconciliation["config"]["repositoryConfig"]
        if forge_native
        else data.get("repositoryConfig")
    )
    config = repo_config(config_value)
    direct = [
        fact["taskId"]
        for fact in reconciliation["blocked"]
        if fact["taskId"] in fact["blockedBy"]
    ]
    lines = [
        escalation_marker(campaign, issue["number"]),
        "",
        "### Spec-build escalation: frontier quiescent",
        "",
        "The worklist is incomplete and no unblocked task is dispatchable.",
        "Tally stopped only after each directly blocked task failed twice with machine steering.",
        "",
        f"Directly blocked tasks: {', '.join(f'`{task_id}`' for task_id in direct)}",
        f"Blocked worklist tasks (including descendants): {len(reconciliation['blocked'])}",
        "",
        "Accumulated machine diagnoses:",
    ]
    lines.extend(
        f"- `{item['taskId']}` attempt {item['attempt']}: "
        f"{compact_summary(item['diagnosis'])}"
        for item in reconciliation["diagnoses"]
    )
    if reconciliation["retries"]:
        lines.extend(["", "Campaign machinery faults that bought a retry:"])
        lines.extend(
            f"- `{item['taskId']}` fault {item['attempt']}: "
            f"{compact_summary(item['reason'])}"
            for item in reconciliation["retries"]
        )
    if reconciliation["warnings"]:
        lines.extend(["", "Reconciler warnings:"])
        lines.extend(
            f"- {compact_summary(warning, 200)}"
            for warning in reconciliation["warnings"][:12]
        )
    body = "\n".join(lines)
    if len(body) > 60_000:
        fail("machine escalation exceeds the bounded GitHub comment size")
    # Quiescence is the campaign's other terminal outcome, and the escalation
    # is what proves it was reached: every later pass reads that comment back
    # and stops before this node runs again. So the digest is published first,
    # exactly as the completion path publishes before it closes the issue. A
    # summary that failed after the escalation had landed could never be
    # retried; a summary that fails before it means the whole terminal act is
    # retried on the next pass, and the marker makes the retry idempotent.
    summary = publish_closing_summary(
        repository,
        config,
        campaign,
        issue["number"],
        campaign_digest(reconciliation, "quiescent"),
    )
    if config["forge"] == "github":
        posted = run(
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
        comment = required_string(
            posted.stdout.strip().splitlines()[-1] if posted.stdout.strip() else "",
            "machine escalation comment URL",
        )
    else:
        ref = f"{local_state_prefix(campaign, issue['number'])}/escalation"
        created, _ = write_local_blob(
            config,
            ref,
            {
                "schemaVersion": 1,
                "kind": "escalation",
                "campaign": campaign,
                "issueNumber": issue["number"],
                "body": body,
            },
        )
        if not created:
            fail(f"local forge escalation {ref!r} appeared concurrently")
        comment = f"local://{repository}/{ref}"
    return {
        "posted": True,
        "comment": comment,
        "summary": summary,
        "diagnosisCount": len(reconciliation["diagnoses"]),
        "retryCount": len(reconciliation["retries"]),
    }


def continuation_spec(value: Any) -> dict[str, Any]:
    spec = object_exact(
        value,
        {"argv", "pool", "priority", "runtimeMaxSec", "eventsDir"},
        "continuation",
    )
    events_dir = Path(required_string(spec.get("eventsDir"), "continuation.eventsDir", 4096))
    if not events_dir.is_absolute():
        fail("continuation.eventsDir must be absolute")
    priority = spec.get("priority")
    if priority not in {"interrupt", "high", "medium", "low"}:
        fail("continuation.priority is not a declared priority")
    runtime = spec.get("runtimeMaxSec")
    if runtime is not None:
        runtime = positive_integer(runtime, "continuation.runtimeMaxSec")
    pool = string_list(spec.get("pool"), "continuation.pool", nonempty=True)
    if len(pool) != len(set(pool)):
        fail("continuation.pool must not repeat a pool")
    return {
        "argv": argv(spec.get("argv"), "continuation.argv"),
        "pool": pool,
        "priority": priority,
        "runtimeMaxSec": runtime,
        "eventsDir": events_dir,
    }


def continuation_run_id(campaign: str, repository: str, issue_number: str, run_id: str) -> str:
    """Derive the successor pass identity from this pass's identity alone.

    Bounded and deterministic: re-running the same pass derives the same
    successor, so a duplicate event carries a byte-identical payload and the
    enqueue kernel collapses it. Successive passes chain, so the identity is
    fresh every time without growing.
    """
    material = "\n".join(["spec-build-continuation:v1", campaign, repository, issue_number, run_id])
    return "continuation-" + hashlib.sha256(material.encode("utf-8")).hexdigest()[:32]


def write_continuation_event(
    spec: dict[str, Any], dedup_key: str, brief: Any
) -> tuple[bool, Path]:
    """Drop one bounded EnqueuePayload into the daemon's events directory.

    `tally daemon drain` claims it atomically, the frozen enqueue kernel
    deduplicates it, and the file is retained as an ingress audit record. The
    name is derived from the dedup key, so an identical event that has not yet
    been drained is refused here rather than collapsed later.
    """
    payload: dict[str, Any] = {
        "argv": spec["argv"],
        "adapter": "shell",
        "pool": spec["pool"],
        "priority": spec["priority"],
        "source": "events-dir",
        "dedupKey": dedup_key,
        # Full submission is what arms the kernel's dedupKey x payloadHash
        # dispositions. Without it the identity below is inert and a duplicate
        # event would start a second pass.
        "submission": {"mode": "full"},
        "evidence": ["exit:0"],
        "noEnqueue": False,
    }
    if spec["runtimeMaxSec"] is not None:
        payload["runtimeMaxSec"] = spec["runtimeMaxSec"]
    if brief is not None:
        payload["brief"] = brief
    events_dir: Path = spec["eventsDir"]
    name = "campaign-continuation-" + hashlib.sha256(dedup_key.encode("utf-8")).hexdigest()[:32]
    path = events_dir / f"{name}.json"
    rendered = (json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
    if len(rendered) > MAX_CONTINUATION_EVENT_BYTES:
        fail("continuation payload exceeds the bounded event size")
    events_dir.mkdir(parents=True, exist_ok=True)
    temporary = events_dir / f".{name}.{uuid.uuid4()}.tmp"
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(rendered)
            handle.flush()
            os.fsync(handle.fileno())
        try:
            os.link(temporary, path)
            created = True
        except FileExistsError:
            created = False
    finally:
        os.unlink(temporary)
    return created, path


def action_continue(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief,
        {
            "campaign",
            "repository",
            "repositoryConfig",
            "issue",
            "runId",
            "continuation",
            "brief",
        },
        "continue brief",
    )
    campaign = required_string(data.get("campaign"), "campaign")
    if not COMPONENT.fullmatch(campaign):
        fail("campaign is not a safe component")
    repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(repository):
        fail("repository must use owner/name form")
    config = repo_config(data.get("repositoryConfig"))
    issue = campaign_issue(data.get("issue"))
    run_id = required_string(data.get("runId"), "runId", 512)
    spec = continuation_spec(data.get("continuation"))
    next_run_id = continuation_run_id(campaign, repository, issue["number"], run_id)
    dedup_key = f"campaign-continuation:{repository}:{issue['number']}:{next_run_id}"
    next_brief = data.get("brief")
    if next_brief is not None:
        if not isinstance(next_brief, dict):
            fail("continue brief.brief must be an object or null")
        next_brief = dict(next_brief)
        next_brief["runId"] = next_run_id
    created, path = write_continuation_event(spec, dedup_key, next_brief)
    # The events directory is forge-independent, so a GitHub campaign posts no
    # comment at all. A local-forge campaign keeps its durable blob receipt:
    # the fixture suite reads continuation facts back from the forge.
    receipt = None
    if config["forge"] != "github":
        ref = f"{local_state_prefix(campaign, issue['number'])}/continuation/{next_run_id}"
        expected = {
            "schemaVersion": 1,
            "kind": "continuation",
            "campaign": campaign,
            "issueNumber": issue["number"],
            "runId": run_id,
            "dedupKey": dedup_key,
        }
        _, observed = write_local_blob(config, ref, expected)
        if observed != expected:
            fail(f"local forge continuation {ref!r} disagrees with this pass")
        receipt = f"local://{repository}/{ref}"
    return {
        "event": str(path),
        "dedupKey": dedup_key,
        "runId": next_run_id,
        "created": created,
        "receipt": receipt,
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
            "sourceRevision",
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
    source_revision = required_string(data.get("sourceRevision"), "sourceRevision")
    if not re.fullmatch(r"[0-9a-f]{40,64}", source_revision):
        fail("sourceRevision must be a full Git object ID")
    identity = {
        "campaign": campaign,
        "repository": repository,
        "issueNumber": issue["number"],
        "runId": run_id,
        "taskId": task_id,
        "taskKind": task_kind,
        "workspaceRoot": workspace_root,
        "sourceRevision": source_revision,
    }
    return data, config, identity


def pass_record_path(workspace_root: Path, run_hash: str) -> Path:
    """Where the pass's daemon-liveness record lives.

    This one is genuinely run-scoped rather than worktree-scoped -- it names a
    flow run, not a lane -- so it stays a file under `.state` while lane
    identity moved into git's own per-worktree configuration.
    """
    return workspace_root / ".state" / "passes" / f"{run_hash}.json"


def prune_empty_ancestors(path: Path, stop: Path) -> None:
    current = path
    while current != stop:
        try:
            current.rmdir()
        except OSError:
            return
        current = current.parent


def lane_identity(
    campaign: str,
    repository: str,
    run_id: str,
    task_id: str,
    task_kind: str,
    branch: str,
    publish_branch: str,
    base_rev: str | None = None,
) -> dict[str, str]:
    """The identity fields git carries for one campaign lane.

    Everything the prep marker used to hold, in the place git already keeps
    per-worktree state. `git worktree add` creates it and `git worktree remove`
    destroys it, so the lane set and the lane identities cannot disagree.
    """
    identity = {
        "driver": "spec-build",
        "campaign": campaign,
        "repository": repository,
        "runid": run_id,
        "taskid": task_id,
        "taskkind": task_kind,
        "branch": branch,
        "publishbranch": publish_branch,
    }
    if base_rev is not None:
        identity["baserev"] = base_rev
    return identity


def parse_worktrees(checkout: Path) -> list[dict[str, str]]:
    return worktree_call(worktrees.parse_worktrees, checkout)


def worktree_call(operation: Any, *arguments: Any, **keywords: Any) -> Any:
    """Run one unified-manager operation in this driver's error vocabulary."""
    try:
        return operation(*arguments, **keywords)
    except worktrees.WorktreeError as error:
        fail(error.message)


def tally_executable(value: Any) -> Path:
    executable = Path(required_string(value, "tally"))
    if not executable.is_absolute() or not executable.is_file() or not os.access(executable, os.X_OK):
        fail("tally must name an absolute executable file")
    return executable


def json_command(command: list[str], context: str) -> dict[str, Any]:
    result = run(command)
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        fail(f"{context} returned invalid JSON: {error}")
    if not isinstance(value, dict):
        fail(f"{context} must return a JSON object")
    return value


def current_flow_run_id(tally: Path) -> str:
    task_uuid = required_string(os.environ.get("TALLY_TASK_UUID"), "TALLY_TASK_UUID")
    try:
        uuid.UUID(task_uuid)
    except ValueError:
        fail("TALLY_TASK_UUID must be a UUID")
    response = json_command(
        [str(tally), "query", "job", task_uuid],
        "tally query job for the sweep node",
    )
    job = response.get("job")
    if not isinstance(job, dict):
        fail("tally query job for the sweep node omitted job")
    orchestration = job.get("orchestration")
    if not isinstance(orchestration, dict):
        fail("tally query job for the sweep node omitted orchestration")
    flow_run_id = required_string(
        orchestration.get("flowRunId"),
        "sweep node orchestration.flowRunId",
    )
    try:
        uuid.UUID(flow_run_id)
    except ValueError:
        fail("sweep node orchestration.flowRunId must be a UUID")
    return flow_run_id


def query_live_flow_jobs(tally: Path, flow_run_id: str) -> list[dict[str, Any]]:
    cursor: str | None = None
    seen_cursors: set[str] = set()
    live: list[dict[str, Any]] = []
    for _ in range(128):
        command = [
            str(tally),
            "query",
            "jobs",
            "--flow-run",
            flow_run_id,
            "--limit",
            "1000",
        ]
        if cursor is not None:
            command.extend(["--cursor", cursor])
        response = json_command(command, f"tally query jobs for flow {flow_run_id}")
        items = response.get("items")
        if not isinstance(items, list):
            fail(f"tally query jobs for flow {flow_run_id} omitted items")
        for index, candidate in enumerate(items):
            if not isinstance(candidate, dict):
                fail(f"tally query jobs for flow {flow_run_id} item {index} is not an object")
            state = candidate.get("liveState")
            if state is None:
                continue
            if state not in LIVE_JOB_STATES:
                fail(
                    f"tally query jobs for flow {flow_run_id} returned unknown live state "
                    f"{state!r}"
                )
            orchestration = candidate.get("orchestration")
            if not isinstance(orchestration, dict) or orchestration.get("flowRunId") != flow_run_id:
                fail(f"tally query jobs for flow {flow_run_id} returned a mismatched job")
            task_ref = candidate.get("taskRef")
            if task_ref is not None and not isinstance(task_ref, str):
                fail(f"tally query jobs for flow {flow_run_id} returned an invalid taskRef")
            live.append(
                {
                    "anchor": required_string(
                        candidate.get("anchor"),
                        f"tally query jobs for flow {flow_run_id} item {index}.anchor",
                    ),
                    "liveState": state,
                    "taskRef": task_ref,
                }
            )
        next_cursor = response.get("nextCursor")
        if next_cursor is None:
            return live
        cursor = required_string(next_cursor, "tally query jobs nextCursor")
        if cursor in seen_cursors:
            fail(f"tally query jobs for flow {flow_run_id} repeated a pagination cursor")
        seen_cursors.add(cursor)
    fail(f"tally query jobs for flow {flow_run_id} exceeded 128 pages")


def query_live_campaign_jobs(
    tally: Path,
    campaign_identity: str,
    current_flow_run_id: str,
) -> list[dict[str, Any]]:
    live: list[dict[str, Any]] = []
    for state in sorted(LIVE_JOB_STATES):
        cursor: str | None = None
        seen_cursors: set[str] = set()
        for _ in range(128):
            command = [
                str(tally),
                "query",
                "jobs",
                "--state",
                state,
                "--limit",
                "1000",
            ]
            if cursor is not None:
                command.extend(["--cursor", cursor])
            response = json_command(command, f"tally query jobs in state {state}")
            items = response.get("items")
            if not isinstance(items, list):
                fail(f"tally query jobs in state {state} omitted items")
            for index, candidate in enumerate(items):
                if not isinstance(candidate, dict):
                    fail(f"tally query jobs in state {state} item {index} is not an object")
                if candidate.get("liveState") != state:
                    fail(f"tally query jobs in state {state} returned a mismatched live state")
                task_ref = candidate.get("taskRef")
                if not isinstance(task_ref, str) or not task_ref.startswith(
                    f"{campaign_identity}/"
                ):
                    continue
                orchestration = candidate.get("orchestration")
                flow_run_id = (
                    orchestration.get("flowRunId")
                    if isinstance(orchestration, dict)
                    else None
                )
                if not isinstance(flow_run_id, str):
                    fail(
                        f"live campaign job {candidate.get('anchor')!r} omitted "
                        "orchestration.flowRunId"
                    )
                if flow_run_id == current_flow_run_id:
                    continue
                live.append(
                    {
                        "anchor": required_string(
                            candidate.get("anchor"),
                            f"tally query jobs in state {state} item {index}.anchor",
                        ),
                        "flowRunId": flow_run_id,
                        "liveState": state,
                        "taskRef": task_ref,
                    }
                )
            next_cursor = response.get("nextCursor")
            if next_cursor is None:
                break
            cursor = required_string(next_cursor, "tally query jobs nextCursor")
            if cursor in seen_cursors:
                fail(f"tally query jobs in state {state} repeated a pagination cursor")
            seen_cursors.add(cursor)
        else:
            fail(f"tally query jobs in state {state} exceeded 128 pages")
    return sorted(live, key=lambda item: (item["flowRunId"], item["anchor"]))


def legacy_state_markers(state_root: Path) -> list[Path]:
    """The per-lane JSON markers a tally from before #312 wrote.

    Lane identity lives in git's own worktree config now and nothing writes
    these again, but an estate that upgraded across #312 keeps whatever its
    last pre-upgrade pass left behind. They are enumerated only so the sweep
    can reclaim them once; `passes/` is this driver's own run-scoped record and
    is never a lane marker.
    """
    if not state_root.is_dir() or state_root.is_symlink():
        return []
    return sorted(
        marker
        for marker in state_root.glob("*/*.json")
        if marker.parent != state_root / "passes" and marker.is_file() and not marker.is_symlink()
    )


def reclaim_legacy_markers(
    state_root: Path,
    campaign: str,
    repository: str,
    current_hash: str,
    protected_hashes: set[str],
    cleaned: list[str],
    warnings: list[str],
) -> None:
    """Delete this campaign's pre-#312 lane markers for runs already proved dead.

    The marker is no longer read by anything, so leaving it costs only disk --
    but leaving it silently means an upgraded estate keeps a directory tree
    nobody will ever explain. A marker is reclaimed on exactly the authority
    the sweep already established for its run: same campaign, same repository,
    and a run hash that is neither this pass's nor protected.
    """
    for marker in legacy_state_markers(state_root):
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
        if not isinstance(saved_run_id, str) or not saved_run_id:
            warnings.append(f"left identity-less campaign state marker untouched: {marker}")
            continue
        saved_hash = hashlib.sha256(saved_run_id.encode()).hexdigest()[:12]
        if saved_hash == current_hash or saved_hash in protected_hashes:
            continue
        try:
            marker.unlink()
        except OSError as error:
            warnings.append(f"could not reclaim campaign state marker {marker}: {error}")
            continue
        prune_empty_ancestors(marker.parent, state_root)
        cleaned.append(f"marker:{marker}")


def validated_pass_record(
    workspace_root: Path,
    run_hash: str,
    campaign: str,
    campaign_identity: str,
    repository: str,
) -> tuple[dict[str, Any] | None, str | None]:
    path = pass_record_path(workspace_root, run_hash)
    if not path.exists():
        return None, "no daemon liveness record exists"
    if path.is_symlink() or not path.is_file():
        return None, f"daemon liveness record is not a regular file: {path}"
    try:
        saved = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return None, f"cannot read daemon liveness record {path}: {error}"
    if not isinstance(saved, dict):
        return None, f"daemon liveness record is not an object: {path}"
    expected_fields = {
        "schemaVersion",
        "campaign",
        "campaignIdentity",
        "repository",
        "runId",
        "runHash",
        "flowRunId",
    }
    if set(saved) != expected_fields:
        return None, f"daemon liveness record has an unexpected shape: {path}"
    saved_run_id = saved.get("runId")
    flow_run_id = saved.get("flowRunId")
    if (
        saved.get("schemaVersion") != PASS_RECORD_SCHEMA_VERSION
        or not isinstance(saved.get("campaign"), str)
        or saved.get("campaignIdentity") != campaign_identity
        or saved.get("repository") != repository
        or not isinstance(saved_run_id, str)
        or not saved_run_id
        or hashlib.sha256(saved_run_id.encode()).hexdigest()[:12] != run_hash
        or saved.get("runHash") != run_hash
        or not isinstance(flow_run_id, str)
    ):
        return None, f"daemon liveness record identity does not match run {run_hash}: {path}"
    try:
        uuid.UUID(flow_run_id)
    except ValueError:
        return None, f"daemon liveness record has an invalid flowRunId: {path}"
    return saved, None


def action_sweep(brief: dict[str, Any]) -> dict[str, Any]:
    if not isinstance(brief, dict):
        fail("sweep brief must be an object")
    workspace_root = Path(required_string(brief.get("workspaceRoot"), "workspaceRoot"))
    if not workspace_root.is_absolute():
        fail("workspaceRoot must be absolute")
    state_root = workspace_root / ".state"
    if state_root.exists() and (state_root.is_symlink() or not state_root.is_dir()):
        fail("workspaceRoot .state must be a real directory")
    state_root.mkdir(parents=True, exist_ok=True)
    lock_path = state_root / "sweep.lock"
    try:
        descriptor = os.open(
            lock_path,
            os.O_CREAT | os.O_RDWR | os.O_CLOEXEC | os.O_NOFOLLOW,
            0o600,
        )
    except OSError as error:
        fail(f"cannot open campaign sweep lock {lock_path}: {error}")
    with os.fdopen(descriptor, "a+", encoding="utf-8") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        return action_sweep_locked(brief)


def action_sweep_locked(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief,
        {
            "campaign",
            "campaignIdentity",
            "repository",
            "repositoryConfig",
            "runId",
            "workspaceRoot",
            "tally",
        },
        "sweep brief",
    )
    campaign = required_string(data.get("campaign"), "campaign")
    if not COMPONENT.fullmatch(campaign):
        fail("campaign is not a safe component")
    campaign_identity = required_string(
        data.get("campaignIdentity", campaign), "campaignIdentity", 80
    )
    repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(repository):
        fail("repository must use owner/name form")
    run_id = required_string(data.get("runId"), "runId", 512)
    workspace_root = Path(required_string(data.get("workspaceRoot"), "workspaceRoot"))
    if not workspace_root.is_absolute():
        fail("workspaceRoot must be absolute")
    tally = tally_executable(data.get("tally"))
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
    live_runs: list[dict[str, Any]] = []

    state_root = workspace_root / ".state"
    passes_root = state_root / "passes"
    if passes_root.exists() and (passes_root.is_symlink() or not passes_root.is_dir()):
        fail("workspaceRoot .state/passes must be a real directory")
    passes_root.mkdir(exist_ok=True)
    flow_run_id = current_flow_run_id(tally)
    write_atomic(
        pass_record_path(workspace_root, current_hash),
        {
            "schemaVersion": PASS_RECORD_SCHEMA_VERSION,
            "campaign": campaign,
            "campaignIdentity": campaign_identity,
            "repository": repository,
            "runId": run_id,
            "runHash": current_hash,
            "flowRunId": flow_run_id,
        },
    )
    blocking_jobs = query_live_campaign_jobs(tally, campaign_identity, flow_run_id)

    # One enumeration, straight from git: the registered worktree set and the
    # lane identity each worktree carries in its own configuration.
    registered_lanes = worktree_call(worktrees.lanes, checkout)
    worktree_records = parse_worktrees(checkout)
    listed = git(
        checkout,
        "for-each-ref",
        "--format=%(refname:short)",
        f"refs/heads/tally-work/{campaign_slug}-",
    ).stdout.splitlines()
    candidate_hashes: set[str] = set()
    for record in worktree_records:
        raw_path = record.get("worktree")
        if not raw_path:
            continue
        try:
            relative = Path(raw_path).resolve().relative_to(repository_root)
        except ValueError:
            continue
        if relative.parts and re.fullmatch(r"[0-9a-f]{12}", relative.parts[0]):
            candidate_hashes.add(relative.parts[0])
    for branch in listed:
        matched = branch_pattern.fullmatch(branch)
        if matched is not None:
            candidate_hashes.add(matched.group(1))
    for lane in registered_lanes:
        identity = lane["identity"]
        if (
            identity.get("campaign") == campaign
            and identity.get("repository") == repository
            and identity.get("runid")
        ):
            candidate_hashes.add(
                hashlib.sha256(identity["runid"].encode()).hexdigest()[:12]
            )
    if passes_root.is_dir() and not passes_root.is_symlink():
        for record in passes_root.glob("*.json"):
            if re.fullmatch(r"[0-9a-f]{12}\.json", record.name):
                candidate_hashes.add(record.stem)
    if repository_root.is_dir():
        for child in repository_root.iterdir():
            if child.is_dir() and re.fullmatch(r"[0-9a-f]{12}", child.name):
                candidate_hashes.add(child.name)
    candidate_hashes.discard(current_hash)

    protected_hashes: set[str] = set()
    for candidate_hash in sorted(candidate_hashes):
        pass_record, reason = validated_pass_record(
            workspace_root,
            candidate_hash,
            campaign,
            campaign_identity,
            repository,
        )
        if pass_record is None:
            protected_hashes.add(candidate_hash)
            warnings.append(
                f"left campaign run {candidate_hash} untouched because {reason}"
            )
            continue
        jobs = query_live_flow_jobs(tally, pass_record["flowRunId"])
        if jobs:
            protected_hashes.add(candidate_hash)
            live_runs.append(
                {
                    "runHash": candidate_hash,
                    "flowRunId": pass_record["flowRunId"],
                    "jobs": jobs,
                }
            )
            summary = ", ".join(
                f"{job['liveState']}:{job['anchor']}"
                + (f":{job['taskRef']}" if job["taskRef"] is not None else "")
                for job in jobs
            )
            warnings.append(
                f"left live campaign run {candidate_hash} untouched: {summary}"
            )
    if blocking_jobs:
        protected_hashes.update(candidate_hashes)
        summary = ", ".join(
            f"{job['liveState']}:{job['anchor']}:{job['taskRef']}"
            for job in blocking_jobs
        )
        warnings.append(
            "deferred campaign reconciliation because older campaign jobs remain live: "
            + summary
        )

    for record in worktree_records:
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
        if lane_hash in protected_hashes:
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
    for branch in listed:
        matched = branch_pattern.fullmatch(branch)
        if matched is None or matched.group(1) == current_hash:
            continue
        if matched.group(1) in protected_hashes:
            continue
        deleted = git(checkout, "branch", "-D", branch, check=False)
        if deleted.returncode == 0:
            cleaned.append(f"branch:{branch}")
        else:
            detail = deleted.stderr.strip() or deleted.stdout.strip() or "no output"
            warnings.append(f"could not sweep branch {branch!r}: {detail}")

    # A lane git never registered -- the runner died inside `git worktree add`,
    # or its administrative directory was lost -- leaves a directory git will
    # not remove. Its authority to be deleted is the campaign's own lane
    # layout: `<repositoryRoot>/<runHash>/<lane>` under this campaign's
    # workspace, for a run this pass already proved dead. That is exactly the
    # authority the marker file used to carry, read from the path the driver
    # itself derived rather than from a second copy of the truth.
    registered_paths = {
        Path(record["worktree"]).resolve()
        for record in parse_worktrees(checkout)
        if record.get("worktree")
    }
    if repository_root.is_dir():
        for run_directory in sorted(repository_root.iterdir()):
            if not run_directory.is_dir() or run_directory.is_symlink():
                continue
            lane_hash = run_directory.name
            if not re.fullmatch(r"[0-9a-f]{12}", lane_hash):
                continue
            if lane_hash == current_hash or lane_hash in protected_hashes:
                continue
            for lane_directory in sorted(run_directory.iterdir()):
                resolved = lane_directory.resolve()
                if resolved in registered_paths:
                    continue
                lane_name = lane_directory.name
                if lane_name != "_campaign-preflight" and not TASK_ID.fullmatch(lane_name):
                    warnings.append(
                        f"left unexpected campaign workspace entry untouched: {lane_directory}"
                    )
                    continue
                if lane_directory.is_symlink() or not lane_directory.is_dir():
                    warnings.append(
                        f"left non-directory campaign worktree path untouched: {lane_directory}"
                    )
                    continue
                try:
                    shutil.rmtree(lane_directory)
                except OSError as error:
                    warnings.append(
                        f"could not sweep unregistered campaign worktree "
                        f"{lane_directory}: {error}"
                    )
                    continue
                cleaned.append(f"worktree:{lane_directory}")
                git(
                    checkout,
                    "branch",
                    "-D",
                    f"tally-work/{campaign_slug}-{lane_hash}/{lane_name}",
                    check=False,
                )

    reclaim_legacy_markers(
        state_root, campaign, repository, current_hash, protected_hashes, cleaned, warnings
    )

    if repository_root.is_dir():
        for child in repository_root.iterdir():
            if child.is_dir():
                prune_empty_ancestors(child, repository_root)

    remaining_hashes: set[str] = set()
    for record in parse_worktrees(checkout):
        raw_path = record.get("worktree")
        if not raw_path:
            continue
        try:
            relative = Path(raw_path).resolve().relative_to(repository_root)
        except ValueError:
            continue
        if relative.parts and re.fullmatch(r"[0-9a-f]{12}", relative.parts[0]):
            remaining_hashes.add(relative.parts[0])
    remaining_branches = git(
        checkout,
        "for-each-ref",
        "--format=%(refname:short)",
        f"refs/heads/tally-work/{campaign_slug}-",
    ).stdout.splitlines()
    for branch in remaining_branches:
        matched = branch_pattern.fullmatch(branch)
        if matched is not None:
            remaining_hashes.add(matched.group(1))
    if repository_root.is_dir():
        for child in repository_root.iterdir():
            if child.is_dir() and re.fullmatch(r"[0-9a-f]{12}", child.name):
                remaining_hashes.add(child.name)
    for candidate_hash in sorted(candidate_hashes - protected_hashes - remaining_hashes):
        record = pass_record_path(workspace_root, candidate_hash)
        if record.is_file() and not record.is_symlink():
            try:
                record.unlink()
            except OSError as error:
                warnings.append(f"could not remove swept pass record {record}: {error}")
            else:
                prune_empty_ancestors(record.parent, state_root)
                cleaned.append(f"pass:{record}")
    return {
        "currentRunHash": current_hash,
        "blockingJobs": blocking_jobs,
        "cleaned": sorted(set(cleaned)),
        "liveRuns": live_runs,
        "warnings": list(dict.fromkeys(warnings)),
    }


def write_atomic(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + f".tmp-{os.getpid()}")
    temporary.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")), encoding="utf-8")
    os.replace(temporary, path)


def action_prep(brief: dict[str, Any]) -> dict[str, Any]:
    data, config, identity = prep_identity(brief)
    checkout: Path = config["checkout"]
    remote = config["remote"]
    base_branch = config["baseBranch"]
    run_hash = hashlib.sha256(identity["runId"].encode()).hexdigest()[:12]
    campaign_slug = safe_slug(identity["campaign"], 24)
    repository_slug = safe_slug(identity["repository"].split("/", 1)[1], 40)
    branch = f"tally-work/{campaign_slug}-{run_hash}/{identity['taskId']}"
    publish_branch = stable_publish_branch(
        identity["campaign"],
        identity["issueNumber"],
        identity["taskId"],
        task_revision(data["task"]),
    )
    worktree = (
        identity["workspaceRoot"] / repository_slug / run_hash / identity["taskId"]
    ).resolve()
    expected = lane_identity(
        identity["campaign"],
        identity["repository"],
        identity["runId"],
        identity["taskId"],
        identity["taskKind"],
        branch,
        publish_branch,
    )

    resumed = worktree_call(
        worktrees.resume, checkout, worktree, expected, required=("baserev",)
    )
    if resumed is not None and resumed["complete"]:
        return {
            "taskId": identity["taskId"],
            "baseRev": required_string(resumed["identity"].get("baserev"), "lane baseRev"),
            "branch": branch,
            "publishBranch": publish_branch,
            "worktreePath": str(worktree),
        }

    git(checkout, "fetch", "--prune", remote)
    base_ref = f"{remote}/{base_branch}"
    base_tip = git(checkout, "rev-parse", "--verify", f"{base_ref}^{{commit}}").stdout.strip()
    # Worklist/worktree revision coherence. The reconciler read the worklist at
    # one revision; this fetch happens later and resolves the base branch to
    # whatever it points at now. A rewound or force-replaced remote would
    # otherwise cut lanes from a history the witnessed worklist never
    # described, silently. Checkpoint lanes already assert exactly this
    # relationship, so implementation lanes fail closed the same way.
    if git(
        checkout,
        "merge-base",
        "--is-ancestor",
        identity["sourceRevision"],
        base_tip,
        check=False,
    ).returncode:
        fail("prepared lane base does not descend from the witnessed worklist revision")
    if resumed is not None:
        # A lane git registered whose identity is short a field: a lane cut by
        # a tally from before identity moved into git, or a runner killed
        # before it was recorded. The lane's own history is the authority for
        # where it forks from base, so it is healed rather than refused.
        lane_head = resumed["head"]
    else:
        publish_ref = f"refs/remotes/{remote}/{publish_branch}"
        published = identity["taskKind"] == "implementation" and git(
            checkout,
            "show-ref",
            "--verify",
            "--quiet",
            publish_ref,
            check=False,
        ).returncode == 0
        if worktrees.branch_exists(checkout, branch):
            # The lane branch outlived its worktree. Adopting it means adopting
            # where it sits, not where a fresh lane would have started.
            start_rev = f"refs/heads/{branch}"
        elif published:
            start_rev = publish_ref
        else:
            start_rev = base_tip
        lane_head = worktree_call(worktrees.add, checkout, worktree, branch, start_rev)
    # The prepared base is where the lane's history forks from the base branch,
    # derived from the lane rather than from whatever the base branch happens
    # to point at now. On a fresh lane that is the base tip (or the published
    # head's merge base) exactly as before; on an adopted one it is an ancestor
    # of the lane head by construction, which is what every downstream node --
    # ownership, diff, rebase -- requires of it.
    base_rev = git(checkout, "merge-base", lane_head, base_tip).stdout.strip()
    if not GIT_OID.fullmatch(base_rev):
        fail(f"cannot derive a base revision for campaign lane {branch!r}")
    worktree_call(worktrees.write_identity, worktree, {**expected, "baserev": base_rev})
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
    expected = lane_identity(
        campaign, repository, run_id, task_id, "preflight", branch, branch
    )

    resumed = worktree_call(
        worktrees.resume, checkout, worktree, expected, required=("baserev",)
    )
    if resumed is not None and resumed["complete"]:
        return {
            "taskId": task_id,
            "baseRev": required_string(
                resumed["identity"].get("baserev"), "preflight lane baseRev"
            ),
            "branch": branch,
            "publishBranch": branch,
            "worktreePath": str(worktree),
        }

    git(checkout, "fetch", "--prune", config["remote"])
    base_ref = f"{config['remote']}/{config['baseBranch']}"
    base_tip = git(checkout, "rev-parse", "--verify", f"{base_ref}^{{commit}}").stdout.strip()
    if resumed is not None:
        lane_head = resumed["head"]
    else:
        start_rev = (
            f"refs/heads/{branch}" if worktrees.branch_exists(checkout, branch) else base_tip
        )
        lane_head = worktree_call(worktrees.add, checkout, worktree, branch, start_rev)
    base_rev = git(checkout, "merge-base", lane_head, base_tip).stdout.strip()
    if not GIT_OID.fullmatch(base_rev):
        fail(f"cannot derive a base revision for campaign lane {branch!r}")
    worktree_call(worktrees.write_identity, worktree, {**expected, "baserev": base_rev})
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


def reject_merge_commits(worktree: Path, union_base: str, head: str) -> None:
    """A merge commit a lane authored makes the whole mainline look authored.

    The path union walks lane history with `git log -m`, which splits a merge
    and attributes both sides to the lane. A lane that merged the base branch
    instead of rebasing onto it therefore claims every path its siblings landed
    while it was live: the union cannot tell those paths from the task's own,
    the ownership receipt misattributes authorship, and the lane fails on paths
    nobody in the task touched. `--first-parent` would hide the false positive
    and reopen the transient-path hole with it, so the lane is rejected by its
    real cause instead.

    The range this reads is the union range, which starts at the base branch
    commit the lane actually sits on. Every campaign merge is `--no-ff`, so a
    base branch that has integrated anything is itself full of merge commits; a
    lane that rebases onto it — the documented remediation — inherits them. A
    range starting at the stale prepared base would contain those inherited
    merges and reject the lane for doing exactly what the steering text tells
    it to do.
    """
    listed = git(
        worktree,
        "rev-list",
        "--merges",
        "--end-of-options",
        f"{union_base}..{head}",
        "--",
    ).stdout.split()
    if not listed:
        return
    preview = ", ".join(listed[:5])
    if len(listed) > 5:
        preview += f", and {len(listed) - 5} more"
    fail(
        f"task lane history contains {len(listed)} merge commit(s) ({preview}); "
        "rebase instead of merging the base into your lane"
    )


def current_base_revision(worktree: Path, config: dict[str, Any]) -> str:
    """The base branch tip, taken from the forge rather than from a local ref.

    A lane worktree is a linked worktree of the campaign checkout, so
    `refs/remotes/<remote>/<baseBranch>` lives in the shared common Git
    directory and anything running in the lane can write it — including the
    agent whose ownership is about to be validated. Pointing it at the lane
    head would collapse the union to nothing and make every declared domain
    vacuously satisfied, which is the one thing the conflict-domain boundary
    exists to prevent.

    The fetch is therefore the read: `FETCH_HEAD` is per-worktree, this node is
    the only thing running in the lane, and the value comes off the wire rather
    than out of a ref an agent can reach. It also brings the objects needed to
    reason about the base branch at all.
    """
    remote = config["remote"]
    base_branch = config["baseBranch"]
    fetched = git(
        worktree,
        "fetch",
        "--no-tags",
        "--end-of-options",
        remote,
        f"refs/heads/{base_branch}",
        check=False,
    )
    if fetched.returncode != 0:
        detail = fetched.stderr.strip() or fetched.stdout.strip() or "no output"
        fail(f"cannot read base branch {base_branch!r} from {remote!r}: {detail}")
    return full_git_oid(
        git(worktree, "rev-parse", "--verify", "FETCH_HEAD^{commit}").stdout.strip(),
        "fetched base branch revision",
    )


def lane_union_base(
    worktree: Path, base_rev: str, head: str, current_base: str | None
) -> str:
    """The revision a lane's path union is resolved against.

    The prepared base goes stale the moment a sibling lane merges. A lane
    rebased onto the advanced base — the documented remediation for a red
    constraint — then carries every mainline commit between the two bases, and
    a union taken from the prepared base claims their paths, and their merge
    commits, for the task.

    The right start is where this lane leaves the base branch, which is the
    merge base of the lane head with the current base. It equals the prepared
    base for a lane that never rebased, and the rebased-onto revision for one
    that did, and — unlike the current tip — it does not move when the base
    advances again afterwards, so a gate receipt and the publication that
    re-checks it read the same range. The prepared base is kept whenever that
    point cannot be established or would not contain it, so nothing here can
    widen what a lane is allowed to touch.
    """
    if current_base is None or current_base == base_rev:
        return base_rev
    resolved = git(
        worktree, "merge-base", "--end-of-options", head, current_base, check=False
    )
    fork = resolved.stdout.strip()
    if resolved.returncode != 0 or not GIT_OID.fullmatch(fork) or fork == base_rev:
        return base_rev
    if git(worktree, "merge-base", "--is-ancestor", base_rev, fork, check=False).returncode:
        return base_rev
    return fork


def changed_paths_in_history(
    worktree: Path, union_base: str, head: str, *, include_deletions: bool = False
) -> list[str]:
    """Every path the lane touched between the base it sits on and its head.

    Callers resolve `union_base` with `lane_union_base` so that both this walk
    and the merge-commit rejection read the lane's own history rather than the
    mainline it may have rebased onto.
    """
    union_base = full_git_oid(union_base, "base revision")
    head = full_git_oid(head, "head revision")
    reject_merge_commits(worktree, union_base, head)
    diff_filter = "ACDMTUXB" if include_deletions else "ACMTUXB"
    changed = git(
        worktree,
        "log",
        "-m",
        "--format=",
        "--name-only",
        "--no-renames",
        f"--diff-filter={diff_filter}",
        "-z",
        "--end-of-options",
        f"{union_base}..{head}",
        "--",
    ).stdout
    return sorted({path for path in changed.split("\0") if path})


def normalize_ownership_receipt(value: Any, context: str) -> dict[str, Any]:
    receipt = object_exact(
        value,
        {
            "taskId",
            "domainsRequired",
            "conflictDomains",
            "ownedPaths",
            "baseRev",
            "head",
        },
        context,
    )
    task_id = required_string(receipt.get("taskId"), f"{context}.taskId")
    if not TASK_ID.fullmatch(task_id):
        fail(f"{context}.taskId is not safe")
    domains_required = required_bool(
        receipt.get("domainsRequired"), f"{context}.domainsRequired"
    )
    domains = normalize_conflict_domains(
        receipt.get("conflictDomains"),
        f"{context}.conflictDomains",
        required=domains_required,
    )
    owned_paths = normalize_owned_paths(receipt.get("ownedPaths"), f"{context}.ownedPaths")
    if owned_paths != sorted(owned_paths):
        fail(f"{context}.ownedPaths must be sorted")
    return {
        "taskId": task_id,
        "domainsRequired": domains_required,
        "conflictDomains": domains,
        "ownedPaths": owned_paths,
        "baseRev": full_git_oid(receipt.get("baseRev"), f"{context}.baseRev"),
        "head": full_git_oid(receipt.get("head"), f"{context}.head"),
    }


def enforce_conflict_domains(
    worktree: Path,
    base_rev: str,
    head: str,
    task: Any,
    expected_task_id: str,
    domains_required: bool,
    current_base: str | None = None,
) -> dict[str, Any]:
    if not isinstance(task, dict):
        fail("task must be an object")
    task_id = required_string(task.get("id"), "task.id")
    if not TASK_ID.fullmatch(task_id):
        fail("task.id is not safe")
    if task_id != expected_task_id:
        fail("task.id does not match workspace.taskId")
    domains_required = required_bool(domains_required, "domainsRequired")
    domains = normalize_conflict_domains(
        task.get("conflictDomains"),
        "task.conflictDomains",
        required=domains_required,
    )
    base_rev = full_git_oid(base_rev, "base revision")
    head = full_git_oid(head, "head revision")
    # The receipt keeps naming the base this lane was prepared and gated on;
    # only the union narrows, and only onto a base branch commit the lane
    # already contains.
    changed_paths = changed_paths_in_history(
        worktree,
        lane_union_base(worktree, base_rev, head, current_base),
        head,
        include_deletions=True,
    )
    outside = (
        [
            path
            for path in changed_paths
            if not any(domains_overlap(path, domain) for domain in domains)
        ]
        if domains
        else []
    )
    if outside:
        preview = ", ".join(json.dumps(path) for path in outside[:20])
        if len(outside) > 20:
            preview += f", and {len(outside) - 20} more"
        declared = ", ".join(json.dumps(domain) for domain in domains)
        fail(
            f"task {task_id!r} changed {len(outside)} path(s) outside its declared "
            f"conflictDomains: {preview}; declared domains: {declared}"
        )
    return {
        "taskId": task_id,
        "domainsRequired": domains_required,
        "conflictDomains": domains,
        "ownedPaths": changed_paths,
        "baseRev": base_rev,
        "head": head,
    }


def action_ownership(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief,
        {"task", "domainsRequired", "repositoryConfig", "workspace"},
        "ownership brief",
    )
    config = repo_config(data.get("repositoryConfig"))
    workspace = object_exact(
        data.get("workspace"),
        {"taskId", "baseRev", "branch", "publishBranch", "worktreePath"},
        "workspace",
    )
    task_id = required_string(workspace.get("taskId"), "workspace.taskId")
    if not TASK_ID.fullmatch(task_id):
        fail("workspace.taskId is not safe")
    base_rev = full_git_oid(workspace.get("baseRev"), "workspace.baseRev")
    branch = required_string(workspace.get("branch"), "workspace.branch")
    required_string(workspace.get("publishBranch"), "workspace.publishBranch")
    worktree = Path(required_string(workspace.get("worktreePath"), "workspace.worktreePath"))
    if not worktree.is_absolute() or not worktree.is_dir():
        fail("workspace.worktreePath must be an absolute existing directory")
    git(worktree, "rev-parse", "--git-dir")
    actual_branch = git(worktree, "branch", "--show-current").stdout.strip()
    if actual_branch != branch:
        fail(f"worktree is on branch {actual_branch!r}, expected {branch!r}")
    if git(worktree, "status", "--porcelain").stdout:
        fail("agent left uncommitted changes; commit the task before ownership validation")
    head = git(worktree, "rev-parse", "--verify", "HEAD^{commit}").stdout.strip()
    if head == base_rev:
        fail("agent produced no commit relative to the prepared base")
    if git(worktree, "merge-base", "--is-ancestor", base_rev, head, check=False).returncode:
        fail("task head is not descended from its prepared base revision")
    return enforce_conflict_domains(
        worktree,
        base_rev,
        head,
        data.get("task"),
        task_id,
        data.get("domainsRequired"),
        current_base_revision(worktree, config),
    )


def evaluate_forbid_paths(
    worktree: Path, union_base: str, head: str, gate_id: str, patterns: list[str]
) -> int:
    changed_paths = changed_paths_in_history(worktree, union_base, head)
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
    data = object_exact(
        brief, {"gate", "repositoryConfig", "workspace"}, "constraint brief"
    )
    gate = normalize_forbid_paths_gate(data.get("gate"), "constraint gate")
    gate_id = gate["id"]
    patterns = gate["forbidPaths"]
    config = repo_config(data.get("repositoryConfig"))

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

    # The receipt names the prepared base, which is what publication compares
    # it against; the walk starts where the lane leaves the base branch, so a
    # lane that rebased onto an advanced base is not gated on mainline paths it
    # never touched.
    checked_paths = evaluate_forbid_paths(
        worktree,
        lane_union_base(
            worktree, base_rev, head, current_base_revision(worktree, config)
        ),
        head,
        gate_id,
        patterns,
    )
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


def normalize_campaign_gates(value: Any, context: str) -> list[dict[str, Any]]:
    """The campaign's configured gates, reduced to the path-policy ones.

    Command gates are named and then dropped: they are witnessed by their own
    job, carry no pattern set, and have nothing for publication to cross-check.
    """
    if not isinstance(value, list):
        fail(f"{context} must be an array")
    if len(value) > 16:
        fail(f"{context} exceeds 16 gates")
    gates: list[dict[str, Any]] = []
    seen: set[str] = set()
    for index, candidate in enumerate(value):
        if not isinstance(candidate, dict):
            fail(f"{context}[{index}] must be an object")
        gate_id = required_string(candidate.get("id"), f"{context}[{index}].id", 80)
        if gate_id in seen:
            fail(f"{context} repeats gate id {gate_id!r}")
        seen.add(gate_id)
        if candidate.get("kind") == "forbidPaths":
            gates.append(normalize_forbid_paths_gate(candidate, f"{context}[{index}]"))
    return gates


def enforce_configured_gates(
    constraints: list[dict[str, Any]], gates: list[dict[str, Any]]
) -> None:
    """Publication answers to the configured gates, not to the receipts.

    A constraint receipt carries the pattern set that was witnessed, and
    publication re-runs it. Re-running a receipt against itself proves only
    that the receipt is self-consistent: a campaign whose `forbidPaths` was
    widened between the gate run and publication would publish against the
    superseded set. The two are cross-checked by gate id and pattern set here
    and drift fails by name, the same way #254 stopped the merge path from
    trusting an upstream flag.
    """
    configured = {gate["id"]: gate["forbidPaths"] for gate in gates}
    witnessed = {constraint["gateId"]: constraint["patterns"] for constraint in constraints}
    for gate_id, patterns in configured.items():
        if gate_id not in witnessed:
            fail(
                f"forbidPaths gate {gate_id!r} is configured for this campaign but no "
                "witnessed receipt reached publication"
            )
        if witnessed[gate_id] != patterns:
            fail(
                f"forbidPaths gate {gate_id!r} was witnessed against patterns "
                f"{json.dumps(witnessed[gate_id])}, but the campaign configures "
                f"{json.dumps(patterns)}"
            )
    for gate_id in sorted(set(witnessed) - set(configured)):
        fail(
            f"forbidPaths gate {gate_id!r} presented a receipt for a gate this "
            "campaign does not configure"
        )


def enforce_constraint_results(
    worktree: Path,
    base_rev: str,
    union_base: str,
    head: str,
    constraints: list[dict[str, Any]],
) -> None:
    """Re-run each witnessed gate at publication.

    The receipt is bound to the base the lane was prepared on, and the walk
    runs over the same range the gate node used — where the lane leaves the
    base branch — so the recounted path total is comparable with the one the
    receipt carries even when the base advanced in between.
    """
    for constraint in constraints:
        if constraint["baseRev"] != base_rev:
            fail(
                f"forbidPaths gate {constraint['gateId']!r} was witnessed against base "
                f"{constraint['baseRev']}, expected {base_rev}"
            )
        checked_paths = evaluate_forbid_paths(
            worktree,
            union_base,
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
        allowed.update({"publication", "constraints", "domainsRequired"})
    if action == "publish":
        allowed.update({"constraints", "domainsRequired", "gates", "steward"})
    if action == "merge":
        allowed.update(
            {
                "integration",
                "domainsRequired",
                "mergeMethod",
                "gitAiBinding",
                "gitAiAwaitSec",
                "assistedBy",
            }
        )
    data = object_exact(brief, allowed, f"{action} brief")
    config = repo_config(data.get("repositoryConfig"))
    workspace = object_exact(
        data.get("workspace"),
        {"taskId", "baseRev", "branch", "publishBranch", "worktreePath"},
        "workspace",
    )
    full_git_oid(workspace.get("baseRev"), "workspace.baseRev")
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


def github_pull_request(
    data: dict[str, Any],
    config: dict[str, Any],
    worktree: Path,
    head: str,
    narration: dict[str, Any],
) -> str:
    repository = required_string(data.get("repository"), "repository")
    workspace = data["workspace"]
    branch = workspace["publishBranch"]
    task = data["task"]
    issue = campaign_issue(data.get("issue"))
    marker = pull_request_marker(
        required_string(data.get("campaign"), "campaign"),
        issue["number"],
        task["id"],
        task_revision(task),
    )
    candidates = pull_requests_by_head(repository, branch, "all", 2)
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
    closes = ""
    if isinstance(task.get("brief"), dict):
        task_issue = campaign_issue(task["brief"].get("issue"))
        closes = f"\n\nCloses #{task_issue['number']}"
    # Steward prose leads; the managed marker and the campaign's own identity
    # lines are appended by this node and are never model-authored. With no
    # steward the narration is the template and this body is byte-identical to
    # the pre-steward one.
    prose = f"{narration['body']}\n\n" if narration["body"] else ""
    body = (
        f"{marker}\n"
        f"{prose}"
        f"Spec-build campaign progress for {repository}#{issue['number']}.\n\n"
        f"Task `{task['id']}`: {task['title']}\n\n"
        f"Task ref: `{task_ref}`\n\n"
        f"Witnessed gates are the merge criterion. Campaign issue: {issue['url']}\n"
        f"Head: `{head}`"
        f"{closes}"
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
            narration["subject"],
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
    enforce_configured_gates(
        constraints, normalize_campaign_gates(data.get("gates"), "publish gates")
    )
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
    current_base = current_base_revision(worktree, config)
    ownership = enforce_conflict_domains(
        worktree,
        base_rev,
        head,
        data.get("task"),
        task_id,
        data.get("domainsRequired"),
        current_base,
    )
    enforce_constraint_results(
        worktree,
        base_rev,
        lane_union_base(worktree, base_rev, head, current_base),
        head,
        constraints,
    )
    git(worktree, "push", config["remote"], f"HEAD:refs/heads/{publish_branch}")
    # The publish node is the crossing point between the two surfaces (§3), so
    # it is where the steward narrates. Everything it is given is already
    # public or about to be: the task brief, the shape of the diff, and the
    # campaign's own identifiers. The narration governs the pull request text
    # here and the squash commit message at the merge node.
    task = data["task"]
    steward = steward_role(data.get("steward"))
    request = (
        {}
        if steward is None
        else {
            "schemaVersion": 1,
            "mission": (
                "Propose the conventional-commit message and pull-request prose for one "
                "completed campaign task. Reply with a single line of the form "
                "TALLY_FINAL_MESSAGE=<json>, where <json> is an object with the keys "
                "type, scope, subject, and body. Text only: you are not running git."
            ),
            "campaign": required_string(data.get("campaign"), "campaign"),
            "task": {
                "id": task["id"],
                "title": task["title"],
                "goal": task.get("goal"),
                "brief": (task.get("brief") or {}).get("body")
                if isinstance(task.get("brief"), dict)
                else None,
                "conflictDomains": task.get("conflictDomains", []),
            },
            "diffStat": git(
                worktree, "diff", "--stat", f"{base_rev}..{head}"
            ).stdout[:MAX_RETRY_CHARS],
            "grammar": {
                "types": sorted(NARRATION_TYPES),
                "headerMaxChars": NARRATION_HEADER_MAX,
                "bodyMaxChars": NARRATION_BODY_MAX,
                "bodyMaxColumns": NARRATION_BODY_LINE_MAX,
            },
        }
    )
    narration, transcript = narrate(steward, task, request)
    if config["forge"] == "github":
        pull_request = github_pull_request(data, config, worktree, head, narration)
    else:
        pull_request = f"local://{data['repository']}/{publish_branch}"
    return {
        "taskId": task_id,
        "branch": publish_branch,
        "head": head,
        "pullRequest": pull_request,
        "narration": narration,
        "narrationAttempts": transcript,
        "ownership": ownership,
    }


def publication(value: Any, context: str = "publication") -> dict[str, Any]:
    item = object_exact(
        value,
        {
            "taskId",
            "branch",
            "head",
            "pullRequest",
            "narration",
            # Observability only: the validator transcript is journaled with the
            # publish node's result and is deliberately not carried onward.
            "narrationAttempts",
            "ownership",
        },
        context,
    )
    task_id = required_string(item.get("taskId"), f"{context}.taskId")
    if not TASK_ID.fullmatch(task_id):
        fail(f"{context}.taskId is not safe")
    head = full_git_oid(item.get("head"), f"{context}.head")
    ownership = normalize_ownership_receipt(item.get("ownership"), f"{context}.ownership")
    if ownership["taskId"] != task_id:
        fail(f"{context}.ownership.taskId does not match {context}.taskId")
    if ownership["head"] != head:
        fail(f"{context}.ownership.head does not match {context}.head")
    return {
        "taskId": task_id,
        "branch": required_string(item.get("branch"), f"{context}.branch"),
        "head": head,
        "pullRequest": required_string(
            item.get("pullRequest"), f"{context}.pullRequest"
        ),
        "narration": narration_record(item.get("narration"), f"{context}.narration"),
        "ownership": ownership,
    }


def abandon_published_head(
    worktree: Path,
    remote: str,
    published: dict[str, str],
    context: str,
) -> None:
    abandoned = git(
        worktree,
        "push",
        f"--force-with-lease=refs/heads/{published['branch']}:{published['head']}",
        remote,
        f":refs/heads/{published['branch']}",
        check=False,
    )
    if abandoned.returncode != 0:
        detail = abandoned.stderr.strip() or abandoned.stdout.strip() or "no output"
        fail(
            f"{context}; exact published head {published['head']} could not be "
            f"abandoned: {detail}"
        )


def action_rebase(brief: dict[str, Any]) -> dict[str, Any]:
    data, config, worktree = publication_identity(brief, "rebase")
    published = publication(data.get("publication"))
    constraints = normalize_constraint_results(data.get("constraints"), "rebase constraints")
    workspace = data["workspace"]
    domains_required = required_bool(data.get("domainsRequired"), "domainsRequired")
    if published["taskId"] != workspace["taskId"]:
        fail("publication.taskId does not match workspace.taskId")
    if published["branch"] != workspace["publishBranch"]:
        fail("publication.branch does not match workspace.publishBranch")
    if published["ownership"]["baseRev"] != workspace["baseRev"]:
        fail("publication.ownership.baseRev does not match workspace.baseRev")
    if published["ownership"]["domainsRequired"] != domains_required:
        fail("publication.ownership.domainsRequired does not match domainsRequired")
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
        ownership = enforce_conflict_domains(
            worktree,
            base_rev,
            local_head,
            data.get("task"),
            published["taskId"],
            domains_required,
        )
        return {
            "taskId": published["taskId"],
            "baseRev": base_rev,
            "branch": published["branch"],
            "head": local_head,
            "pullRequest": published["pullRequest"],
            "narration": published["narration"],
            "regate": False,
            "ownership": ownership,
        }

    rebased = git(worktree, "rebase", base_rev, check=False)
    if rebased.returncode != 0:
        detail = rebased.stderr.strip() or rebased.stdout.strip() or "no output"
        aborted = git(worktree, "rebase", "--abort", check=False)
        abandon_published_head(
            worktree,
            remote,
            published,
            f"cannot rebase task onto current base {base_rev}: {detail}; "
            f"rebase abort exited {aborted.returncode}",
        )
        if aborted.returncode != 0:
            abort_detail = aborted.stderr.strip() or aborted.stdout.strip() or "no output"
            fail(
                f"cannot rebase task onto current base {base_rev}: {detail}; "
                f"published head {published['head']} was abandoned, but rebase abort "
                f"failed: {abort_detail}"
            )
        fail(
            f"cannot rebase task onto current base {base_rev}: {detail}; "
            f"rebase was aborted and exact published head {published['head']} was abandoned; "
            "a fresh pass can rebuild the task from current base"
        )
    rebased_head = git(worktree, "rev-parse", "HEAD").stdout.strip()
    try:
        ownership = enforce_conflict_domains(
            worktree,
            base_rev,
            rebased_head,
            data.get("task"),
            published["taskId"],
            domains_required,
        )
        for constraint in constraints:
            evaluate_forbid_paths(
                worktree,
                base_rev,
                rebased_head,
                constraint["gateId"],
                constraint["patterns"],
            )
    except DriverError as error:
        abandon_published_head(
            worktree,
            remote,
            published,
            f"rebased task failed integration policy against current base {base_rev}: {error}",
        )
        fail(
            f"rebased task failed integration policy against current base {base_rev}: {error}; "
            f"exact published head {published['head']} was abandoned so a fresh pass can "
            "rebuild the task"
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
        "narration": published["narration"],
        "regate": True,
        "ownership": ownership,
    }


def cleanup_worktree(checkout: Path, worktree: Path, branch: str) -> None:
    worktree_call(worktrees.remove, checkout, worktree, branch)


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
    prune_empty_ancestors(expected_worktree.parent, repository_root)
    return {"taskId": task_id, "cleaned": True}


def merge_commit_body(narration: dict[str, Any], trailer: str | None) -> str:
    """Validated prose plus the node's own provenance pointer, in that order."""
    parts = [part for part in (narration["body"], trailer) if part]
    return "\n\n".join(parts)


def merge_commit_message(narration: dict[str, Any], trailer: str | None = None) -> str:
    """The validated message the node writes. The model never runs git."""
    body = merge_commit_body(narration, trailer)
    if body:
        return f"{narration['subject']}\n\n{body}\n"
    return f"{narration['subject']}\n"


def git_ai_available(checkout: Path) -> str | None:
    """The externally provisioned binary's version, or None when unusable.

    tally.nix does not package, vendor, or pin git-ai (doc/git-ai-authorship.md
    states the deployment boundary); the estate installs it on every host where
    a campaign may merge. Absence is an ordinary advisory outcome, not a fault.
    """
    if shutil.which(GIT_AI_PROGRAM) is None:
        return None
    probed = run([GIT_AI_PROGRAM, "--version"], cwd=checkout, check=False)
    if probed.returncode != 0:
        return None
    return (probed.stdout.strip() or probed.stderr.strip() or "unknown")[:80]


def reconstruct_squash(
    config: dict[str, Any],
    data: dict[str, Any],
    base_rev: str,
    head: str,
    message: str,
    await_sec: int = GIT_AI_AWAIT_SEC,
) -> tuple[str | None, str | None]:
    """Mint the campaign's squash a second time, where the checkpoints live.

    `test/git-ai-squash-fidelity.sh` measured what a squash carries
    (doc/src/flows/git-ai-squash-fidelity.md): attribution is re-minted per
    line, by git-ai's background service, at `git commit` time, and only in the
    repository that made the commit. A forge-side squash is therefore
    unbound -- `remote-squash` in that table is the decisive negative -- and
    nothing is recoverable by fetching or reading it afterwards.

    So the binding re-executes the same integration in a detached worktree of
    the campaign checkout, which is the one place that still holds the task
    branch's checkpoints and shares its refs/notes/ai. The result is a commit
    with a different object ID and, by construction, the same parent and the
    same tree as the one the forge minted; the caller proves both before
    treating its note as the integrated commit's.

    The reconstruction commits under its own identity, not the merge node's.
    A forge squashes with its own committer and clock so the two object IDs
    always differ in production, but a local-forge merge that shares an
    identity, a tree, a parent, a message and a committer second produces the
    *same* object ID -- and then the copy the whole binding turns on is a
    no-op onto itself that nothing exercises. Naming the reconstruction is
    both more honest and what keeps that path real.
    """
    checkout: Path = config["checkout"]
    workspace_root = Path(data["workspaceRoot"])
    workspace_root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="git-ai-bind-", dir=workspace_root) as temporary:
        worktree = Path(temporary) / "squash"
        added = git(
            checkout, "worktree", "add", "--detach", "--quiet", str(worktree), base_rev,
            check=False,
        )
        if added.returncode != 0:
            detail = added.stderr.strip() or added.stdout.strip() or "no output"
            return None, f"cannot reconstruct the squash: {detail}"
        try:
            merged = git(worktree, "merge", "--squash", head, check=False)
            if merged.returncode != 0:
                detail = merged.stderr.strip() or merged.stdout.strip() or "no output"
                return None, f"reconstructed squash did not apply: {detail}"
            if not git(worktree, "diff", "--cached", "--quiet", check=False).returncode:
                return None, "reconstructed squash staged no change against the merged base"
            committed = git(
                worktree,
                "-c",
                "user.name=tally spec-build binding",
                "-c",
                "user.email=tally-spec-build-binding@invalid",
                "commit",
                "--quiet",
                "--file",
                "-",
                input_text=message,
                check=False,
            )
            if committed.returncode != 0:
                detail = committed.stderr.strip() or committed.stdout.strip() or "no output"
                return None, f"reconstructed squash did not commit: {detail}"
            local = git(worktree, "rev-parse", "HEAD").stdout.strip()
            # The note is minted asynchronously by git-ai's background service,
            # so a read taken before it settles sees nothing. The barrier is
            # bounded and its failure is reported, never retried blindly.
            settled = run(
                [GIT_AI_PROGRAM, "await", "--timeout", str(await_sec)],
                cwd=worktree,
                check=False,
                timeout=await_sec + 5,
            )
            if settled.returncode != 0:
                detail = settled.stderr.strip() or settled.stdout.strip() or "no output"
                return local, f"git-ai await exited {settled.returncode}: {detail[:200]}"
            return local, None
        finally:
            git(checkout, "worktree", "remove", "--force", str(worktree), check=False)


def note_blob(checkout: Path, note: bytes) -> str:
    """Write exact note bytes into the object store and return the blob id.

    `git notes add -m/-F` runs the message through stripspace; `-C <blob>`
    does not. A git-ai note is a structured record whose bytes are hashed into
    the receipt, so it is written verbatim or not at all.
    """
    with tempfile.NamedTemporaryFile(prefix="git-ai-note-") as handle:
        handle.write(note)
        handle.flush()
        written = git_bytes(checkout, "hash-object", "-w", handle.name)
    blob = written.stdout.decode().strip()
    if not GIT_OID.fullmatch(blob):
        fail("git hash-object returned an invalid blob id for an authorship note")
    return blob


def remote_note(checkout: Path, remote: str, revision: str) -> tuple[str | None, bytes | None]:
    """The remote's notes-ref target, and its note for one revision.

    Fetched into a scratch ref: the campaign checkout's own `refs/notes/ai` is
    never rewritten from remote content. It carries the daemon's witnessed
    code-result bindings, and `tally witness verify-authorship` compares those
    note bytes exactly -- a fold-in would turn every one of them into a
    permanent `note-content-mismatch`.
    """
    git(checkout, "update-ref", "-d", GIT_AI_REMOTE_REF, check=False)
    fetched = git(
        checkout, "fetch", remote, f"+{GIT_AI_NOTE_REF}:{GIT_AI_REMOTE_REF}", check=False
    )
    if fetched.returncode != 0:
        return None, None
    resolved = git(checkout, "rev-parse", "--verify", GIT_AI_REMOTE_REF, check=False)
    target = resolved.stdout.strip() if resolved.returncode == 0 else ""
    if not GIT_OID.fullmatch(target):
        return None, None
    existing = git_bytes(
        checkout, "notes", "--ref", GIT_AI_REMOTE_REF, "show", revision, check=False
    )
    return target, existing.stdout if existing.returncode == 0 else None


def publish_authorship_note(
    checkout: Path, remote: str, revision: str, note: bytes
) -> tuple[str, bytes | None, str | None, str | None]:
    """Publish exactly one authorship note, and nothing else.

    Returns `(status, published bytes, remote notes-ref target, reason)`.

    Two things this deliberately does not do. It does not push the campaign
    checkout's whole `refs/notes/ai`: that ref accumulates a note for every
    commit the shared checkout has ever made, including abandoned attempts and
    the binding's own throwaway reconstruction, and none of that was ever
    chosen for a public forge. It assembles a scratch ref from the remote's
    own tip plus this one entry instead, so what is published is exactly the
    integrated commit's note.

    And it never merges two notes for the same commit. `cat_sort_uniq` is
    line-oriented; a git-ai `authorship/3.0.0` note is a two-section record
    whose line order is semantic, so folding two of them yields a structurally
    invalid note under a schema version it no longer satisfies. A remote that
    already carries a *different* note for this revision is reported as a
    typed `conflict` and nothing is written or pushed.
    """
    blob = note_blob(checkout, note)
    reason: str | None = None
    try:
        for attempt in range(2):
            target, existing = remote_note(checkout, remote, revision)
            if existing is not None and existing != note:
                return (
                    "conflict",
                    None,
                    target,
                    f"{GIT_AI_NOTE_REF} on {remote} already carries a different note for "
                    f"{revision}; refusing to merge two authorship records",
                )
            git(checkout, "update-ref", "-d", GIT_AI_PUBLISH_REF, check=False)
            if target is not None:
                git(checkout, "update-ref", GIT_AI_PUBLISH_REF, target)
            added = git(
                checkout,
                "notes",
                "--ref",
                GIT_AI_PUBLISH_REF,
                "add",
                "-f",
                "-C",
                blob,
                revision,
                check=False,
            )
            if added.returncode != 0:
                detail = added.stderr.strip() or added.stdout.strip() or "no output"
                return "error", None, target, f"cannot stage the published note: {detail[:200]}"
            pushed = git(
                checkout,
                "push",
                remote,
                f"{GIT_AI_PUBLISH_REF}:{GIT_AI_NOTE_REF}",
                check=False,
            )
            if pushed.returncode == 0:
                break
            # The remote moved between the fetch and the push. One re-read is
            # enough to distinguish a race from a standing divergence.
            reason = (
                f"cannot publish {GIT_AI_NOTE_REF} to {remote}: git push exited "
                f"{pushed.returncode}"
            )
            if attempt == 1:
                return "error", None, None, reason
        else:  # pragma: no cover - the loop always breaks or returns
            return "error", None, None, reason
        # The receipt attests what the campaign remote carries, not what the
        # checkout hoped to send, so the digest is taken from a read-back.
        target, landed = remote_note(checkout, remote, revision)
        if landed is None:
            return (
                "error",
                None,
                target,
                f"{GIT_AI_NOTE_REF} on {remote} carries no note for {revision} after the push",
            )
        if landed != note:
            return (
                "error",
                landed,
                target,
                f"{GIT_AI_NOTE_REF} on {remote} carries different bytes for {revision} "
                "than the ones this node published",
            )
        return "bound", landed, target, None
    finally:
        git(checkout, "update-ref", "-d", GIT_AI_PUBLISH_REF, check=False)
        git(checkout, "update-ref", "-d", GIT_AI_REMOTE_REF, check=False)


def bind_authorship(
    data: dict[str, Any],
    config: dict[str, Any],
    integration: dict[str, Any],
    method: str,
    merge_commit: str,
    binding: str,
    message: str,
    await_sec: int = GIT_AI_AWAIT_SEC,
) -> dict[str, Any] | None:
    """Bind Git AI authorship on the commit this node just integrated.

    §7 says the binding point "must move to the publish node". Taken
    literally that is impossible: GitHub mints the squash at merge time, so at
    publication the object being bound does not exist. This is that step, one
    node later and inside the same merge action -- no new flow node, so the
    51-node pin is untouched.

    Under `advisory` this function cannot fail the node, and that is enforced
    here rather than promised: every outcome, including an unexpected one, is
    turned into a typed receipt. The merge has already landed irreversibly by
    the time this runs, so an advisory subsystem that raised would report a
    merged task as failed. §9.1.4 is explicit about why advisory has to come
    first, and the spike doc records the reason it cannot be skipped: an
    unprovisioned host and a squash that lost its attribution produce
    identical evidence.
    """
    if binding == "off":
        return None
    # Under `merge` the working commits stay reachable from base carrying the
    # notes git-ai already minted for them, so the bound revision is the task
    # head. Under `squash` the forge minted a commit nothing has ever seen, and
    # the bound revision is that commit.
    bound_revision = merge_commit if method == "squash" else integration["head"]
    receipt = {
        "binding": binding,
        "status": "error",
        "revision": bound_revision,
        "noteRef": GIT_AI_NOTE_REF,
        "notesRefTarget": None,
        "noteSha256": None,
        "published": False,
        "reason": None,
    }
    try:
        bind_authorship_into(
            receipt, data, config, integration, method, merge_commit, message, await_sec
        )
    except DriverError as error:
        settle_binding(receipt, "error", str(error))
    except OSError as error:
        settle_binding(
            receipt, "error", f"the binding could not use the campaign workspace: {error}"
        )
    if binding == "required" and (receipt["status"] != "bound" or not receipt["published"]):
        fail(
            f"git-ai binding for {bound_revision} is {receipt['status']} under required "
            f"mode: {receipt['reason'] or 'no reason recorded'}"
        )
    return receipt


def settle_binding(receipt: dict[str, Any], status: str, reason: str | None) -> None:
    receipt["status"] = status
    receipt["reason"] = reason[:400] if reason else None


def bind_authorship_into(
    receipt: dict[str, Any],
    data: dict[str, Any],
    config: dict[str, Any],
    integration: dict[str, Any],
    method: str,
    merge_commit: str,
    message: str,
    await_sec: int,
) -> None:
    """The binding proper. Records its outcome in `receipt`; never returns one."""
    checkout: Path = config["checkout"]
    remote = config["remote"]
    bound_revision = receipt["revision"]
    version = git_ai_available(checkout)
    if version is None:
        settle_binding(
            receipt,
            "unavailable",
            f"{GIT_AI_PROGRAM} is not usable on this host; the estate provisions it "
            "and tally.nix does not ship it",
        )
        return
    # Never `check=True` past this point: the merge has landed and this is an
    # advisory subsystem. Nothing here quotes raw git stderr either -- the
    # receipt is quotable in a public failure report and a transport error
    # names the remote URL.
    fetched = git(checkout, "fetch", "--prune", remote, check=False)
    if fetched.returncode != 0:
        settle_binding(
            receipt,
            "error",
            f"cannot refresh {remote} in the campaign checkout: "
            f"git fetch exited {fetched.returncode}",
        )
        return
    if git(checkout, "cat-file", "-e", f"{merge_commit}^{{commit}}", check=False).returncode:
        settle_binding(
            receipt,
            "error",
            f"integrated commit {merge_commit} is absent from the campaign checkout",
        )
        return
    source = full_git_oid(integration["head"], "integration.head")
    if method == "squash":
        base_rev = full_git_oid(integration["baseRev"], "integration.baseRev")
        parent = git(
            checkout, "rev-parse", "--verify", f"{merge_commit}^1", check=False
        ).stdout.strip()
        if parent != base_rev:
            settle_binding(
                receipt,
                "mismatch",
                f"squash commit {merge_commit} has parent {parent or 'none'}, not the "
                f"gated base {base_rev}",
            )
            return
        local, reason = reconstruct_squash(config, data, base_rev, source, message, await_sec)
        if local is None:
            settle_binding(receipt, "error", reason)
            return
        if reason is not None:
            settle_binding(receipt, "unavailable", reason)
            return
        local_tree = git(
            checkout, "rev-parse", "--verify", f"{local}^{{tree}}", check=False
        ).stdout.strip()
        merged_tree = git(
            checkout, "rev-parse", "--verify", f"{merge_commit}^{{tree}}", check=False
        ).stdout.strip()
        if not local_tree or local_tree != merged_tree:
            settle_binding(
                receipt,
                "mismatch",
                f"reconstructed squash {local} carries tree {local_tree or 'none'}, but "
                f"the integrated commit carries {merged_tree or 'none'}; nothing may be "
                "copied",
            )
            return
        copied = git(
            checkout,
            "notes",
            "--ref",
            GIT_AI_NOTE_REF,
            "copy",
            "-f",
            local,
            merge_commit,
            check=False,
        )
        if copied.returncode != 0:
            # A campaign pass is re-enterable: a later reconcile can dispatch
            # the merge node again for a task whose pull request is already
            # MERGED. That pass reconstructs the identical commit, and git-ai
            # does not re-annotate an object its service has already processed,
            # so the copy has no source. If the integrated commit already
            # carries the note this step would have produced, the binding is
            # done rather than broken.
            already = git(
                checkout,
                "notes",
                "--ref",
                GIT_AI_NOTE_REF,
                "list",
                merge_commit,
                check=False,
            )
            if already.returncode != 0:
                detail = copied.stderr.strip() or copied.stdout.strip() or "no output"
                settle_binding(
                    receipt,
                    "missing-note",
                    f"git-ai {version} minted no note for the reconstructed squash "
                    f"{local}: {detail[:200]}",
                )
                return
        if local != merge_commit:
            # A notes entry is keyed by commit id as a path in the notes tree,
            # so it outlives the commit it annotates. The reconstruction is
            # unreachable the moment its worktree is removed, and leaving its
            # entry behind would accumulate one dead note per merged task.
            git(checkout, "notes", "--ref", GIT_AI_NOTE_REF, "remove", local, check=False)
    note = git_bytes(
        checkout, "notes", "--ref", GIT_AI_NOTE_REF, "show", bound_revision, check=False
    )
    if note.returncode != 0:
        settle_binding(
            receipt,
            "missing-note",
            f"{GIT_AI_NOTE_REF} has no note for {bound_revision} in the campaign checkout",
        )
        return
    status, landed, target, reason = publish_authorship_note(
        checkout, remote, bound_revision, note.stdout
    )
    receipt["notesRefTarget"] = target
    if landed is not None:
        receipt["noteSha256"] = "sha256:" + hashlib.sha256(landed).hexdigest()
    receipt["published"] = status == "bound"
    settle_binding(receipt, status, reason)


def merge_local(
    data: dict[str, Any],
    config: dict[str, Any],
    integration: dict[str, Any],
    method: str,
    narration: dict[str, Any],
    trailer: str | None = None,
) -> str:
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
        if method == "squash":
            git(
                integration_checkout,
                "merge",
                "--squash",
                f"origin/{integration['branch']}",
            )
            if not git(
                integration_checkout, "diff", "--cached", "--quiet", check=False
            ).returncode:
                fail("squash merge staged no change against the witnessed base")
            git(
                integration_checkout,
                "commit",
                "--quiet",
                "--file",
                "-",
                input_text=merge_commit_message(narration, trailer),
            )
        else:
            git(
                integration_checkout,
                "merge",
                "--no-ff",
                "--no-edit",
                f"origin/{integration['branch']}",
            )
        merge_commit = git(integration_checkout, "rev-parse", "HEAD").stdout.strip()
        if method == "squash":
            # Published before the base advances, and forced. A receipt naming
            # a commit that never reached base proves nothing, because the
            # reader requires it to be an ancestor of the witnessed base
            # anyway, so the ref carries no authority that fast-forward
            # protection would defend. It does need to be replaceable: if the
            # base push below loses a race to a sibling lane, the next pass
            # rebases and mints a squash with a different parent and a
            # different oid, and a non-forced push of that oid is a
            # non-fast-forward. Refusing it would fail the node before the base
            # push it needs to make progress, wedging the task permanently
            # behind a hidden ref no operator has been told about.
            receipt = merge_receipt_ref(
                required_string(data.get("campaign"), "campaign"),
                campaign_issue(data.get("issue"))["number"],
                integration["taskId"],
                task_revision(data["task"]),
            )
            git(
                integration_checkout,
                "push",
                "--force",
                "origin",
                f"{merge_commit}:{receipt}",
            )
        git(
            integration_checkout,
            "push",
            "origin",
            f"HEAD:refs/heads/{config['baseBranch']}",
        )
    return merge_commit


def merge_github(
    data: dict[str, Any],
    config: dict[str, Any],
    integration: dict[str, Any],
    capabilities: dict[str, bool],
    method: str,
    narration: dict[str, Any],
    trailer: str | None = None,
) -> str:
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
        command = [
            "gh",
            "pr",
            "merge",
            url,
            "--repo",
            repository,
            f"--{method}",
            "--match-head-commit",
            integration["head"],
        ]
        if method == "squash":
            # The squash message is the validated narration, passed explicitly
            # rather than left to whatever GitHub would assemble from the
            # working commits. `--body` is sent even when empty so the default
            # commit-list body is never substituted.
            command += [
                "--subject",
                narration["subject"],
                "--body",
                merge_commit_body(narration, trailer),
            ]
        run(command)
        viewed = run(
            ["gh", "pr", "view", url, "--repo", repository, "--json", "state,mergeCommit"]
        )
        state = json.loads(viewed.stdout)
    if state.get("state") != "MERGED":
        fail(f"pull request {url} did not reach MERGED")
    merge_commit = (state.get("mergeCommit") or {}).get("oid")
    merge_commit = required_string(merge_commit, "pull request merge commit")
    if method == "squash":
        full_git_oid(merge_commit, "pull request merge commit")
    git(checkout, "fetch", "--prune", config["remote"])
    # A squash mints a commit whose parent is the base tip, so the task head it
    # replaced is never an ancestor of the base and the pre-squash assertion
    # would fail on every successful merge. The merge commit itself is the
    # squash-compatible proof, and it is the same oid the read path already
    # validates against the witnessed base.
    contained = merge_commit if method == "squash" else integration["head"]
    if (
        git(
            checkout,
            "merge-base",
            "--is-ancestor",
            contained,
            f"{config['remote']}/{config['baseBranch']}",
            check=False,
        ).returncode
        != 0
    ):
        if method == "squash":
            fail(
                f"current remote base does not contain squash merge commit {merge_commit}"
            )
        fail("current remote base does not contain the merged task head")
    if not capabilities["subIssueWalk"]:
        github_merge_checkbox_repair(data)
    return merge_commit


def github_checkpoint_progress_comment(
    data: dict[str, Any], reference: str, revision: str, source_sha256: str
) -> None:
    """The degraded checkpoint projection: one comment plus the checkbox.

    Suppressed wherever the sub-issue walk is available, for the same reason
    the per-merge comment is: the parent renders its own progress.
    """
    repository = required_string(data.get("repository"), "repository")
    campaign = required_string(data.get("campaign"), "campaign")
    issue = campaign_issue(data.get("issue"))
    task = data["task"]
    task_id = task["id"]
    marker = (
        "<!-- tally:spec-build:v1 "
        f"campaign={campaign} issue={issue['number']} checkpoint={task_id} "
        f"source={source_sha256} revision={revision} passed -->"
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
    issue_view = github_json(
        ["api", f"repos/{repository}/issues/{issue['number']}"],
        "campaign issue",
    )
    issue_body = issue_view.get("body") if isinstance(issue_view, dict) else None
    if not isinstance(issue_body, str):
        fail("campaign issue has no body while recording checkpoint progress")
    task_marker = f"{TASK_MARKER_PREFIX}{task_id} -->"
    updated_lines: list[str] = []
    found = False
    for line in issue_body.splitlines(keepends=True):
        if task_marker in line:
            if found:
                fail(f"campaign worklist repeats task marker {task_id!r}")
            found = True
            line = re.sub(r"^- \[[ xX]\]", "- [x]", line)
        updated_lines.append(line)
    if not found:
        fail(f"campaign worklist lacks task marker {task_id!r}")
    updated_body = "".join(updated_lines)
    if updated_body != issue_body:
        run(
            [
                "gh",
                "issue",
                "edit",
                issue["number"],
                "--repo",
                repository,
                "--body-file",
                "-",
            ],
            input_text=updated_body,
        )


def action_checkpoint(brief: dict[str, Any]) -> dict[str, Any]:
    brief, capabilities = take_capabilities(brief)
    data = object_exact(
        brief,
        {
            "campaign",
            "repository",
            "repositoryConfig",
            "issue",
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
        {
            "id",
            "kind",
            "title",
            "argv",
            "runtimeMaxSec",
            "dependencies",
            "brief",
            "revision",
        },
        "checkpoint task",
    )
    task_id = required_string(task.get("id"), "checkpoint task.id", 80)
    if not TASK_ID.fullmatch(task_id) or task.get("kind") != "checkpoint":
        fail("checkpoint task must carry a safe id and kind checkpoint")
    required_string(task.get("title"), "checkpoint task.title", 300)
    argv(task.get("argv"), "checkpoint task.argv")
    positive_integer(task.get("runtimeMaxSec"), "checkpoint task.runtimeMaxSec")
    string_list(task.get("dependencies"), "checkpoint task.dependencies")
    revision = task_revision(task)
    source_value = data.get("source")
    if isinstance(source_value, dict) and source_value.get("kind") == "github-issue":
        source = object_exact(
            source_value, {"kind", "url", "sha256", "revision"}, "source"
        )
        required_string(source.get("url"), "source.url")
        if revision is None:
            fail("issue-backed checkpoint task must carry its admitted revision")
    else:
        source = object_exact(source_value, {"path", "sha256", "revision"}, "source")
        required_string(source.get("path"), "source.path")
    source_sha256 = required_string(source.get("sha256"), "source.sha256")
    source_revision = required_string(source.get("revision"), "source.revision")
    if not re.fullmatch(r"[0-9a-f]{40,64}", source_revision):
        fail("source.revision must be a full Git object ID")
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
    if git(
        checkout,
        "merge-base",
        "--is-ancestor",
        source_revision,
        base_rev,
        check=False,
    ).returncode:
        fail("prepared checkpoint base does not descend from the witnessed worklist revision")
    if git(
        checkout,
        "merge-base",
        "--is-ancestor",
        base_rev,
        current_base,
        check=False,
    ).returncode:
        fail("remote base diverged after the checkpoint command was witnessed")
    # A receipt already published under the pre-#307 visible tag namespace is
    # honored where it stands; the hidden namespace is only ever written new.
    legacy = legacy_checkpoint_tag(
        campaign, issue["number"], task_id, source_sha256, base_rev
    )
    reference = (
        legacy
        if remote_ref_oid(worktree, remote, legacy) == base_rev
        else checkpoint_ref(campaign, issue["number"], task_id, source_sha256, base_rev)
    )
    existing = remote_ref_oid(worktree, remote, reference)
    if existing is not None and existing != base_rev:
        fail(f"immutable checkpoint ref {reference!r} already points to another object")
    if existing is None:
        pushed = git(worktree, "push", remote, f"{base_rev}:{reference}", check=False)
        if pushed.returncode and remote_ref_oid(worktree, remote, reference) != base_rev:
            detail = pushed.stderr.strip() or pushed.stdout.strip() or "no output"
            fail(f"cannot create immutable checkpoint ref {reference!r}: {detail}")
    if remote_ref_oid(worktree, remote, reference) != base_rev:
        fail("checkpoint completion ref did not expose the witnessed base revision")
    if current_base != base_rev:
        # The tested point is recorded first and stays recorded: the receipt is
        # true, and nothing will ever re-test this revision. What it cannot do
        # is complete the task. Reconciliation reads a checkpoint under the
        # revision it reconciled, so a base that has already moved past the
        # tested one leaves this receipt unread, the checkpoint incomplete, and
        # the next pass re-executing it. Reporting that as progress is what let
        # a base branch moving faster than the checkpoint runs keep a campaign
        # "advanced" for ever. The lane fails instead, which spends a steering
        # attempt and reaches escalation in a bounded number of passes. This
        # only fires on movement from outside the pass: a campaign's own merges
        # all land before its checkpoint lanes prepare.
        fail(
            f"checkpoint {task_id!r} recorded {base_rev} in {reference!r}, but the base "
            f"branch has already advanced to {current_base}: the receipt cannot complete "
            "the task and the base branch is moving faster than this checkpoint runs"
        )
    if config["forge"] == "github" and not capabilities["subIssueWalk"]:
        github_checkpoint_progress_comment(data, reference, base_rev, source_sha256)
    return {"taskId": task_id, "ref": reference, "revision": base_rev}


def github_merge_checkbox_repair(data: dict[str, Any]) -> None:
    """Tick this task's worklist checkbox on the master issue.

    The degraded projection only. A campaign whose forge serves the sub-issue
    walk renders progress from the parent's own `subIssuesSummary`, so tally
    writes neither this checkbox nor the per-merge progress comment that used
    to accompany it.
    """
    repository = required_string(data.get("repository"), "repository")
    issue = campaign_issue(data.get("issue"))
    issue_number = issue["number"]
    task = data.get("task")
    if not isinstance(task, dict):
        fail("task must be an object")
    task_id = required_string(task.get("id"), "task.id")
    if not isinstance(task.get("brief"), dict):
        return
    issue_view = github_json(
        ["api", f"repos/{repository}/issues/{issue_number}"],
        "campaign issue",
    )
    issue_body = issue_view.get("body") if isinstance(issue_view, dict) else None
    if not isinstance(issue_body, str):
        fail("campaign issue has no body while recording merge progress")
    task_marker = f"{TASK_MARKER_PREFIX}{task_id} -->"
    updated_lines: list[str] = []
    found_line = False
    for line in issue_body.splitlines(keepends=True):
        if task_marker in line:
            if found_line:
                fail(f"campaign worklist repeats task marker {task_id!r}")
            found_line = True
            line = re.sub(r"^- \[[ xX]\]", "- [x]", line)
        updated_lines.append(line)
    if not found_line:
        fail(f"campaign worklist lacks task marker {task_id!r}")
    updated_body = "".join(updated_lines)
    if updated_body != issue_body:
        run(
            [
                "gh",
                "issue",
                "edit",
                issue_number,
                "--repo",
                repository,
                "--body-file",
                "-",
            ],
            input_text=updated_body,
        )


def action_merge(brief: dict[str, Any]) -> dict[str, Any]:
    brief, capabilities = take_capabilities(brief)
    data, config, worktree = publication_identity(brief, "merge")
    integration = object_exact(
        data.get("integration"),
        {
            "taskId",
            "baseRev",
            "branch",
            "head",
            "pullRequest",
            "narration",
            "regate",
            "ownership",
        },
        "integration",
    )
    method = merge_method(data.get("mergeMethod"), "mergeMethod")
    binding = git_ai_binding(data.get("gitAiBinding"), "gitAiBinding")
    await_sec = git_ai_await_sec(data.get("gitAiAwaitSec"), "gitAiAwaitSec")
    narration = narration_record(integration.get("narration"), "integration.narration")
    # The provenance pointer is the node's, from the witnessed attempt the
    # reconciler already correlates. Only a squash gets one: a merge commit
    # carries git's own template message and the working commits it collects
    # keep their own notes.
    assisted_by = assisted_by_record(data.get("assistedBy"), "assistedBy")
    trailer = assisted_by_trailer(assisted_by) if method == "squash" else None
    # The campaign's own parallelism decides whether domains are required. The
    # receipt carries an upstream copy of that bit, and merge is the last node
    # that can still refuse to act on it, so the two are compared here rather
    # than normalized and trusted — the same repair #254 made elsewhere.
    domains_required = required_bool(data.get("domainsRequired"), "domainsRequired")
    task_id = required_string(integration.get("taskId"), "integration.taskId")
    base_rev = full_git_oid(integration.get("baseRev"), "integration.baseRev")
    required_string(integration.get("branch"), "integration.branch")
    head = full_git_oid(integration.get("head"), "integration.head")
    pull_request = required_string(integration.get("pullRequest"), "integration.pullRequest")
    required_bool(integration.get("regate"), "integration.regate")
    ownership = normalize_ownership_receipt(
        integration.get("ownership"), "integration.ownership"
    )
    if ownership["taskId"] != task_id:
        fail("integration.ownership.taskId does not match integration.taskId")
    if ownership["domainsRequired"] != domains_required:
        fail("integration.ownership.domainsRequired does not match domainsRequired")
    if ownership["baseRev"] != base_rev:
        fail("integration.ownership.baseRev does not match integration.baseRev")
    if ownership["head"] != head:
        fail("integration.ownership.head does not match integration.head")
    if task_id != data["workspace"]["taskId"]:
        fail("integration.taskId does not match workspace.taskId")
    if integration["branch"] != data["workspace"]["publishBranch"]:
        fail("integration.branch does not match workspace.publishBranch")
    if config["forge"] == "github":
        merge_commit = merge_github(
            data, config, integration, capabilities, method, narration, trailer
        )
    else:
        merge_commit = merge_local(
            data, config, integration, method, narration, trailer
        )
    authorship = bind_authorship(
        data,
        config,
        integration,
        method,
        merge_commit,
        binding,
        merge_commit_message(narration, trailer),
        await_sec,
    )
    return {
        "taskId": task_id,
        "head": head,
        "mergeCommit": merge_commit,
        "pullRequest": pull_request,
        "regated": integration["regate"],
        "ownership": ownership,
        "authorship": authorship,
        "trailer": trailer,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "action",
        choices=(
            "worklist",
            "sweep",
            "reconcile",
            "diff",
            "steer",
            "retry",
            "escalate",
            "continue",
            "preflight",
            "prep",
            "cleanup",
            "ownership",
            "constraint",
            "checkpoint",
            "publish",
            "rebase",
            "merge",
        ),
    )
    arguments = parser.parse_args()
    brief = load_brief()
    actions = {
        "worklist": action_worklist,
        "sweep": action_sweep,
        "reconcile": action_reconcile,
        "diff": action_diff,
        "steer": action_steer,
        "retry": action_retry,
        "escalate": action_escalate,
        "continue": action_continue,
        "preflight": action_preflight,
        "prep": action_prep,
        "ownership": action_ownership,
        "constraint": action_constraint,
        "checkpoint": action_checkpoint,
        "cleanup": action_cleanup,
        "publish": action_publish,
        "rebase": action_rebase,
        "merge": action_merge,
    }
    emit(actions[arguments.action](brief))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (DriverError, worktrees.WorktreeError) as error:
        print(f"spec-build-driver: {error}", file=sys.stderr)
        raise SystemExit(1)
