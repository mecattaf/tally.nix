#!/usr/bin/env python3
"""One worktree manager for both campaign drivers.

`spec_build_driver.py` and `agency_nightly_driver.py` used to carry two
incompatible implementations of the same job: create a lane, resume the one
that is already there, prove it belongs to the configured checkout, and clean
it up. They disagreed about what "already there" means and they recorded lane
identity in two different places.

This module is the single implementation, and it keeps lane identity where git
already keeps per-worktree state: `extensions.worktreeConfig` plus
`git config --worktree`. The bespoke JSON markers under
`<workspaceRoot>/.state/<runHash>/<taskId>.json` were a second copy of the
truth that could drift from the actual worktree set -- a lane could exist with
no marker, or a marker could outlive its lane. Git's own metadata cannot drift:
it is created by `git worktree add` and destroyed by `git worktree remove`, so
`git worktree list --porcelain` plus one config read per lane is a complete and
self-consistent enumeration.

Nothing here posts, publishes, or decides policy. Callers translate
`WorktreeError` into their own driver vocabulary.
"""

from __future__ import annotations

import re
import subprocess
from pathlib import Path
from typing import Any

# Identity fields are written as `tally.<key>`. Git folds a configuration key
# to lower case, so the keys are lower case here too and round-trip byte for
# byte.
IDENTITY_PREFIX = "tally."
IDENTITY_PATTERN = r"^tally\."
IDENTITY_KEY = re.compile(r"^[a-z][a-z0-9]*$")
IDENTITY_VALUE = re.compile(r"^[\x20-\x7e]{1,512}$")


class WorktreeError(RuntimeError):
    """A lane could not be created, resumed, or validated.

    `code` is one of `worktree-conflict`, `worktree-invalid`,
    `worktree-create-failed`, or `branch-invalid`. Each driver maps those onto
    the error vocabulary its own contract publishes.
    """

    def __init__(self, code: str, message: str, details: dict[str, Any] | None = None) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.details = details or {}


def _git(
    directory: Path, *arguments: str, check: bool = True, code: str = "worktree-invalid"
) -> subprocess.CompletedProcess[str]:
    command = ["git", "-C", str(directory), *arguments]
    try:
        result = subprocess.run(
            command,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as error:
        raise WorktreeError(code, f"cannot execute git: {error}", {"command": command}) from error
    if check and result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no output"
        raise WorktreeError(
            code,
            f"git {' '.join(arguments)} exited {result.returncode}: {detail}",
            {"command": command},
        )
    return result


def validate_identity(identity: dict[str, str]) -> dict[str, str]:
    """Refuse an identity git could not store and hand back unchanged."""
    validated: dict[str, str] = {}
    for key, value in identity.items():
        if not IDENTITY_KEY.fullmatch(key):
            raise WorktreeError(
                "worktree-invalid",
                f"lane identity key {key!r} is not a safe git configuration key",
                {"key": key},
            )
        if not isinstance(value, str) or not IDENTITY_VALUE.fullmatch(value):
            raise WorktreeError(
                "worktree-invalid",
                f"lane identity value for {key!r} is not storable in git configuration",
                {"key": key},
            )
        validated[key] = value
    return validated


def check_branch_name(branch: str) -> None:
    try:
        result = subprocess.run(
            ["git", "check-ref-format", "--branch", branch],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as error:
        raise WorktreeError("branch-invalid", f"cannot execute git: {error}") from error
    if result.returncode != 0:
        raise WorktreeError(
            "branch-invalid", f"branch name {branch!r} is not a valid git branch", {"branch": branch}
        )


def enable_worktree_config(checkout: Path) -> None:
    """Turn on git's per-worktree configuration file, once, idempotently.

    The extension is repository-wide and additive: it only decides whether
    `$GIT_DIR/worktrees/<lane>/config.worktree` is read. A repository that
    already has it on is left alone rather than rewritten on every pass.
    """
    current = _git(checkout, "config", "--get", "extensions.worktreeConfig", check=False)
    if current.stdout.strip().casefold() == "true":
        return
    _git(checkout, "config", "extensions.worktreeConfig", "true")


def parse_worktrees(checkout: Path) -> list[dict[str, str]]:
    """The porcelain worktree list, one record per worktree."""
    records: list[dict[str, str]] = []
    current: dict[str, str] = {}
    for line in _git(checkout, "worktree", "list", "--porcelain").stdout.splitlines():
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


def read_identity(worktree: Path) -> dict[str, str]:
    """Every `tally.*` field git holds for this lane, or an empty mapping."""
    result = _git(
        worktree,
        "config",
        "--worktree",
        "-z",
        "--get-regexp",
        IDENTITY_PATTERN,
        check=False,
    )
    if result.returncode != 0:
        return {}
    identity: dict[str, str] = {}
    for entry in result.stdout.split("\0"):
        if not entry:
            continue
        key, _, value = entry.partition("\n")
        if key.startswith(IDENTITY_PREFIX):
            identity[key[len(IDENTITY_PREFIX) :]] = value
    return identity


def write_identity(worktree: Path, identity: dict[str, str]) -> None:
    for key, value in validate_identity(identity).items():
        _git(worktree, "config", "--worktree", f"{IDENTITY_PREFIX}{key}", value)


def registered(checkout: Path, worktree: Path) -> dict[str, str] | None:
    """The porcelain record git holds for this exact path, if it holds one."""
    target = worktree.resolve()
    for record in parse_worktrees(checkout):
        raw = record.get("worktree")
        if raw and Path(raw).resolve() == target:
            return record
    return None


def lanes(checkout: Path) -> list[dict[str, Any]]:
    """Enumerate every registered worktree together with its lane identity.

    This is the single enumeration: the worktree set comes from git and the
    identity comes from git, so there is no second store to fall out of step
    with. A lane whose directory has been deleted underneath git reports an
    empty identity and is left for `prune`.
    """
    enumerated: list[dict[str, Any]] = []
    for record in parse_worktrees(checkout):
        raw = record.get("worktree")
        if not raw:
            continue
        path = Path(raw)
        branch = record.get("branch", "").removeprefix("refs/heads/")
        identity = read_identity(path) if path.is_dir() else {}
        enumerated.append(
            {
                "worktree": path,
                "branch": branch or None,
                "head": record.get("HEAD"),
                "detached": "detached" in record,
                "identity": identity,
            }
        )
    return enumerated


def same_repository(checkout: Path, worktree: Path) -> bool:
    """Does this worktree belong to the configured checkout's repository?"""
    checkout_common = _git(checkout, "rev-parse", "--git-common-dir").stdout.strip()
    worktree_common = _git(worktree, "rev-parse", "--git-common-dir").stdout.strip()
    return (checkout / checkout_common).resolve() == (worktree / worktree_common).resolve()


def current_branch(worktree: Path) -> str:
    """The lane's checked-out branch, or the empty string when detached."""
    return _git(worktree, "branch", "--show-current").stdout.strip()


def branch_exists(checkout: Path, branch: str) -> bool:
    return (
        _git(
            checkout, "show-ref", "--verify", "--quiet", f"refs/heads/{branch}", check=False
        ).returncode
        == 0
    )


def resume(checkout: Path, worktree: Path, expected: dict[str, str]) -> dict[str, str] | None:
    """Adopt the lane already sitting at this path, or report there is none.

    Returns the lane's full recorded identity when it is this caller's lane,
    and `None` when the path is free. Anything else in the way is a conflict
    rather than something to clobber: that refusal is the whole reason a lane
    is safe to resume after its runner was killed.

    A lane that git registered but that carries no identity yet -- the runner
    died between `git worktree add` and the first `git config --worktree` -- is
    healed rather than rejected. Its path and branch are derived from the same
    identity the caller is asking for, so there is no other lane it could be.
    """
    expected = validate_identity(expected)
    enable_worktree_config(checkout)
    record = registered(checkout, worktree)
    if record is None:
        if worktree.exists():
            raise WorktreeError(
                "worktree-conflict",
                f"path {worktree} exists but is not a worktree of {checkout}",
                {"worktreePath": str(worktree)},
            )
        return None
    if not worktree.is_dir():
        # Git still lists a lane whose directory was deleted underneath it.
        # Prune it and let the caller create the lane again.
        prune(checkout)
        return None
    if not same_repository(checkout, worktree):
        raise WorktreeError(
            "worktree-conflict",
            f"worktree {worktree} is not a worktree of the configured checkout",
            {"worktreePath": str(worktree)},
        )
    recorded = read_identity(worktree)
    mismatched = sorted(
        key for key, value in expected.items() if key in recorded and recorded[key] != value
    )
    if mismatched:
        raise WorktreeError(
            "worktree-conflict",
            f"worktree {worktree} carries a different lane identity: {', '.join(mismatched)}",
            {
                "worktreePath": str(worktree),
                "expected": {key: expected[key] for key in mismatched},
                "recorded": {key: recorded[key] for key in mismatched},
            },
        )
    branch = expected.get("branch")
    if branch is not None:
        actual = current_branch(worktree)
        if actual == "":
            if branch_exists(checkout, branch):
                _git(worktree, "switch", branch)
            else:
                _git(worktree, "switch", "-c", branch)
        elif actual != branch:
            raise WorktreeError(
                "worktree-conflict",
                f"worktree {worktree} is on branch {actual!r}, expected {branch!r}",
                {
                    "worktreePath": str(worktree),
                    "expectedBranch": branch,
                    "actualBranch": actual,
                },
            )
    if any(recorded.get(key) != value for key, value in expected.items()):
        write_identity(worktree, expected)
        recorded = read_identity(worktree)
    return recorded


def create(
    checkout: Path, worktree: Path, branch: str, start_rev: str, identity: dict[str, str]
) -> dict[str, str]:
    """Create one lane and record its identity in git's own worktree config.

    An existing branch of the same name is adopted rather than refused: the
    branch name is derived from the lane identity, so the only thing it can be
    is this lane's own work outliving a removed worktree.
    """
    identity = validate_identity(identity)
    check_branch_name(branch)
    enable_worktree_config(checkout)
    worktree.parent.mkdir(parents=True, exist_ok=True)
    arguments = ["worktree", "add"]
    if not branch_exists(checkout, branch):
        arguments.extend(["-b", branch])
        arguments.extend([str(worktree), start_rev])
    else:
        arguments.extend([str(worktree), branch])
    _git(checkout, *arguments, code="worktree-create-failed")
    write_identity(worktree, identity)
    return read_identity(worktree)


def remove(checkout: Path, worktree: Path, branch: str | None) -> None:
    """Delete one lane and its branch, tolerating a lane already half gone."""
    removed = _git(checkout, "worktree", "remove", "--force", str(worktree), check=False)
    if removed.returncode != 0 and worktree.exists():
        detail = removed.stderr.strip() or removed.stdout.strip() or "no output"
        raise WorktreeError(
            "worktree-invalid",
            f"cannot remove worktree {worktree}: {detail}",
            {"worktreePath": str(worktree)},
        )
    if branch is None:
        return
    deleted = _git(checkout, "branch", "-D", branch, check=False)
    if deleted.returncode != 0 and branch_exists(checkout, branch):
        detail = deleted.stderr.strip() or deleted.stdout.strip() or "no output"
        raise WorktreeError(
            "worktree-invalid",
            f"cannot remove branch {branch!r}: {detail}",
            {"branch": branch},
        )


def prune(checkout: Path) -> None:
    _git(checkout, "worktree", "prune", check=False)
