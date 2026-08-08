"""Durable-census and semantic usage evidence cases (#402/#403/#408/#409)."""

from __future__ import annotations

import json
from pathlib import Path
import uuid
from typing import Any

from support import (
    SUITE_ROOT,
    ConformanceFailure,
    Context,
    case,
    make_case_directory,
    require,
)


DECLARED_CODEX = [
    "cacheReadTokens",
    "cacheWriteTokens",
    "inputTokensWithCacheRead",
    "outputTokens",
    "reasoningTokens",
]


def shell_config(context: Context) -> dict[str, Any]:
    return {
        "enqueue": {"requireDedupKey": False},
        "pools": {"stock": {"resource": "slot", "capacity": 4}},
        "adapters": {"shell": context.presets["shell"]},
    }


def result_json(result: Any, context: str) -> dict[str, Any]:
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ConformanceFailure(
            f"{context} did not emit JSON: {error}; stdout={result.stdout!r}; stderr={result.stderr!r}"
        ) from error
    require(isinstance(value, dict), f"{context} output must be an object: {value!r}")
    return value


def job_projection(daemon: Any, task: str) -> dict[str, Any]:
    result = daemon.tally("query", "job", task)
    require(result.returncode == 0, f"query job failed: {(result.stderr or result.stdout)[-1000:]}")
    value = result_json(result, "query job")
    job = value.get("job", value)
    require(isinstance(job, dict), f"query job omitted job object: {value!r}")
    return job


def create_rows(
    context: Context,
    root: Path,
    flow_run_id: str,
    attempts: list[int],
) -> tuple[Path, Path, list[dict[str, Any]]]:
    identities: list[dict[str, Any]] = []
    with context.daemon(root / "daemon", shell_config(context)) as daemon:
        for ordinal, count in enumerate(attempts):
            task_ref = f"usage/task-{ordinal}"
            orchestration = json.dumps(
                {
                    "flowRunId": flow_run_id,
                    "nodeOrdinal": ordinal,
                    "taskRef": task_ref,
                    "maxNodes": len(attempts),
                },
                separators=(",", ":"),
            )
            program = "false" if count > 1 else "true"
            submitted = daemon.tally(
                "enqueue",
                "--pool",
                "stock",
                "--adapter",
                "shell",
                "--orchestration",
                orchestration,
                "--evidence",
                "exit:0",
                "--wait",
                "--",
                "/bin/sh",
                "-c",
                program,
                timeout=60,
            )
            value = result_json(submitted, "enqueue")
            task = value.get("task_uuid") or value.get("taskUuid")
            require(isinstance(task, str), f"enqueue omitted task UUID: {value!r}")
            per_attempt: list[dict[str, Any]] = []
            first = job_projection(daemon, task)
            per_attempt.append(
                {
                    "attempt": int(first.get("attempt", 1)),
                    "leaseEpoch": int(first.get("leaseEpoch", 1)),
                }
            )
            for _ in range(1, count):
                retried = daemon.tally("queue", "retry", task)
                require(
                    retried.returncode == 0,
                    f"queue retry failed: {(retried.stderr or retried.stdout)[-1000:]}",
                )
                settled = daemon.tally("queue", "await-job", task, timeout=60)
                # A terminal failed job may make the CLI non-zero; its JSON is
                # still the public terminal result and the row must persist.
                require(settled.stdout.strip(), f"await-job emitted no terminal result: {settled.stderr}")
                projected = job_projection(daemon, task)
                per_attempt.append(
                    {
                        "attempt": int(projected.get("attempt", len(per_attempt) + 1)),
                        "leaseEpoch": int(projected.get("leaseEpoch", len(per_attempt) + 1)),
                    }
                )
            identities.append(
                {
                    "task": task,
                    "taskRef": task_ref,
                    "attempts": per_attempt,
                }
            )
        state = daemon.state
        data = daemon.data
    return state, data, identities


def reset_attestations(data: Path) -> Path:
    ledger = data / "attestations.jsonl"
    if ledger.exists():
        ledger.rename(data / "attestations.original.jsonl")
    return ledger


def append(context: Context, ledger: Path, payload: dict[str, Any]) -> dict[str, Any]:
    result = context.command(
        context.tally,
        "witness",
        "append",
        "--ledger",
        ledger,
        "--payload",
        json.dumps(payload, separators=(",", ":")),
    )
    require(
        result.returncode == 0,
        f"public attestation append failed: {(result.stderr or result.stdout)[-1400:]}",
    )
    return result_json(result, "witness append")


def query_run(context: Context, state: Path, data: Path, flow: str) -> dict[str, Any]:
    result = context.command(
        context.tally,
        "query",
        "run",
        flow,
        "--json",
        "--durable",
        "--state-dir",
        state,
        "--data-dir",
        data,
        timeout=60,
    )
    require(
        result.returncode == 0,
        f"durable query run failed: {(result.stderr or result.stdout)[-1800:]}",
    )
    return result_json(result, "query run")


def identity_payload(identity: dict[str, Any], attempt_index: int) -> dict[str, Any]:
    attempt = identity["attempts"][attempt_index]
    return {
        "kind": "adapter-scrape",
        "taskUuid": identity["task"],
        "jobId": identity["task"],
        "adapter": "fixture",
        "attempt": attempt["attempt"],
        "leaseEpoch": attempt["leaseEpoch"],
        "captures": {},
        "usageAuthority": "advisory-only",
    }


def observation(
    *,
    input_tokens: int | None = None,
    input_reported: int | None = None,
    cache_read: int | None = None,
    cache_write: int | None = None,
    output: int | None = None,
    reasoning: int | None = None,
    total: int | None = None,
    total_source: str = "derived-from-components",
    cost: float | None = None,
) -> dict[str, Any]:
    breakdown: dict[str, Any] = {"shape": "components"}
    values = {
        "inputTokens": input_tokens,
        "inputTokensAsReported": input_reported,
        "cacheReadTokens": cache_read,
        "cacheWriteTokens": cache_write,
        "outputTokens": output,
        "reasoningTokens": reasoning,
    }
    for key, value in values.items():
        if value is not None:
            breakdown[key] = value
    if total is not None:
        breakdown["totalTokens"] = {"value": total, "source": total_source}
    if cost is not None:
        breakdown["cost"] = {"amount": cost, "currency": "USD"}
    if all(value is None for value in (input_tokens, cache_read, cache_write, output, reasoning)):
        breakdown["shape"] = "lump"
    return {"state": "reported", "breakdown": breakdown}


def evidence(
    declared: list[str],
    scope: str,
    derivation: str,
    contribution: dict[str, Any] | None,
    *,
    lineage: dict[str, Any] | None = None,
    predecessor: dict[str, Any] | None = None,
) -> dict[str, Any]:
    value: dict[str, Any] = {
        "schemaVersion": 1,
        "declaredFields": sorted(set(declared)),
        "counterScope": scope,
        "derivation": derivation,
    }
    if lineage is not None:
        value["lineage"] = lineage
    if predecessor is not None:
        value["predecessor"] = predecessor
    if contribution is not None:
        value["contribution"] = contribution
    return value


def caveats(usage: dict[str, Any]) -> set[str]:
    return {str(item) for item in usage.get("caveats", [])}


@case(
    "usage-attempt-census",
    (402,),
    "a durable attempt-3 row with only attempt 3 attested reports two missing attempts",
)
def usage_attempt_census(context: Context) -> None:
    root = make_case_directory(context, "usage-attempt-census")
    flow = str(uuid.UUID("019f1000-0000-7000-8000-000000000402"))
    state, data, identities = create_rows(context, root, flow, [3])
    ledger = reset_attestations(data)
    payload = identity_payload(identities[0], 2)
    payload["usage"] = {"state": "not-declared"}
    payload["usageEvidence"] = evidence([], "attempt", "attempt", {"state": "not-declared"})
    append(context, ledger, payload)
    view = query_run(context, state, data, flow)
    usage = view.get("usage", {})
    coverage = usage.get("coverage", {})
    expected = {
        "tasks": 1,
        "attemptsExpected": 3,
        "attemptsObserved": 1,
        "attemptsMissingAttestation": 2,
        "tasksWithoutAttestation": 0,
        "ledgerVerified": True,
    }
    failures = [
        f"coverage.{key}={coverage.get(key)!r}, expected {value!r}"
        for key, value in expected.items()
        if coverage.get(key) != value
    ]
    if "attempts-missing-attestation" not in caveats(usage):
        failures.append(f"missing attempts-missing-attestation caveat: {usage.get('caveats')!r}")
    if usage.get("isComplete") is not False:
        failures.append(f"usage.isComplete={usage.get('isComplete')!r}, expected false")
    require(not failures, "attempt census failures:\n- " + "\n- ".join(failures))


@case(
    "usage-codex-cumulative-delta",
    (403,),
    "recorded Codex fresh/resumed checkpoints contribute zero-baseline plus verified delta",
)
def usage_codex_cumulative_delta(context: Context) -> None:
    root = make_case_directory(context, "usage-codex-cumulative-delta")
    corpus = json.loads((SUITE_ROOT / "fixtures/usage/corpus.json").read_text(encoding="utf-8"))
    flow = str(uuid.UUID("019f1000-0000-7000-8000-000000000403"))
    state, data, identities = create_rows(context, root, flow, [2])
    identity = identities[0]
    ledger = reset_attestations(data)
    lineage = {"adapter": "codex", "sessionRef": corpus["probe"]["lineage"]}
    fresh_usage = observation(
        input_tokens=5042,
        input_reported=16050,
        cache_read=11008,
        cache_write=0,
        output=5,
        reasoning=0,
        total=16055,
    )
    fresh = identity_payload(identity, 0)
    fresh["adapter"] = "codex"
    fresh["captures"] = {"sessionRef": lineage["sessionRef"], "usage": corpus["probe"]["fresh"]}
    fresh["usage"] = fresh_usage
    fresh["usageEvidence"] = evidence(
        DECLARED_CODEX,
        "session-cumulative",
        "fresh-zero",
        fresh_usage,
        lineage=lineage,
    )
    first_record = append(context, ledger, fresh)

    resumed_raw = observation(
        input_tokens=10101,
        input_reported=32117,
        cache_read=22016,
        cache_write=0,
        output=11,
        reasoning=0,
        total=32128,
    )
    delta = observation(
        input_tokens=5059,
        input_reported=16067,
        cache_read=11008,
        cache_write=0,
        output=6,
        reasoning=0,
        total=16073,
    )
    resumed = identity_payload(identity, 1)
    resumed["adapter"] = "codex"
    resumed["captures"] = {"sessionRef": lineage["sessionRef"], "usage": corpus["probe"]["resumed"]}
    resumed["usage"] = resumed_raw
    resumed["usageEvidence"] = evidence(
        DECLARED_CODEX,
        "session-cumulative",
        "delta",
        delta,
        lineage=lineage,
        predecessor={
            "taskUuid": identity["task"],
            "attempt": identity["attempts"][0]["attempt"],
            "leaseEpoch": identity["attempts"][0]["leaseEpoch"],
            "sequence": first_record["seq"],
            "hash": first_record["hash"],
        },
    )
    append(context, ledger, resumed)
    view = query_run(context, state, data, flow)
    usage = view.get("usage", {})
    tokens = usage.get("tokens", {})
    expected = corpus["probe"]["normalizedRun"]
    failures: list[str] = []
    for field, amount in expected.items():
        actual = tokens.get(field)
        if isinstance(actual, dict):
            actual = actual.get("value")
        if actual != amount:
            failures.append(f"tokens.{field}={actual!r}, expected {amount}")
    if usage.get("coverage", {}).get("attemptsExpected") != 2:
        failures.append(f"expected two census attempts: {usage.get('coverage')!r}")
    if usage.get("isComplete") is not True:
        failures.append(f"verified delta rollup is not complete: {usage!r}")
    raw_wrong = corpus["probe"]["fresh"]["input_tokens"] + corpus["probe"]["resumed"]["input_tokens"]
    input_reported = tokens.get("inputTokensAsReported")
    if isinstance(input_reported, dict):
        input_reported = input_reported.get("value")
    if input_reported == raw_wrong:
        failures.append("rollup summed the two cumulative input checkpoints and double-charged")

    missing_flow = str(uuid.UUID("019f1000-0000-7000-8000-000000004003"))
    missing_state, missing_data, missing_identities = create_rows(
        context,
        make_case_directory(context, "usage-codex-missing"),
        missing_flow,
        [1],
    )
    missing_ledger = reset_attestations(missing_data)
    missing_raw = observation(
        input_tokens=42,
        input_reported=142,
        cache_read=100,
        cache_write=0,
        output=8,
        reasoning=0,
        total=150,
    )
    missing_payload = identity_payload(missing_identities[0], 0)
    missing_payload["adapter"] = "codex"
    missing_payload["usage"] = missing_raw
    missing_payload["usageEvidence"] = evidence(
        DECLARED_CODEX,
        "session-cumulative",
        "baseline-missing",
        None,
        lineage={"adapter": "codex", "sessionRef": "missing-predecessor-fixture"},
    )
    append(context, missing_ledger, missing_payload)
    missing_view = query_run(context, missing_state, missing_data, missing_flow)
    missing_usage = missing_view.get("usage", {})
    if "cumulative-baseline-missing" not in caveats(missing_usage):
        failures.append(
            "missing-predecessor cumulative evidence omitted cumulative-baseline-missing caveat"
        )
    if missing_usage.get("isComplete") is not False:
        failures.append("missing-predecessor cumulative evidence was graded complete")
    missing_tokens = missing_usage.get("tokens", {})
    for field in ("inputTokensAsReported", "inputTokens", "cacheReadTokens", "outputTokens"):
        amount = missing_tokens.get(field, 0)
        if isinstance(amount, dict):
            amount = amount.get("value", 0)
        if amount not in (None, 0):
            failures.append(
                f"missing-predecessor cumulative checkpoint charged tokens.{field}={amount!r}"
            )
    require(not failures, "Codex cumulative evidence failures:\n- " + "\n- ".join(failures))


@case(
    "usage-declared-surfaces",
    (408,),
    "field completeness follows dispatch-time declarations across heterogeneous adapters",
)
def usage_declared_surfaces(context: Context) -> None:
    root = make_case_directory(context, "usage-declared-surfaces")
    corpus = json.loads((SUITE_ROOT / "fixtures/usage/corpus.json").read_text(encoding="utf-8"))
    flow = str(uuid.UUID("019f1000-0000-7000-8000-000000000408"))
    state, data, identities = create_rows(context, root, flow, [1, 1, 1, 1])
    ledger = reset_attestations(data)
    rows = [
        (
            ["inputTokens", "outputTokens"],
            observation(input_tokens=7, input_reported=7, output=3, total=10),
        ),
        (["costUsd"], observation(cost=1.25)),
        (
            ["totalTokens"],
            observation(total=12, total_source="harness-reported"),
        ),
        (
            ["inputTokens", "cacheReadTokens", "cacheWriteTokens", "outputTokens", "totalTokens"],
            observation(total=100, total_source="harness-reported"),
        ),
    ]
    for identity, (declared, observed) in zip(identities, rows):
        payload = identity_payload(identity, 0)
        payload["usage"] = observed
        payload["usageEvidence"] = evidence(declared, "attempt", "attempt", observed)
        append(context, ledger, payload)
    view = query_run(context, state, data, flow)
    usage = view.get("usage", {})
    coverage = usage.get("coverage", {})
    failures: list[str] = []
    for field in ("declaredByField", "reportedByField"):
        expected = corpus["declaredSurface"][field]
        if coverage.get(field) != expected:
            failures.append(f"coverage.{field}={coverage.get(field)!r}, expected {expected!r}")
    current_caveats = caveats(usage)
    if "partial-components" not in current_caveats:
        failures.append(f"drifted declared components were not caveated: {current_caveats!r}")
    if "partial-fresh-input" in current_caveats:
        # The two-field adapter declares input only; missing cache-write is not
        # a defect on that attempt. The drift task may name its missing fields,
        # but must not collapse that into the old universal-pair inference.
        failures.append("legacy universal partial-fresh-input inference fired")
    missing = coverage.get("missingDeclaredFields", [])
    for field in ("inputTokens", "cacheReadTokens", "cacheWriteTokens", "outputTokens"):
        if field not in missing:
            failures.append(f"declared drift did not name missing field {field}: {missing!r}")
    if usage.get("isComplete") is not False:
        failures.append("components-plus-total drift graded complete")
    require(not failures, "declared-surface failures:\n- " + "\n- ".join(failures))


@case(
    "usage-legacy-total-only",
    (409,),
    "two ambiguous total-only legacy attempts beside a component attempt retain the legacy caveat",
)
def usage_legacy_total_only(context: Context) -> None:
    root = make_case_directory(context, "usage-legacy-total-only")
    flow = str(uuid.UUID("019f1000-0000-7000-8000-000000000409"))
    state, data, identities = create_rows(context, root, flow, [1, 1, 1])
    ledger = reset_attestations(data)
    observations = [
        observation(input_tokens=4, input_reported=4, output=1, total=5),
        observation(total=9, total_source="harness-reported"),
        observation(total=11, total_source="harness-reported"),
    ]
    for identity, observed in zip(identities, observations):
        payload = identity_payload(identity, 0)
        payload["usage"] = observed
        # Deliberately no usageEvidence: these are literal legacy records.
        append(context, ledger, payload)
    view = query_run(context, state, data, flow)
    usage = view.get("usage", {})
    current = caveats(usage)
    failures: list[str] = []
    for expected in ("total-only-attempts", "declared-surface-unknown"):
        if expected not in current:
            failures.append(f"legacy rollup omitted {expected}: {current!r}")
    docs = "\n".join(
        path.read_text(encoding="utf-8", errors="replace")
        for path in (
            context.target / "crates/tally-core/src/usage_rollup.rs",
            context.target / "doc/src/operating/observability.md",
        )
        if path.is_file()
    ).lower()
    for phrase in ("legacy", "reported-shape", "declared-surface-unknown"):
        if phrase not in docs:
            failures.append(f"public/source contract wording omits {phrase!r}")
    retired = "drift cannot hide behind a stated total"
    if retired in docs:
        failures.append(f"retired certainty claim remains: {retired!r}")
    require(not failures, "legacy usage failures:\n- " + "\n- ".join(failures))
