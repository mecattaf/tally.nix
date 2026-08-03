#!/usr/bin/env python3
"""Refuse a no-op or existence-only campaign preflight probe on the Nix surface.

A command gate's `preflightArgv` is the only thing standing between a campaign
and a whole agent cycle spent discovering a broken host, and a fixture is
something operators copy. The first version of this guard matched one exact
spelling -- `[ "/bin/true" ]` -- and was therefore green while the very file it
guarded shipped `[ "true" ]` twice. This one reads the declarations instead of
one string.

Two families are refused:

* **No-op.** The probe runs a command that cannot fail: `true`, `/bin/true`,
  `/usr/bin/true`, `:`, or a shell whose whole `-c` script is one of those.
* **Existence-only.** The probe's entire `-c` script is a single existence
  test -- `test -x ...`, `command -v ...`, `which ...`, `type ...`. That is the
  shape `doc/src/flows/campaigns.md` warns about in its own words: a version or
  presence check alone proves nothing about a toolchain the gate needs. Two or
  more clauses are enough to clear it; the point is to refuse the reflex, not to
  grade the probe.

A declaration may opt out with a `no-op-probe-allowed:` comment on the same line
or within the three lines above it, stating why. That exists for negative-eval
fixtures -- configurations that are evaluated precisely to prove the module
rejects them, where no probe could ever run.

Usage: preflight_probe_drift.py <nix-file>...
"""

from __future__ import annotations

import re
import sys
from pathlib import Path, PurePosixPath

# `preflightArgv = [ ... ];` in either the one-line or the multi-line layout.
DECLARATION = re.compile(r"preflightArgv\s*=\s*\[(.*?)\];", re.DOTALL)
STRING = re.compile(r'"((?:[^"\\]|\\.)*)"')
ALLOW_MARKER = "no-op-probe-allowed:"

NO_OP_COMMANDS = {"", ":", "true", "/bin/true", "/usr/bin/true"}
SHELL_NAMES = {"sh", "bash", "dash", "ash", "ksh", "zsh"}
EXISTENCE_TESTS = ("test ", "[ ", "command -v", "which ", "type ", "hash ")


def shell_script(argv: list[str]) -> str | None:
    """The `-c` script of a shell invocation, or None if this is not one."""
    if PurePosixPath(argv[0]).name not in SHELL_NAMES:
        return None
    for index, token in enumerate(argv[1:], start=1):
        # `-c`, `-eu -c`, and the bundled `-euc` all name the same option.
        if token.startswith("-") and token.rstrip().endswith("c") and index + 1 < len(argv):
            return argv[index + 1]
    return None


def clauses(script: str) -> list[str]:
    parts = re.split(r"&&|\|\||;|\n", script)
    return [part.strip() for part in parts if part.strip()]


def verdict(argv: list[str]) -> str | None:
    """The reason this probe is refused, or None when it is acceptable."""
    if not argv:
        return "an empty probe asserts nothing"
    script = shell_script(argv)
    if script is None:
        if len(argv) == 1 and argv[0] in NO_OP_COMMANDS:
            return f"{argv[0]!r} is a command that cannot fail"
        return None
    parts = clauses(script)
    if not parts or all(part in NO_OP_COMMANDS for part in parts):
        return f"the shell script {script!r} cannot fail"
    if len(parts) == 1 and parts[0].startswith(EXISTENCE_TESTS):
        return (
            f"the whole probe is one existence test ({parts[0]!r}); probe what the "
            "gate actually needs, not merely that something is installed"
        )
    return None


def allowed(text: str, start: int) -> bool:
    """True when the declaration carries an explicit opt-out comment.

    The marker may sit on the declaration's own line or anywhere in the
    contiguous comment block immediately above it, so the reason can be as long
    as it needs to be without the length silently disarming the opt-out.
    """
    lines = text.splitlines()
    index = text.count("\n", 0, start)
    if ALLOW_MARKER in lines[index]:
        return True
    index -= 1
    while index >= 0 and lines[index].lstrip().startswith("#"):
        if ALLOW_MARKER in lines[index]:
            return True
        index -= 1
    return False


def main(paths: list[str]) -> int:
    failures: list[str] = []
    declarations = 0
    for raw in paths:
        path = Path(raw)
        text = path.read_text(encoding="utf-8")
        for match in DECLARATION.finditer(text):
            declarations += 1
            argv = [STRING.sub(lambda item: item.group(1), value) for value in STRING.findall(match.group(1))]
            reason = verdict(argv)
            if reason is None or allowed(text, match.start()):
                continue
            line = text.count("\n", 0, match.start()) + 1
            failures.append(f"{path}:{line}: {argv!r}: {reason}")
    if declarations == 0:
        print("no preflightArgv declaration was found at all", file=sys.stderr)
        return 1
    for failure in failures:
        print(failure, file=sys.stderr)
    if failures:
        print(
            f"{len(failures)} campaign preflight probe(s) prove nothing; give each one a "
            f"probe of the toolchain its gate needs, or annotate it with '{ALLOW_MARKER}'",
            file=sys.stderr,
        )
        return 1
    print(f"checked {declarations} campaign preflight probe declarations")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
