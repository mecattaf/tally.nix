#!/usr/bin/env python3
"""Run the desired-state final conformance bar against one tally.nix tree."""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
import json
from pathlib import Path
import shutil
import sys
import tempfile
import time
import traceback

from support import CASES, ConformanceFailure, Context, HarnessError, SUITE_ROOT

# Registration imports are intentionally explicit: missing a case module is a
# harness defect rather than an accidentally smaller dynamically-discovered bar.
from cases import adapters  # noqa: F401,E402
from cases import eval_manifest  # noqa: F401,E402
from cases import git_ai  # noqa: F401,E402
from cases import manifest  # noqa: F401,E402
from cases import parallel  # noqa: F401,E402
from cases import pipeline  # noqa: F401,E402
from cases import reader_state  # noqa: F401,E402
from cases import recovery  # noqa: F401,E402
from cases import registry  # noqa: F401,E402
from cases import usage  # noqa: F401,E402


@dataclass
class Result:
    case: str
    issues: list[int]
    status: str
    seconds: float
    detail: str


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", nargs="?", type=Path, help="tally.nix working tree")
    parser.add_argument("--list", action="store_true", help="list cases without building")
    parser.add_argument("--case", action="append", default=[], help="select one case ID")
    parser.add_argument("--artifacts", type=Path, help="preserve logs and report here")
    parser.add_argument("--tally", type=Path, help="prebuilt tally package or executable")
    parser.add_argument("--driver", type=Path, help="prebuilt driver package or executable")
    parser.add_argument("--presets-json", type=Path, help="pre-evaluated stock preset JSON")
    parser.add_argument("--core-test-binary", type=Path, help="prebuilt tally_core test binary")
    parser.add_argument("--n-minus-one-tally", type=Path, help="pinned N-1 tally executable")
    return parser.parse_args()


def list_cases() -> None:
    for item in sorted(CASES, key=lambda value: value.case_id):
        issues = ",".join(f"#{number}" for number in item.issues)
        suffix = " [long]" if item.long else ""
        print(f"{item.case_id:34} {issues:24} {item.description}{suffix}")


def main() -> int:
    arguments = parse_arguments()
    if arguments.list:
        list_cases()
        return 0
    if arguments.target is None:
        raise SystemExit("TARGET is required unless --list is used")
    target = arguments.target.resolve()
    if not (target / "flake.nix").is_file() or not (target / "Cargo.toml").is_file():
        raise SystemExit(f"target is not a tally.nix working tree: {target}")

    by_id = {item.case_id: item for item in CASES}
    unknown = sorted(set(arguments.case) - set(by_id))
    if unknown:
        raise SystemExit(f"unknown --case value(s): {', '.join(unknown)}")
    selected = (
        [by_id[name] for name in arguments.case]
        if arguments.case
        else sorted(CASES, key=lambda value: value.case_id)
    )

    temporary = arguments.artifacts is None
    work = (
        Path(tempfile.mkdtemp(prefix="tally-final-bar-"))
        if temporary
        else arguments.artifacts.resolve()
    )
    work.mkdir(parents=True, exist_ok=True)
    context = Context(
        target=target,
        work=work,
        tally_override=arguments.tally,
        driver_override=arguments.driver,
        presets_override=arguments.presets_json,
        core_test_binary_override=arguments.core_test_binary,
        n_minus_one_tally_override=arguments.n_minus_one_tally,
    )

    results: list[Result] = []
    print(f"final-bar: target={target}")
    print(f"final-bar: selected={len(selected)} artifacts={work}")
    for item in selected:
        print(f"RUN   {item.case_id} ({', '.join(f'#{issue}' for issue in item.issues)})", flush=True)
        started = time.monotonic()
        try:
            item.function(context)
        except ConformanceFailure as error:
            status = "FAIL"
            detail = str(error)
        except HarnessError as error:
            status = "ERROR"
            detail = str(error)
        except Exception:
            status = "ERROR"
            detail = traceback.format_exc()
        else:
            status = "PASS"
            detail = "desired-state assertion satisfied"
        elapsed = time.monotonic() - started
        results.append(Result(item.case_id, list(item.issues), status, elapsed, detail))
        print(f"{status:5} {item.case_id} {elapsed:.2f}s")
        if status != "PASS":
            for line in detail.splitlines()[-30:]:
                print(f"      {line}")

    summary = {
        status: sum(result.status == status for result in results)
        for status in ("PASS", "FAIL", "ERROR")
    }
    report = {
        "schemaVersion": 1,
        "target": str(target),
        "suite": str(SUITE_ROOT),
        "summary": summary,
        "results": [asdict(result) for result in results],
    }
    report_path = work / "report.json"
    report_path.write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(
        "final-bar: "
        + " ".join(f"{key.lower()}={value}" for key, value in summary.items())
        + f" report={report_path}"
    )
    exit_code = 2 if summary["ERROR"] else (1 if summary["FAIL"] else 0)
    if temporary:
        print("final-bar: report-json=" + json.dumps(report, sort_keys=True))
        shutil.rmtree(work, ignore_errors=True)
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
