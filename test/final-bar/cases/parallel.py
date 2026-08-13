"""Measured default-parallel tally_core verification wave (#419)."""

from __future__ import annotations

import json
from pathlib import Path
import re

from support import SUITE_ROOT, Context, case, copy_executable, make_case_directory, require


FOCUSED = (
    "daemon::tests::fleet_conformance_coordinator_switch_bumps_epoch_and_re_adopts_remote_work",
    "daemon::tests::preset_gate_defaults_distinguish_absent_manifest_from_gates_passed",
    "daemon::tests::public_continuation_uses_the_scraped_session_without_manual_captures",
)


def rust_function_body(path: Path, name: str) -> tuple[int, str]:
    function_name = name.rsplit("::", 1)[-1]
    lines = path.read_text(encoding="utf-8").splitlines()
    start = next(
        (index for index, line in enumerate(lines) if f"fn {function_name}(" in line),
        None,
    )
    require(start is not None, f"target source omits focused #419 test body {name}")
    depth = 0
    opened = False
    for end in range(start, len(lines)):
        for character in lines[end]:
            if character == "{":
                depth += 1
                opened = True
            elif character == "}":
                depth -= 1
        if opened and depth == 0:
            return start + 1, "\n".join(lines[start : end + 1])
    require(False, f"could not delimit focused #419 test body {name}")
    raise AssertionError("unreachable")


def bare_deadlines(first_line: int, body: str) -> list[int]:
    lines = body.splitlines()
    found: list[int] = []
    for index, line in enumerate(lines):
        if "tokio::time::timeout(" not in line:
            continue
        window = "\n".join(lines[index : index + 16])
        if re.search(r"\.await\s*\.unwrap\(\)", window):
            found.append(first_line + index)
    return found


@case(
    "parallel-causal-regressions",
    (419,),
    "the three named race regressions use causal barriers and pass independently",
)
def parallel_causal_regressions(context: Context) -> None:
    names = context.core_test_names()
    failures: list[str] = []
    source = context.target / "crates/tally-core/src/daemon/tests.rs"
    require(source.is_file(), f"target omits tally-core daemon tests: {source}")
    for name in FOCUSED:
        if name not in names:
            failures.append(f"missing deterministic regression {name}")
            continue
        first_line, body = rust_function_body(source, name)
        deadlines = bare_deadlines(first_line, body)
        if deadlines:
            failures.append(
                f"{name} retains wall-clock-only Elapsed unwrap(s) at lines {deadlines}; "
                "the focused case must wait on a counted causal event or explicit barrier"
            )
        result = context.run_core_test(name, timeout=240)
        if result.returncode != 0:
            failures.append(
                f"{name} failed under its focused causal probe:\n"
                + (result.stdout + result.stderr)[-4000:]
            )
    require(not failures, "focused #419 regressions failed:\n- " + "\n- ".join(failures))


def audit_default_test_parallelism(context: Context, root: Path, probe: Path) -> None:
    audit = root / "parallelism-audit"
    audit.mkdir()
    sentinel = audit / "test-binary-sentinel"
    copy_executable(SUITE_ROOT / "fixtures/parallel/test-binary-sentinel.py", sentinel)
    log = audit / "invocations.jsonl"
    environment = context.environment(FINAL_BAR_TEST_BINARY_LOG=log)
    environment.pop("RUST_TEST_THREADS", None)
    result = context.command(
        probe,
        sentinel,
        "1",
        "3",
        cwd=audit,
        env=environment,
        timeout=30,
    )
    require(result.returncode == 0, f"parallelism audit probe failed: {(result.stderr or result.stdout)[-3000:]}")
    require(log.is_file(), "parallelism audit never invoked its test-binary sentinel")
    invocations = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]
    require(len(invocations) >= 3, f"parallelism audit observed too few suite launches: {invocations!r}")
    serialized = [
        value
        for value in invocations
        if value.get("argv") or value.get("rustTestThreads") not in (None, "")
    ]
    require(
        not serialized,
        "flake-probe serialized the target test binary instead of using default parallelism: "
        + repr(serialized[:5]),
    )


@case(
    "parallel-population-wave",
    (419,),
    "the exact target test binary completes a 480-second three-lane wave with zero failures",
    long=True,
)
def parallel_population_wave(context: Context) -> None:
    root = make_case_directory(context, "parallel-population-wave")
    binary = context.core_test_binary.resolve()
    require(
        str(binary).startswith(str((context.work / "cargo-target").resolve()))
        or context.core_test_binary_override is not None,
        f"flake probe would use an untracked/stale test binary: {binary}",
    )
    probe = context.target / "test/flake-probe.sh"
    require(probe.is_file(), f"target omits measured population probe: {probe}")
    audit_default_test_parallelism(context, root, probe)
    result = context.command(probe, binary, "480", "3", cwd=root, timeout=600)
    detail = result.stdout + "\n" + result.stderr
    require(result.returncode == 0, f"flake-probe harness failed: {detail[-5000:]}")
    require(
        f"3 concurrent suites of {binary} for 480s" in detail,
        f"probe did not record the required load shape: {detail[-2000:]}",
    )
    match = re.search(r"flake-probe: (\d+) / (\d+) runs had at least one failing test", detail)
    require(match is not None, f"probe omitted its measured total: {detail[-3000:]}")
    failed, total = (int(value) for value in match.groups())
    require(total > 0, "480-second wave completed no full tally_core run")
    require(failed == 0, f"parallel population measured {failed} failing runs out of {total}")
