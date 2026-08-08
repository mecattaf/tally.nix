"""Measured default-parallel tally_core verification wave (#419)."""

from __future__ import annotations

import re

from support import Context, case, make_case_directory, require


FOCUSED = (
    "daemon::tests::confirmed_pool_loss_witnesses_and_return_re_presents_the_same_row",
    "daemon::tests::fleet_conformance_coordinator_switch_bumps_epoch_and_re_adopts_remote_work",
    "daemon::tests::preset_gate_defaults_distinguish_absent_manifest_from_gates_passed",
    "daemon::tests::public_continuation_uses_the_scraped_session_without_manual_captures",
)


@case(
    "parallel-causal-regressions",
    (419,),
    "the four named causal race regressions exist and pass independently",
)
def parallel_causal_regressions(context: Context) -> None:
    names = context.core_test_names()
    failures: list[str] = []
    for name in FOCUSED:
        if name not in names:
            failures.append(f"missing deterministic regression {name}")
            continue
        result = context.run_core_test(name, timeout=240)
        if result.returncode != 0:
            failures.append(
                f"{name} failed under its focused causal probe:\n"
                + (result.stdout + result.stderr)[-4000:]
            )
    require(not failures, "focused #419 regressions failed:\n- " + "\n- ".join(failures))


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
