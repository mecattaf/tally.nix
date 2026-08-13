#!/usr/bin/env python3
"""Deterministic policy driver for the shipped spec-build tally flow."""

from __future__ import annotations

import argparse
from datetime import datetime, timedelta
import fcntl
import fnmatch
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from typing import Any
import uuid

# The unified worktree manager ships beside this driver, so both campaign
# drivers create, resume, validate, and clean lanes through one implementation.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import campaign_worktrees as worktrees  # noqa: E402


MISSING = object()
TASK_ID = re.compile(r"^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$")
COMPONENT = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_.-]*$")
REPOSITORY = re.compile(r"^[^/ \t]+/[^/ \t]+$")
GIT_OID = re.compile(r"^[0-9a-f]{40,64}$")
# A campaign machinery fault is not evidence that the task's work is wrong, so
# it buys a retry instead of a steering attempt. The budget is bounded and read
# back from durable campaign state: past it, the fault is treated as a failed
# attempt.
MAX_MACHINE_RETRIES = 2
MAX_RETRY_CHARS = 2_000
ATTEMPT_RECEIPTS_SCHEMA_VERSION = 1
ATTEMPT_RECEIPTS_FILE = "attempt-receipts-v1.jsonl"
MAX_ATTEMPT_RECEIPTS_LOG_BYTES = 128 * 1024 * 1024
# A worker's final message is a durable finding, not an unbounded transcript.
# The bound covers the complete local receipt after redaction.
MAX_WORKER_FINDINGS_BYTES = 8 * 1024
WORKER_FINDINGS_TRUNCATION = "\n[... worker findings truncated after redaction ...]"
# Checkpoint output is retained for diagnosis, not as a second unbounded raw
# capture. Keep the tail, where command harnesses normally put the actionable
# failure, under the daemon's existing capture-archive retention envelope.
CHECKPOINT_CAPTURE_MAX_BYTES = 8 * 1024
CHECKPOINT_CAPTURE_SCHEMA_VERSION = 1
CHECKPOINT_CAPTURE_FILE = "checkpoint.json"
CHECKPOINT_STDERR_LINES = 10
CHECKPOINT_PUBLIC_NOTE_MAX_CHARS = 1_000
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
# `Assisted-by: <adapter>:<model> (tally:<taskUuid> witness:<seq>)`. The exact
# established trailer format; the trailer is a pointer into the witness, never
# the proof (§7). Reject a narrator that proposes one: the
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
# Release narration can become both commit prose and public projection prose.
# Closing keywords and mentions carry side effects there, so neither belongs
# to the model-authored slot. A bare `#<n>` cross reference stays allowed: it
# backlinks and notifies nobody.
NARRATION_CLOSING_KEYWORD = re.compile(
    r"(?i)\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\b\s*:?\s*"
    r"(?:\#\d+|GH-\d+|[\w.-]+/[\w.-]+\#\d+|https?://\S+/(?:issues|pull)/\d+)"
)
NARRATION_MENTION = re.compile(r"(?<![0-9A-Za-z._-])@[0-9A-Za-z][0-9A-Za-z-]*")

# #385: the managed-agents content guidelines the chips-and-log rendering
# assumes -- an outcome-first leading sentence, a past-tense opening verb, no
# exclamation, a bounded length -- made machine-checkable and applied to every
# prose surface that crosses the publish boundary: PR prose (the narrate
# slot's proposal body), the closing summary, and steering notes. One
# validator, three callers, so the contract lives in exactly one place.
OUTCOME_FIRST_LIST_MARKER = re.compile(r"^\s*(?:[-*]\s+|\d+[.)]\s+)")
# Regular past-tense verbs end in "ed"; the irregulars below are the ones this
# driver's own templates and a steward's plausible vocabulary actually use.
# The set is deliberately not exhaustive prose-wide -- it only has to cover
# what a campaign's own surfaces say -- and growing it never tightens an
# already-accepted proposal, only widens what a future one may open with.
OUTCOME_FIRST_IRREGULAR_VERBS = frozenset(
    {
        "began", "bound", "brought", "bought", "built", "came", "caught",
        "chose", "cut", "did", "fell", "found", "gave", "grew", "held",
        "hit", "kept", "knew", "left", "lost", "made", "met", "put", "ran",
        "read", "said", "sat", "saw", "sent", "set", "shut", "spent",
        "stood", "sold", "sought", "spoke", "spent", "taught", "thought",
        "took", "understood", "went", "won", "wrote",
    }
)
OUTCOME_FIRST_VERB = re.compile(
    r"^(?:[A-Za-z]+ed|" + "|".join(sorted(OUTCOME_FIRST_IRREGULAR_VERBS)) + r")\b",
    re.IGNORECASE,
)
OUTCOME_FIRST_LEAD_MAX = 240


def validate_outcome_first(text: Any, *, max_chars: int, context: str) -> str | None:
    """The machine-checkable half of the managed-agents content contract.

    Returns `None` when `text` satisfies it, or the one reason it does not.
    The four checks are independent and each names itself: a leading sentence
    (ending `.` or `:`) precedes any list rather than a list opening the text,
    that sentence opens with a past-tense verb, nothing in the text carries an
    exclamation mark, and the whole text stays under `max_chars`. Applied
    uniformly whether the text is steward-proposed or driver-rendered, so a
    template that drifts from its own contract fails loudly instead of
    shipping unchecked.
    """
    if not isinstance(text, str) or not text.strip():
        return f"{context} must be non-empty text"
    if len(text) > max_chars:
        return f"{context} is over the {max_chars} character cap"
    if "!" in text:
        return f"{context} contains an exclamation mark"
    first_line = text.strip().split("\n", 1)[0].strip()
    if OUTCOME_FIRST_LIST_MARKER.match(first_line):
        return f"{context} must open with a sentence, not a list"
    first_sentence = re.split(r"(?<=[.:])\s", first_line, maxsplit=1)[0].strip()
    if not first_sentence.endswith((".", ":")):
        return f"{context} leading sentence must end with a period"
    if len(first_sentence) > OUTCOME_FIRST_LEAD_MAX:
        return f"{context} leading sentence is over {OUTCOME_FIRST_LEAD_MAX} characters"
    opening = re.match(r"^[A-Za-z]+", first_sentence)
    if not opening or not OUTCOME_FIRST_VERB.match(opening.group(0)):
        return f"{context} must open with a past-tense verb"
    return None


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


def object_complete(value: Any, fields: set[str], context: str) -> dict[str, Any]:
    """Decode a canonical object whose normalized members are all present."""
    record = object_exact(value, fields, context)
    missing = sorted(fields - set(record))
    if missing:
        fail(f"{context} is missing canonical fields: {', '.join(missing)}")
    return record


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
    encoded = canonical_json(value).encode("utf-8")
    return "sha256:" + hashlib.sha256(encoded).hexdigest()


def canonical_json(value: Any) -> str:
    """Compact recursively key-sorted JSON shared with campaign_contract."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def json_type(value: Any) -> str:
    """Return a canonical JSON type, distinguishing booleans from integers."""
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "boolean"
    if isinstance(value, int):
        return "integer"
    if isinstance(value, float):
        return "number"
    if isinstance(value, str):
        return "string"
    if isinstance(value, list):
        return "array"
    if isinstance(value, dict):
        return "object"
    return "unknown"


def first_divergent_canonical_path(armed: Any, live: Any, prefix: str = "") -> str | None:
    """Describe the first canonical shape divergence without exposing values."""
    where = prefix or "<root>"
    if isinstance(armed, dict) and isinstance(live, dict):
        for key in sorted(set(armed) | set(live)):
            child = f"{prefix}.{key}" if prefix else key
            if key not in armed:
                return f"{child}: absent-in-armed / present-in-live"
            if key not in live:
                return f"{child}: present-in-armed / absent-in-live"
            found = first_divergent_canonical_path(armed[key], live[key], child)
            if found is not None:
                return found
        return None
    if isinstance(armed, list) and isinstance(live, list):
        if len(armed) != len(live):
            return f"{where}: array-length armed={len(armed)} live={len(live)}"
        for index, (armed_item, live_item) in enumerate(zip(armed, live)):
            found = first_divergent_canonical_path(
                armed_item, live_item, f"{prefix}[{index}]"
            )
            if found is not None:
                return found
        return None
    if type(armed) is not type(live):
        return f"{where}: type-mismatch armed={json_type(armed)} live={json_type(live)}"
    if armed != live:
        return f"{where}: value-differs ({json_type(armed)}); value withheld"
    return None


def graph_digest_mismatch_receipt(
    armed_manifest: Any,
    live_manifest: dict[str, Any],
    admitted_digest: str,
    live_digest: str,
) -> str:
    """Render bounded local graph-divergence evidence without manifest values."""
    lines = [
        "live local executable graph does not match the armed digest; "
        "inspect it and explicitly re-arm",
        f"armed digest: {admitted_digest}",
        f"live digest: {live_digest}",
    ]
    if isinstance(armed_manifest, dict):
        path = first_divergent_canonical_path(armed_manifest, live_manifest)
        if path is None:
            lines.append(
                "first divergent canonical path: none within the manifest; the "
                "divergence lies in the task set or its canonicalization"
            )
        else:
            lines.append(f"first divergent canonical path: manifest.{path}")
    else:
        lines.append(
            "first divergent canonical path: unavailable (no armed manifest "
            "recorded at arm); compare the armed and live digests above"
        )
    return "; ".join(lines)


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
        # Campaign checkouts arrive filesystem-canonical from Rust. Keep that
        # admitted identity instead of independently resolving a second
        # spelling at the consumer boundary.
        "checkout": checkout,
        "baseBranch": base_branch,
        "remote": remote,
        "forge": forge,
    }


# A campaign has three repository coordinates. `repository` and
# `repositoryConfig` are the *code* coordinate: where lanes, stable task
# branches, and integration commits live. A spec-corpus campaign may read its
# worklist from a second repository and keep its local summary and findings
# refs on a third. Both are optional and both default inward: spec falls back
# to code, receipt storage falls back to spec. A campaign that configures
# neither resolves all three to the same pair.
SEAM_COORDINATES = ("specRepository", "issueRepository")


def campaign_coordinate(value: Any, context: str) -> dict[str, Any]:
    entry = object_exact(value, {"repository", "repositoryConfig"}, context)
    repository = required_string(entry.get("repository"), f"{context}.repository")
    if not REPOSITORY.fullmatch(repository):
        fail(f"{context}.repository must use owner/name form")
    return {
        "repository": repository,
        "config": repo_config(entry.get("repositoryConfig")),
    }


def seam_fields(brief: Any, fields: set[str]) -> set[str]:
    """Admit whichever seam coordinates this brief actually carries.

    A brief that names none of them is validated against exactly the field set
    it was validated against before, so an unknown key is still refused.
    """
    if not isinstance(brief, dict):
        return fields
    return fields | {name for name in SEAM_COORDINATES if name in brief}


def campaign_coordinates(
    data: dict[str, Any], repository: str, config: dict[str, Any]
) -> dict[str, dict[str, Any]]:
    code = {"repository": repository, "config": config}
    spec = (
        campaign_coordinate(data["specRepository"], "specRepository")
        if data.get("specRepository") is not None
        else code
    )
    issue = (
        campaign_coordinate(data["issueRepository"], "issueRepository")
        if data.get("issueRepository") is not None
        else spec
    )
    return {"code": code, "spec": spec, "issue": issue}


def carried_coordinates(data: dict[str, Any]) -> dict[str, Any]:
    """The seam block to forward verbatim into a nested brief."""
    return {
        name: data[name]
        for name in SEAM_COORDINATES
        if data.get(name) is not None
    }


def same_repository(left: dict[str, Any], right: dict[str, Any]) -> bool:
    return (
        left["repository"] == right["repository"]
        and left["config"]["checkout"] == right["config"]["checkout"]
    )


def observed_base_revision(config: dict[str, Any]) -> str:
    """The base branch tip of one repository, taken from its remote."""
    checkout: Path = config["checkout"]
    git(checkout, "fetch", "--prune", "--no-tags", config["remote"])
    return git(
        checkout,
        "rev-parse",
        "--verify",
        f"{config['remote']}/{config['baseBranch']}^{{commit}}",
    ).stdout.strip()


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


def portable_steward_pattern(pattern: str, context: str) -> None:
    """Defensively enforce the portable Rust/Python regular-expression subset."""
    characters = list(pattern)
    index = 0
    in_class = False
    portable_escapes = {
        "\\",
        ".",
        "^",
        "$",
        "|",
        "?",
        "*",
        "+",
        "(",
        ")",
        "[",
        "]",
        "{",
        "}",
        "-",
        "/",
        "d",
        "D",
        "s",
        "S",
        "w",
        "W",
        "n",
        "r",
        "t",
        "f",
    }
    while index < len(characters):
        character = characters[index]
        if character == "\\":
            index += 1
            if index == len(characters):
                fail(f"internal campaign contract violation: {context} ends in an escape")
            escaped = characters[index]
            if (escaped.isascii() and escaped.isdigit()) or escaped in {"k", "g"}:
                fail(f"internal campaign contract violation: {context} contains a backreference")
            if escaped == "x":
                hexadecimal = characters[index + 1 : index + 3]
                if len(hexadecimal) != 2 or any(
                    digit not in "0123456789abcdefABCDEF" for digit in hexadecimal
                ):
                    fail(
                        f"internal campaign contract violation: "
                        f"{context} contains an invalid hexadecimal escape"
                    )
                index += 2
            elif escaped not in portable_escapes:
                fail(
                    f"internal campaign contract violation: "
                    f"{context} contains a non-portable escape"
                )
        elif character == "[":
            if in_class:
                fail(
                    f"internal campaign contract violation: "
                    f"{context} contains a nested character class"
                )
            in_class = True
        elif character == "]":
            in_class = False
        elif (
            in_class
            and index + 1 < len(characters)
            and (character, characters[index + 1])
            in {("&", "&"), ("-", "-"), ("~", "~"), ("|", "|")}
        ):
            fail(
                f"internal campaign contract violation: "
                f"{context} contains a non-portable character-class operation"
            )
        elif (
            not in_class
            and character == "("
            and characters[index + 1 : index + 2] == ["?"]
            and characters[index + 2 : index + 3] != [":"]
        ):
            fail(
                f"internal campaign contract violation: "
                f"{context} contains a non-portable group"
            )
        index += 1

    try:
        compiled = re.compile(pattern)
    except re.error as error:
        fail(
            "internal campaign contract violation: "
            f"{context} did not compile in Python: {error}"
        )
    if compiled.groups != 1:
        fail(
            "internal campaign contract violation: "
            f"{context} does not have exactly one capture group"
        )


def steward_role(value: Any, context: str = "campaign steward") -> dict[str, Any] | None:
    """Decode the fully normalized §2 steward catalog role.

    `adapter` names the entry in the estate's adapter table; that entry's argv
    and env are what decide which model answers, at which endpoint, with which
    credentials. Rust campaign admission (or the Nix module for module-declared
    campaigns) has already supplied every member and admitted the portable
    final-message grammar. Null is the shipped state: no steward, template
    narration, no model call. No defaults live on this side of the boundary.
    """
    if value is None:
        return None
    role = object_complete(
        value,
        {"adapter", "argv", "env", "finalMessagePattern", "runtimeMaxSec"},
        context,
    )
    adapter = required_string(role.get("adapter"), f"{context}.adapter", 80)
    if not COMPONENT.fullmatch(adapter):
        fail(f"{context}.adapter is not a safe component")
    arguments = argv(role.get("argv"), f"{context}.argv")
    environment = role["env"]
    if not isinstance(environment, dict):
        fail(f"{context}.env must be an object")
    if len(environment) > 64:
        fail(f"internal campaign contract violation: {context}.env exceeds 64 entries")
    for key, item in environment.items():
        if not isinstance(key, str) or not ENVIRONMENT_NAME.fullmatch(key):
            fail(f"{context}.env names must be environment identifiers")
        if key in RESERVED_STEWARD_ENVIRONMENT:
            fail(f"{context}.env must not set reserved variable {key}")
        required_string(item, f"{context}.env.{key}", 4096)
    pattern = role["finalMessagePattern"]
    pattern = required_string(pattern, f"{context}.finalMessagePattern", 1024)
    portable_steward_pattern(pattern, f"{context}.finalMessagePattern")
    runtime = role["runtimeMaxSec"]
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


def normalize_conflict_domains(
    value: Any, context: str, *, required: bool
) -> list[str] | None:
    if value is MISSING:
        if required:
            fail(f"{context} must be a non-empty array")
        return None
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
    normalized = {
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
    }
    domains = normalize_conflict_domains(
        task["conflictDomains"] if "conflictDomains" in task else MISSING,
        f"{context}.conflictDomains",
        required=require_conflict_domains,
    )
    if domains is not None:
        normalized["conflictDomains"] = domains
    return normalized


def action_worklist(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief,
        seam_fields(
            brief,
            {"repository", "repositoryConfig", "worklist", "maxTasks", "maxParallel"},
        ),
        "worklist brief",
    )
    repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(repository):
        fail("repository must use owner/name form")
    config = repo_config(data.get("repositoryConfig"))
    coordinates = campaign_coordinates(data, repository, config)
    spec = coordinates["spec"]
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

    # The worklist is read from the spec coordinate at a pinned revision. With
    # no spec repository configured that is the code checkout and this is the
    # same fetch, the same base ref, and the same revision as before.
    checkout: Path = spec["config"]["checkout"]
    remote = spec["config"]["remote"]
    git(checkout, "fetch", "--prune", "--no-tags", remote)
    base_ref = f"{remote}/{spec['config']['baseBranch']}"
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
    source = {
        "path": source_path,
        "sha256": "sha256:" + hashlib.sha256(raw).hexdigest(),
        "revision": base_rev,
    }
    if not same_repository(spec, coordinates["code"]):
        # Only a split campaign records this. With one repository the pinned
        # revision is unambiguous and the witness stays byte-identical.
        source["repository"] = spec["repository"]
    for task in tasks:
        task["revision"] = file_task_completion_revision(repository, source, task)
    return {
        "schemaVersion": 1,
        "repository": repository,
        "source": source,
        "tasks": tasks,
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
    if body:
        # The body is PR prose (§3's crossing point splices it into the pull
        # request) as well as the squash commit body, so it is where the
        # outcome-first content contract applies -- the header stays
        # conventional-commit-shaped (lower-case, no trailing period), which
        # is a different, narrower grammar than a sentence. Checked last: a
        # forged marker or trailer is the more serious refusal and names
        # itself first when a proposal manages to trip both.
        outcome_reason = validate_outcome_first(
            body, max_chars=NARRATION_BODY_MAX, context="proposal body"
        )
        if outcome_reason:
            return None, outcome_reason
    return {"subject": header, "body": body}, None


def template_narration(task: dict[str, Any], *, body: str = "") -> dict[str, Any]:
    """The brief-derived fallback: exactly the pre-steward publication text.

    `body` defaults to empty: no steward configured, nothing to say. A
    narrate() call that spent both attempts passes a durable fallback note
    instead (#385) -- refusing loudly beats degrading silently, so the fact
    that the fallback fired is never silent.
    """
    return {
        "source": "template",
        "subject": f"{task['id']}: {task['title']}",
        "body": body,
    }


def narration_fallback_note(transcript: list[dict[str, Any]]) -> str:
    """The durable fact a steward's narration fallback fires (#385).

    Bounded and deterministic -- built from the driver's own rejection
    reasons, never from the model's raw, unvalidated text -- and itself
    outcome-first grammar-shaped, so it survives being spliced into the same
    PR body and squash commit the steward's own prose would have occupied.
    """
    reasons = "; ".join(
        f"attempt {entry['attempt']} ({entry['status']}): {entry['reason']}"
        for entry in transcript
        if entry.get("reason")
    )
    note = (
        f"Rejected {len(transcript)} steward narration proposal(s) and used "
        f"the task-id template instead. Reasons: {reasons}."
    )
    if len(note) > NARRATION_BODY_MAX:
        note = note[: NARRATION_BODY_MAX - 1].rstrip() + "…"
    return note


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
    fallback_body = narration_fallback_note(transcript)
    self_check = validate_outcome_first(
        fallback_body, max_chars=NARRATION_BODY_MAX, context="narration fallback note"
    )
    if self_check:
        fail(f"narration fallback note violates its own grammar: {self_check}")
    return template_narration(task, body=fallback_body), transcript


def canonical_forbid_paths_gate(value: Any, context: str) -> dict[str, Any]:
    """Decode the forbidPaths variant shared by contract and gate actions."""
    gate = object_complete(
        value,
        {"kind", "id", "forbidPaths", "runtimeMaxSec"},
        context,
    )
    if gate["kind"] != "forbidPaths":
        fail(f"{context}.kind must equal forbidPaths")
    gate_id = required_string(gate["id"], f"{context}.id", 80)
    if not COMPONENT.fullmatch(gate_id):
        fail(f"{context}.id is not a safe component")
    patterns = string_list(gate["forbidPaths"], f"{context}.forbidPaths", nonempty=True)
    if len(patterns) > 128:
        fail(f"{context}.forbidPaths exceeds 128 entries")
    seen: set[str] = set()
    for pattern_index, pattern in enumerate(patterns):
        if len(pattern) > 1024:
            fail(f"{context}.forbidPaths[{pattern_index}] exceeds 1024 characters")
        components = pattern.split("/")
        if (
            pattern.startswith("/")
            or pattern.endswith("/")
            or ".." in components
            or any("**" in component and component != "**" for component in components)
            or pattern in seen
        ):
            fail(
                f"internal campaign contract violation: "
                f"{context}.forbidPaths[{pattern_index}] is not canonical"
            )
        seen.add(pattern)
    positive_integer(gate["runtimeMaxSec"], f"{context}.runtimeMaxSec")
    return {"kind": "forbidPaths", "id": gate_id, "forbidPaths": patterns}


def canonical_gate_list(value: Any, context: str) -> list[dict[str, Any]]:
    """Decode the closed, already-normalized canonical gate variants."""
    if not isinstance(value, list) or not 1 <= len(value) <= 16:
        fail(f"{context} must contain 1..=16 entries")
    for index, candidate in enumerate(value):
        gate_context = f"{context}[{index}]"
        if not isinstance(candidate, dict):
            fail(f"{gate_context} must be an object")
        kind = candidate.get("kind")
        if kind == "command":
            gate = object_complete(
                candidate,
                {"kind", "id", "preflightArgv", "argv", "runtimeMaxSec"},
                gate_context,
            )
            required_string(gate["id"], f"{gate_context}.id", 80)
            argv(gate["preflightArgv"], f"{gate_context}.preflightArgv")
            argv(gate["argv"], f"{gate_context}.argv")
            positive_integer(gate["runtimeMaxSec"], f"{gate_context}.runtimeMaxSec")
        elif kind == "forbidPaths":
            canonical_forbid_paths_gate(candidate, gate_context)
        else:
            fail(f"{gate_context}.kind must be command or forbidPaths")
    return value


def canonical_nullable_string(value: Any, context: str) -> str | None:
    if value is None:
        return None
    return required_string(value, context)


def canonical_nullable_positive_integer(value: Any, context: str) -> int | None:
    if value is None:
        return None
    return positive_integer(value, context)


def canonical_agent(value: Any, context: str) -> dict[str, Any]:
    fields = {
        "adapter",
        "argv",
        "priority",
        "runtimeMaxSec",
        "approvalPolicy",
        "sandboxPolicy",
        "diagnosisSandboxPolicy",
        "model",
    }
    agent = object_complete(value, fields, context)
    required_string(agent["adapter"], f"{context}.adapter")
    argv(agent["argv"], f"{context}.argv")
    required_string(agent["priority"], f"{context}.priority")
    canonical_nullable_positive_integer(agent["runtimeMaxSec"], f"{context}.runtimeMaxSec")
    canonical_nullable_string(agent["approvalPolicy"], f"{context}.approvalPolicy")
    canonical_nullable_string(agent["sandboxPolicy"], f"{context}.sandboxPolicy")
    canonical_nullable_string(
        agent["diagnosisSandboxPolicy"], f"{context}.diagnosisSandboxPolicy"
    )
    model = canonical_nullable_string(agent["model"], f"{context}.model")
    if model is not None and len(model) > 128:
        fail(f"{context}.model exceeds 128 characters")
    return agent


def canonical_task_references(
    value: Any, context: str, *, require_conflict_domains: bool
) -> list[dict[str, Any]]:
    if not isinstance(value, list) or not value:
        fail(f"{context} must be a non-empty array")
    common_fields = {
        "id",
        "kind",
        "issue",
        "dependencies",
        "argv",
        "runtimeMaxSec",
    }
    for index, candidate in enumerate(value):
        task_context = f"{context}[{index}]"
        if not isinstance(candidate, dict):
            fail(f"{task_context} must be an object")
        kind = candidate.get("kind")
        if kind == "implementation":
            task = object_exact(candidate, common_fields | {"conflictDomains"}, task_context)
        elif kind == "checkpoint":
            task = object_exact(candidate, common_fields, task_context)
        else:
            fail(f"{task_context}.kind must be implementation or checkpoint")
        missing = sorted(common_fields - set(task))
        if missing:
            fail(f"{task_context} is missing canonical fields: {', '.join(missing)}")
        required_string(task["id"], f"{task_context}.id", 80)
        positive_integer(task["issue"], f"{task_context}.issue")
        string_list(task["dependencies"], f"{task_context}.dependencies")
        if kind == "implementation":
            normalize_conflict_domains(
                task["conflictDomains"] if "conflictDomains" in task else MISSING,
                f"{task_context}.conflictDomains",
                required=require_conflict_domains,
            )
            if task["argv"] is not None or task["runtimeMaxSec"] is not None:
                fail(f"{task_context} implementation argv and runtimeMaxSec must be null")
        else:
            argv(task["argv"], f"{task_context}.argv")
            positive_integer(task["runtimeMaxSec"], f"{task_context}.runtimeMaxSec")
    return value


def canonical_manifest(value: Any) -> dict[str, Any]:
    """Exact decoder for the normalized manifest carried by Rust.

    This pure contract mirror remains until the scheduled driver port removes
    the cross-language contract corpus. It is not a worklist or forge decoder.
    """
    fields = {
        "schemaVersion",
        "name",
        "repository",
        "maxTasks",
        "maxParallel",
        "driverRuntimeMaxSec",
        "runtimeMaxSec",
        "pool",
        "mergeMethod",
        "agent",
        "steward",
        "gates",
        "tasks",
    }
    manifest = object_complete(value, fields, "canonical campaign manifest v1")
    if (
        not isinstance(manifest["schemaVersion"], int)
        or isinstance(manifest["schemaVersion"], bool)
        or manifest["schemaVersion"] != 1
    ):
        fail("canonical campaign manifest v1.schemaVersion must equal 1")
    required_string(manifest["name"], "canonical campaign manifest v1.name", 80)
    repository = object_complete(
        manifest["repository"],
        {"checkout", "baseBranch", "remote", "forge"},
        "canonical campaign manifest v1.repository",
    )
    required_string(repository["checkout"], "canonical campaign manifest v1.repository.checkout")
    required_string(
        repository["baseBranch"], "canonical campaign manifest v1.repository.baseBranch"
    )
    required_string(repository["remote"], "canonical campaign manifest v1.repository.remote")
    required_string(repository["forge"], "canonical campaign manifest v1.repository.forge")
    max_tasks = positive_integer(manifest["maxTasks"], "canonical campaign manifest v1.maxTasks")
    max_parallel = positive_integer(
        manifest["maxParallel"], "canonical campaign manifest v1.maxParallel"
    )
    positive_integer(
        manifest["driverRuntimeMaxSec"], "canonical campaign manifest v1.driverRuntimeMaxSec"
    )
    canonical_nullable_positive_integer(
        manifest["runtimeMaxSec"], "canonical campaign manifest v1.runtimeMaxSec"
    )
    required_string(manifest["pool"], "canonical campaign manifest v1.pool", 80)
    required_string(manifest["mergeMethod"], "canonical campaign manifest v1.mergeMethod")
    canonical_agent(manifest["agent"], "canonical campaign manifest v1.agent")
    steward_role(manifest["steward"], "canonical campaign manifest v1.steward")
    canonical_gate_list(manifest["gates"], "canonical campaign manifest v1.gates")
    references = canonical_task_references(
        manifest["tasks"],
        "canonical campaign manifest v1.tasks",
        require_conflict_domains=max_parallel > 1,
    )
    if len(references) > max_tasks:
        fail("canonical campaign manifest v1.tasks exceeds maxTasks")
    return manifest


def canonical_campaign_graph(value: Any) -> dict[str, Any]:
    """Decode and verify the pure graph corpus emitted by Rust."""
    graph = object_complete(
        value,
        {"manifest", "tasks", "executableDigest"},
        "canonical campaign graph v1",
    )
    manifest = canonical_manifest(graph["manifest"])
    tasks = graph["tasks"]
    digest = graph["executableDigest"]
    if not isinstance(tasks, list) or not 1 <= len(tasks) <= 100:
        fail("canonical campaign graph v1.tasks must contain 1..=100 entries")
    if not isinstance(digest, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        fail("canonical campaign graph v1.executableDigest must be a lowercase SHA-256 identity")
    canonical_tasks: list[dict[str, Any]] = []
    for index, candidate in enumerate(tasks):
        task = object_complete(
            candidate,
            {"number", "title", "body"},
            f"canonical campaign graph v1.tasks[{index}]",
        )
        number = task.get("number")
        if not isinstance(number, int) or isinstance(number, bool) or number < 1:
            fail(f"canonical campaign graph v1.tasks[{index}].number must be positive")
        title = required_string(
            task.get("title"), f"canonical campaign graph v1.tasks[{index}].title", 300
        )
        body = required_body(
            task.get("body"), f"canonical campaign graph v1.tasks[{index}].body", 64_000
        )
        canonical_tasks.append({"number": number, "title": title, "body": body})
    calculated = canonical_sha256({"manifest": manifest, "tasks": canonical_tasks})
    if calculated != digest:
        fail(
            "internal campaign contract violation: canonical graph digest "
            f"{digest} does not match its carried manifest/tasks {calculated}"
        )
    return {
        "manifest": manifest,
        "tasks": canonical_tasks,
        "executableDigest": digest,
    }


def file_task_completion_revision(
    repository: str,
    source: dict[str, str],
    task: dict[str, Any],
) -> str:
    """Identity for one file-worklist task proof.

    The worklist digest and Git revision cover the complete file. Completion is
    narrower: an unrelated task or base-branch edit must not invalidate this
    task's proof, while its normalized content and source coordinate must.
    """
    return canonical_sha256(
        {
            "contractVersion": 1,
            "repository": repository,
            "source": {
                "repository": source.get("repository", repository),
                "path": source["path"],
            },
            "task": task,
        }
    )


def campaign_issue(value: Any) -> dict[str, str]:
    issue = object_exact(value, {"number", "url"}, "issue")
    number = required_string(issue.get("number"), "issue.number")
    if not number.isdigit() or number.startswith("0"):
        fail("issue.number must be a positive decimal string")
    url = required_string(issue.get("url"), "issue.url")
    return {"number": number, "url": url}


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


def normalized_worker_findings(value: Any) -> dict[str, str] | None:
    """Validate one projected final message; blank capture means silence."""
    if value is None:
        return None
    findings = object_complete(
        value,
        {"taskUuid", "message"},
        "workerFindings",
    )
    task_uuid = required_string(findings.get("taskUuid"), "workerFindings.taskUuid", 64)
    try:
        parsed_uuid = uuid.UUID(task_uuid)
    except ValueError:
        fail("workerFindings.taskUuid must be a UUID")
    if str(parsed_uuid) != task_uuid.lower():
        fail("workerFindings.taskUuid must use canonical UUID spelling")
    message = findings.get("message")
    if not isinstance(message, str):
        fail("workerFindings.message must be text")
    message = message.replace("\r\n", "\n").replace("\r", "\n")
    message = "".join(
        character
        if character in "\n\t" or not (ord(character) < 32 or 127 <= ord(character) < 160)
        else "�"
        for character in message
    ).strip()
    if not message:
        return None
    return {"taskUuid": task_uuid.lower(), "message": message}


def bound_worker_findings(prefix: str, text: str) -> str:
    """Bound the complete UTF-8 receipt without splitting a code point."""
    prefix_bytes = prefix.encode("utf-8")
    if len(prefix_bytes) >= MAX_WORKER_FINDINGS_BYTES:
        fail("worker findings marker exceeds its receipt bound")
    available = MAX_WORKER_FINDINGS_BYTES - len(prefix_bytes)
    encoded = text.encode("utf-8")
    if len(encoded) <= available:
        return prefix + text
    truncation = WORKER_FINDINGS_TRUNCATION.encode("utf-8")
    width = max(0, available - len(truncation))
    clipped = encoded[:width].decode("utf-8", errors="ignore").rstrip()
    body = prefix + clipped + WORKER_FINDINGS_TRUNCATION
    if len(body.encode("utf-8")) > MAX_WORKER_FINDINGS_BYTES:
        fail("worker findings escaped its receipt byte bound")
    return body


def read_capture_tail(path: Path) -> tuple[str, bool]:
    """Read one private stream tail with the checkpoint's 8 KiB bound."""
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
        )
    except OSError as error:
        fail(f"cannot open checkpoint capture stream {path}: {error}")
    with os.fdopen(descriptor, "rb") as stream:
        metadata = os.fstat(stream.fileno())
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            fail(f"checkpoint capture stream is not a private regular file: {path}")
        start = max(0, metadata.st_size - CHECKPOINT_CAPTURE_MAX_BYTES)
        stream.seek(start)
        captured = stream.read(CHECKPOINT_CAPTURE_MAX_BYTES)
    if start:
        # A tail can begin inside a UTF-8 scalar. Drop only continuation bytes;
        # malformed bytes elsewhere remain visible as replacement characters.
        prefix = 0
        while prefix < len(captured) and captured[prefix] & 0b1100_0000 == 0b1000_0000:
            prefix += 1
        captured = captured[prefix:]
    text = captured.decode("utf-8", errors="replace").replace("\0", "�")
    encoded = text.encode("utf-8")
    additionally_truncated = False
    if len(encoded) > CHECKPOINT_CAPTURE_MAX_BYTES:
        encoded = encoded[-CHECKPOINT_CAPTURE_MAX_BYTES:]
        prefix = 0
        while (
            prefix < len(encoded)
            and encoded[prefix] & 0b1100_0000 == 0b1000_0000
        ):
            prefix += 1
        text = encoded[prefix:].decode("utf-8")
        additionally_truncated = True
    return text, bool(start) or additionally_truncated


def write_private_json(path: Path, value: dict[str, Any]) -> None:
    parent = path.parent
    parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    if parent.is_symlink() or not parent.is_dir():
        fail(f"checkpoint capture parent must be a real directory: {parent}")
    os.chmod(parent, 0o700)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp")
    document = json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    try:
        descriptor = os.open(
            temporary,
            os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_CLOEXEC | os.O_NOFOLLOW,
            0o600,
        )
        with os.fdopen(descriptor, "wb") as output:
            output.write(document)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        os.chmod(path, 0o600)
        directory = os.open(parent, os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    except OSError as error:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass
        fail(f"cannot persist checkpoint capture {path}: {error}")


def persist_checkpoint_capture(
    capture_root_value: Any,
    execution_value: Any,
    campaign: str,
    issue_number: str,
    task_id: str,
) -> dict[str, Any]:
    capture_root = Path(required_string(capture_root_value, "captureRoot", 4096))
    if (
        not capture_root.is_absolute()
        or capture_root.name != "archive"
        or capture_root.parent.name != "capture"
    ):
        fail("captureRoot must name an absolute capture/archive directory")
    if capture_root.exists() and (capture_root.is_symlink() or not capture_root.is_dir()):
        fail("captureRoot must be a real directory")
    execution = object_exact(
        execution_value,
        {"taskUuid", "verdict", "exitCode"},
        "checkpoint execution",
    )
    task_uuid = required_string(execution.get("taskUuid"), "checkpoint execution.taskUuid")
    try:
        parsed_uuid = uuid.UUID(task_uuid)
    except ValueError:
        fail("checkpoint execution.taskUuid must be a UUID")
    if str(parsed_uuid) != task_uuid:
        fail("checkpoint execution.taskUuid must be a canonical UUID")
    verdict = required_string(execution.get("verdict"), "checkpoint execution.verdict")
    if verdict not in {
        "pass",
        "substituted",
        "failed",
        "skipped",
        "cancelled",
        "pool-vanished",
        "preempted",
        "runtime-exceeded",
        "clean-exit-no-artifact",
    }:
        fail("checkpoint execution.verdict is not a terminal verdict")
    exit_code = execution.get("exitCode")
    if exit_code is not None and (not isinstance(exit_code, int) or isinstance(exit_code, bool)):
        fail("checkpoint execution.exitCode must be an integer or null")

    capture_stem = f"{task_uuid}.{task_id}"
    current_root = capture_root.parent
    stdout, stdout_truncated = read_capture_tail(current_root / f"{capture_stem}.out")
    stderr, stderr_truncated = read_capture_tail(
        current_root / f"{capture_stem}.adapter.err"
    )
    path = capture_root / capture_stem / CHECKPOINT_CAPTURE_FILE
    write_private_json(
        path,
        {
            "schemaVersion": CHECKPOINT_CAPTURE_SCHEMA_VERSION,
            "campaign": campaign,
            "issueNumber": issue_number,
            "taskId": task_id,
            "taskUuid": task_uuid,
            "verdict": verdict,
            "exitCode": exit_code,
            "stdout": stdout,
            "stdoutTruncated": stdout_truncated,
            "stderr": stderr,
            "stderrTruncated": stderr_truncated,
        },
    )
    return {
        "path": str(path),
        "stdoutTruncated": stdout_truncated,
        "stderrTruncated": stderr_truncated,
    }


def read_checkpoint_capture(path: Path, campaign: str, task_id: str) -> dict[str, Any]:
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
        )
    except OSError as error:
        fail(f"cannot open checkpoint capture {path}: {error}")
    with os.fdopen(descriptor, "rb") as capture:
        metadata = os.fstat(capture.fileno())
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
            # JSON escaping can expand an 8 KiB stream of control bytes sixfold.
            # Both decoded streams remain independently bounded below; this
            # outer cap only keeps the containing receipt finite.
            or metadata.st_size > 128 * 1024
        ):
            fail(f"checkpoint capture is not a bounded private regular file: {path}")
        try:
            value = json.load(capture)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            fail(f"checkpoint capture is not valid JSON: {path}: {error}")
    fields = {
        "schemaVersion",
        "campaign",
        "issueNumber",
        "taskId",
        "taskUuid",
        "verdict",
        "exitCode",
        "stdout",
        "stdoutTruncated",
        "stderr",
        "stderrTruncated",
    }
    capture = object_complete(value, fields, "checkpoint capture")
    if (
        capture.get("schemaVersion") != CHECKPOINT_CAPTURE_SCHEMA_VERSION
        or capture.get("campaign") != campaign
        or capture.get("taskId") != task_id
    ):
        fail("checkpoint capture identity does not match the machine receipt")
    for stream in ("stdout", "stderr"):
        content = capture.get(stream)
        if (
            not isinstance(content, str)
            or len(content.encode("utf-8")) > CHECKPOINT_CAPTURE_MAX_BYTES
        ):
            fail(f"checkpoint capture {stream} exceeds its 8 KiB bound")
    required_bool(capture.get("stdoutTruncated"), "checkpoint capture stdoutTruncated")
    required_bool(capture.get("stderrTruncated"), "checkpoint capture stderrTruncated")
    return capture


def checkpoint_capture_note(
    value: Any,
    campaign: str,
    task_id: str,
) -> str:
    publication = object_exact(
        value,
        {"path", "postFailureEvidence", "postFailureStderr"},
        "checkpointCapture",
    )
    path_text = required_string(publication.get("path"), "checkpointCapture.path", 700)
    path = Path(path_text)
    if not path.is_absolute():
        fail("checkpointCapture.path must be absolute")
    post_evidence = required_bool(
        publication.get("postFailureEvidence"),
        "checkpointCapture.postFailureEvidence",
    )
    post_stderr = required_bool(
        publication.get("postFailureStderr"),
        "checkpointCapture.postFailureStderr",
    )
    if post_stderr and not post_evidence:
        fail("checkpointCapture.postFailureStderr requires postFailureEvidence")
    note = f"Checkpoint capture: {path_text}"
    if not (post_evidence and post_stderr):
        return note
    capture = read_checkpoint_capture(path, campaign, task_id)
    lines = capture["stderr"].splitlines()[-CHECKPOINT_STDERR_LINES:]
    if not lines:
        return note
    excerpt = "\n".join(f"    {line}" for line in lines)
    heading = (
        f"{note}\n\nCheckpoint stderr (last {len(lines)} line(s), before public redaction):"
        "\n\n"
    )
    available = max(0, CHECKPOINT_PUBLIC_NOTE_MAX_CHARS - len(heading))
    if len(excerpt) > available:
        marker = "    [... earlier checkpoint stderr lines shortened ...]\n"
        tail_width = max(0, available - len(marker))
        excerpt = marker + excerpt[-tail_width:] if tail_width else marker[:available]
    return heading + excerpt


def checkpoint_capture_paths(values: list[str]) -> list[str]:
    prefix = "Checkpoint capture: "
    return list(
        dict.fromkeys(
            line[len(prefix) :]
            for value in values
            for line in value.splitlines()
            if line.startswith(prefix) and line[len(prefix) :]
        )
    )


def append_checkpoint_capture_note(value: str, note: str, maximum: int) -> str:
    if not note:
        return value
    suffix = f"\n\n{note}"
    if len(value) + len(suffix) <= maximum:
        return value + suffix
    marker = "\n[... earlier machine detail shortened for checkpoint capture ...]"
    available = max(0, maximum - len(marker) - len(suffix))
    return value[:available].rstrip() + marker + suffix


def bound_public_diagnosis(value: str) -> str:
    if len(value) <= MAX_DIAGNOSIS_CHARS:
        return value
    width = MAX_DIAGNOSIS_CHARS - len(PUBLIC_DIAGNOSIS_TRUNCATION)
    return value[:width].rstrip() + PUBLIC_DIAGNOSIS_TRUNCATION


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
            fail("the campaign remote returned a malformed state ref")
        refs[fields[1]] = fields[0]
    return refs


def read_local_blob(config: dict[str, Any], ref: str) -> dict[str, Any]:
    checkout: Path = config["checkout"]
    git(checkout, "fetch", "--quiet", config["remote"], ref)
    content = git(checkout, "cat-file", "blob", "FETCH_HEAD").stdout
    try:
        value = json.loads(content)
    except json.JSONDecodeError as error:
        fail(f"local campaign state {ref!r} is invalid JSON: {error}")
    if not isinstance(value, dict):
        fail(f"local campaign state {ref!r} must contain an object")
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
        "local campaign state object",
    )
    git(checkout, "push", "--quiet", config["remote"], f"{object_id}:{ref}")
    return True, value


def attempt_receipts_path(value: Any, campaign: str) -> Path:
    """Validate the flow-carried coordinate of one campaign's attempt log."""
    source = object_complete(
        value, {"schemaVersion", "kind", "path"}, "attemptReceipts"
    )
    if (
        source.get("schemaVersion") != ATTEMPT_RECEIPTS_SCHEMA_VERSION
        or source.get("kind") != "local-jsonl"
    ):
        fail("attemptReceipts must use local-jsonl schema version 1")
    path = Path(required_string(source.get("path"), "attemptReceipts.path", 4096))
    if not path.is_absolute():
        fail("attemptReceipts.path must be absolute")
    if (
        path.name != ATTEMPT_RECEIPTS_FILE
        or path.parent.name != campaign
        or path.parent.parent.name != "attempt-receipts"
    ):
        fail("attemptReceipts.path does not name this campaign's attempt-receipts log")
    return path


def attempt_receipt_url(campaign: str, sequence: int) -> str:
    return f"local://campaign/{campaign}/attempt-receipts/{sequence}"


def _attempt_receipts_parent(path: Path) -> None:
    try:
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        if path.parent.is_symlink() or not path.parent.is_dir():
            fail("attempt-receipts parent must be a real directory")
        os.chmod(path.parent, 0o700)
    except OSError as error:
        fail(f"cannot prepare attempt-receipts directory {path.parent}: {error}")


def _validate_attempt_receipt(
    candidate: Any,
    path: Path,
    expected_sequence: int,
    campaign: str,
    issue_number: str,
) -> dict[str, Any]:
    context = f"attempt receipt {expected_sequence} in {path}"
    if not isinstance(candidate, dict):
        fail(f"{context} must be an object")
    kind = candidate.get("kind")
    common = {"schemaVersion", "sequence", "kind", "campaign", "issueNumber"}
    fields = {
        "diagnosis": common | {"taskId", "attempt", "diagnosis", "redaction"},
        "retry": common | {"taskId", "attempt", "reason", "redaction"},
        "escalation": common | {"body"},
        # `reason`/`actor`/`nonce` are audit metadata for the later local
        # campaign-resume writer. The fold depends only on the ordered scope.
        "pardon": common | {"tasks", "reason", "actor", "nonce"},
    }.get(kind)
    if fields is None:
        fail(f"{context} has unknown kind {kind!r}")
    record = dict(object_exact(candidate, fields, context))
    sequence = record.get("sequence")
    if (
        record.get("schemaVersion") != ATTEMPT_RECEIPTS_SCHEMA_VERSION
        or not isinstance(sequence, int)
        or isinstance(sequence, bool)
        or sequence != expected_sequence
        or record.get("campaign") != campaign
        or record.get("issueNumber") != issue_number
    ):
        fail(f"{context} has invalid identity or sequence")
    if kind in {"diagnosis", "retry"}:
        task_id = required_string(record.get("taskId"), f"{context}.taskId")
        if not TASK_ID.fullmatch(task_id):
            fail(f"{context}.taskId is unsafe")
        if record.get("attempt") not in {1, 2}:
            fail(f"{context}.attempt must equal 1 or 2")
        if record.get("redaction") not in PUBLIC_REDACTIONS:
            fail(f"{context}.redaction is unsupported")
        payload = "diagnosis" if kind == "diagnosis" else "reason"
        record[payload] = required_text(
            record.get(payload),
            f"{context}.{payload}",
            MAX_DIAGNOSIS_CHARS if kind == "diagnosis" else MAX_RETRY_CHARS,
        )
    elif kind == "escalation":
        record["body"] = required_text(record.get("body"), f"{context}.body", 60_000)
    else:
        if "tasks" not in record:
            fail(f"{context}.tasks is required")
        tasks = record.get("tasks")
        if tasks is not None:
            tasks = string_list(tasks, f"{context}.tasks", nonempty=True)
            if len(tasks) != len(set(tasks)) or any(
                not TASK_ID.fullmatch(task_id) for task_id in tasks
            ):
                fail(f"{context}.tasks must contain unique safe task IDs")
            record["tasks"] = tasks
        if "reason" in record:
            record["reason"] = required_text(record["reason"], f"{context}.reason", 4_000)
        if "actor" in record:
            required_string(record["actor"], f"{context}.actor", 128)
        if "nonce" in record:
            nonce = required_string(record["nonce"], f"{context}.nonce", 36)
            try:
                uuid.UUID(nonce)
            except ValueError:
                fail(f"{context}.nonce must be a UUID")
    return record


def _read_attempt_receipts_descriptor(
    descriptor: int,
    path: Path,
    campaign: str,
    issue_number: str,
    *,
    repair_tail: bool,
) -> list[dict[str, Any]]:
    metadata = os.fstat(descriptor)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_nlink != 1
        or metadata.st_size > MAX_ATTEMPT_RECEIPTS_LOG_BYTES
    ):
        fail(f"attempt-receipts log is not a bounded private regular file: {path}")
    os.lseek(descriptor, 0, os.SEEK_SET)
    chunks: list[bytes] = []
    remaining = metadata.st_size
    while remaining:
        chunk = os.read(descriptor, min(1024 * 1024, remaining))
        if not chunk:
            break
        chunks.append(chunk)
        remaining -= len(chunk)
    raw = b"".join(chunks)
    complete = raw.rfind(b"\n") + 1
    if complete != len(raw):
        # Match the witness ledger: an interrupted append contributes no fact.
        # Readers fold the last complete prefix; the next exclusive writer
        # truncates and fsyncs that same prefix before appending.
        if repair_tail:
            os.ftruncate(descriptor, complete)
            os.fsync(descriptor)
        raw = raw[:complete]
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        fail(f"attempt-receipts log {path} is not UTF-8: {error}")
    records: list[dict[str, Any]] = []
    for sequence, line in enumerate(text.splitlines(), 1):
        if not line:
            fail(f"attempt-receipts log {path} contains a blank record")
        try:
            candidate = json.loads(line)
        except json.JSONDecodeError as error:
            fail(f"attempt receipt {sequence} in {path} is invalid JSON: {error}")
        records.append(
            _validate_attempt_receipt(
                candidate, path, sequence, campaign, issue_number
            )
        )
    return records


def read_attempt_receipts(
    source: Any, campaign: str, issue_number: str
) -> list[dict[str, Any]]:
    path = attempt_receipts_path(source, campaign)
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    except FileNotFoundError:
        return []
    except OSError as error:
        fail(f"cannot open attempt-receipts log {path}: {error}")
    try:
        fcntl.flock(descriptor, fcntl.LOCK_SH)
        return _read_attempt_receipts_descriptor(
            descriptor, path, campaign, issue_number, repair_tail=False
        )
    except OSError as error:
        fail(f"cannot read attempt-receipts log {path}: {error}")
    finally:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_UN)
        finally:
            os.close(descriptor)


def _open_attempt_receipts_writer(path: Path) -> tuple[int, bool]:
    _attempt_receipts_parent(path)
    flags = os.O_RDWR | os.O_APPEND | os.O_CLOEXEC | os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags | os.O_CREAT | os.O_EXCL, 0o600)
        created = True
    except FileExistsError:
        try:
            descriptor = os.open(path, flags)
        except OSError as error:
            fail(f"cannot open attempt-receipts log {path}: {error}")
        created = False
    except OSError as error:
        fail(f"cannot create attempt-receipts log {path}: {error}")
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            fail(f"attempt-receipts log is not a private regular file: {path}")
        os.fchmod(descriptor, 0o600)
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        if created:
            directory = os.open(
                path.parent, os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY
            )
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        return descriptor, created
    except BaseException:
        os.close(descriptor)
        raise


def _events_from_local_records(
    records: list[dict[str, Any]], campaign: str
) -> list[dict[str, Any]]:
    return [
        {**record, "comment": attempt_receipt_url(campaign, record["sequence"])}
        for record in records
    ]


def fold_attempt_receipts(
    events: list[dict[str, Any]],
    task_ids: set[str] | None,
    warnings: list[str] | None = None,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], str | None, list[str]]:
    """Fold ordered counters, contiguity, pardons, and escalation state."""
    warnings = [] if warnings is None else warnings
    visible: dict[str, dict[str, list[dict[str, Any]]]] = {
        "diagnosis": {},
        "retry": {},
    }
    # Pardon counts and escalation causality use every accepted receipt, just
    # as the former boundary arithmetic did before contiguity was projected.
    counters: dict[str, dict[str, list[dict[str, Any]]]] = {
        "diagnosis": {},
        "retry": {},
    }
    escalations: list[dict[str, Any]] = []

    def accepted(kind: str, task_id: str) -> bool:
        if task_ids is None or task_id in task_ids:
            return True
        warnings.append(
            f"dropped machine {kind} for {task_id!r}: the worklist no longer names that task"
        )
        return False

    for event in events:
        kind = event["kind"]
        if kind in {"diagnosis", "retry"}:
            task_id = event["taskId"]
            if not accepted(kind, task_id):
                continue
            counters[kind].setdefault(task_id, []).append(event)
            kept = visible[kind].setdefault(task_id, [])
            expected = len(kept) + 1
            if event["attempt"] != expected:
                warnings.append(
                    f"dropped machine {kind} for {task_id!r} attempt "
                    f"{event['attempt']}: no attempt {expected} receipt precedes it"
                )
                continue
            payload = "diagnosis" if kind == "diagnosis" else "reason"
            kept.append(
                {
                    "taskId": task_id,
                    "attempt": event["attempt"],
                    "comment": event["comment"],
                    payload: event[payload],
                }
            )
            continue
        if kind == "escalation":
            contributors = {
                task_id
                for task_id, receipts in counters["diagnosis"].items()
                if {receipt["attempt"] for receipt in receipts} == {1, 2}
            }
            escalations.append(
                {
                    "comment": event["comment"],
                    "contributors": contributors,
                    "covered": set(),
                }
            )
            continue

        tasks = event["tasks"]
        pardoned = 0
        if tasks is None:
            pardoned += sum(
                len(receipts)
                for by_task in counters.values()
                for receipts in by_task.values()
            )
            for by_task in counters.values():
                by_task.clear()
            for by_task in visible.values():
                by_task.clear()
            pardoned += len(escalations)
            escalations.clear()
        else:
            scope = set(tasks)
            for kind_name in ("diagnosis", "retry"):
                for task_id in scope:
                    pardoned += len(counters[kind_name].pop(task_id, []))
                    visible[kind_name].pop(task_id, None)
            remaining_escalations: list[dict[str, Any]] = []
            for escalation in escalations:
                escalation["covered"].update(scope & escalation["contributors"])
                if escalation["contributors"] and escalation["contributors"].issubset(
                    escalation["covered"]
                ):
                    pardoned += 1
                else:
                    remaining_escalations.append(escalation)
            escalations = remaining_escalations
        if pardoned:
            scope_text = ""
            if tasks is not None:
                scope_text = " for task(s) " + ", ".join(
                    repr(task_id) for task_id in sorted(tasks)
                )
            boundary = event.get("boundaryLabel", "pardon")
            warnings.append(
                f"campaign {boundary} {event['comment']} pardoned "
                f"{pardoned} earlier machine receipt(s){scope_text}"
            )

    if len(escalations) > 1:
        fail("multiple machine escalations claim this campaign")
    diagnoses = [
        receipt
        for receipts in visible["diagnosis"].values()
        for receipt in receipts
    ]
    retries = [
        receipt for receipts in visible["retry"].values() for receipt in receipts
    ]
    order_source = task_ids or {record["taskId"] for record in diagnoses + retries}
    task_order = {task_id: index for index, task_id in enumerate(sorted(order_source))}
    for records in (diagnoses, retries):
        records.sort(
            key=lambda item: (
                task_order.get(item["taskId"], len(task_order)),
                item["attempt"],
            )
        )
    escalation = escalations[0]["comment"] if escalations else None
    return diagnoses, retries, escalation, warnings


def append_attempt_receipt(
    source: Any,
    campaign: str,
    issue_number: str,
    payload: dict[str, Any],
) -> tuple[bool, dict[str, Any]]:
    """Append one counter under flock, fsync it, and return its ordered fact."""
    path = attempt_receipts_path(source, campaign)
    descriptor, _ = _open_attempt_receipts_writer(path)
    try:
        records = _read_attempt_receipts_descriptor(
            descriptor, path, campaign, issue_number, repair_tail=True
        )
        events = _events_from_local_records(records, campaign)
        diagnoses, retries, escalation, _ = fold_attempt_receipts(events, None)
        kind = payload.get("kind")
        if kind in {"diagnosis", "retry"}:
            active = diagnoses if kind == "diagnosis" else retries
            matching = [
                receipt
                for receipt in active
                if receipt["taskId"] == payload.get("taskId")
                and receipt["attempt"] == payload.get("attempt")
            ]
            if matching:
                sequence = int(matching[0]["comment"].rsplit("/", 1)[1])
                return False, events[sequence - 1]
            spent = len(
                [receipt for receipt in active if receipt["taskId"] == payload.get("taskId")]
            )
            if payload.get("attempt") != spent + 1:
                fail(
                    f"task {payload.get('taskId')!r} {kind} attempt "
                    f"{payload.get('attempt')} is not next after {spent} log receipts"
                )
        elif kind == "escalation" and escalation is not None:
            sequence = int(escalation.rsplit("/", 1)[1])
            return False, events[sequence - 1]
        elif kind == "pardon" and payload.get("nonce") is not None:
            prior = next(
                (
                    event
                    for event in events
                    if event["kind"] == "pardon"
                    and event.get("nonce") == payload.get("nonce")
                ),
                None,
            )
            if prior is not None:
                return False, prior
        sequence = len(records) + 1
        record = _validate_attempt_receipt(
            {
                "schemaVersion": ATTEMPT_RECEIPTS_SCHEMA_VERSION,
                "sequence": sequence,
                "campaign": campaign,
                "issueNumber": issue_number,
                **payload,
            },
            path,
            sequence,
            campaign,
            issue_number,
        )
        line = json.dumps(record, sort_keys=True, separators=(",", ":")).encode() + b"\n"
        if os.fstat(descriptor).st_size + len(line) > MAX_ATTEMPT_RECEIPTS_LOG_BYTES:
            fail("attempt-receipts log exceeds 128 MiB")
        view = memoryview(line)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                fail(f"cannot append attempt-receipts log {path}: short write")
            view = view[written:]
        os.fsync(descriptor)
        return True, {**record, "comment": attempt_receipt_url(campaign, sequence)}
    except OSError as error:
        fail(f"cannot append attempt-receipts log {path}: {error}")
    finally:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_UN)
        finally:
            os.close(descriptor)


def campaign_attempt_state(
    source: Any,
    campaign: str,
    issue_number: str,
    task_ids: set[str] | None = None,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], str | None, list[str]]:
    """Fold the campaign's local append-only attempt receipts."""
    records = read_attempt_receipts(source, campaign, issue_number)
    return fold_attempt_receipts(
        _events_from_local_records(records, campaign), task_ids, []
    )

def task_revision(task: dict[str, Any]) -> str | None:
    value = task.get("revision")
    if value is None:
        return None
    value = required_string(value, f"task {task.get('id')} revision")
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", value):
        fail("task revision must be a lowercase SHA-256 identity")
    return value


def pull_request_marker(
    campaign: str, issue_number: str, task_id: str, revision: str
) -> str:
    if not isinstance(revision, str) or not re.fullmatch(
        r"sha256:[0-9a-f]{64}", revision
    ):
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
    """Where a checkpoint receipt is published.

    Receipts live in the same hidden namespace as the campaign's other durable
    state. Hidden refs are served on request and cloned by nobody.
    """
    identity = checkpoint_identity(campaign, task_id, source_sha256, base_rev)
    return f"{local_state_prefix(campaign, issue_number)}/checkpoint/{identity}"


def merge_receipt_ref(
    campaign: str, campaign_id: str, task_id: str, revision: str | None = None
) -> str:
    """Where a local squash records the layer commit it produced.

    The ref is an audit index, never completion authority. The local oracle
    independently reads exact task markers from the campaign's witnessed
    integration history, so a missing or damaged index cannot change truth.
    """
    suffix = "" if revision is None else "-" + revision.removeprefix("sha256:")[:16]
    return f"{local_state_prefix(campaign, campaign_id)}/merge/{task_id}{suffix}"


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


def campaign_branch_prefix(campaign: str, campaign_id: str) -> str:
    """The private branch namespace for one armed campaign generation."""
    campaign_slug = safe_slug(campaign, 32)
    identity_slug = safe_slug(campaign_id, 64)
    return f"tally/{campaign_slug}-campaign-{identity_slug}"


def integration_branch(campaign: str, campaign_id: str) -> str:
    """The never-rewritten local branch witnessed lane merges advance."""
    return f"{campaign_branch_prefix(campaign, campaign_id)}/integration"


def stable_publish_branch(
    campaign: str, campaign_id: str, task_id: str, revision: str | None = None
) -> str:
    suffix = "" if revision is None else "-" + revision.removeprefix("sha256:")[:16]
    return f"{campaign_branch_prefix(campaign, campaign_id)}/{task_id}{suffix}"


def local_refs(checkout: Path, prefix: str) -> dict[str, str]:
    """List refs below ``prefix`` without consulting a configured remote."""
    viewed = git(
        checkout,
        "for-each-ref",
        "--format=%(objectname)%09%(refname)",
        prefix,
    )
    refs: dict[str, str] = {}
    for line in viewed.stdout.splitlines():
        fields = line.split("\t", 1)
        if (
            len(fields) != 2
            or not GIT_OID.fullmatch(fields[0])
            or not fields[1].startswith("refs/")
        ):
            fail("local campaign branch listing returned malformed output")
        refs[fields[1]] = fields[0]
    return refs


def local_branch_oid(checkout: Path, branch: str) -> str | None:
    return local_refs(checkout, f"refs/heads/{branch}").get(f"refs/heads/{branch}")


def merged_local_tasks(
    repository: str,
    config: dict[str, Any],
    campaign: str,
    campaign_id: str,
    issue_number: str,
    base_rev: str | None,
    tasks: list[dict[str, Any]],
) -> list[dict[str, str]]:
    """Read completion solely from marked integration-branch commits."""
    checkout: Path = config["checkout"]
    branch_tip = local_branch_oid(checkout, integration_branch(campaign, campaign_id))
    if branch_tip is None:
        return []
    if base_rev is None:
        base_rev = branch_tip
    else:
        base_rev = full_git_oid(base_rev, "local integration revision")
        if git(
            checkout,
            "merge-base",
            "--is-ancestor",
            base_rev,
            branch_tip,
            check=False,
        ).returncode:
            fail("witnessed local integration revision is not on the integration branch")
    facts: list[dict[str, str]] = []
    implementations = [task for task in tasks if task["kind"] == "implementation"]
    for task in implementations:
        revision = task_revision(task)
        if revision is None:
            fail(f"local completion task {task['id']!r} carries no revision marker")
        marker = pull_request_marker(campaign, issue_number, task["id"], revision)
        branch = stable_publish_branch(campaign, campaign_id, task["id"], revision)
        matches = git(
            checkout,
            "log",
            "--first-parent",
            "--format=%H",
            "--fixed-strings",
            f"--grep={marker}",
            base_rev,
        ).stdout.split()
        if len(matches) > 1:
            fail(
                f"multiple local integration commits claim campaign task "
                f"{task['id']!r} revision {revision}"
            )
        if not matches:
            continue
        merge_commit = matches[0]
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
    base_rev: str | None = None,
) -> list[dict[str, str]]:
    checkpoints = [task for task in tasks if task["kind"] == "checkpoint"]
    if not checkpoints:
        return []
    checkout: Path = config["checkout"]
    remote = config["remote"]
    git(checkout, "fetch", "--prune", "--no-tags", remote)
    # A checkpoint receipt names a revision of the *code* repository. Current
    # reconciliation passes that anchor explicitly because it may be a local
    # integration tip or belong to a repository other than the worklist.
    # Compatibility callers may omit it when the worklist revision is the code
    # anchor too.
    base_rev = required_string(
        source.get("revision") if base_rev is None else base_rev,
        "campaign base revision",
    )
    if not re.fullmatch(r"[0-9a-f]{40,64}", base_rev):
        fail("campaign base revision must be a full Git object ID")
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
    task_domains = task.get("conflictDomains")
    if not isinstance(task_domains, list):
        task_domains = []
    return any(
        domains_overlap(left, right)
        for other in selected
        for left in task_domains
        for right in (
            other["conflictDomains"]
            if isinstance(other.get("conflictDomains"), list)
            else []
        )
    )


def closing_summary_marker(
    campaign: str, issue_number: str, outcome: str, source_sha256: str
) -> str:
    """The idempotence marker one terminal outcome's summary carries.

    Completion keeps the pre-summary `campaign-complete` marker so existing
    local receipts and the release renderer share one idempotence key.
    """
    if outcome == "complete":
        return f"<!-- tally:campaign-complete:v1 source={source_sha256} -->"
    return (
        "<!-- tally:campaign-summary:v1 "
        f"campaign={campaign} issue={issue_number} outcome={outcome} -->"
    )


def campaign_digest(reconciliation: dict[str, Any], outcome: str) -> dict[str, Any]:
    """One run-scoped digest of a campaign's terminal state.

    Every field is a projection of facts this pass already witnessed -- local
    merges, checkpoint refs, append-only attempt receipts, and the reconciler's
    own set arithmetic. Nothing here reads or writes a second coordination
    surface.
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
        # The code anchor every merge and checkpoint row below is a fact about.
        # Equal to the worklist revision unless the campaign is split, in which
        # case the two name commits in two different histories.
        "baseRevision": reconciliation["baseRevision"],
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
    # A split campaign's worklist revision resolves only in the spec
    # repository, while every merge and checkpoint row below names code
    # repository artifacts. Saying which repository each revision belongs to is
    # the difference between evidence and a revision that looks like a lie to
    # anyone who tries to check it out. An unsplit campaign has one history and
    # keeps the one-line form it always had.
    worklist_source = digest["source"]
    worklist_repository = worklist_source.get("repository")
    # #385: the closing summary is driver-rendered, not steward-proposed, but
    # the outcome-first content contract governs it the same as PR prose and
    # steering notes -- the outcome leads, in a past-tense sentence, before
    # the provenance detail. `validate_outcome_first` below is a self-check:
    # a future edit that drifts from the contract fails this node loudly
    # instead of publishing a summary nothing enforced.
    outcome_sentence = (
        f"Settled {settled} of {digest['taskCount']} task(s) against durable "
        "merge/checkpoint facts."
    )
    provenance_sentence = (
        f"Worklist `{worklist_source['sha256']}` at "
        f"`{worklist_source['revision']}`."
        if worklist_repository is None
        else (
            f"Worklist `{worklist_source['sha256']}` at "
            f"`{worklist_source['revision']}` in `{worklist_repository}`; "
            f"code base `{digest['baseRevision']}` in "
            f"`{digest['repository']}`."
        )
    )
    outcome_reason = validate_outcome_first(
        f"{outcome_sentence}\n{provenance_sentence}",
        max_chars=2_000,
        context="closing summary intro",
    )
    if outcome_reason:
        fail(f"closing summary intro violates the outcome-first grammar: {outcome_reason}")
    lines = [
        heading,
        "",
        outcome_sentence,
        provenance_sentence,
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
    """Record one idempotent closing summary in local campaign state."""
    outcome = digest["outcome"]
    marker = closing_summary_marker(
        campaign, issue_number, outcome, digest["source"]["sha256"]
    )
    body = f"{marker}\n\n{render_campaign_summary(digest)}"
    if len(body) > 60_000:
        fail("campaign closing summary exceeds 60,000 characters")
    ref = f"{local_state_prefix(campaign, issue_number)}/summary/{outcome}"
    expected = {
        "schemaVersion": 1,
        "kind": "closing-summary",
        "campaign": campaign,
        "issueNumber": issue_number,
        "outcome": outcome,
        "body": body,
    }
    _, observed = write_local_blob(config, ref, expected)
    if observed != expected:
        fail(f"local campaign summary {ref!r} disagrees with this outcome")
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
        task_domains = task.get("conflictDomains")
        if not isinstance(task_domains, list):
            task_domains = []
        for other in frontier:
            other_domains = other.get("conflictDomains")
            if not isinstance(other_domains, list):
                other_domains = []
            for left in task_domains:
                for right in other_domains:
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


def action_reconcile(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief,
        seam_fields(
            brief,
            {
                "campaign",
                "campaignIdentity",
                "repository",
                "repositoryConfig",
                "issue",
                "worklist",
                "maxTasks",
                "maxParallel",
                "attemptReceipts",
            },
        ),
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
            **carried_coordinates(data),
        }
    )
    repository = worklist["repository"]
    config = repo_config(data.get("repositoryConfig"))
    max_parallel = data.get("maxParallel", 1)
    coordinates = campaign_coordinates(data, repository, config)
    code = coordinates["code"]
    issue_target = coordinates["issue"]
    source_revision = required_string(
        worklist["source"].get("revision"), "worklist source revision"
    )
    # The worklist revision witnesses the spec history; the integration branch
    # is the code-history anchor consumed by every merge and checkpoint.
    base_rev = (
        source_revision
        if same_repository(coordinates["spec"], code)
        else observed_base_revision(code["config"])
    )
    local_campaign_id = campaign_id(data)
    base_rev = ensure_integration_branch(
        code["config"], campaign, local_campaign_id, base_rev
    )
    merged = merged_local_tasks(
        code["repository"],
        code["config"],
        campaign,
        local_campaign_id,
        issue["number"],
        base_rev,
        worklist["tasks"],
    )
    checkpoints = completed_checkpoint_tasks(
        code["config"],
        campaign,
        issue["number"],
        worklist["tasks"],
        worklist["source"],
        merged,
        base_rev,
    )
    completed_ids = {fact["taskId"] for fact in merged + checkpoints}
    task_ids = {task["id"] for task in worklist["tasks"]}
    diagnoses, retries, escalation, warnings = campaign_attempt_state(
        data.get("attemptReceipts"),
        campaign,
        issue["number"],
        task_ids,
    )
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
            blocked.append(
                {"taskId": task["id"], "blockedBy": sorted(roots, key=order.get)}
            )
    blocked_ids = {fact["taskId"] for fact in blocked}
    ready = [
        task
        for task in remaining
        if task["id"] not in blocked_ids
        and all(dependency in completed_ids for dependency in task["dependencies"])
    ]
    deferrals = checkpoint_deferrals(
        worklist["tasks"], remaining, completed_ids, blocked_ids
    )
    deferred_ids = {deferral["taskId"] for deferral in deferrals}
    ready.sort(key=lambda task: task["id"] in deferred_ids)
    frontier: list[dict[str, Any]] = []
    for task in ready:
        if len(frontier) == max_parallel:
            break
        if not task_conflicts(task, frontier):
            frontier.append(task)
    warnings.extend(parallelism_warnings(ready, frontier, max_parallel))
    result: dict[str, Any] = {
        "schemaVersion": 1,
        "campaign": campaign,
        "repository": repository,
        "source": worklist["source"],
        "baseRevision": base_rev,
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
        "warnings": warnings,
        "closingSummary": None,
    }
    if not remaining:
        result["closingSummary"] = publish_closing_summary(
            issue_target["repository"],
            issue_target["config"],
            campaign,
            issue["number"],
            campaign_digest(result, "complete"),
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


def local_actor(value: Any, context: str) -> str:
    """Validate the actor frozen into authority schema v3 at arm time."""
    if (
        not isinstance(value, str)
        or not value
        or len(value) > 128
        or "\0" in value
        or "/" in value
        or "\\" in value
        or any(char.isspace() for char in value)
    ):
        fail(f"{context} is not a valid local actor")
    return value


def steering_comment(value: Any, context: str) -> dict[str, Any]:
    comment = object_complete(
        value,
        {"id", "url", "author", "body", "createdAt", "updatedAt"},
        context,
    )
    identifier = comment.get("id")
    if not isinstance(identifier, int) or isinstance(identifier, bool) or identifier < 1:
        fail(f"{context}.id must be a positive integer")
    body = comment.get("body")
    if not isinstance(body, str) or "\0" in body:
        fail(f"{context}.body must be text without NUL bytes")
    if len(body) > 64_000:
        fail(f"{context}.body exceeds 64000 characters")
    return {
        "id": identifier,
        "url": required_string(comment.get("url"), f"{context}.url"),
        "author": local_actor(comment.get("author"), f"{context}.author"),
        "body": body,
        "createdAt": required_string(
            comment.get("createdAt"), f"{context}.createdAt"
        ),
        "updatedAt": required_string(
            comment.get("updatedAt"), f"{context}.updatedAt"
        ),
    }


RFC3339_TIMESTAMP = re.compile(
    r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$"
)


def steering_timestamp(value: Any, context: str) -> datetime:
    text = required_string(value, context)
    if not RFC3339_TIMESTAMP.fullmatch(text):
        fail(f"{context} is not an RFC 3339 timestamp")
    try:
        return datetime.fromisoformat(text.replace("Z", "+00:00"))
    except ValueError:
        fail(f"{context} is not an RFC 3339 timestamp")


def authorized_steering_comments(
    comments: Any, allowed_actor: str, context: str
) -> list[dict[str, Any]]:
    """Apply the arm-time local actor authorization to one fresh read."""
    if not isinstance(comments, list):
        fail(f"{context} must be an array")
    authorized: list[dict[str, Any]] = []
    seen: set[int] = set()
    for index, candidate in enumerate(comments):
        comment = steering_comment(candidate, f"{context}[{index}]")
        if comment["author"] != allowed_actor:
            continue
        if comment["id"] in seen:
            fail(f"{context} repeated comment id {comment['id']}")
        seen.add(comment["id"])
        authorized.append(comment)
    if len(authorized) > 2_000:
        fail(f"{context} has more than 2000 approved steering comments")
    return authorized


def steering_source(value: Any, local_actor_value: str) -> dict[str, Any]:
    source = object_complete(
        value,
        {
            "schemaVersion",
            "kind",
            "registrationId",
            "localActor",
            "logPath",
            "lockPath",
            "preparedCursor",
        },
        "steeringSource",
    )
    if source.get("schemaVersion") != 1 or source.get("kind") != "local-jsonl":
        fail("steeringSource must use local-jsonl schema version 1")
    registration_id = required_string(
        source.get("registrationId"), "steeringSource.registrationId", 128
    )
    try:
        if str(uuid.UUID(registration_id)) != registration_id:
            raise ValueError
    except ValueError:
        fail("steeringSource.registrationId must be a canonical UUID")
    source_actor = local_actor(source.get("localActor"), "steeringSource.localActor")
    if source_actor != local_actor_value:
        fail("steeringSource.localActor does not match localActor")
    log_path = Path(required_string(source.get("logPath"), "steeringSource.logPath"))
    lock_path = Path(required_string(source.get("lockPath"), "steeringSource.lockPath"))
    if not log_path.is_absolute() or not lock_path.is_absolute():
        fail("steeringSource paths must be absolute")
    if (
        log_path.name != "steering-v1.jsonl"
        or lock_path.name != "steering.lock"
        or log_path.parent != lock_path.parent
        or log_path.parent.name != registration_id
        or log_path.parent.parent.name != "steering"
        or log_path.parent.parent.parent.name != "campaigns"
    ):
        fail("steeringSource paths do not identify one campaign steering source")
    prepared_cursor = source.get("preparedCursor")
    if (
        not isinstance(prepared_cursor, int)
        or isinstance(prepared_cursor, bool)
        or prepared_cursor < 0
    ):
        fail("steeringSource.preparedCursor must be a non-negative integer")
    return {
        "registrationId": registration_id,
        "localActor": source_actor,
        "logPath": log_path,
        "lockPath": lock_path,
        "preparedCursor": prepared_cursor,
    }


def _open_regular_nofollow(path: Path, flags: int, context: str) -> int:
    nofollow = getattr(os, "O_NOFOLLOW", 0)
    cloexec = getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(path, flags | nofollow | cloexec)
    except OSError as error:
        fail(f"cannot open {context} {path}: {error}")
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            fail(f"{context} {path} is not a regular file")
    except BaseException:
        os.close(descriptor)
        raise
    return descriptor


def local_steering_comments(
    source: dict[str, Any], task_id: str
) -> tuple[list[dict[str, Any]], int]:
    """Read one complete append-only snapshot under the writer's flock."""
    lock_descriptor = _open_regular_nofollow(
        source["lockPath"], os.O_RDWR, "campaign steering lock"
    )
    try:
        fcntl.flock(lock_descriptor, fcntl.LOCK_SH)
        log_descriptor = _open_regular_nofollow(
            source["logPath"], os.O_RDONLY, "campaign steering log"
        )
        try:
            metadata = os.fstat(log_descriptor)
            if metadata.st_size > 128 * 1024 * 1024:
                fail("campaign steering log exceeds 128 MiB")
            with os.fdopen(log_descriptor, "rb", closefd=False) as handle:
                raw = handle.read(128 * 1024 * 1024 + 1)
        finally:
            os.close(log_descriptor)
    finally:
        try:
            fcntl.flock(lock_descriptor, fcntl.LOCK_UN)
        finally:
            os.close(lock_descriptor)
    if len(raw) > 128 * 1024 * 1024:
        fail("campaign steering log exceeds 128 MiB")
    if raw and not raw.endswith(b"\n"):
        fail("campaign steering log has an incomplete final record")
    comments: list[dict[str, Any]] = []
    target_counts: dict[str | None, int] = {}
    prior_embargo: datetime | None = None
    lines = raw.splitlines()
    for index, line in enumerate(lines):
        if not line:
            fail(f"campaign steering log has an empty record at line {index + 1}")
        try:
            value = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            fail(f"campaign steering log record {index + 1} is invalid: {error}")
        record = object_complete(
            value,
            {
                "schemaVersion",
                "sequence",
                "registrationId",
                "taskId",
                "doNotDispatchBefore",
                "comment",
            },
            f"campaign steering record {index + 1}",
        )
        sequence = index + 1
        target = record.get("taskId")
        if target is not None and (
            not isinstance(target, str)
            or len(target) > 80
            or not TASK_ID.fullmatch(target)
        ):
            fail(f"campaign steering record {sequence}.taskId is invalid")
        comment = steering_comment(
            record.get("comment"), f"campaign steering record {sequence}.comment"
        )
        expected_url = (
            f"local://campaign/{source['registrationId']}/steering/{sequence}"
        )
        if (
            record.get("schemaVersion") != 1
            or record.get("sequence") != sequence
            or record.get("registrationId") != source["registrationId"]
            or comment["id"] != sequence
            or comment["url"] != expected_url
            or comment["author"] != source["localActor"]
        ):
            fail(f"campaign steering record {sequence} violates steering-v1 invariants")
        created = steering_timestamp(
            comment["createdAt"],
            f"campaign steering record {sequence}.comment.createdAt",
        )
        updated = steering_timestamp(
            comment["updatedAt"],
            f"campaign steering record {sequence}.comment.updatedAt",
        )
        embargo = steering_timestamp(
            record.get("doNotDispatchBefore"),
            f"campaign steering record {sequence}.doNotDispatchBefore",
        )
        if updated != created or embargo != created + timedelta(milliseconds=1_000):
            fail(
                f"campaign steering record {sequence} has inconsistent "
                "append-only timestamps"
            )
        if prior_embargo is not None and embargo <= prior_embargo:
            fail(
                f"campaign steering record {sequence} does not advance "
                "doNotDispatchBefore"
            )
        prior_embargo = embargo
        target_counts[target] = target_counts.get(target, 0) + 1
        if target_counts[target] > 1_000:
            fail(f"campaign steering target {target!r} has more than 1000 records")
        if target is None or target == task_id:
            comments.append(comment)
    return comments, len(lines)


def action_steering_recheck(brief: dict[str, Any]) -> dict[str, Any]:
    """Fold steering that arrived after prep into this attempt's own brief."""
    data = object_complete(
        brief,
        {
            "campaign",
            "campaignIdentity",
            "taskId",
            "localActor",
            "steeringSource",
            "preparedComments",
        },
        "steering re-check brief",
    )
    campaign = required_string(data.get("campaign"), "campaign")
    if not COMPONENT.fullmatch(campaign):
        fail("campaign is not a safe component")
    campaign_identity = required_string(
        data.get("campaignIdentity"), "campaignIdentity", 128
    )
    task_id = required_string(data.get("taskId"), "taskId")
    if len(task_id) > 80 or not TASK_ID.fullmatch(task_id):
        fail("taskId is not safe")
    actor = local_actor(data.get("localActor"), "localActor")
    source = steering_source(data.get("steeringSource"), actor)
    if source["registrationId"] != campaign_identity:
        fail("steeringSource.registrationId does not match campaignIdentity")

    prepared_value = data.get("preparedComments")
    if not isinstance(prepared_value, list):
        fail("preparedComments must be an array")
    if len(prepared_value) > 2_000:
        fail("preparedComments has more than 2000 local steering records")
    prepared: list[dict[str, Any]] = []
    prepared_ids: set[int] = set()
    for index, value in enumerate(prepared_value):
        comment = steering_comment(value, f"preparedComments[{index}]")
        if comment["author"] != actor:
            fail("preparedComments contains an actor outside local authority")
        if comment["id"] > source["preparedCursor"]:
            fail("preparedComments contains an ID beyond the prepared cursor")
        expected_url = (
            f"local://campaign/{source['registrationId']}/steering/{comment['id']}"
        )
        if comment["url"] != expected_url:
            fail("preparedComments contains a record outside the local source")
        if comment["id"] in prepared_ids:
            fail(f"preparedComments repeated comment id {comment['id']}")
        prepared_ids.add(comment["id"])
        prepared.append(comment)

    raw_comments, rechecked_cursor = local_steering_comments(source, task_id)
    if rechecked_cursor < source["preparedCursor"]:
        fail("campaign steering log is behind the prepared cursor")
    rechecked = authorized_steering_comments(
        raw_comments, actor, "task steering re-check comments"
    )

    merged = list(prepared)
    positions = {comment["id"]: index for index, comment in enumerate(merged)}
    late_ids: list[int] = []
    for comment in rechecked:
        position = positions.get(comment["id"])
        if position is None:
            positions[comment["id"]] = len(merged)
            merged.append(comment)
            late_ids.append(comment["id"])
        elif merged[position] != comment:
            # An edit in the race window is steering that arrived late too.
            merged[position] = comment
            late_ids.append(comment["id"])

    return {
        "taskId": task_id,
        "authorizedComments": merged,
        "receipt": {
            "source": {
                "kind": "local-jsonl",
                "registrationId": source["registrationId"],
                "path": str(source["logPath"]),
                "preparedCursor": source["preparedCursor"],
                "recheckedCursor": rechecked_cursor,
            },
            "rechecked": True,
            "recheckTruncated": False,
            "preparedCommentIds": [comment["id"] for comment in prepared],
            "lateRecheckCommentIds": late_ids,
        },
    }


FORBID_PATHS_DETAIL = re.compile(
    r'forbidPaths gate \S+ rejected \d+ path\(s\) touched in lane history '
    r'\(a later removal does not clear this; the path must never appear in any lane commit\): '
    r'"((?:[^"\\]|\\.)*)"'
)


def gate_evidence_requirements(evidence: Any) -> tuple[str | None, str | None]:
    """What a steering note must name, when gate evidence supplies it (#385).

    Returns `(required_id, required_path)`, either half `None` when the
    caller supplied nothing to require. `evidence` is
    `{"id": <failing check id>, "detail": <raw gate failure text>}`: the id
    is always structural (the failing gate's own identifier, e.g. from
    `failure.stage`); the path is extracted only when the detail is shaped
    like a `forbidPaths` rejection (`evaluate_forbid_paths`'s own message
    format), since that is the only gate kind this driver runs that names an
    offending path at all.
    """
    if not isinstance(evidence, dict):
        return None, None
    gate_id = evidence.get("id")
    gate_id = gate_id if isinstance(gate_id, str) and gate_id else None
    detail = evidence.get("detail")
    path = None
    if isinstance(detail, str):
        matched = FORBID_PATHS_DETAIL.search(detail)
        if matched:
            path = matched.group(1)
    return gate_id, path


def diagnosis_fallback_note(
    reason: str,
    gate_id: str | None,
    path: str | None,
    rejected: str | None = None,
) -> str:
    """The durable fact a steering note's validation failure fires (#385).

    Deterministic and never model-authored -- built only from the driver's
    own rejection reason and the gate evidence it was already given -- and
    itself outcome-first shaped, so it survives being recorded in the same
    receipt the rejected diagnosis would have occupied. The rejected prose is
    an explicitly marked excerpt, not prose admitted through the grammar.
    """
    note = (
        "Recorded a grammar-rejected steward diagnosis. "
        f"Validation rejected the proposal because {reason}."
    )
    if gate_id:
        note += f" Required literal check id: {gate_id!r}."
    if path:
        note += f" Required literal offending path: {path!r}."
    if rejected:
        excerpt = re.sub(r"\s+", " ", rejected).strip().replace("!", ".")
        excerpt = excerpt[:2000].rstrip(" .:;?")
        if excerpt:
            note += f" Redacted proposal excerpt: {excerpt}."
    if len(note) > MAX_DIAGNOSIS_CHARS:
        note = note[: MAX_DIAGNOSIS_CHARS - 1].rstrip() + "…"
    return note


def publish_worker_findings(data: dict[str, Any]) -> str | None:
    """Record one redacted implementation result in local campaign state."""
    findings = normalized_worker_findings(data.get("workerFindings"))
    if findings is None:
        return None
    campaign = required_string(data.get("campaign"), "campaign")
    if not COMPONENT.fullmatch(campaign):
        fail("campaign is not a safe component")
    code_repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(code_repository):
        fail("repository must use owner/name form")
    target = campaign_coordinates(
        data,
        code_repository,
        repo_config(data.get("repositoryConfig")),
    )["issue"]
    repository = target["repository"]
    config = target["config"]
    issue = campaign_issue(data.get("issue"))
    task = data.get("task")
    if not isinstance(task, dict):
        fail("task must be an object")
    task_id = required_string(task.get("id"), "task.id")
    if not TASK_ID.fullmatch(task_id):
        fail("task.id is not safe")
    public_text, _ = redact_public_text(findings["message"])
    prefix = (
        "### Worker findings\n\n"
        "_Captured from the implementation worker's final message; "
        "redacted and bounded by tally._\n\n"
    )
    body = bound_worker_findings(prefix, public_text)
    ref = (
        f"{local_state_prefix(campaign, issue['number'])}/findings/"
        f"{task_id}/{findings['taskUuid']}"
    )
    expected = {
        "schemaVersion": 1,
        "kind": "worker-findings",
        "campaign": campaign,
        "issueNumber": issue["number"],
        "taskId": task_id,
        "agentTaskUuid": findings["taskUuid"],
        "body": body,
        "redaction": PUBLIC_REDACTION,
    }
    _, observed = write_local_blob(config, ref, expected)
    if observed != expected:
        fail(f"local campaign worker findings {ref!r} disagree with this attempt")
    return f"local://{repository}/{ref}"


def record_diagnosis(
    source: Any,
    campaign: str,
    issue_number: str,
    task_id: str,
    attempt: int,
    text: str,
) -> str:
    """Append one structured diagnosis receipt and return its local address."""
    if source is None:
        fail("attemptReceipts is required for a diagnosis")
    _, receipt = append_attempt_receipt(
        source,
        campaign,
        issue_number,
        {
            "kind": "diagnosis",
            "taskId": task_id,
            "attempt": attempt,
            "diagnosis": text,
            "redaction": PUBLIC_REDACTION,
        },
    )
    return receipt["comment"]
def diagnosis_rejection_reason(
    diagnosis: str, gate_evidence: Any
) -> tuple[str | None, str | None, str | None]:
    """Return the public-contract rejection without spending an attempt."""
    required_id, required_path = gate_evidence_requirements(gate_evidence)
    reason = validate_outcome_first(
        diagnosis, max_chars=MAX_DIAGNOSIS_CHARS, context="diagnosis"
    )
    if not reason and required_id and required_id not in diagnosis:
        reason = f"diagnosis omits the failing check id {required_id!r}"
    if not reason and required_path and required_path not in diagnosis:
        reason = f"diagnosis omits the offending path {required_path!r}"
    return reason, required_id, required_path


def validated_diagnosis(diagnosis: str, gate_evidence: Any) -> str:
    """#385's content contract on a non-retryable steering surface.

    Total: returns either the diagnosis unchanged or a deterministic,
    grammar-compliant note naming the one reason it was refused. Both the
    ordinary steering path and #386's breach path run it, so the same bad
    diagnosis is refused identically on both local receipt paths.
    """
    reason, required_id, required_path = diagnosis_rejection_reason(
        diagnosis, gate_evidence
    )
    if reason:
        return diagnosis_fallback_note(reason, required_id, required_path, diagnosis)
    return diagnosis


# The label sentence each lane-aborting tree-delta verdict posts. Two verdicts
# abort a lane and neither may be published under the other's sentence (#424):
# a gate that CAUGHT an out-of-allowlist write and a gate that could not judge
# the pass at all are different facts about the repository, and a receipt is a
# claim surface.
ABORT_REASONS = {
    "tree-delta-breach": (
        "Aborted the lane: a tree-delta permission breach found "
        "out-of-allowlist change(s), so this task will not be retried."
    ),
    "tree-delta-ungated": (
        "Aborted the lane: the tree-delta permission gate could not judge this "
        "pass -- the agent node failed, so the ownership node never ran and "
        "certified no paths, and this task declares no conflictDomains, leaving "
        "no allowlist. No out-of-allowlist change has been established. Declare "
        "conflictDomains for this task and re-arm; this task will not be retried "
        "until then."
    ),
}


def breach_note(diagnosis: str, detail_text: str, reason: str = "tree-delta-breach") -> str:
    """The recorded lane-abort body: a deterministic label plus witnessed evidence.

    #386: an out-of-allowlist delta aborts the lane -- a breach, not a
    gate-fail, because the write already happened and gates are for redoable
    work. The offending paths must be witnessed regardless of what the model
    wrote, so the driver's own tree-delta detail is always appended verbatim
    rather than merely required as a substring the model might paraphrase
    away. The abort identifies itself through this leading sentence instead of
    a second heading grammar.

    #424: `reason` selects which leading sentence. Both abort and both are
    priced identically; only one of them is a claim that a write happened, and
    that claim is only made when the gate actually found one.
    """
    parts = [ABORT_REASONS[reason]]
    if diagnosis:
        parts.append(diagnosis)
    if detail_text:
        parts.append(f"Witnessed evidence: {detail_text}")
    return "\n\n".join(parts)


def bounded_breach_note(
    diagnosis: str, detail_text: str, reason: str = "tree-delta-breach"
) -> str:
    """`breach_note` held to the same durable bound the ordinary path keeps.

    The ordinary steering path records `bound_public_diagnosis(diagnosis)`, so
    a breach must not record ~2x that just because it concatenates two bounded
    strings. The squeeze is deliberately asymmetric: the label sentence and
    the witnessed evidence are driver-authored and load-bearing -- the
    offending paths live in the evidence -- so the steward's elastic prose is
    what gives way, rather than truncating the paths off the end of the one
    receipt that exists to name them.
    """
    composed = breach_note(diagnosis, detail_text, reason)
    overflow = len(composed) - MAX_DIAGNOSIS_CHARS
    if overflow > 0:
        kept = max(0, len(diagnosis) - overflow)
        composed = breach_note(diagnosis[:kept].rstrip(), detail_text, reason)
    # Backstop for the pathological case where the evidence alone exceeds the
    # bound: truncation there is unavoidable, and `bound_public_diagnosis`
    # leaves a visible marker rather than trimming silently.
    return bound_public_diagnosis(composed)


def action_steer(brief: dict[str, Any]) -> dict[str, Any]:
    fields = {
        "campaign",
        "repository",
        "repositoryConfig",
        "issue",
        "taskId",
        "attempt",
        "diagnosis",
        "attemptReceipts",
    }
    if "checkpointCapture" in brief:
        fields.add("checkpointCapture")
    if "gateEvidence" in brief:
        fields.add("gateEvidence")
    if "breach" in brief:
        fields.add("breach")
    if "breachDetail" in brief:
        fields.add("breachDetail")
    if "abortReason" in brief:
        fields.add("abortReason")
    data = object_exact(brief, seam_fields(brief, fields), "steer brief")
    campaign = required_string(data.get("campaign"), "campaign")
    if not COMPONENT.fullmatch(campaign):
        fail("campaign is not a safe component")
    code_repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(code_repository):
        fail("repository must use owner/name form")
    campaign_coordinates(
        data, code_repository, repo_config(data.get("repositoryConfig"))
    )
    issue = campaign_issue(data.get("issue"))
    task_id = required_string(data.get("taskId"), "taskId")
    if not TASK_ID.fullmatch(task_id):
        fail("taskId is not safe")
    attempt = data.get("attempt")
    if attempt not in {1, 2}:
        fail("attempt must equal 1 or 2")

    def public_checkpoint_capture() -> tuple[str, bool]:
        """Prepare new public text lazily, after idempotency is established."""
        if "checkpointCapture" not in data:
            return "", False
        return redact_public_text(
            checkpoint_capture_note(data.get("checkpointCapture"), campaign, task_id)
        )

    breach = bool(data.get("breach", False))
    # #424: which lane-aborting tree-delta verdict this receipt is for. Absent
    # is the #386 breach, which is what every caller before this sent.
    abort_reason = data.get("abortReason", "tree-delta-breach")
    if abort_reason not in ABORT_REASONS:
        fail("abortReason is not a declared lane-abort reason")
    existing, _, _, _ = campaign_attempt_state(
        data.get("attemptReceipts"),
        campaign,
        issue["number"],
        None,
    )
    task_receipts = [receipt for receipt in existing if receipt["taskId"] == task_id]

    if breach:
        # #386: a breach aborts the lane -- it is not a redoable gate-fail,
        # so it never spends only one of the task's two ordinary steering
        # attempts and waits for a second failure to block. It reuses the
        # same diagnosis ledger the ordinary path writes (so the reconciler's
        # existing `attempt == 2` block rule needs no change at all) by
        # posting whichever of attempt 1 and attempt 2 do not exist yet, both
        # in this one call, so the task is permanently blocked as of this
        # pass and the ordered fold never sees a lone attempt 2.
        already_blocked = next(
            (receipt for receipt in task_receipts if receipt["attempt"] == 2), None
        )
        if already_blocked is not None:
            return {
                "kind": "diagnosis",
                "taskId": task_id,
                "attempt": 2,
                "comment": already_blocked["comment"],
                "blocked": True,
                "posted": False,
                "redacted": False,
            }
        capture_note, capture_redacted = public_checkpoint_capture()
        diagnosis = required_text(
            data.get("diagnosis"), "diagnosis", MAX_DIAGNOSIS_CHARS
        )
        diagnosis, redacted_diagnosis = redact_public_text(diagnosis)
        diagnosis = bound_public_diagnosis(diagnosis)
        # #385's content contract governs this receipt too. Rejection replaces
        # the steward's prose with the durable fallback note and nothing more:
        # the breach still aborts, still records both receipts, and still
        # witnesses its paths, because the label sentence and the evidence
        # below are the node's own and never the model's to lose.
        diagnosis = validated_diagnosis(diagnosis, data.get("gateEvidence"))
        detail = data.get("breachDetail")
        detail_text = ""
        redacted_detail = False
        if isinstance(detail, str) and detail.strip():
            detail_text, redacted_detail = redact_public_text(detail)
            detail_text = bound_public_diagnosis(detail_text)
        composed = bounded_breach_note(diagnosis, detail_text, abort_reason)
        composed = append_checkpoint_capture_note(
            composed,
            capture_note,
            MAX_DIAGNOSIS_CHARS,
        )
        posted_comment: str | None = None
        for post_attempt in (1, 2):
            if any(receipt["attempt"] == post_attempt for receipt in task_receipts):
                continue
            posted_comment = record_diagnosis(
                data.get("attemptReceipts"),
                campaign,
                issue["number"],
                task_id,
                post_attempt,
                composed,
            )
        return {
            "kind": "diagnosis",
            "taskId": task_id,
            "attempt": 2,
            "comment": posted_comment,
            "blocked": True,
            "posted": True,
            "redacted": redacted_diagnosis or redacted_detail or capture_redacted,
        }

    if any(receipt["attempt"] == attempt for receipt in task_receipts):
        receipt = next(
            receipt for receipt in task_receipts if receipt["attempt"] == attempt
        )
        return {
            "kind": "diagnosis",
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
            f"{len(task_receipts)} durable receipts"
        )
    capture_note, capture_redacted = public_checkpoint_capture()
    diagnosis = required_text(
        data.get("diagnosis"), "diagnosis", MAX_DIAGNOSIS_CHARS
    )
    diagnosis, redacted = redact_public_text(diagnosis)
    diagnosis = bound_public_diagnosis(diagnosis)
    # Validation still distinguishes admitted prose from a rejected proposal.
    # Rejection now degrades to a marked, driver-authored wrapper in this same
    # diagnosis receipt, so the already-redacted excerpt reaches the next
    # worker instead of being withheld behind machinery retry receipts.
    diagnosis = validated_diagnosis(diagnosis, data.get("gateEvidence"))
    diagnosis = append_checkpoint_capture_note(
        diagnosis,
        capture_note,
        MAX_DIAGNOSIS_CHARS,
    )
    comment = record_diagnosis(
        data.get("attemptReceipts"),
        campaign,
        issue["number"],
        task_id,
        attempt,
        diagnosis,
    )
    return {
        "kind": "diagnosis",
        "taskId": task_id,
        "attempt": attempt,
        "comment": comment,
        "blocked": attempt == 2,
        "posted": True,
        "redacted": redacted or capture_redacted,
    }


def action_retry(brief: dict[str, Any]) -> dict[str, Any]:
    """Record a campaign machinery fault without spending a steering attempt.

    Prep, lane, rebase, merge and publish faults say nothing about whether the
    task's work is right, so they buy a bounded retry instead. The budget is
    read back from durable campaign state like every other campaign fact; once
    it is spent the caller must treat the next fault as a failed attempt.
    """
    fields = {
        "campaign",
        "repository",
        "repositoryConfig",
        "issue",
        "taskId",
        "stage",
        "detail",
        "attemptReceipts",
    }
    if "checkpointCapture" in brief:
        fields.add("checkpointCapture")
    data = object_exact(brief, seam_fields(brief, fields), "retry brief")
    campaign = required_string(data.get("campaign"), "campaign")
    if not COMPONENT.fullmatch(campaign):
        fail("campaign is not a safe component")
    code_repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(code_repository):
        fail("repository must use owner/name form")
    campaign_coordinates(
        data, code_repository, repo_config(data.get("repositoryConfig"))
    )
    issue = campaign_issue(data.get("issue"))
    task_id = required_string(data.get("taskId"), "taskId")
    if not TASK_ID.fullmatch(task_id):
        fail("taskId is not safe")
    stage = required_string(data.get("stage"), "stage")
    if not re.fullmatch(r"[a-z][a-z0-9:._-]{0,63}", stage):
        fail("stage is not a safe campaign stage name")
    detail = required_text(data.get("detail"), "detail", MAX_RETRY_CHARS)
    _, existing, _, _ = campaign_attempt_state(
        data.get("attemptReceipts"),
        campaign,
        issue["number"],
        None,
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
    capture_note = ""
    if "checkpointCapture" in data:
        capture_note = checkpoint_capture_note(
            data.get("checkpointCapture"),
            campaign,
            task_id,
        )
    raw_reason = f"Stage `{stage}` faulted."
    if capture_note:
        raw_reason += f"\n\n{capture_note}"
    raw_reason += f"\n\n{detail}"
    # The optional checkpoint excerpt deliberately enters the existing public
    # steering redaction with the stage detail. There is one reviewed
    # conservative-v2 pass, not a checkpoint-specific redactor.
    reason, redacted = redact_public_text(raw_reason)
    if len(reason) > MAX_RETRY_CHARS:
        reason = reason[: MAX_RETRY_CHARS - 3].rstrip() + "..."
    reason = required_text(reason, "retry reason", MAX_RETRY_CHARS)
    created, receipt = append_attempt_receipt(
        data.get("attemptReceipts"),
        campaign,
        issue["number"],
        {
            "kind": "retry",
            "taskId": task_id,
            "attempt": attempt,
            "reason": reason,
            "redaction": PUBLIC_REDACTION,
        },
    )
    comment = receipt["comment"]
    return {
        "taskId": task_id,
        "attempt": attempt,
        "comment": comment,
        "exhausted": attempt == MAX_MACHINE_RETRIES,
        "posted": created,
        "redacted": redacted,
    }


def compact_summary(value: str, maximum: int = 64) -> str:
    compact = re.sub(r"\s+", " ", value).strip()
    return compact if len(compact) <= maximum else compact[: maximum - 3] + "..."


def action_escalate(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief,
        seam_fields(
            brief,
            {
                "campaign",
                "campaignIdentity",
                "repository",
                "repositoryConfig",
                "issue",
                "worklist",
                "maxTasks",
                "maxParallel",
                "attemptReceipts",
            },
        ),
        "escalate brief",
    )
    reconciliation = action_reconcile(brief)
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
    # The pass that selected this node and the eligibility check above both
    # observed an empty frontier, but a merge or checkpoint can settle while
    # this terminal action is preparing its writes. Re-read every durable fact
    # once more at the publication boundary. A stale quiescent digest is worse
    # than a failed node: the marked escalation would make every later pass
    # believe the campaign had stopped even though work was now dispatchable.
    refreshed = action_reconcile(brief)
    if refreshed["complete"] or not refreshed["quiescent"]:
        fail(
            "campaign quiescence changed during the pre-post durable refresh; "
            "refusing to post outcome=quiescent"
        )
    if refreshed["escalation"] is not None:
        return {
            "posted": False,
            "comment": refreshed["escalation"],
            "summary": None,
            "diagnosisCount": len(refreshed["diagnoses"]),
            "retryCount": len(refreshed["retries"]),
        }
    reconciliation = refreshed
    campaign = required_string(reconciliation.get("campaign"), "campaign")
    issue = campaign_issue(data.get("issue"))
    target = campaign_coordinates(
        data,
        reconciliation["repository"],
        repo_config(data.get("repositoryConfig")),
    )["issue"]
    repository = target["repository"]
    config = target["config"]
    direct = [
        fact["taskId"]
        for fact in reconciliation["blocked"]
        if fact["taskId"] in fact["blockedBy"]
    ]
    lines = [
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
    capture_paths = checkpoint_capture_paths(
        [item["diagnosis"] for item in reconciliation["diagnoses"]]
        + [item["reason"] for item in reconciliation["retries"]]
    )
    if capture_paths:
        lines.extend(["", "Checkpoint captures:"])
        lines.extend(f"- {path}" for path in capture_paths)
    if reconciliation["warnings"]:
        lines.extend(["", "Reconciler warnings:"])
        lines.extend(
            f"- {compact_summary(warning, 200)}"
            for warning in reconciliation["warnings"][:12]
        )
    body = "\n".join(lines)
    if len(body) > 60_000:
        fail("machine escalation exceeds 60,000 characters")
    # Quiescence is the campaign's other terminal outcome, and the escalation
    # is what proves it was reached: every later pass reads that receipt back
    # and stops before this node runs again. So the digest is recorded first,
    # exactly as the completion path records its terminal receipt. A
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
    created, receipt = append_attempt_receipt(
        data.get("attemptReceipts"),
        campaign,
        issue["number"],
        {
            "kind": "escalation",
            "body": body,
        },
    )
    comment = receipt["comment"]
    return {
        "posted": created,
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
    # This directory is the campaign's whole continuation mechanism and it is
    # the one write in this node that a sandbox, a full filesystem or a wrong
    # owner can refuse. An OSError escaping here would leave a Python traceback
    # on the node's stderr instead of the driver's bounded, redacted failure
    # line, so every step of the write reports through fail().
    temporary = events_dir / f".{name}.{uuid.uuid4()}.tmp"
    try:
        events_dir.mkdir(parents=True, exist_ok=True)
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
    except OSError as error:
        fail(f"cannot write the continuation event {path}: {error.strerror or error}")
        raise AssertionError("unreachable")
    return created, path


def action_continue(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief,
        seam_fields(
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
        ),
        "continue brief",
    )
    campaign = required_string(data.get("campaign"), "campaign")
    if not COMPONENT.fullmatch(campaign):
        fail("campaign is not a safe component")
    repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(repository):
        fail("repository must use owner/name form")
    # The dedup key stays anchored to the code coordinate; the durable
    # continuation receipt uses the configured local receipt store.
    target = campaign_coordinates(
        data, repository, repo_config(data.get("repositoryConfig"))
    )["issue"]
    config = target["config"]
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
        fail(f"local campaign continuation {ref!r} disagrees with this pass")
    receipt = f"local://{target['repository']}/{ref}"
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


def campaign_id(data: dict[str, Any]) -> str:
    """Return the arm identity, with the pre-v3 issue key as a narrow fallback."""
    value = data.get("campaignIdentity")
    if value is not None:
        return required_string(value, "campaignIdentity", 128)
    # Compatibility briefs admitted before authority v3 still carry no arm
    # identity. They remain readable during this transition, but every current
    # flow forwards the registration id explicitly.
    issue = data.get("issue")
    if issue is not None:
        return campaign_issue(issue)["number"]
    return required_string(data.get("campaign"), "campaign", 128)


def ensure_integration_branch(
    config: dict[str, Any],
    campaign: str,
    identity: str,
    start_rev: str,
    lineage_rev: str | None = None,
) -> str:
    """Create one campaign's local integration branch or validate its lineage."""
    checkout: Path = config["checkout"]
    start_rev = full_git_oid(start_rev, "integration branch start revision")
    lineage_rev = full_git_oid(
        start_rev if lineage_rev is None else lineage_rev,
        "integration branch witnessed revision",
    )
    git(checkout, "cat-file", "-e", f"{start_rev}^{{commit}}")
    git(checkout, "cat-file", "-e", f"{lineage_rev}^{{commit}}")
    branch = integration_branch(campaign, identity)
    reference = f"refs/heads/{branch}"
    current = local_branch_oid(checkout, branch)
    if current is None:
        created = git(
            checkout,
            "update-ref",
            reference,
            start_rev,
            "0" * len(start_rev),
            check=False,
        )
        if created.returncode == 0:
            return start_rev
        # A concurrent initializer may have won the compare-and-swap. Read its
        # result and put it through the same lineage check below.
        current = local_branch_oid(checkout, branch)
        if current is None:
            detail = created.stderr.strip() or created.stdout.strip() or "no output"
            fail(f"cannot create local integration branch {branch!r}: {detail}")
    common = git(checkout, "merge-base", start_rev, current, check=False)
    if common.returncode or not GIT_OID.fullmatch(common.stdout.strip()):
        fail(
            f"local integration branch {branch!r} shares no history with "
            f"repository revision {start_rev}"
        )
    if lineage_rev != start_rev and git(
        checkout,
        "merge-base",
        "--is-ancestor",
        lineage_rev,
        current,
        check=False,
    ).returncode:
        fail(
            f"local integration branch {branch!r} does not descend from "
            f"witnessed revision {lineage_rev}"
        )
    return current


def required_integration_revision(
    config: dict[str, Any], campaign: str, identity: str
) -> str:
    branch = integration_branch(campaign, identity)
    revision = local_branch_oid(config["checkout"], branch)
    if revision is None:
        fail(f"local integration branch {branch!r} does not exist")
    return revision


def prep_identity(brief: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    data = object_exact(
        brief,
        {
            "campaign",
            "campaignIdentity",
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
        "campaignId": campaign_id(data),
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


def snapshot_before_agent(worktree: Path) -> bool:
    """The pre-agent change-set fingerprint the tree-delta gate compares
    against (#386). Called once per `prep` of an implementation task -- the
    earliest point after the lane is known-good and the latest point before
    anything the gate must witness could happen.

    **A baseline is never overwritten unjudged (#424).** `action_tree_delta`
    clears the snapshot the instant it reads it, pass or fail, so a snapshot
    still on disk at prep time means exactly one thing: the pass it belongs to
    ended without the gate judging it. Overwriting it there was the laundering
    path -- pass 1's agent clobbered an out-of-allowlist file and failed, pass
    2's prep re-fingerprinted the worktree with that write already in it, and
    the write could never be seen again by anything. So: judged (no snapshot on
    disk) rotates, unjudged (snapshot present) is preserved, and the next gate
    to run judges the whole span since the last judged baseline.

    Returns whether a fresh baseline was taken, so a caller can say which of
    the two happened rather than guess.
    """
    if worktree_call(worktrees.read_change_set_snapshot, worktree) is not None:
        return False
    fingerprint = worktree_call(worktrees.change_set_fingerprint, worktree)
    worktree_call(worktrees.write_change_set_snapshot, worktree, fingerprint)
    return True


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
        identity["campaignId"],
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

    # The fetch and the coherence check come before the resume door, not after
    # it. A prep node that re-runs inside one flow run takes the resume path,
    # and returning an already-prepared lane before this check let exactly the
    # history the fresh-cut door refuses come back through the other one: after
    # a remote force-replacement the retry handed back the stale lane and its
    # stale baseRev with no error at all.
    git(checkout, "fetch", "--prune", remote)
    base_ref = f"{remote}/{base_branch}"
    remote_tip = git(
        checkout, "rev-parse", "--verify", f"{base_ref}^{{commit}}"
    ).stdout.strip()
    # The remote establishes repository continuity and seeds first use; the
    # integration branch is the witnessed lane base and may advance locally
    # while the remote base remains unchanged.
    base_tip = ensure_integration_branch(
        config,
        identity["campaign"],
        identity["campaignId"],
        remote_tip,
        identity["sourceRevision"],
    )

    resumed = worktree_call(
        worktrees.resume, checkout, worktree, expected, required=("baserev",)
    )
    if resumed is not None and resumed["complete"]:
        resumed_base = required_string(resumed["identity"].get("baserev"), "lane baseRev")
        # The lane already exists, so nothing above narrowed where *it* forks
        # from. An existing lane whose own base no longer descends from the
        # witnessed revision is refused rather than resumed: it was cut from a
        # history this pass is not reasoning about.
        if git(
            checkout,
            "merge-base",
            "--is-ancestor",
            identity["sourceRevision"],
            resumed_base,
            check=False,
        ).returncode:
            fail("resumed lane base does not descend from the witnessed worklist revision")
        if identity["taskKind"] == "implementation":
            snapshot_before_agent(worktree)
        return {
            "taskId": identity["taskId"],
            "baseRev": resumed_base,
            "branch": branch,
            "publishBranch": publish_branch,
            "worktreePath": str(worktree),
        }
    if resumed is not None:
        # A lane git registered whose identity is short a field: a lane cut by
        # a tally from before identity moved into git, or a runner killed
        # before it was recorded. The lane's own history is the authority for
        # where it forks from base, so it is healed rather than refused.
        lane_head = resumed["head"]
    else:
        publish_ref = f"refs/heads/{publish_branch}"
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
    if identity["taskKind"] == "implementation":
        snapshot_before_agent(worktree)
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
    """The base branch tip, taken from the remote rather than a local ref.

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
        receipt["conflictDomains"] if "conflictDomains" in receipt else MISSING,
        f"{context}.conflictDomains",
        required=domains_required,
    )
    owned_paths = normalize_owned_paths(receipt.get("ownedPaths"), f"{context}.ownedPaths")
    if owned_paths != sorted(owned_paths):
        fail(f"{context}.ownedPaths must be sorted")
    normalized = {
        "taskId": task_id,
        "domainsRequired": domains_required,
        "ownedPaths": owned_paths,
        "baseRev": full_git_oid(receipt.get("baseRev"), f"{context}.baseRev"),
        "head": full_git_oid(receipt.get("head"), f"{context}.head"),
    }
    if domains is not None:
        normalized["conflictDomains"] = domains
    return normalized


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
        task["conflictDomains"] if "conflictDomains" in task else MISSING,
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
        []
        if domains is None
        else [
            path
            for path in changed_paths
            if not any(domains_overlap(path, domain) for domain in domains)
        ]
    )
    if outside:
        preview = ", ".join(json.dumps(path) for path in outside[:20])
        if len(outside) > 20:
            preview += f", and {len(outside) - 20} more"
        declared = ", ".join(json.dumps(domain) for domain in domains or [])
        fail(
            f"task {task_id!r} changed {len(outside)} path(s) outside its declared "
            f"conflictDomains: {preview}; declared domains: {declared}"
        )
    receipt = {
        "taskId": task_id,
        "domainsRequired": domains_required,
        "ownedPaths": changed_paths,
        "baseRev": base_rev,
        "head": head,
    }
    if domains is not None:
        receipt["conflictDomains"] = domains
    return receipt


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


def action_tree_delta(brief: dict[str, Any]) -> dict[str, Any]:
    """The tree-delta permission gate around the campaign agent node (#386).

    Detective, not preventive -- the SSSF `permissions.py` import: "permission
    is verified the way every other claim in this system is -- after the
    fact, against the repo itself." `prep` fingerprinted the worktree's full
    tracked-and-untracked content before the agent ran; this compares that
    fingerprint against the worktree's content right now. A path appearing,
    disappearing, or changing all count identically, so a reversion of an
    uncommitted change back to its prior content is caught the same as a
    forward edit no commit ever recorded.

    Scope (#424): this node runs on both outcomes of the agent node. On a pass
    whose agent node passed it runs after `ownership`, which is what lets an
    undeclared allowlist fall back to certified `ownedPaths`. On a pass whose
    agent node FAILED it runs instead of `ownership` -- a failing agent is the
    single most likely context for a rogue write, and it used to be the one
    context this gate was silent in -- and `ownershipRan` is false, so no
    `ownedPaths` exist and only a declared allowlist can govern.

    The allowlist is per-task, derived from the brief/worklist entry, with no
    silently permissive default -- an absent allowlist and an empty one are
    different outcomes, not the same "anything goes":
      - `task.conflictDomains` declared and non-empty: those path prefixes,
        compared with `domains_overlap` at path-component boundaries and
        case-folded -- identical semantics to the ownership gate's own
        allowlist, and not glob matching.
      - `task.conflictDomains` declared and explicitly empty (`[]`): the
        allowlist is empty, so any delta at all is a breach.
      - `task.conflictDomains` absent AND `ownershipRan`: the allowlist falls
        back to exactly `ownedPaths`, the paths the ownership node just
        certified as this task's own committed change-set. The agent's proven
        work is self-authorizing; nothing else is.
      - `task.conflictDomains` absent AND ownership never ran: no allowlist,
        no pass (#424). The node refuses, naming exactly why, and leaves the
        snapshot on disk so the writes it could not judge stay judgeable by a
        later pass that does have an allowlist. Passing here would be a gate
        that reports success over content it never inspected.

    An out-of-allowlist delta fails this node with every offending path
    named -- the same mechanism every other campaign gate uses to become
    queryable via `query job`/`query proof`, since this driver runs as an
    ordinary witnessed job like any other campaign node.
    """
    data = object_exact(
        brief, {"task", "workspace", "ownedPaths", "ownershipRan"}, "tree-delta brief"
    )
    # Absent means true: the node's original and still most common caller is
    # the post-ownership one, and a brief written before this field existed
    # described exactly that call.
    ownership_ran = data.get("ownershipRan", True)
    if not isinstance(ownership_ran, bool):
        fail("ownershipRan must be a boolean")
    task = data.get("task")
    if not isinstance(task, dict):
        fail("task must be an object")
    task_id = required_string(task.get("id"), "task.id")
    if not TASK_ID.fullmatch(task_id):
        fail("task.id is not safe")
    workspace = object_exact(
        data.get("workspace"),
        {"taskId", "baseRev", "branch", "publishBranch", "worktreePath"},
        "workspace",
    )
    if workspace.get("taskId") != task_id:
        fail("workspace.taskId does not match task.id")
    worktree = Path(required_string(workspace.get("worktreePath"), "workspace.worktreePath"))
    if not worktree.is_absolute() or not worktree.is_dir():
        fail("workspace.worktreePath must be an absolute existing directory")
    git(worktree, "rev-parse", "--git-dir")

    # The allowlist is settled before the snapshot is touched. A pass this gate
    # cannot judge must leave the baseline exactly as it found it, and reading
    # it here then refusing below would put the clear() between the two.
    if "conflictDomains" in task:
        allowlist = normalize_conflict_domains(
            task["conflictDomains"], "task.conflictDomains", required=False
        )
        assert allowlist is not None
        basis = "declared" if allowlist else "declared-empty"
    elif ownership_ran:
        owned = data.get("ownedPaths")
        allowlist = list(string_list(owned, "ownedPaths")) if owned is not None else []
        basis = "owned-paths-fallback"
    else:
        # #424 rule 3: no allowlist, no pass. This is a gate refusing to
        # certify, not the agent being at fault, so it must be loud and it must
        # not look like a clean pass. The snapshot stays on disk: the writes
        # this pass could not judge remain judgeable once an allowlist exists.
        fail(
            f"tree-delta gate refuses to judge task {task_id!r}: its agent node "
            "failed, so the ownership node never ran and certified no ownedPaths, "
            "and the task declares no conflictDomains -- there is no allowlist to "
            "judge the worktree against. Declare conflictDomains for this task and "
            "re-arm. The pre-agent baseline is left in place, so the writes this "
            "pass could not judge are still judgeable then."
        )

    before = worktree_call(worktrees.read_change_set_snapshot, worktree)
    if before is None:
        fail(
            "no change-set snapshot was recorded before the agent node; "
            "cannot evaluate the tree-delta gate"
        )
    after = worktree_call(worktrees.change_set_fingerprint, worktree)
    deltas = worktrees.change_set_delta(before, after)

    breaches = [
        delta
        for delta in deltas
        if not any(domains_overlap(delta["path"], domain) for domain in allowlist)
    ]
    # The snapshot's job is done the instant this pass reads it, whether the
    # gate passes or fails: a stale snapshot from this attempt must never be
    # compared against a later attempt's "after" state.
    worktree_call(worktrees.clear_change_set_snapshot, worktree)
    if breaches:
        preview = "; ".join(
            f"{item['kind']} {json.dumps(item['path'])}" for item in breaches[:20]
        )
        if len(breaches) > 20:
            preview += f"; and {len(breaches) - 20} more"
        fail(
            f"tree-delta gate detected {len(breaches)} out-of-allowlist change(s) "
            f"({basis} allowlist): {preview}"
        )
    return {
        "taskId": task_id,
        "checkedPaths": len(deltas),
        "allowlistBasis": basis,
        "allowlist": allowlist,
        # #424: which of the gate's two call sites this verdict came from, so a
        # reader of the witnessed result knows whether an `ownedPaths` fallback
        # was even available to it -- and can see that a pass whose agent
        # failed was in fact judged.
        "ownershipRan": ownership_ran,
    }


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
            f"forbidPaths gate {gate_id!r} rejected {len(violations)} path(s) touched "
            "in lane history (a later removal does not clear this; the path must "
            f"never appear in any lane commit): {preview}"
        )
    return len(changed_paths)


def action_constraint(brief: dict[str, Any]) -> dict[str, Any]:
    data = object_exact(
        brief, {"gate", "repositoryConfig", "workspace"}, "constraint brief"
    )
    gate = canonical_forbid_paths_gate(data.get("gate"), "constraint gate")
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
        gate = canonical_forbid_paths_gate(
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
            gates.append(canonical_forbid_paths_gate(candidate, f"{context}[{index}]"))
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
        "campaignIdentity",
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
        allowed.update(
            {
                "constraints",
                "domainsRequired",
                "gates",
                "steward",
                "workerFindings",
            }
        )
    if action == "merge":
        allowed.update(
            {
                "integration",
                "domainsRequired",
                "mergeMethod",
                "assistedBy",
            }
        )
    data = object_exact(brief, seam_fields(brief, allowed), f"{action} brief")
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
    identity = campaign_id(data)
    current_base = required_integration_revision(
        config, required_string(data.get("campaign"), "campaign"), identity
    )
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
    # The implementation has now passed the same identity, ownership, and gate
    # checks that authorize the stable local snapshot. Retain its report before
    # that snapshot is advanced; an exact marker makes a retry idempotent.
    publish_worker_findings(data)
    expected_branch = stable_publish_branch(
        required_string(data.get("campaign"), "campaign"),
        identity,
        task_id,
        task_revision(data["task"]),
    )
    if publish_branch != expected_branch:
        fail(
            f"workspace.publishBranch is {publish_branch!r}, expected local stable "
            f"branch {expected_branch!r}"
        )
    git(
        config["checkout"],
        "update-ref",
        f"refs/heads/{publish_branch}",
        head,
    )
    # Narration is fixed beside the stable local snapshot so the later layer
    # commit cannot drift from the gated task identity or diff it describes.
    task = data["task"]
    steward = steward_role(data.get("steward"))
    narration_task = {
        "id": task["id"],
        "title": task["title"],
        "goal": task.get("goal"),
        "brief": (task.get("brief") or {}).get("body")
        if isinstance(task.get("brief"), dict)
        else None,
    }
    if "conflictDomains" in task:
        narration_task["conflictDomains"] = task["conflictDomains"]
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
            "task": narration_task,
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
    checkout: Path,
    published: dict[str, str],
    context: str,
) -> None:
    abandoned = git(
        checkout,
        "update-ref",
        "-d",
        f"refs/heads/{published['branch']}",
        published["head"],
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
    campaign = required_string(data.get("campaign"), "campaign")
    identity = campaign_id(data)
    base_rev = required_integration_revision(config, campaign, identity)
    branch_head = local_branch_oid(checkout, published["branch"])
    if branch_head != published["head"]:
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
            checkout,
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
            checkout,
            published,
            f"rebased task failed integration policy against current base {base_rev}: {error}",
        )
        fail(
            f"rebased task failed integration policy against current base {base_rev}: {error}; "
            f"exact published head {published['head']} was abandoned so a fresh pass can "
            "rebuild the task"
        )
    advanced = git(
        checkout,
        "update-ref",
        f"refs/heads/{published['branch']}",
        rebased_head,
        published["head"],
        check=False,
    )
    if advanced.returncode != 0:
        detail = advanced.stderr.strip() or advanced.stdout.strip() or "no output"
        fail(
            f"published branch moved while rebasing exact head "
            f"{published['head']}: {detail}"
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


def local_completion_marker(data: dict[str, Any]) -> str:
    task = data.get("task")
    if not isinstance(task, dict):
        fail("local merge task must be an object")
    revision = task_revision(task)
    if revision is None:
        fail("local merge task must carry a completion revision")
    return pull_request_marker(
        required_string(data.get("campaign"), "campaign"),
        campaign_issue(data.get("issue"))["number"],
        required_string(task.get("id"), "task.id"),
        revision,
    )


def merge_commit_body(
    narration: dict[str, Any],
    trailer: str | None,
    marker: str | None = None,
) -> str:
    """Validated prose plus the node's own provenance pointer, in that order."""
    parts = [part for part in (narration["body"], marker, trailer) if part]
    return "\n\n".join(parts)


def merge_commit_message(
    narration: dict[str, Any],
    trailer: str | None = None,
    marker: str | None = None,
) -> str:
    """The validated message the node writes. The model never runs git."""
    body = merge_commit_body(narration, trailer, marker)
    if body:
        return f"{narration['subject']}\n\n{body}\n"
    return f"{narration['subject']}\n"


def merge_local(
    data: dict[str, Any],
    config: dict[str, Any],
    integration: dict[str, Any],
    method: str,
    narration: dict[str, Any],
    trailer: str | None = None,
) -> str:
    """Integrate one gated snapshot without cloning or touching the remote base."""
    checkout: Path = config["checkout"]
    campaign = required_string(data.get("campaign"), "campaign")
    identity = campaign_id(data)
    message = merge_commit_message(
        narration, trailer, local_completion_marker(data)
    )
    integration_name = integration_branch(campaign, identity)
    integration_ref = f"refs/heads/{integration_name}"
    published_ref = f"refs/heads/{integration['branch']}"
    actual_base = local_branch_oid(checkout, integration_name)
    actual_head = local_branch_oid(checkout, integration["branch"])
    if actual_base != integration["baseRev"]:
        fail("local integration branch moved after the rebased head was gated")
    if actual_head != integration["head"]:
        fail("published branch moved after the rebased head was gated")

    workspace_root = Path(data["workspaceRoot"])
    workspace_root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="merge-", dir=workspace_root) as temporary:
        integration_checkout = Path(temporary) / "worktree"
        git(
            checkout,
            "worktree",
            "add",
            "--detach",
            "--quiet",
            str(integration_checkout),
            actual_base,
        )
        try:
            if method == "squash":
                git(integration_checkout, "merge", "--squash", actual_head)
                if not git(
                    integration_checkout, "diff", "--cached", "--quiet", check=False
                ).returncode:
                    fail("squash merge staged no change against the witnessed base")
                git(
                    integration_checkout,
                    "-c",
                    "user.name=tally spec-build",
                    "-c",
                    "user.email=tally-spec-build@invalid",
                    "commit",
                    "--quiet",
                    "--file",
                    "-",
                    input_text=message,
                )
            else:
                git(
                    integration_checkout,
                    "-c",
                    "user.name=tally spec-build",
                    "-c",
                    "user.email=tally-spec-build@invalid",
                    "merge",
                    "--no-ff",
                    "--no-commit",
                    actual_head,
                )
                git(
                    integration_checkout,
                    "-c",
                    "user.name=tally spec-build",
                    "-c",
                    "user.email=tally-spec-build@invalid",
                    "commit",
                    "--quiet",
                    "--file",
                    "-",
                    input_text=message,
                )
            merge_commit = git(
                integration_checkout, "rev-parse", "HEAD"
            ).stdout.strip()

            transaction = [
                "start",
                f"verify {published_ref} {actual_head}",
                f"update {integration_ref} {merge_commit} {actual_base}",
            ]
            if method == "squash":
                receipt = merge_receipt_ref(
                    campaign,
                    identity,
                    integration["taskId"],
                    task_revision(data["task"]),
                )
                transaction.append(f"update {receipt} {merge_commit}")
            transaction.extend(("prepare", "commit"))
            git(
                checkout,
                "update-ref",
                "--stdin",
                input_text="\n".join(transaction) + "\n",
            )
        finally:
            git(
                checkout,
                "worktree",
                "remove",
                "--force",
                str(integration_checkout),
                check=False,
            )
    return merge_commit


def action_checkpoint(brief: dict[str, Any]) -> dict[str, Any]:
    fields = {
        "campaign",
        "campaignIdentity",
        "repository",
        "repositoryConfig",
        "issue",
        "task",
        "source",
        "workspace",
    }
    if "baseRevision" in brief:
        fields.add("baseRevision")
    capture_fields = {"captureRoot", "execution"}
    present_capture_fields = capture_fields.intersection(brief)
    if present_capture_fields and present_capture_fields != capture_fields:
        fail("checkpoint brief must carry captureRoot and execution together")
    fields.update(present_capture_fields)
    data = object_exact(brief, seam_fields(brief, fields), "checkpoint brief")
    campaign = required_string(data.get("campaign"), "campaign")
    if not COMPONENT.fullmatch(campaign):
        fail("campaign is not a safe component")
    repository = required_string(data.get("repository"), "repository")
    if not REPOSITORY.fullmatch(repository):
        fail("repository must use owner/name form")
    issue = campaign_issue(data.get("issue"))
    config = repo_config(data.get("repositoryConfig"))
    campaign_coordinates(data, repository, config)
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
    source_value = data.get("source")
    # `repository` is present exactly when the worklist was read from a spec
    # repository that is not the code repository.
    artifact_fields = {"path", "sha256", "revision"}
    if isinstance(source_value, dict) and "repository" in source_value:
        artifact_fields.add("repository")
    source = object_exact(source_value, artifact_fields, "source")
    required_string(source.get("path"), "source.path")
    if "repository" in source:
        source_repository = required_string(
            source.get("repository"), "source.repository"
        )
        if not REPOSITORY.fullmatch(source_repository):
            fail("source.repository must use owner/name form")
    source_sha256 = required_string(source.get("sha256"), "source.sha256")
    source_revision = required_string(source.get("revision"), "source.revision")
    if not re.fullmatch(r"[0-9a-f]{40,64}", source_revision):
        fail("source.revision must be a full Git object ID")
    # The lineage assertion below reasons inside the *code* checkout. A split
    # campaign's worklist revision belongs to another history, while a local
    # campaign's code anchor advances on its integration branch. Current flows
    # therefore carry the reconciled code anchor explicitly; compatibility
    # briefs may still use the worklist revision when those identities match.
    code_revision = source_revision
    if data.get("baseRevision") is not None:
        code_revision = required_string(data.get("baseRevision"), "baseRevision")
        if not re.fullmatch(r"[0-9a-f]{40,64}", code_revision):
            fail("baseRevision must be a full Git object ID")
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
    capture: dict[str, Any] | None = None
    if "execution" in data:
        capture = persist_checkpoint_capture(
            data.get("captureRoot"),
            data.get("execution"),
            campaign,
            issue["number"],
            task_id,
        )
        execution = data["execution"]
        if execution["verdict"] != "pass":
            return {
                "taskId": task_id,
                "passed": False,
                "ref": None,
                "revision": base_rev,
                "capturePath": capture["path"],
                "stdoutTruncated": capture["stdoutTruncated"],
                "stderrTruncated": capture["stderrTruncated"],
            }
    if git(worktree, "branch", "--show-current").stdout.strip() != branch:
        fail("checkpoint worktree changed branches during validation")
    if git(worktree, "rev-parse", "HEAD^{commit}").stdout.strip() != base_rev:
        fail("checkpoint command changed HEAD instead of validating the prepared base")
    if git(worktree, "status", "--porcelain", "--untracked-files=no").stdout:
        fail("checkpoint command changed tracked files instead of validating the prepared base")

    checkout: Path = config["checkout"]
    remote = config["remote"]
    git(checkout, "fetch", "--prune", "--no-tags", remote)
    integration_identity = data.get("campaignIdentity")
    uses_integration = integration_identity is not None
    if uses_integration:
        current_base = required_integration_revision(
            config,
            campaign,
            required_string(integration_identity, "campaignIdentity", 128),
        )
    else:
        # Compatibility checkpoint briefs admitted before campaign identity
        # was carried still validate the remote base. Current local flows name
        # their arm and therefore validate the integration branch above.
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
        code_revision,
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
        subject = "integration branch" if uses_integration else "remote base"
        fail(f"{subject} diverged after the checkpoint command was witnessed")
    reference = checkpoint_ref(
        campaign, issue["number"], task_id, source_sha256, base_rev
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
    result = {"taskId": task_id, "ref": reference, "revision": base_rev}
    if capture is not None:
        result.update(
            {
                "passed": True,
                "capturePath": capture["path"],
                "stdoutTruncated": capture["stdoutTruncated"],
                "stderrTruncated": capture["stderrTruncated"],
            }
        )
    return result


def action_merge(brief: dict[str, Any]) -> dict[str, Any]:
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
    narration = narration_record(integration.get("narration"), "integration.narration")
    # The provenance pointer is the node's, from the witnessed attempt the
    # reconciler already correlates. Only a squash gets one: a merge commit
    # carries git's own template message and keeps the working commits intact.
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
    merge_commit = merge_local(
        data, config, integration, method, narration, trailer
    )
    return {
        "taskId": task_id,
        "head": head,
        "mergeCommit": merge_commit,
        "pullRequest": pull_request,
        "regated": integration["regate"],
        "ownership": ownership,
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
            "steeringRecheck",
            "steer",
            "retry",
            "escalate",
            "continue",
            "preflight",
            "prep",
            "cleanup",
            "ownership",
            "treeDelta",
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
        "steeringRecheck": action_steering_recheck,
        "steer": action_steer,
        "retry": action_retry,
        "escalate": action_escalate,
        "continue": action_continue,
        "preflight": action_preflight,
        "prep": action_prep,
        "ownership": action_ownership,
        "treeDelta": action_tree_delta,
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
