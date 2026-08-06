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

import hashlib
import json
import os
import re
import shutil
import subprocess
from pathlib import Path
from typing import Any

# The file the change-set snapshot lives in, alongside `config.worktree`: both
# are per-worktree git-dir state, both are irrelevant outside the lane's own
# lifecycle, and neither is ever a campaign artifact a reader sees.
CHANGE_SET_SNAPSHOT_NAME = "tally-changeset-snapshot.json"

# Identity fields are written as `tally.<key>`. Git folds a configuration key
# to lower case, so the keys are lower case here too and round-trip byte for
# byte.
IDENTITY_SECTION = "tally"
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


def worktree_config_path(worktree: Path) -> Path:
    """The file `git config --worktree` writes for this linked worktree."""
    git_dir = Path(_git(worktree, "rev-parse", "--absolute-git-dir").stdout.strip())
    common_dir = Path(
        _git(
            worktree, "rev-parse", "--path-format=absolute", "--git-common-dir"
        ).stdout.strip()
    )
    if git_dir.resolve() == common_dir.resolve():
        # In the main worktree `git config --worktree` acts on the shared
        # config, so writing `config.worktree` there would record identity in a
        # file that means something different from what a read would return. No
        # lane is ever the main worktree; refuse rather than diverge.
        raise WorktreeError(
            "worktree-invalid",
            f"{worktree} is the main worktree and cannot carry lane identity",
            {"worktreePath": str(worktree)},
        )
    return git_dir / "config.worktree"


def write_identity(worktree: Path, identity: dict[str, str]) -> None:
    """Record a lane's whole identity in one atomic act.

    One `git config --worktree` call per field is one crash window per field,
    and a lane that survives with some of its identity looks valid to every
    later pass while being unable to answer for the fields it lost. The marker
    file this replaced was written with a single `os.replace`; that property is
    restored here without leaving git's own metadata: the replacement file is
    built with `git config --file` -- so git does the escaping, and any foreign
    per-worktree key already present survives -- and then renamed into place.
    """
    identity = validate_identity(identity)
    path = worktree_config_path(worktree)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    try:
        temporary.unlink(missing_ok=True)
        if path.exists():
            shutil.copyfile(path, temporary)
            _git(
                worktree,
                "config",
                "--file",
                str(temporary),
                "--remove-section",
                IDENTITY_SECTION,
                check=False,
            )
        else:
            temporary.touch(mode=0o644)
        for key, value in identity.items():
            _git(worktree, "config", "--file", str(temporary), f"{IDENTITY_PREFIX}{key}", value)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


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


def resume(
    checkout: Path,
    worktree: Path,
    expected: dict[str, str],
    *,
    required: tuple[str, ...] = (),
) -> dict[str, Any] | None:
    """Adopt the lane already sitting at this path, or report there is none.

    Returns `None` when the path is free. Otherwise returns

        {"identity": <recorded>, "complete": <bool>, "head": <resolved HEAD>}

    Anything else in the way is a conflict rather than something to clobber:
    that refusal is the whole reason a lane is safe to resume after its runner
    was killed.

    `complete` is false when the recorded identity is missing any key of
    `expected` or `required` -- a lane an older tally created before identity
    moved into git, or one whose runner died before the identity was written.
    Such a lane is reported, never invented: writing back only the keys this
    function happens to hold would make the lane look valid to every later pass
    while leaving it permanently unable to answer for a field its caller
    requires. The caller re-derives what it needs from the lane itself -- its
    `head` is returned for exactly that -- and records a complete identity.
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
    missing = sorted((set(expected) | set(required)) - set(recorded))
    return {
        "identity": recorded,
        "complete": not missing,
        "missing": missing,
        "head": _git(worktree, "rev-parse", "--verify", "HEAD^{commit}").stdout.strip(),
    }


def add(checkout: Path, worktree: Path, branch: str, start_rev: str) -> str:
    """Cut one lane and report the commit it actually ended up on.

    An existing branch of the same name is adopted rather than refused: the
    branch name is derived from the lane identity, so the only thing it can be
    is this lane's own work outliving a removed worktree. That is also why the
    resolved head is returned rather than assumed to be `start_rev` -- an
    adopted branch sits wherever the killed runner left it, and every value
    derived from the lane's position has to be derived from *that*.
    """
    check_branch_name(branch)
    enable_worktree_config(checkout)
    worktree.parent.mkdir(parents=True, exist_ok=True)
    if branch_exists(checkout, branch):
        _git(checkout, "worktree", "add", str(worktree), branch, code="worktree-create-failed")
    else:
        _git(
            checkout,
            "worktree",
            "add",
            "-b",
            branch,
            str(worktree),
            start_rev,
            code="worktree-create-failed",
        )
    return _git(worktree, "rev-parse", "--verify", "HEAD^{commit}").stdout.strip()


def create(
    checkout: Path, worktree: Path, branch: str, start_rev: str, identity: dict[str, str]
) -> dict[str, str]:
    """Cut one lane and record its identity, for a caller that derives nothing.

    A caller whose identity depends on where the lane actually landed calls
    `add` and `write_identity` itself, so that the identity it commits is the
    one the lane can answer for.
    """
    identity = validate_identity(identity)
    add(checkout, worktree, branch, start_rev)
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


def change_set_fingerprint(worktree: Path) -> dict[str, str]:
    """A path -> sha256 content digest for every tracked and untracked file.

    Comparable before and after an agent node runs (#386): a path's digest
    changing, a path disappearing, or a path appearing are all detectable
    this way, regardless of whether the change was ever committed -- a
    reversion of an uncommitted change back to its prior content shows up
    exactly the same as a forward edit, which a commit-history diff cannot
    see at all. Ignored paths never enter it, matching what a commit could
    ever contain. This is one fingerprint of whatever is on disk right now;
    nothing here reads history or watches writes as they happen.
    """
    listed = _git(
        worktree, "ls-files", "--cached", "--others", "--exclude-standard", "-z"
    ).stdout
    fingerprint: dict[str, str] = {}
    for relative in (path for path in listed.split("\0") if path):
        digest = hashlib.sha256()
        try:
            with (worktree / relative).open("rb") as handle:
                for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                    digest.update(chunk)
        except OSError:
            # Listed by git, gone by the time this reads it: the same fact a
            # second listing after the read would report, so it is dropped
            # rather than failing a fingerprint over a benign race.
            continue
        fingerprint[relative] = digest.hexdigest()
    return fingerprint


def change_set_snapshot_path(worktree: Path) -> Path:
    git_dir = Path(_git(worktree, "rev-parse", "--absolute-git-dir").stdout.strip())
    return git_dir / CHANGE_SET_SNAPSHOT_NAME


def write_change_set_snapshot(worktree: Path, fingerprint: dict[str, str]) -> None:
    """Persist one fingerprint atomically, the same way `write_identity` does."""
    path = change_set_snapshot_path(worktree)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    try:
        temporary.write_text(json.dumps(fingerprint, sort_keys=True), encoding="utf-8")
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def read_change_set_snapshot(worktree: Path) -> dict[str, str] | None:
    """The fingerprint written before this lane's agent node ran, or `None`."""
    path = change_set_snapshot_path(worktree)
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None


def clear_change_set_snapshot(worktree: Path) -> None:
    change_set_snapshot_path(worktree).unlink(missing_ok=True)


def change_set_delta(
    before: dict[str, str], after: dict[str, str]
) -> list[dict[str, str]]:
    """Every path whose content differs between the two fingerprints (#386).

    Appearing, disappearing, and changing are reported uniformly -- the
    caller decides which of them are authorized; this module only reports
    facts, exactly as its module docstring promises.
    """
    deltas: list[dict[str, str]] = []
    for path in sorted(set(before) | set(after)):
        before_hash = before.get(path)
        after_hash = after.get(path)
        if before_hash == after_hash:
            continue
        kind = (
            "appeared"
            if before_hash is None
            else "disappeared" if after_hash is None else "changed"
        )
        deltas.append({"path": path, "kind": kind})
    return deltas
