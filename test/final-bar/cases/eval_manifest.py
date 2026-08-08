"""Mechanical eval-manifest outcome contract (#426)."""

from __future__ import annotations

from pathlib import Path

from support import SUITE_ROOT, Context, case, make_case_directory, require


@case(
    "eval-manifest-zero-covered",
    (426,),
    "zero declared coverage is exit 4 with verification=none and defined precedence",
)
def eval_manifest_zero_covered(context: Context) -> None:
    make_case_directory(context, "eval-manifest-zero-covered")
    checker = context.target / "test/eval_manifest_check.py"
    require(checker.is_file(), f"public eval manifest checker is missing: {checker}")
    target_fixtures = context.target / "test/fixtures/eval-manifest"
    suite_fixtures = SUITE_ROOT / "fixtures/eval-manifest"
    all_failed = target_fixtures / "all-declared-failed.md"
    all_reused = suite_fixtures / "all-reused.md"
    mixed = suite_fixtures / "mixed-covered.md"
    undeclared = suite_fixtures / "undeclared-covered.md"
    unchecked = target_fixtures / "no-expected-surface.md"
    refused = target_fixtures / "no-manifest.md"
    valid = target_fixtures / "valid.md"
    failures: list[str] = []

    def check(paths: list[Path], expected: int, verification: str | None) -> None:
        result = context.command("python3", checker, *paths)
        label = ",".join(path.name for path in paths) or "<no args>"
        if result.returncode != expected:
            failures.append(
                f"{label}: exit {result.returncode}, expected {expected}; "
                f"output={(result.stdout + result.stderr)[-900:]!r}"
            )
        if verification is not None:
            token = f"verification={verification}"
            if token not in result.stdout:
                failures.append(f"{label}: output omitted stable token {token!r}: {result.stdout!r}")

    no_args = context.command("python3", checker)
    if no_args.returncode != 2:
        failures.append(f"no arguments: exit {no_args.returncode}, expected 2")
    check([all_failed], 4, "none")
    check([all_reused], 4, "none")
    check([undeclared], 4, "none")
    check([mixed], 0, "present")
    check([valid], 0, "present")
    check([valid, all_failed], 4, None)
    check([all_failed, unchecked], 3, None)
    check([all_failed, unchecked, refused], 1, None)

    header = checker.read_text(encoding="utf-8")[:9000]
    for text in (
        "4",
        "zero declared items",
        "verification=present",
        "verification=none",
    ):
        if text not in header:
            failures.append(f"checker contract header does not document {text!r}")
    require(not failures, "eval-manifest contract failures:\n- " + "\n- ".join(failures))
