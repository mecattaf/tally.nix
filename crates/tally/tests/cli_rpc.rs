use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tally_client::{RequestFrame, WireError};
use tally_core::wire::{serve_connection, RpcHandler};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;
use tokio::process::Command;

#[path = "support/shell_program.rs"]
mod shell_program;

#[derive(Clone, Copy)]
struct CliHandler;

impl RpcHandler for CliHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            match request.method.as_str() {
                "queue.enqueue"
                    if request
                        .params
                        .as_ref()
                        .and_then(|value| value.get("pool"))
                        .and_then(Value::as_str)
                        == Some("invalid") =>
                {
                    Err(WireError::invalid("invalid pool"))
                }
                "queue.enqueue"
                    if request
                        .params
                        .as_ref()
                        .and_then(|value| value.get("pool"))
                        .and_then(Value::as_str)
                        == Some("metadata") =>
                {
                    let params = request.params.as_ref().unwrap();
                    assert_eq!(
                        params["evidenceClass"],
                        serde_json::json!({
                            "arbitrary": [true, 7, {"nested": null}]
                        })
                    );
                    assert_eq!(
                        params["manifestHash"],
                        "deliberately-not-validated://manifest value"
                    );
                    Ok(serde_json::json!({
                        "task_uuid": "00000000-0000-4000-8000-000000000002",
                        "job_id": "job-metadata"
                    }))
                }
                "queue.enqueue"
                    if request
                        .params
                        .as_ref()
                        .and_then(|value| value.get("pool"))
                        .is_some_and(Value::is_array) =>
                {
                    assert_eq!(
                        request.params.as_ref().unwrap()["pool"],
                        serde_json::json!(["slot", "zeta"])
                    );
                    assert_eq!(
                        request.params.as_ref().unwrap()["executor"],
                        serde_json::json!("worker")
                    );
                    Ok(serde_json::json!({
                        "task_uuid": "00000000-0000-4000-8000-000000000003",
                        "job_id": "job-multi",
                        "verdict": "pass"
                    }))
                }
                "lease.acquire" => {
                    assert_eq!(
                        request.params.as_ref().unwrap()["pool"],
                        serde_json::json!(["slot", "zeta"])
                    );
                    Ok(serde_json::json!({
                        "epoch": 1,
                        "outcome": {"granted": {"leaseId": "lease-multi", "pools": ["slot", "zeta"]}}
                    }))
                }
                "queue.enqueue" => Ok(serde_json::json!({
                    "task_uuid": "00000000-0000-4000-8000-000000000001",
                    "job_id": "job-1",
                    "verdict": "pass"
                })),
                "query.status" => Err(WireError::not_found("status row not found")),
                "query.jobs" => {
                    let params = request.params.as_ref().unwrap();
                    assert_eq!(params["liveState"], "running");
                    assert_eq!(params["terminalVerdict"], "pass");
                    assert_eq!(params["pool"], "slot");
                    assert_eq!(params["executor"], "worker");
                    assert_eq!(params["adapter"], "codex");
                    assert_eq!(params["source"], "calendar");
                    assert_eq!(params["origin"], "nightly");
                    assert_eq!(params["parent"], "parent-24");
                    assert_eq!(params["flowRun"], "00000000-0000-4000-8000-000000000045");
                    assert_eq!(params["session"], "session-24");
                    assert_eq!(params["since"], "2026-07-24T00:00:00Z");
                    assert_eq!(params["until"], "2026-07-25T00:00:00Z");
                    assert_eq!(params["limit"], 17);
                    assert_eq!(params["cursor"], "page-v1:jobs");
                    Ok(serde_json::json!({
                        "schemaVersion": 1,
                        "protocolVersion": 5,
                        "items": [],
                        "nextCursor": null,
                        "snapshot": {}
                    }))
                }
                "query.job" => {
                    assert_eq!(
                        request.params.as_ref().unwrap()["id"],
                        "00000000-0000-4000-8000-000000000024"
                    );
                    Ok(serde_json::json!({"schemaVersion": 1, "protocolVersion": 5}))
                }
                "query.run" => {
                    assert_eq!(
                        request.params.as_ref().unwrap()["id"],
                        "00000000-0000-4000-8000-000000000045"
                    );
                    Ok(serde_json::json!({
                        "schemaVersion": 1,
                        "protocolVersion": 5,
                        "flowRunId": "00000000-0000-4000-8000-000000000045",
                        "flowName": "spec-build",
                        "campaign": "crm",
                        "state": "running",
                        "counts": {"done": 1, "running": 1, "blocked": 0, "pending": 0},
                        "items": [],
                        "currentNodes": [],
                        "failures": [],
                        "snapshot": {}
                    }))
                }
                "query.log" => {
                    let params = request.params.as_ref().unwrap();
                    assert_eq!(params["attempt"], 2);
                    assert_eq!(params["session"], "session-24");
                    assert_eq!(params["event"], "evidence_pass");
                    assert_eq!(params["source"], "manual");
                    assert_eq!(params["since"], "2026-07-24T00:00:00Z");
                    assert_eq!(params["until"], "2026-07-25T00:00:00Z");
                    assert_eq!(params["limit"], 23);
                    assert_eq!(params["cursor"], "page-v1:log");
                    // The durable position rides its own parameter: `since`
                    // stays a wall-clock time filter.
                    assert_eq!(
                        params["after"],
                        "log-v1:00000000000000000041:00000000000000000007"
                    );
                    assert_eq!(params["provenance"], false);
                    Ok(serde_json::json!({
                        "schemaVersion": 1,
                        "protocolVersion": 5,
                        "items": [],
                        "nextCursor": null,
                        "snapshot": {}
                    }))
                }
                "query.proof" => {
                    let params = request.params.as_ref().unwrap();
                    assert_eq!(params["task"], "00000000-0000-4000-8000-000000000024");
                    assert_eq!(params["attempt"], 2);
                    Ok(serde_json::json!({
                        "schemaVersion": 1,
                        "protocolVersion": 5,
                        "status": "verified"
                    }))
                }
                "query.trace" => {
                    let params = request.params.as_ref().unwrap();
                    assert_eq!(params["task"], "00000000-0000-4000-8000-000000000024");
                    assert_eq!(params["attempt"], 2);
                    assert_eq!(params["limit"], 29);
                    assert_eq!(params["cursor"], "page-v1:trace");
                    Ok(serde_json::json!({
                        "schemaVersion": 1,
                        "protocolVersion": 5,
                        "items": [],
                        "nextCursor": null,
                        "snapshot": {},
                        "generations": []
                    }))
                }
                "query.producers" => {
                    let params = request.params.as_ref().unwrap();
                    assert_eq!(params["name"], "nightly");
                    assert_eq!(params["kind"], "calendar");
                    Ok(serde_json::json!({
                        "schemaVersion": 1,
                        "protocolVersion": 5,
                        "items": [],
                        "nextCursor": null,
                        "snapshot": {}
                    }))
                }
                "query.watch" => {
                    let params = request.params.as_ref().unwrap();
                    assert_eq!(params["after"], "change:00000000000000000024");
                    assert_eq!(params["limit"], 100);
                    Ok(serde_json::json!({
                        "schemaVersion": 1,
                        "protocolVersion": 5,
                        "status": "ok",
                        "items": [{
                            "schemaVersion": 1,
                            "protocolVersion": 5,
                            "sequence": 25,
                            "cursor": "change:00000000000000000025",
                            "observedAt": "2026-07-24T00:00:00Z",
                            "kind": "job",
                            "payload": {}
                        }],
                        "nextCursor": "change:00000000000000000025"
                    }))
                }
                method => Err(WireError::invalid(format!("unexpected method {method}"))),
            }
        })
    }
}

#[derive(Clone, Copy)]
struct HumanQueryHandler;

impl RpcHandler for HumanQueryHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            match request.method.as_str() {
                "query.log" => {
                    let mut response = serde_json::json!({
                    "schemaVersion": 1,
                    "protocolVersion": 5,
                    "items": [
                        {
                            "origin": "journal", "eventId": "event:1", "cursor": "event:1",
                            "timestamp": "2026-08-01T10:00:00.000Z", "event": "enqueued",
                            "taskUuid": "00000000-0000-4000-8000-000000000261",
                            "taskRef": "crm/t07", "nodeLabel": "agent-t07",
                            "attempt": 1, "leaseEpoch": 7,
                            "authority": "tally-lifecycle-observation",
                            "provenance": "durable-lifecycle-history"
                        },
                        {
                            "origin": "journal", "eventId": "event:2", "cursor": "event:2",
                            "timestamp": "2026-08-01T10:00:01.000Z", "event": "started",
                            "taskUuid": "00000000-0000-4000-8000-000000000261",
                            "taskRef": "crm/t07", "nodeLabel": "agent-t07",
                            "attempt": 1, "leaseEpoch": 7, "adapter": "codex", "pool": ["campaign-agent"],
                            "authority": "tally-lifecycle-observation",
                            "provenance": "durable-lifecycle-history"
                        },
                        {
                            "origin": "journal", "eventId": "event:3", "cursor": "event:3",
                            "timestamp": "2026-08-01T10:00:02.000Z", "event": "evidence_pass",
                            "taskUuid": "00000000-0000-4000-8000-000000000261",
                            "taskRef": "crm/t07", "nodeLabel": "agent-t07",
                            "attempt": 1, "leaseEpoch": 7,
                            "authority": "tally-lifecycle-observation",
                            "provenance": "durable-lifecycle-history"
                        },
                        {
                            "origin": "journal", "eventId": "event:4", "cursor": "event:4",
                            "timestamp": "2026-08-01T10:00:03.000Z", "event": "completed",
                            "taskUuid": "00000000-0000-4000-8000-000000000261",
                            "taskRef": "crm/t07", "nodeLabel": "agent-t07",
                            "attempt": 1, "leaseEpoch": 7, "exitCode": 0,
                            "authority": "tally-lifecycle-observation",
                            "provenance": "durable-lifecycle-history"
                        },
                        {
                            "origin": "witness", "eventId": "witness:11", "cursor": "witness:11",
                            "timestamp": "2026-08-01T10:00:03.100Z", "event": "witness_emitted",
                            "taskUuid": "00000000-0000-4000-8000-000000000261",
                            "taskRef": "crm/t07", "nodeLabel": "agent-t07",
                            "attempt": 1, "leaseEpoch": 7, "exitCode": 0,
                            "terminalVerdict": "pass", "witnessSeq": 11, "wallClockSeconds": 3.1,
                            "authority": "canonical-witness-fact", "provenance": "witness-ledger"
                        }
                    ],
                    "nextCursor": null,
                        "snapshot": {}
                    });
                    if request.params.as_ref().unwrap()["provenance"] == false {
                        let queued = response["items"][0].clone();
                        let started = response["items"][1].clone();
                        let mut terminal = response["items"][3].clone();
                        terminal["origin"] = Value::String("journal+witness".to_owned());
                        terminal["authority"] = Value::String("canonical-witness-fact".to_owned());
                        terminal["provenance"] =
                            Value::String("durable-lifecycle-history+witness-ledger".to_owned());
                        terminal["terminalVerdict"] = Value::String("pass".to_owned());
                        terminal["witnessSeq"] = serde_json::json!(11);
                        terminal["wallClockSeconds"] = serde_json::json!(3.1);
                        response["items"] = Value::Array(vec![queued, started, terminal]);
                    }
                    Ok(response)
                }
                "query.run" => Ok(serde_json::json!({
                    "schemaVersion": 1,
                    "protocolVersion": 5,
                    "flowRunId": "00000000-0000-4000-8000-000000000262",
                    "flowName": "spec-build",
                    "campaign": "crm",
                    "repository": "mecattaf/tally.nix",
                    "state": "needs-attention",
                    "counts": {"done": 1, "running": 0, "blocked": 1, "pending": 0},
                    "usage": {
                        "authority": "advisory-provider-capture",
                        "provenance": "adapter-scrape attestations, per attempt, keyed by taskUuid/attempt/leaseEpoch",
                        "composition": "freshInputTokens = inputTokens + cacheWriteTokens",
                        "coverage": {
                            "tasks": 3, "tasksWithReportedUsage": 2, "tasksWithoutAttestation": 1,
                            "attemptsObserved": 3, "attemptsReported": 2,
                            "attemptsReportedWithoutFigures": 0, "attemptsReportedWithComponents": 2,
                            "attemptsNotReported": 1,
                            "attemptsNotDeclared": 0, "attemptsWithoutUsageRecord": 0,
                            "ledgerVerified": true
                        },
                        "tokens": {
                            "inputTokens": {"value": 262169, "attempts": 2},
                            "cacheReadTokens": {"value": 17891220, "attempts": 2},
                            "cacheWriteTokens": {"value": 265127, "attempts": 2},
                            "outputTokens": {"value": 55140, "attempts": 2},
                            "reasoningTokens": {"value": 15163, "attempts": 1},
                            "freshInputTokens": {"value": 527296, "attemptsComplete": 2, "attemptsPartial": 0},
                            "totalTokens": {"value": 18473656, "attempts": 2, "source": "mixed"}
                        },
                        "cost": {
                            "amountUsd": 8.755705,
                            "attempts": 1,
                            "basis": "harness-reported costUsd only, summed over the attempts that reported it. Tally's cgroup charge is a distinct quantity, is not summed here, and is a floor: it includes tally's own exit-recorder overhead and is not pure job cost"
                        },
                        "caveats": ["members-without-attestation", "attempts-without-usage", "mixed-total-authority", "partial-cost"]
                    },
                    "tasks": [
                        {"taskRef": "crm/t01", "title": "Done task", "status": "done", "blockedBy": [], "pullRequest": "https://example.test/pr/1"},
                        {"taskRef": "crm/t02", "title": "Failed task", "status": "blocked", "blockedBy": [], "failureStage": "agent-t02"}
                    ],
                    "anomalies": [{
                        "kind": "closed-without-merged-proof",
                        "taskRef": "crm/t02",
                        "issue": "42",
                        "url": "https://example.test/issues/42",
                        "detail": "sub-issue #42 is closed but task 't02' holds no revision-valid merged pull request"
                    }],
                    "currentNodes": [{
                        "taskUuid": "00000000-0000-4000-8000-000000000263",
                        "taskRef": "crm/t02", "ordinal": 3, "label": "cleanup-t02", "state": "running",
                        "startedAt": "2026-08-01T10:00:00Z", "elapsedSeconds": 9,
                        "runtimeMaxSec": 60, "budgetRemainingSeconds": 51
                    }, {
                        "taskUuid": "00000000-0000-4000-8000-000000000265",
                        "taskRef": "crm/t03", "ordinal": 4, "label": "gate-t03", "state": "running",
                        "startedAt": "2026-08-01T09:00:00Z", "elapsedSeconds": 460,
                        "runtimeMaxSec": 60, "budgetRemainingSeconds": -400
                    }, {
                        "taskUuid": "00000000-0000-4000-8000-000000000266",
                        "taskRef": "crm/t04", "ordinal": 5, "label": "gate-t04", "state": "queued"
                    }],
                    "failures": [{
                        "taskUuid": "00000000-0000-4000-8000-000000000264",
                        "taskRef": "crm/t02", "ordinal": 2, "stage": "agent-t02", "verdict": "failed",
                        "attempt": 1, "leaseEpoch": 4, "timestamp": "2026-08-01T10:00:03Z",
                        "capturePath": "/tmp/tally/crm.t02.err",
                        "stderrTail": "\u{1b}[2Jactionable failure\n    at gate.rs:1\n",
                        "stderrTruncated": false
                    }, {
                        "taskUuid": "00000000-0000-4000-8000-000000000267",
                        "taskRef": "crm/t05", "ordinal": 6, "stage": "agent-t05", "verdict": "failed",
                        "attempt": 1, "leaseEpoch": 2, "timestamp": "2026-08-01T10:00:05Z",
                        "error": {
                            "code": "executor-validation-failed",
                            "message": "execution request is invalid: git-ai await timeout must be positive",
                            "details": {"validationMessage": "git-ai await timeout must be positive"}
                        }
                    }],
                    "snapshot": {}
                })),
                method => panic!("unexpected method {method}"),
            }
        })
    }
}

/// Every daemon-sourced string the human renderers touch, with terminal
/// control planted in it. The fields are trusted-source today; the renderer
/// must not be what makes that load-bearing.
#[derive(Clone, Copy)]
struct HostileQueryHandler;

impl RpcHandler for HostileQueryHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            match request.method.as_str() {
                "query.log" => {
                    // The human path follows the cursor to the end of the
                    // window, so the hostile page must terminate: a
                    // continuation request gets the same control-laden rows
                    // and a null cursor. Both pages stay hostile, so the
                    // sanitization assertions cover the walk, not just its
                    // first step.
                    let continuation = request
                        .params
                        .as_ref()
                        .is_some_and(|params| params["cursor"].is_string());
                    Ok(serde_json::json!({
                    "schemaVersion": 1,
                    "protocolVersion": 5,
                    "items": [
                        {
                            "origin": "jour\u{1b}[2Jnal", "eventId": "event:1", "cursor": "event:1",
                            "timestamp": "\u{1b}[2J2026-08-01T10:00:01.000Z", "event": "started",
                            "taskUuid": "00000000-0000-4000-8000-000000000261",
                            "taskRef": "crm/t07", "nodeLabel": "agent-t07",
                            "attempt": 1, "leaseEpoch": 7,
                            "adapter": "co\u{1b}[2Jdex",
                            "pool": ["camp\u{1b}]0;pwned\u{7}aign-agent", "sl\u{9b}2Jot"],
                            "authority": "tally-lifecycle-observation",
                            "provenance": "durable-lifecycle\u{1b}[2J-history"
                        },
                        {
                            "origin": "journal+witness", "eventId": "event:4", "cursor": "event:4",
                            "timestamp": "2026-08-01T10:00:03.000Z\u{202e}", "event": "completed",
                            "taskUuid": "00000000-0000-4000-8000-000000000261",
                            "taskRef": "crm/t07", "nodeLabel": "agent-t07",
                            "attempt": 1, "leaseEpoch": 7, "exitCode": 0,
                            "terminalVerdict": "pa\u{1b}[31mss", "witnessSeq": 11,
                            "authority": "canonical-witness-fact",
                            "provenance": "durable-lifecycle-history+witness-ledger"
                        }
                    ],
                    "nextCursor": if continuation { Value::Null } else { Value::String("\u{1b}[2Jpage-v1:log".to_owned()) },
                    "snapshot": {}
                    }))
                }
                "query.run" => Ok(serde_json::json!({
                    "schemaVersion": 1,
                    "protocolVersion": 5,
                    "flowRunId": "\u{1b}[2J00000000-0000-4000-8000-000000000262",
                    "flowName": "\u{1b}[2Jspec-build",
                    "campaign": "c\u{1b}]0;pwned\u{7}rm",
                    "state": "needs-\u{1b}[31mattention",
                    "counts": {"done": 0, "running": 1, "blocked": 0, "pending": 0},
                    "usage": {
                        "authority": "advisory-provider-capture",
                        "provenance": "atte\u{1b}[2Jstations",
                        "composition": "compo\u{1b}[2Jsition",
                        "coverage": {
                            "tasks": 1, "tasksWithReportedUsage": 1, "tasksWithoutAttestation": 0,
                            "attemptsObserved": 1, "attemptsReported": 1, "attemptsNotReported": 0,
                            "attemptsNotDeclared": 0, "attemptsWithoutUsageRecord": 0,
                            "ledgerVerified": true
                        },
                        "tokens": {
                            "inputTokens": {"value": 1, "attempts": 1},
                            "cacheReadTokens": {"value": 2, "attempts": 1},
                            "cacheWriteTokens": {"value": 3, "attempts": 1},
                            "outputTokens": {"value": 4, "attempts": 1},
                            "reasoningTokens": {"value": 5, "attempts": 1},
                            "freshInputTokens": {"value": 4, "attemptsComplete": 1, "attemptsPartial": 0},
                            "totalTokens": {"value": 10, "attempts": 1, "source": "harness-\u{1b}[2Jreported"}
                        },
                        "cost": {"amountUsd": 1.5, "attempts": 1, "basis": "ba\u{1b}[2Jsis"},
                        "caveats": ["mixed-\u{9b}2Jtotal-authority"]
                    },
                    "tasks": [{
                        "taskRef": "crm/t02", "title": "Hostile task",
                        "status": "bl\u{1b}[2Jocked", "blockedBy": [],
                        "failureStage": "agent\u{9b}2J-t02"
                    }],
                    "anomalies": [],
                    "currentNodes": [{
                        "taskUuid": "00000000-0000-4000-8000-000000000263",
                        "taskRef": "crm/t02", "ordinal": 3,
                        "label": "clean\u{1b}[2Jup-t02", "state": "run\u{1b}[2Jning",
                        "startedAt": "2026-08-01T10:00:00Z", "elapsedSeconds": 9
                    }],
                    "failures": [{
                        "taskUuid": "00000000-0000-4000-8000-000000000264",
                        "taskRef": "crm/t02", "ordinal": 2,
                        "stage": "agent\u{1b}[2J-t02", "verdict": "fail\u{1b}[2Jed",
                        "attempt": 1, "leaseEpoch": 4, "timestamp": "2026-08-01T10:00:03Z",
                        "capturePath": "/tmp/tally/\u{1b}[2Jcrm.t02.err",
                        "stderrTail": "\u{1b}[2Jactionable failure\n",
                        "stderrTruncated": false
                    }],
                    "snapshot": {}
                })),
                method => panic!("unexpected method {method}"),
            }
        })
    }
}

/// A run board far larger than a pipe buffer, so a reader that takes one line
/// and leaves is guaranteed to hang up mid-write rather than after the last
/// byte already fit.
#[derive(Clone, Copy)]
struct FloodQueryHandler;

impl RpcHandler for FloodQueryHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method, "query.run");
            let tasks = (0..4_000)
                .map(|index| {
                    serde_json::json!({
                        "taskRef": format!("crm/t{index:04}"),
                        "title": "A task with a title long enough to fill a pipe buffer",
                        "status": "pending",
                        "blockedBy": []
                    })
                })
                .collect::<Vec<_>>();
            Ok(serde_json::json!({
                "schemaVersion": 1,
                "protocolVersion": 5,
                "flowRunId": "00000000-0000-4000-8000-000000000262",
                "flowName": "spec-build",
                "state": "running",
                "counts": {"done": 0, "running": 0, "blocked": 0, "pending": 4000},
                "tasks": tasks,
                "anomalies": [],
                "currentNodes": [],
                "failures": [],
                "snapshot": {}
            }))
        })
    }
}

/// Acknowledges an admission whose run membership could not be recorded.
#[derive(Clone, Copy)]
struct DegradedMembershipHandler {
    degraded: bool,
}

impl RpcHandler for DegradedMembershipHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        let degraded = self.degraded;
        Box::pin(async move {
            assert_eq!(request.method.as_str(), "queue.enqueue");
            let mut response = serde_json::json!({
                "schemaVersion": 1,
                "disposition": "created",
                "task_uuid": "00000000-0000-4000-8000-000000000380",
                "taskUuid": "00000000-0000-4000-8000-000000000380",
                "job_id": "00000000-0000-4000-8000-000000000380",
                "state": "queued",
            });
            if degraded {
                response["membershipDegraded"] = serde_json::json!({
                    "flowRunId": "8f2d1c40-0000-4000-8000-0000000003c0",
                    "taskUuid": "00000000-0000-4000-8000-000000000380",
                    "admitted": true,
                    "reason": "flow membership I/O error: No space left on device",
                    "resolution": "repair-flow-membership-ledger",
                });
            }
            Ok(response)
        })
    }
}

/// #380: an admission the daemon acknowledged with its run membership
/// unrecorded must say so to the operator who caused it, at the point they
/// caused it. Otherwise the only trace is a daemon journal line they have to
/// already know to grep for — which is the gap this warning exists to close.
#[tokio::test(flavor = "current_thread")]
async fn a_degraded_membership_admission_warns_the_caller_that_the_node_will_be_invisible() {
    for degraded in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("tally.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let handler = DegradedMembershipHandler { degraded };
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let server = tokio::task::spawn_local(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    serve_connection(stream, handler).await.unwrap();
                });
                let output = run_tally(&socket, &["enqueue", "--pool", "slot", "--", "true"]).await;
                let stderr = String::from_utf8(output.stderr).unwrap();
                assert!(output.status.success(), "{stderr}");
                if degraded {
                    assert!(
                        stderr.contains("run membership was NOT recorded"),
                        "a degraded admission printed no warning:\n{stderr}"
                    );
                    // The three things an operator needs: which run is now
                    // incomplete, which node is missing from it, and what to do.
                    assert!(
                        stderr.contains("8f2d1c40-0000-4000-8000-0000000003c0"),
                        "the warning did not name the run:\n{stderr}"
                    );
                    assert!(
                        stderr.contains("00000000-0000-4000-8000-000000000380"),
                        "the warning did not name the task:\n{stderr}"
                    );
                    assert!(
                        stderr.contains("repair-flow-membership-ledger"),
                        "the warning did not name the resolution:\n{stderr}"
                    );
                    // And the admission still succeeded, because it did.
                    assert!(
                        String::from_utf8(output.stdout)
                            .unwrap()
                            .contains("00000000-0000-4000-8000-000000000380"),
                        "a degraded admission must still return its task UUID"
                    );
                } else {
                    assert!(
                        stderr.is_empty(),
                        "an ordinary admission must be silent:\n{stderr}"
                    );
                }
                server.await.unwrap();
            })
            .await;
    }
}

/// Answers the two methods that share `submit_payload`.
#[derive(Clone, Copy)]
struct SubmitHandler;

impl RpcHandler for SubmitHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            assert!(matches!(
                request.method.as_str(),
                "queue.enqueue" | "queue.continue"
            ));
            Ok(serde_json::json!({
                "task_uuid": "00000000-0000-4000-8000-000000000141",
                "job_id": "00000000-0000-4000-8000-000000000141",
                "state": "queued"
            }))
        })
    }
}

const NO_ENQUEUE_JOB: &str = "00000000-0000-4000-8000-000000000132";

#[derive(Clone, Default)]
struct ContinueGuardrailHandler {
    admitted_no_enqueue: Arc<Mutex<Option<String>>>,
}

impl RpcHandler for ContinueGuardrailHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            let params = request.params.as_ref().unwrap();
            match request.method.as_str() {
                "queue.enqueue" => {
                    assert_eq!(params["noEnqueue"], true);
                    assert!(params["callerJobId"].is_null());
                    assert!(params["callerJobToken"].is_null());
                    *self.admitted_no_enqueue.lock().unwrap() = Some(NO_ENQUEUE_JOB.to_owned());
                    Ok(serde_json::json!({
                        "task_uuid": NO_ENQUEUE_JOB,
                        "job_id": NO_ENQUEUE_JOB,
                        "state": "queued"
                    }))
                }
                "queue.continue" => {
                    assert_eq!(params["resumeFrom"], NO_ENQUEUE_JOB);
                    let caller = params["callerJobId"].as_str();
                    let admitted = self.admitted_no_enqueue.lock().unwrap().clone();
                    match caller {
                        Some(caller) if admitted.as_deref() == Some(caller) => {
                            Err(WireError::invalid(format!(
                                "job {caller} carries the noEnqueue capability"
                            )))
                        }
                        None => Ok(serde_json::json!({
                            "task_uuid": "00000000-0000-4000-8000-000000000133",
                            "job_id": "00000000-0000-4000-8000-000000000133",
                            "verdict": "pass"
                        })),
                        Some(caller) => panic!("unexpected caller job {caller}"),
                    }
                }
                method => panic!("unexpected method {method}"),
            }
        })
    }
}

#[derive(Clone, Default)]
struct SubmissionCaptureHandler {
    requests: Arc<Mutex<Vec<Value>>>,
}

impl RpcHandler for SubmissionCaptureHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method, "queue.enqueue");
            self.requests.lock().unwrap().push(request.params.unwrap());
            Ok(serde_json::json!({
                "task_uuid": "00000000-0000-4000-8000-000000000141",
                "job_id": "00000000-0000-4000-8000-000000000141",
                "state": "queued"
            }))
        })
    }
}

async fn run_tally(socket: &Path, args: &[&str]) -> std::process::Output {
    run_tally_with_job_id(socket, args, None).await
}

async fn run_tally_with_job_id(
    socket: &Path,
    args: &[&str],
    job_id: Option<&str>,
) -> std::process::Output {
    run_tally_with_identity(socket, args, job_id, None).await
}

async fn run_tally_with_identity(
    socket: &Path,
    args: &[&str],
    job_id: Option<&str>,
    job_token: Option<&str>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tally"));
    command
        .arg("--socket")
        .arg(socket)
        .args(args)
        .env_remove("TALLY_JOB_ID")
        .env_remove("TALLY_JOB_TOKEN");
    if let Some(job_id) = job_id {
        command.env("TALLY_JOB_ID", job_id);
    }
    if let Some(job_token) = job_token {
        command.env("TALLY_JOB_TOKEN", job_token);
    }
    command.output().await.unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn cli_maps_rpc_and_waited_verdict_exit_codes() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            tokio::task::spawn_local(async move {
                for _ in 0..3 {
                    let (stream, _) = listener.accept().await.unwrap();
                    serve_connection(stream, CliHandler).await.unwrap();
                }
            });

            let passed = run_tally(
                &socket,
                &["enqueue", "--pool", "gpu", "--wait", "--", "true"],
            )
            .await;
            assert_eq!(passed.status.code(), Some(0));

            let invalid = run_tally(&socket, &["enqueue", "--pool", "invalid", "--", "true"]).await;
            assert_eq!(invalid.status.code(), Some(2));

            let missing = run_tally(&socket, &["query", "status"]).await;
            assert_eq!(missing.status.code(), Some(4));
        })
        .await;

    let absent = temp.path().join("absent.sock");
    let unreachable = run_tally(&absent, &["query", "status"]).await;
    assert_eq!(unreachable.status.code(), Some(3));
}

#[tokio::test(flavor = "current_thread")]
async fn no_enqueue_job_cannot_continue_and_empty_job_id_is_unset() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let handler = ContinueGuardrailHandler::default();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                for _ in 0..3 {
                    let (stream, _) = listener.accept().await.unwrap();
                    serve_connection(stream, handler.clone()).await.unwrap();
                }
            });

            let admitted = run_tally_with_job_id(
                &socket,
                &["enqueue", "--pool", "gpu", "--no-enqueue", "--", "true"],
                Some(""),
            )
            .await;
            assert_eq!(admitted.status.code(), Some(0));

            let rejected = run_tally_with_job_id(
                &socket,
                &["queue", "continue", NO_ENQUEUE_JOB, "--", "true"],
                Some(NO_ENQUEUE_JOB),
            )
            .await;
            assert_eq!(rejected.status.code(), Some(2));

            let cooperative = run_tally_with_job_id(
                &socket,
                &["queue", "continue", NO_ENQUEUE_JOB, "--", "true"],
                Some("  "),
            )
            .await;
            assert_eq!(cooperative.status.code(), Some(0));
            server.await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn cli_forwards_the_inherited_job_token_with_the_caller_identity() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let handler = SubmissionCaptureHandler::default();
    let requests = handler.requests.clone();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                serve_connection(stream, handler).await.unwrap();
            });
            let token = "ab".repeat(32);
            let output = run_tally_with_identity(
                &socket,
                &["enqueue", "--pool", "slot", "--", "true"],
                Some(NO_ENQUEUE_JOB),
                Some(&token),
            )
            .await;
            assert_eq!(output.status.code(), Some(0));
            server.await.unwrap();

            let requests = requests.lock().unwrap();
            assert_eq!(requests[0]["callerJobId"], NO_ENQUEUE_JOB);
            assert_eq!(requests[0]["callerJobToken"], token);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn cli_forwards_opaque_evidence_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                serve_connection(stream, CliHandler).await.unwrap();
            });
            let output = run_tally(
                &socket,
                &[
                    "enqueue",
                    "--pool",
                    "metadata",
                    "--evidence-class",
                    r#"{"arbitrary":[true,7,{"nested":null}]}"#,
                    "--manifest-hash",
                    "deliberately-not-validated://manifest value",
                    "--",
                    "true",
                ],
            )
            .await;
            assert_eq!(output.status.code(), Some(0));
            server.await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn cli_submission_flag_preserves_legacy_wire_bytes_and_omits_keyless_mode() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let handler = SubmissionCaptureHandler::default();
    let requests = handler.requests.clone();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                for _ in 0..4 {
                    let (stream, _) = listener.accept().await.unwrap();
                    serve_connection(stream, handler.clone()).await.unwrap();
                }
            });

            let default_full = run_tally(
                &socket,
                &[
                    "enqueue",
                    "--pool",
                    "slot",
                    "--dedup-key",
                    "review:42",
                    "--",
                    "true",
                ],
            )
            .await;
            assert_eq!(default_full.status.code(), Some(0));

            let explicit_full = run_tally(
                &socket,
                &[
                    "enqueue",
                    "--pool",
                    "slot",
                    "--dedup-key",
                    "review:42",
                    "--submission",
                    "full",
                    "--",
                    "true",
                ],
            )
            .await;
            assert_eq!(explicit_full.status.code(), Some(0));

            let legacy = run_tally(
                &socket,
                &[
                    "enqueue",
                    "--pool",
                    "slot",
                    "--dedup-key",
                    "review:42",
                    "--submission",
                    "legacy",
                    "--",
                    "true",
                ],
            )
            .await;
            assert_eq!(legacy.status.code(), Some(0));

            let keyless = run_tally(
                &socket,
                &[
                    "enqueue",
                    "--pool",
                    "slot",
                    "--submission",
                    "full",
                    "--",
                    "true",
                ],
            )
            .await;
            assert_eq!(keyless.status.code(), Some(0));
            server.await.unwrap();
        })
        .await;

    let requests = requests.lock().unwrap();
    assert_eq!(
        requests[0]["submission"],
        serde_json::json!({"mode": "full"})
    );
    assert_eq!(requests[0], requests[1]);

    let mut full_without_submission = requests[0].clone();
    full_without_submission
        .as_object_mut()
        .unwrap()
        .shift_remove("submission");
    assert_eq!(
        serde_json::to_vec(&full_without_submission).unwrap(),
        serde_json::to_vec(&requests[2]).unwrap(),
        "legacy mode must reproduce the pre-flag enqueue params byte-for-byte"
    );
    assert!(!requests[3].as_object().unwrap().contains_key("submission"));
}

#[tokio::test(flavor = "current_thread")]
async fn cli_carries_a_canonical_multi_pool_set_over_enqueue_and_acquire_rpc() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                for _ in 0..2 {
                    let (stream, _) = listener.accept().await.unwrap();
                    serve_connection(stream, CliHandler).await.unwrap();
                }
            });
            let enqueued = run_tally(
                &socket,
                &[
                    "enqueue",
                    "--pool",
                    "zeta",
                    "--pool",
                    "slot",
                    "--executor",
                    "worker",
                    "--",
                    "true",
                ],
            )
            .await;
            assert_eq!(enqueued.status.code(), Some(0));

            let acquired = run_tally(&socket, &["lease", "acquire", "zeta", "slot"]).await;
            assert_eq!(acquired.status.code(), Some(0));
            server.await.unwrap();
        })
        .await;
}

#[tokio::test]
async fn internal_exit_recorder_is_silent_and_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let record = temp.path().join("exit.json");
    let unit = "tally-job-recorder-test.service";
    // A working accounting probe, so the recorder's happy path stays silent
    // (#382): silence here is a property of nothing going wrong, not of the
    // accounting probe being skipped. The probe's own failure-is-logged
    // behavior is covered by
    // `crates/tally/tests/record_unit_exit_accounting.rs`.
    // Installed through the immutable provider, never written-then-chmoded:
    // an executable this process wrote is unexecutable for as long as any
    // process on the host still holds a write fd on it (#396).
    let systemctl = temp.path().join("fake-systemctl-ok");
    shell_program::install(
        &systemctl,
        "#!/bin/sh\necho \"CPUUsageNSec=1000000000\"\necho \"ExecMainStartTimestampMonotonic=0\"\necho \"ExecMainExitTimestampMonotonic=1000000\"\n",
    );
    let success = Command::new(env!("CARGO_BIN_EXE_tally"))
        .args([
            "__record-unit-exit",
            "--record",
            record.to_str().unwrap(),
            "--unit",
            unit,
            "--systemctl",
            systemctl.to_str().unwrap(),
        ])
        .env_clear()
        .env("INVOCATION_ID", "recorder-test")
        .env("SERVICE_RESULT", "success")
        .env("TALLY_ATTEMPT", "1")
        .env("TALLY_LEASE_EPOCH", "7")
        .env("EXIT_CODE", "exited")
        .env("EXIT_STATUS", "0")
        .output()
        .await
        .unwrap();
    assert!(success.status.success());
    assert!(success.stdout.is_empty());
    assert!(success.stderr.is_empty());
    let json = std::fs::read_to_string(&record).unwrap();
    assert!(json.contains("\"invocationId\":\"recorder-test\""));
    assert!(json.contains("\"cpuUsageNsec\":1000000000"));
    assert!(json.contains("\"attempt\":1"));
    assert!(json.contains("\"leaseEpoch\":7"));

    let missing = temp.path().join("missing.json");
    let failure = Command::new(env!("CARGO_BIN_EXE_tally"))
        .args([
            "__record-unit-exit",
            "--record",
            missing.to_str().unwrap(),
            "--unit",
            unit,
        ])
        .env_clear()
        .output()
        .await
        .unwrap();
    assert!(!failure.status.success());
    assert!(failure.stdout.is_empty());
    assert!(failure.stderr.is_empty());
    assert!(!missing.exists());
}

#[tokio::test(flavor = "current_thread")]
async fn query_v5_cli_forwards_all_durable_observability_commands() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                for _ in 0..8 {
                    let (stream, _) = listener.accept().await.unwrap();
                    serve_connection(stream, CliHandler).await.unwrap();
                }
            });
            let task = "00000000-0000-4000-8000-000000000024";
            for output in [
                run_tally(
                    &socket,
                    &[
                        "query",
                        "jobs",
                        "--state",
                        "running",
                        "--verdict",
                        "pass",
                        "--pool",
                        "slot",
                        "--executor",
                        "worker",
                        "--adapter",
                        "codex",
                        "--source",
                        "calendar",
                        "--origin",
                        "nightly",
                        "--parent",
                        "parent-24",
                        "--flow-run",
                        "00000000-0000-4000-8000-000000000045",
                        "--session",
                        "session-24",
                        "--since",
                        "2026-07-24T00:00:00Z",
                        "--until",
                        "2026-07-25T00:00:00Z",
                        "--limit",
                        "17",
                        "--cursor",
                        "page-v1:jobs",
                    ],
                )
                .await,
                run_tally(&socket, &["query", "job", task]).await,
                run_tally(
                    &socket,
                    &[
                        "query",
                        "run",
                        "00000000-0000-4000-8000-000000000045",
                        "--json",
                    ],
                )
                .await,
                run_tally(
                    &socket,
                    &[
                        "query",
                        "log",
                        "--task",
                        task,
                        "--attempt",
                        "2",
                        "--session",
                        "session-24",
                        "--event",
                        "evidence_pass",
                        "--source",
                        "manual",
                        "--since",
                        "2026-07-24T00:00:00Z",
                        "--until",
                        "2026-07-25T00:00:00Z",
                        "--limit",
                        "23",
                        "--cursor",
                        "page-v1:log",
                        "--after",
                        "log-v1:00000000000000000041:00000000000000000007",
                        "--json",
                    ],
                )
                .await,
                run_tally(
                    &socket,
                    &["query", "proof", "--task", task, "--attempt", "2"],
                )
                .await,
                run_tally(
                    &socket,
                    &[
                        "query",
                        "trace",
                        "--task",
                        task,
                        "--attempt",
                        "2",
                        "--limit",
                        "29",
                        "--cursor",
                        "page-v1:trace",
                    ],
                )
                .await,
                run_tally(
                    &socket,
                    &[
                        "query",
                        "producers",
                        "--name",
                        "nightly",
                        "--kind",
                        "calendar",
                    ],
                )
                .await,
                run_tally(
                    &socket,
                    &[
                        "query",
                        "watch",
                        "--after",
                        "change:00000000000000000024",
                        "--once",
                    ],
                )
                .await,
            ] {
                assert!(output.status.success(), "{:?}", output);
                let value: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(value["protocolVersion"], 5);
            }
            server.await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn query_log_is_human_first_and_collapses_echoes_in_human_and_json_modes() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                for _ in 0..3 {
                    let (stream, _) = listener.accept().await.unwrap();
                    serve_connection(stream, HumanQueryHandler).await.unwrap();
                }
            });

            let human = run_tally(&socket, &["query", "log"]).await;
            assert!(human.status.success(), "{human:?}");
            let human = String::from_utf8(human.stdout).unwrap();
            assert_eq!(human.lines().count(), 3, "{human}");
            assert!(human.contains("crm/t07"));
            assert!(human.contains("agent-t07"));
            assert!(human.contains("pass"));
            assert!(!human.contains("evidence-pass"));
            assert!(!human.contains("schemaVersion"));

            let json = run_tally(&socket, &["query", "log", "--json"]).await;
            assert!(json.status.success(), "{json:?}");
            let json: Value = serde_json::from_slice(&json.stdout).unwrap();
            assert_eq!(json["items"].as_array().unwrap().len(), 3);
            assert_eq!(json["items"][2]["origin"], "journal+witness");
            assert_eq!(json["items"][2]["terminalVerdict"], "pass");
            assert_eq!(json["items"][2]["witnessSeq"], 11);

            let raw = run_tally(&socket, &["query", "log", "--json", "--provenance"]).await;
            assert!(raw.status.success(), "{raw:?}");
            let raw: Value = serde_json::from_slice(&raw.stdout).unwrap();
            assert_eq!(raw["items"].as_array().unwrap().len(), 5);
            assert!(raw["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["event"] == "evidence_pass"));
            server.await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn query_run_human_view_includes_tasks_budget_and_failure_pointer() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                serve_connection(stream, HumanQueryHandler).await.unwrap();
            });
            let output = run_tally(
                &socket,
                &["query", "run", "00000000-0000-4000-8000-000000000262"],
            )
            .await;
            assert!(output.status.success(), "{output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            for expected in [
                "spec-build crm",
                "needs-attention",
                "crm/t01",
                "crm/t02",
                "budget=51s",
                "/tmp/tally/crm.t02.err",
                "actionable failure",
                // A node past its budget is distinguishable from one on it.
                "budget=-6m40s",
                // A node that never started says so rather than "unknown".
                "elapsed=not-started",
                // An absent capture pointer is stated, not omitted.
                "capture: <not retained>",
                // A pre-launch executor rejection remains visible even though
                // there is no stderr capture to inspect.
                "error [executor-validation-failed]: execution request is invalid: git-ai await timeout must be positive",
                // Failure-tail indentation survives; the six-space frame plus
                // the line's own four spaces.
                "\n          at gate.rs:1",
                // A hand-closed sub-issue is rendered above the board, not
                // buried in the pass projection.
                "!! ANOMALIES: 1 closed sub-issue(s) hold no merged proof",
                "https://example.test/issues/42",
                // What the run cost, and never a bare total: the sum arrives
                // with the attempts it is over and the grade of its evidence.
                "Usage: 18473656 tokens, mixed (2 of 3 scraped attempt(s) over 3 member task(s), advisory adapter captures)",
                // The fresh-input figure states its own addition, because
                // `inputTokens` alone understates any cache-writing harness.
                "fresh input 527296 (= input 262169 + cache write 265127)",
                "output 55140 (reasoning 15163 nested inside)",
                // Cost carries the charge-floor statement beside it, and the
                // sentence is the daemon's own `cost.basis`, not a literal the
                // client could drift from.
                "cost $8.7557 over 1 attempt(s) -- harness-reported costUsd only",
                "Tally's cgroup charge is a distinct quantity, is not summed here, and is a floor",
                "partial: members-without-attestation, attempts-without-usage",
            ] {
                assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
            }
            let anomaly_line = text.find("!! ANOMALIES").expect("anomaly banner");
            assert!(
                anomaly_line < text.find("STATUS").expect("task table"),
                "the anomaly banner must precede the task board:\n{text}"
            );
            assert!(
                !text.contains('\u{1b}'),
                "adapter-controlled escape reached the terminal:\n{text:?}"
            );
            server.await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn query_run_status_filter_narrows_the_board_but_not_the_counts() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                serve_connection(stream, HumanQueryHandler).await.unwrap();
            });
            let output = run_tally(
                &socket,
                &[
                    "query",
                    "run",
                    "00000000-0000-4000-8000-000000000262",
                    "--status",
                    "blocked",
                ],
            )
            .await;
            assert!(output.status.success(), "{output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(text.contains("1 done, 0 running, 1 blocked, 0 pending"));
            assert!(text.contains("crm/t02"), "{text}");
            assert!(!text.contains("crm/t01"), "{text}");
            server.await.unwrap();
        })
        .await;
}

/// Three usage shapes the run header must not flatten into each other, keyed
/// by run ID: an attempt that reported usage no declared mapping could read, a
/// run where nothing reported, and a component absence beside a measured zero.
#[derive(Clone, Copy)]
struct UsageEdgeQueryHandler;

impl RpcHandler for UsageEdgeQueryHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method, "query.run");
            let id = request.params.as_ref().unwrap()["id"].as_str().unwrap();
            // An attempt that reported usage the adapter's mapping resolved
            // nothing out of: reported == 1, every component over 0 attempts,
            // no total.
            let unmapped = serde_json::json!({
                "authority": "advisory-provider-capture",
                "provenance": "attestations", "composition": "composition",
                "coverage": {
                    "tasks": 1, "tasksWithReportedUsage": 0, "tasksWithoutAttestation": 0,
                    "attemptsObserved": 1, "attemptsReported": 1,
                    "attemptsReportedWithoutFigures": 1, "attemptsReportedWithComponents": 1,
                    "attemptsNotReported": 0,
                    "attemptsNotDeclared": 0, "attemptsWithoutUsageRecord": 0,
                    "ledgerVerified": true
                },
                "tokens": {
                    "inputTokens": {"value": 0, "attempts": 0},
                    "cacheReadTokens": {"value": 0, "attempts": 0},
                    "cacheWriteTokens": {"value": 0, "attempts": 0},
                    "outputTokens": {"value": 0, "attempts": 0},
                    "reasoningTokens": {"value": 0, "attempts": 0},
                    "freshInputTokens": {"value": 0, "attemptsComplete": 0, "attemptsPartial": 0}
                },
                "cost": {"attempts": 0, "basis": "harness-reported costUsd only"},
                "caveats": ["reported-without-figures"]
            });
            // A measured zero (attempts 1) beside an absence (attempts 0).
            let measured_zero = serde_json::json!({
                "authority": "advisory-provider-capture",
                "provenance": "attestations", "composition": "composition",
                "coverage": {
                    "tasks": 1, "tasksWithReportedUsage": 1, "tasksWithoutAttestation": 0,
                    "attemptsObserved": 1, "attemptsReported": 1,
                    "attemptsReportedWithoutFigures": 0, "attemptsReportedWithComponents": 1,
                    "attemptsNotReported": 0,
                    "attemptsNotDeclared": 0, "attemptsWithoutUsageRecord": 0,
                    "ledgerVerified": true
                },
                "tokens": {
                    "inputTokens": {"value": 262086, "attempts": 1},
                    "cacheReadTokens": {"value": 6798080, "attempts": 1},
                    "cacheWriteTokens": {"value": 0, "attempts": 1},
                    "outputTokens": {"value": 32842, "attempts": 1},
                    "reasoningTokens": {"value": 0, "attempts": 0},
                    "freshInputTokens": {"value": 262086, "attemptsComplete": 1, "attemptsPartial": 0},
                    "totalTokens": {"value": 7093008, "attempts": 1, "source": "derived-from-components"}
                },
                "cost": {"attempts": 0, "basis": "harness-reported costUsd only"},
                "caveats": []
            });
            // Nothing reported at all: both attempts are typed absences.
            let nothing = serde_json::json!({
                "authority": "advisory-provider-capture",
                "provenance": "attestations", "composition": "composition",
                "coverage": {
                    "tasks": 1, "tasksWithReportedUsage": 0, "tasksWithoutAttestation": 0,
                    "attemptsObserved": 2, "attemptsReported": 0,
                    "attemptsReportedWithoutFigures": 0, "attemptsReportedWithComponents": 0,
                    "attemptsNotReported": 2,
                    "attemptsNotDeclared": 0, "attemptsWithoutUsageRecord": 0,
                    "ledgerVerified": true
                },
                "tokens": {
                    "inputTokens": {"value": 0, "attempts": 0},
                    "cacheReadTokens": {"value": 0, "attempts": 0},
                    "cacheWriteTokens": {"value": 0, "attempts": 0},
                    "outputTokens": {"value": 0, "attempts": 0},
                    "reasoningTokens": {"value": 0, "attempts": 0},
                    "freshInputTokens": {"value": 0, "attemptsComplete": 0, "attemptsPartial": 0}
                },
                "cost": {"attempts": 0, "basis": "harness-reported costUsd only"},
                "caveats": ["attempts-without-usage"]
            });
            // One declared key drifted: the attempt reported usage and
            // contributed, the component left the total, and the run is not
            // complete. Real claude-code numbers minus the cache-read half.
            let one_key_drifted = serde_json::json!({
                "authority": "advisory-provider-capture",
                "provenance": "attestations", "composition": "composition",
                "coverage": {
                    "tasks": 1, "tasksWithReportedUsage": 1, "tasksWithoutAttestation": 0,
                    "attemptsObserved": 1, "attemptsReported": 1,
                    "attemptsReportedWithoutFigures": 0, "attemptsReportedWithComponents": 1,
                    "attemptsNotReported": 0,
                    "attemptsNotDeclared": 0, "attemptsWithoutUsageRecord": 0,
                    "ledgerVerified": true
                },
                "tokens": {
                    "inputTokens": {"value": 83, "attempts": 1},
                    "cacheReadTokens": {"value": 0, "attempts": 0},
                    "cacheWriteTokens": {"value": 265127, "attempts": 1},
                    "outputTokens": {"value": 22298, "attempts": 1},
                    "reasoningTokens": {"value": 0, "attempts": 0},
                    "freshInputTokens": {"value": 265210, "attemptsComplete": 1, "attemptsPartial": 0},
                    "totalTokens": {"value": 287508, "attempts": 1, "source": "derived-from-components"}
                },
                "cost": {"attempts": 0, "basis": "harness-reported costUsd only"},
                "caveats": ["partial-components"]
            });
            let usage = match id.chars().last().unwrap() {
                '1' => unmapped,
                '2' => measured_zero,
                '4' => one_key_drifted,
                _ => nothing,
            };
            Ok(serde_json::json!({
                "schemaVersion": 1,
                "protocolVersion": 5,
                "flowRunId": id,
                "flowName": "spec-build",
                "state": "complete",
                "counts": {"done": 1, "running": 0, "blocked": 0, "pending": 0},
                "usage": usage,
                "items": [],
                "currentNodes": [],
                "failures": [],
                "snapshot": {}
            }))
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn query_run_never_renders_an_absent_figure_as_a_measured_zero() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                for _ in 0..4 {
                    let (stream, _) = listener.accept().await.unwrap();
                    serve_connection(stream, UsageEdgeQueryHandler)
                        .await
                        .unwrap();
                }
            });

            // An adapter whose mapping drifted: the attempt DID report usage,
            // so the header must not say otherwise, nothing may render as a
            // zero, and the reader must be told the run is partial.
            let unmapped = run_tally(
                &socket,
                &["query", "run", "00000000-0000-4000-8000-000000000381"],
            )
            .await;
            assert!(unmapped.status.success(), "{unmapped:?}");
            let text = String::from_utf8(unmapped.stdout).unwrap();
            assert!(
                !text.contains("no attempt reported usage"),
                "one attempt did report usage:\n{text}"
            );
            for expected in [
                "Usage: no total (1 of 1 scraped attempt(s) reported usage over 1 member task(s), advisory adapter captures)",
                "fresh input -- (= input -- + cache write --)  cache read --  output -- (reasoning -- nested inside)",
                "partial: reported-without-figures",
            ] {
                assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
            }
            let usage_lines = text
                .lines()
                .skip_while(|line| !line.starts_with("Usage:"))
                .take_while(|line| !line.starts_with("No reconciled"))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !usage_lines.contains('0'),
                "an absence rendered as a zero:\n{usage_lines}"
            );

            // A measured zero is a measurement and keeps rendering as `0`;
            // the component nobody reported beside it renders `--`.
            let measured = run_tally(
                &socket,
                &["query", "run", "00000000-0000-4000-8000-000000000382"],
            )
            .await;
            assert!(measured.status.success(), "{measured:?}");
            let text = String::from_utf8(measured.stdout).unwrap();
            assert!(
                text.contains("fresh input 262086 (= input 262086 + cache write 0)  cache read 6798080  output 32842 (reasoning -- nested inside)"),
                "{text}"
            );
            assert!(!text.contains("partial:"), "{text}");

            // And a run where nothing reported says exactly that, with no
            // component line of zeros underneath contradicting it.
            let quiet = run_tally(
                &socket,
                &["query", "run", "00000000-0000-4000-8000-000000000383"],
            )
            .await;
            assert!(quiet.status.success(), "{quiet:?}");
            let text = String::from_utf8(quiet.stdout).unwrap();
            assert!(
                text.contains("Usage: no attempt reported usage (2 scraped attempt(s) over 1 member task(s), advisory adapter captures)"),
                "{text}"
            );
            assert!(!text.contains("fresh input"), "{text}");
            assert!(text.contains("partial: attempts-without-usage"), "{text}");

            // A total that looks like a total, one `--` component, and the
            // `partial:` line that stops the reader trusting the number: this
            // is the shape a single renamed harness key ships.
            let drifted = run_tally(
                &socket,
                &["query", "run", "00000000-0000-4000-8000-000000000384"],
            )
            .await;
            assert!(drifted.status.success(), "{drifted:?}");
            let text = String::from_utf8(drifted.stdout).unwrap();
            for expected in [
                "Usage: 287508 tokens, derived-from-components (1 of 1 scraped attempt(s) over 1 member task(s), advisory adapter captures)",
                "fresh input 265210 (= input 83 + cache write 265127)  cache read --  output 22298 (reasoning -- nested inside)",
                "partial: partial-components",
            ] {
                assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
            }
            server.await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn human_query_output_carries_no_daemon_sourced_terminal_control() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                for _ in 0..3 {
                    let (stream, _) = listener.accept().await.unwrap();
                    serve_connection(stream, HostileQueryHandler).await.unwrap();
                }
            });

            let run = run_tally(
                &socket,
                &["query", "run", "00000000-0000-4000-8000-000000000262"],
            )
            .await;
            assert!(run.status.success(), "{run:?}");
            let stdout = String::from_utf8(run.stdout).unwrap();
            let stderr = String::from_utf8(run.stderr).unwrap();
            for (surface, text) in [("stdout", &stdout), ("stderr", &stderr)] {
                assert!(
                    !text.contains(['\u{1b}', '\u{9b}', '\u{7}', '\u{202e}']),
                    "terminal control reached {surface}:\n{text:?}"
                );
            }
            // The identity fields survive as readable text rather than being
            // dropped along with the control they carried.
            for expected in [
                "spec-build",
                "crm",
                "00000000-0000-4000-8000-000000000262",
                "needs-attention",
                "blocked",
                "agent2J-t02",
                "cleanup-t02",
                "running",
                "/tmp/tally/crm.t02.err",
                "actionable failure",
            ] {
                assert!(
                    stdout.contains(expected),
                    "missing {expected:?} in:\n{stdout}"
                );
            }

            let log = run_tally(&socket, &["query", "log"]).await;
            assert!(log.status.success(), "{log:?}");
            let stdout = String::from_utf8(log.stdout).unwrap();
            let stderr = String::from_utf8(log.stderr).unwrap();
            for (surface, text) in [("stdout", &stdout), ("stderr", &stderr)] {
                assert!(
                    !text.contains(['\u{1b}', '\u{9b}', '\u{7}', '\u{202e}']),
                    "terminal control reached {surface}:\n{text:?}"
                );
            }
            for expected in [
                "2026-08-01T10:00:01.000Z",
                "adapter=codex",
                "pool=campaign-agent,sl2Jot",
                "pass",
            ] {
                assert!(
                    stdout.contains(expected),
                    "missing {expected:?} in:\n{stdout}"
                );
            }
            // The human path follows the cursor rather than handing one over,
            // so both hostile pages land here and neither is announced as
            // unfinished business.
            assert_eq!(stdout.lines().count(), 4, "{stdout}");
            assert!(!stderr.contains("--cursor"), "{stderr}");

            // The pagination hint is an operator instruction on stderr and is
            // sanitized on the same terms as the rows. It now lives on the
            // single-page surface, where the caller owns the cursor: the
            // control-laden cursor reaches the reader compacted or not at all.
            let json = run_tally(&socket, &["query", "log", "--json"]).await;
            assert!(json.status.success(), "{json:?}");
            let stderr = String::from_utf8(json.stderr).unwrap();
            assert!(
                !stderr.contains(['\u{1b}', '\u{9b}', '\u{7}', '\u{202e}']),
                "terminal control reached stderr:\n{stderr:?}"
            );
            assert!(
                stderr.contains("--cursor page-v1:log"),
                "missing the cursor hint in:\n{stderr}"
            );
            server.await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_human_query_whose_reader_hangs_up_exits_without_a_panic() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                // The client dies mid-response-consumption; the connection
                // ending under the server is the point of the test.
                let _ = serve_connection(stream, FloodQueryHandler).await;
            });

            let mut child = Command::new(env!("CARGO_BIN_EXE_tally"))
                .arg("--socket")
                .arg(&socket)
                .args(["query", "run", "00000000-0000-4000-8000-000000000262"])
                .env_remove("TALLY_JOB_ID")
                .env_remove("TALLY_JOB_TOKEN")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();

            // `head -1`: read one line, then close the read end.
            let mut reader = BufReader::new(child.stdout.take().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).await.unwrap();
            assert!(first.starts_with("spec-build"), "{first:?}");
            drop(reader);

            let output = child.wait_with_output().await.unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            assert!(
                !stderr.contains("panicked"),
                "a hung-up reader panicked the CLI:\n{stderr}"
            );
            assert!(stderr.is_empty(), "unexpected stderr:\n{stderr}");
            // Exit 0 from the mapped BrokenPipe, and — the other half of the
            // contract — no signal. The CLI leaves the process-wide SIGPIPE
            // disposition ignored exactly as the runtime set it, which is what
            // `daemon run`, `__remote-executor`, and `__record-unit-exit`
            // depend on: had this path restored SIG_DFL for the process, the
            // kernel would have killed this write and the status below would
            // read signal 13 rather than code 0.
            let signal = std::os::unix::process::ExitStatusExt::signal(&output.status);
            assert_eq!(
                (output.status.code(), signal),
                (Some(0), None),
                "a hung-up reader must end the command quietly: {:?}",
                output.status
            );
            server.await.unwrap();
        })
        .await;
}

/// Run `tally` with a stdout whose reader is already gone: the deterministic
/// form of `| head -1` exiting before the first write, with no race to lose.
///
/// `Command::output()` is deliberately not used — it replaces the configured
/// stdout with a pipe of its own, which would quietly un-close the reader and
/// make the assertion vacuous.
async fn tally_writing_into_a_closed_pipe(
    args: &[&str],
    socket: Option<&Path>,
) -> std::process::Output {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut command = Command::new(env!("CARGO_BIN_EXE_tally"));
    if let Some(socket) = socket {
        command.arg("--socket").arg(socket);
    }
    command
        .args(args)
        .env_remove("TALLY_JOB_ID")
        .env_remove("TALLY_JOB_TOKEN")
        .stdout(Stdio::from(writer))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
        .wait_with_output()
        .await
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn submitting_commands_exit_quietly_when_their_reader_is_gone() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                for _ in 0..2 {
                    let (stream, _) = listener.accept().await.unwrap();
                    let _ = serve_connection(stream, SubmitHandler).await;
                }
            });

            // `enqueue` and `queue continue` share `submit_payload`, the print
            // site the first pass left on stock `println!`.
            for args in [
                vec!["enqueue", "--pool", "slot", "--", "true"],
                vec![
                    "queue",
                    "continue",
                    "00000000-0000-4000-8000-000000000141",
                    "--",
                    "true",
                ],
            ] {
                let output = tally_writing_into_a_closed_pipe(&args, Some(&socket)).await;
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                assert!(stderr.is_empty(), "{args:?} wrote to stderr:\n{stderr}");
                assert_eq!(
                    (
                        output.status.code(),
                        std::os::unix::process::ExitStatusExt::signal(&output.status)
                    ),
                    (Some(0), None),
                    "{args:?} did not end quietly: {:?}",
                    output.status
                );
            }
            server.await.unwrap();
        })
        .await;
}

/// A handler that is listening and refuses, so the "daemon is absent" case can
/// be told apart from "the drain itself failed".
#[derive(Clone, Copy)]
struct RefusingDrainHandler;

impl RpcHandler for RefusingDrainHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method, "queue.drain");
            Err(WireError::invalid("drain refused"))
        })
    }
}

/// A handler that is listening, connects, and then never answers
/// `queue.drain` — the busy-daemon shape whose client deadline #427 absorbs.
#[derive(Clone, Copy)]
struct HangingDrainHandler;

impl RpcHandler for HangingDrainHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method, "queue.drain");
            std::future::pending::<Result<Value, WireError>>().await
        })
    }
}

/// Issue #411: `tally-drain` runs every five seconds, so a daemon restart
/// under it is routine, and exiting 3 on that turned every deploy that touches
/// the daemon unit into a per-user unit failure the fleet's journal watcher
/// reports as if it were the SIGABRT burst it was built to catch.
///
/// The absorption is scoped to the socket-absent case only, which is what this
/// pins in all three directions: absent daemon exits 0 and still says so, the
/// same absence on any other verb still exits 3, and a daemon that is present
/// and refuses the drain still fails.
#[tokio::test(flavor = "current_thread")]
async fn a_periodic_drain_that_finds_no_daemon_is_not_a_failure() {
    let temp = tempfile::tempdir().unwrap();
    let absent = temp.path().join("absent.sock");

    let skipped = run_tally(&absent, &["daemon", "drain"]).await;
    assert_eq!(skipped.status.code(), Some(0), "{:?}", skipped.status);
    let stderr = String::from_utf8_lossy(&skipped.stderr).into_owned();
    assert!(
        stderr.contains(&format!(
            "daemon socket {} is unreachable",
            absent.display()
        )),
        "the absorbed case must still name itself:\n{stderr}"
    );
    // Absence, not emptiness: nothing may claim a drain happened.
    let stdout = String::from_utf8_lossy(&skipped.stdout).into_owned();
    assert!(stdout.is_empty(), "{stdout}");

    // The identical absence reached through another verb is untouched.
    let unreachable = run_tally(&absent, &["queue", "drain"]).await;
    assert_eq!(
        unreachable.status.code(),
        Some(3),
        "{:?}",
        unreachable.status
    );

    // A daemon that is listening and refuses is still a failure.
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                serve_connection(stream, RefusingDrainHandler)
                    .await
                    .unwrap();
            });
            let refused = run_tally(&socket, &["daemon", "drain"]).await;
            assert_eq!(refused.status.code(), Some(2), "{:?}", refused.status);
            server.await.unwrap();
        })
        .await;
}

/// Issue #427: the daemon is present — the connection is established — but
/// saturated enough that `queue.drain` cannot answer within the client's
/// deadline. On the coordinator this surfaced as ~52 `tally-drain.service`
/// failures in one day, every one self-healing on the next tick, because the
/// producer event files are durable on disk and the next drain picks them up.
/// The periodic drain therefore records a retryable skip — systemd success
/// plus the warning line — and the pin holds the scope in all three
/// directions: the skip happens, the identical hang through `queue drain`
/// still fails, and the predicate unit tests hold every other
/// established-connection error outside the skip.
#[tokio::test(flavor = "current_thread")]
async fn a_drain_whose_deadline_expires_on_a_busy_daemon_is_a_retryable_skip() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                // Never returns on its own: the handler is hung, and this
                // server is aborted once the client side is proven.
                let _ = serve_connection(stream, HangingDrainHandler).await;
            });

            // The periodic drain records a skip: exit 0, and the line naming
            // the case is still written so an operator running the verb by
            // hand still sees which one it was.
            let skipped = run_tally(&socket, &["--rpc-timeout-sec", "1", "daemon", "drain"]).await;
            assert_eq!(skipped.status.code(), Some(0), "{:?}", skipped.status);
            let stderr = String::from_utf8_lossy(&skipped.stderr).into_owned();
            assert!(
                stderr.contains("RPC method queue.drain exceeded its 1s deadline"),
                "the absorbed case must still name itself:\n{stderr}"
            );
            assert!(
                stderr.contains("retryable skip"),
                "the absorbed case must say why it is safe:\n{stderr}"
            );
            // A skip, not a drain: nothing may claim a result happened.
            let stdout = String::from_utf8_lossy(&skipped.stdout).into_owned();
            assert!(stdout.is_empty(), "{stdout}");

            // The identical hang reached through the manual verb is untouched:
            // the skip belongs to the periodic spelling alone.
            let failed = run_tally(&socket, &["--rpc-timeout-sec", "1", "queue", "drain"]).await;
            assert_eq!(failed.status.code(), Some(1), "{:?}", failed.status);

            server.abort();
        })
        .await;
}

#[tokio::test]
async fn a_closed_stream_never_panics_the_help_or_the_error_printer() {
    let temp = tempfile::tempdir().unwrap();
    let absent = temp.path().join("absent.sock");

    // No arguments: clap writes the help text itself, so the mapping has to
    // reach a writer this crate does not own.
    let help = tally_writing_into_a_closed_pipe(&[], None).await;
    let stderr = String::from_utf8_lossy(&help.stderr).into_owned();
    assert!(stderr.is_empty(), "help wrote to stderr:\n{stderr}");
    assert_eq!(help.status.code(), Some(0), "{:?}", help.status);

    // A failing command whose *stderr* is the closed stream: the last-resort
    // printer in `cli::main` has no `Result` to return, so it drops the line
    // rather than panicking — and the exit code stays the error's own (3,
    // daemon unreachable), never the panic's 101 and never a silent 0.
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let unreachable = Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("--socket")
        .arg(&absent)
        .args(["query", "run", "00000000-0000-4000-8000-000000000262"])
        .stdout(Stdio::null())
        .stderr(Stdio::from(writer))
        .spawn()
        .unwrap()
        .wait_with_output()
        .await
        .unwrap();
    assert_eq!(
        unreachable.status.code(),
        Some(3),
        "{:?}",
        unreachable.status
    );
}

#[tokio::test]
async fn witness_verify_json_is_complete_and_red_exits_nonzero() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/fixtures/ledger");
    let temp = tempfile::tempdir().unwrap();
    let attestations = temp.path().join("attestations.jsonl");
    let verify = |name: &str| {
        let fixtures = fixtures.clone();
        let attestations = attestations.clone();
        let name = name.to_owned();
        async move {
            Command::new(env!("CARGO_BIN_EXE_tally"))
                .args(["witness", "verify"])
                .arg(fixtures.join(name))
                .arg("--attestations")
                .arg(attestations)
                .args(["--format", "json"])
                .output()
                .await
                .unwrap()
        }
    };

    let valid = verify("valid.jsonl").await;
    assert!(valid.status.success());
    let valid_json: Value = serde_json::from_slice(&valid.stdout).unwrap();
    assert_eq!(valid_json["schemaVersion"], 2);
    assert_eq!(valid_json["protocolVersion"], 5);
    assert_eq!(valid_json["ok"], true);
    assert_eq!(valid_json["chains"]["verdict"]["report"]["records"], 4);
    assert_eq!(valid_json["chains"]["verdict"]["chainHead"]["seq"], 4);

    let tampered = verify("tampered.jsonl").await;
    assert!(!tampered.status.success());
    let tampered_json: Value = serde_json::from_slice(&tampered.stdout).unwrap();
    assert_eq!(tampered_json["ok"], false);
    assert!(tampered_json["chains"]["verdict"]["report"]["problems"]
        .as_array()
        .is_some_and(|problems| !problems.is_empty()));

    let old_format = verify("old-format.jsonl").await;
    assert!(!old_format.status.success());
    let old_format_json: Value = serde_json::from_slice(&old_format.stdout).unwrap();
    assert_eq!(old_format_json["ok"], false);
    assert!(old_format_json["chains"]["verdict"]["report"]["problems"]
        .as_array()
        .is_some_and(|problems| problems
            .iter()
            .any(|problem| { problem["kind"] == "schema-version-invalid" })));
}

/// A daemon stand-in that pages a synthetic lifecycle window through the real
/// `PageCache`, so the CLI faces exactly the byte cap, page cursors, and
/// expiry the daemon produces. `expire_after` drops the snapshot cache after
/// N page calls to force a mid-window `CursorExpired`.
#[derive(Clone)]
struct PagedLogHandler {
    envelope: Value,
    cache: Arc<Mutex<tally_core::pagination::PageCache>>,
    calls: Arc<Mutex<usize>>,
    expire_after: Option<usize>,
}

impl PagedLogHandler {
    fn new(envelope: Value, expire_after: Option<usize>) -> Self {
        Self {
            envelope,
            cache: Arc::new(Mutex::new(tally_core::pagination::PageCache::default())),
            calls: Arc::new(Mutex::new(0)),
            expire_after,
        }
    }
}

impl RpcHandler for PagedLogHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            let params = request.params.unwrap_or_default();
            let cursor = params["cursor"].as_str().map(ToOwned::to_owned);
            let served = {
                let mut calls = self.calls.lock().unwrap();
                *calls += 1;
                *calls
            };
            let page = self.cache.lock().unwrap().page(
                &request.method,
                "fixture",
                params["limit"].as_u64().map(|limit| limit as usize),
                cursor.as_deref(),
                cursor.is_none().then(|| self.envelope.clone()),
            );
            if self.expire_after == Some(served) {
                // Evicting every snapshot is what the daemon does under
                // pressure; the next continuation must fail, not lie.
                *self.cache.lock().unwrap() = tally_core::pagination::PageCache::default();
            }
            page.map_err(|error| match error {
                tally_core::pagination::PaginationError::CursorExpired => {
                    WireError::not_found(error.to_string())
                }
                other => WireError::new(tally_client::WireErrorCode::Internal, other.to_string()),
            })
        })
    }
}

fn synthetic_lifecycle_window(items: usize) -> Value {
    let items = (0..items)
        .map(|index| {
            serde_json::json!({
                "origin": "journal",
                "eventId": format!("lifecycle:{index:020}"),
                "cursor": format!("lifecycle:{index:020}"),
                "timestamp": format!("2026-08-01T10:{:02}:{:02}.000Z", index / 60, index % 60),
                "event": "heartbeat",
                "taskUuid": format!("00000000-0000-4000-8000-{index:012}"),
                "taskRef": format!("crm/t{index:03}"),
                "nodeLabel": format!("agent-t{index:03}"),
                "attempt": 1,
                "authority": "tally-lifecycle-observation",
                "provenance": "durable-lifecycle-history",
                // Padding sized so the 48 KiB response cap, not the item
                // limit, decides where pages break.
                "stderrTail": "s".repeat(1_024),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "schemaVersion": 1,
        "protocolVersion": 5,
        "items": items,
        "nextCursor": null,
        "position": "log-v1:00000000000000000600:00000000000000000000",
        "snapshot": {"createdAt": "2026-08-01T10:30:00.000Z"},
    })
}

/// #316/#247: the human view of a long flow run must show the whole window,
/// not the first capped page with no indication that anything was withheld.
#[tokio::test(flavor = "current_thread")]
async fn human_query_log_follows_cursors_across_the_byte_cap_and_prints_the_whole_window() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let handler = PagedLogHandler::new(synthetic_lifecycle_window(600), None);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                serve_connection(stream, handler).await.unwrap();
            });
            let output = run_tally(
                &socket,
                &[
                    "query",
                    "log",
                    "--flow-run",
                    "00000000-0000-4000-8000-000000000045",
                ],
            )
            .await;
            assert!(output.status.success(), "{output:?}");
            let stdout = String::from_utf8(output.stdout).unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert_eq!(
                stdout.lines().count(),
                600,
                "the human window was short:\n{stderr}"
            );
            assert!(stdout.contains("crm/t000"));
            assert!(
                stdout.contains("crm/t599"),
                "the tail of the window is missing"
            );
            // More than one page was needed, and the reader is told so rather
            // than being handed a cursor to chase.
            assert!(
                stderr.contains("assembled from") && stderr.contains("pages"),
                "{stderr}"
            );
            assert!(!stderr.contains("continue with --cursor"), "{stderr}");
            assert!(!stderr.contains("INCOMPLETE"), "{stderr}");
            assert!(stderr.contains("position: log-v1:"), "{stderr}");
            server.await.unwrap();
        })
        .await;
}

/// Killing completeness mid-window must produce an explicit notice, never a
/// silently short list.
#[tokio::test(flavor = "current_thread")]
async fn human_query_log_restarts_once_and_says_so_when_the_page_cursor_expires() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let handler = PagedLogHandler::new(synthetic_lifecycle_window(200), Some(1));
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                serve_connection(stream, handler).await.unwrap();
            });
            let output = run_tally(&socket, &["query", "log"]).await;
            let stdout = String::from_utf8(output.stdout).unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(output.status.success(), "stdout={stdout} stderr={stderr}");
            assert!(
                stderr.contains("the page cursor expired mid-window"),
                "the restart was silent:\n{stderr}"
            );
            assert_eq!(stdout.lines().count(), 200, "{stderr}");
            server.await.unwrap();
        })
        .await;
}

/// The oversized-item case: `query jobs` on a run holding one monstrous row
/// succeeds, marks the elision, and exits 0. Before #316 the same input was a
/// hard `one collection item exceeds the bounded response size` failure.
#[tokio::test(flavor = "current_thread")]
async fn query_jobs_serves_an_oversized_item_with_a_marked_elision_and_exits_zero() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let envelope = serde_json::json!({
        "schemaVersion": 1,
        "protocolVersion": 5,
        "items": [
            {
                "anchor": "00000000-0000-4000-8000-000000000045",
                "liveState": "running",
                "argv": ["tally", "campaign", "run", "--brief", "b".repeat(120 * 1024)],
            },
            {"anchor": "00000000-0000-4000-8000-000000000046", "liveState": "queued"},
        ],
        "nextCursor": null,
        "snapshot": {"createdAt": "2026-08-01T10:30:00.000Z"},
    });
    let handler = PagedLogHandler::new(envelope, None);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                serve_connection(stream, handler).await.unwrap();
            });
            let output = run_tally(
                &socket,
                &[
                    "query",
                    "jobs",
                    "--flow-run",
                    "00000000-0000-4000-8000-000000000045",
                ],
            )
            .await;
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(output.status.success(), "{stderr}");
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            let items = value["items"].as_array().unwrap();
            assert_eq!(items.len(), 2, "the oversized row destroyed the page");
            assert_eq!(items[0]["anchor"], "00000000-0000-4000-8000-000000000045");
            assert_eq!(items[0]["liveState"], "running");
            assert_eq!(items[0]["elided"]["fields"], serde_json::json!(["/argv/4"]));
            assert_eq!(value["elidedItems"], 1);
            assert_eq!(value["truncated"], false);
            assert!(value["nextCursor"].is_null());
            assert!(
                stderr.contains("exceeded the bounded response size"),
                "{stderr}"
            );
            server.await.unwrap();
        })
        .await;
}

/// `--json` keeps single-page semantics — the caller owns the cursor — but the
/// envelope and stderr both say the page is not the window.
#[tokio::test(flavor = "current_thread")]
async fn json_query_log_keeps_one_page_and_marks_it_truncated() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let handler = PagedLogHandler::new(synthetic_lifecycle_window(600), None);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                serve_connection(stream, handler).await.unwrap();
            });
            let output = run_tally(&socket, &["query", "log", "--json"]).await;
            assert!(output.status.success(), "{output:?}");
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(value["items"].as_array().unwrap().len() < 600);
            assert_eq!(value["truncated"], true);
            assert!(value["nextCursor"].is_string());
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(stderr.contains("one page of a larger window"), "{stderr}");
            server.await.unwrap();
        })
        .await;
}

/// The intersection of #315 and #316: the window-walking human path buffers
/// every page and then prints hundreds of lines at once, so it is exactly the
/// shape that turns a hung-up reader into a panic. It must stay a quiet exit
/// 0, and the stderr notices that follow the rows must not start speaking
/// after the reader has gone.
#[tokio::test(flavor = "current_thread")]
async fn a_walked_window_whose_reader_hangs_up_exits_quietly_without_notices() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let handler = PagedLogHandler::new(synthetic_lifecycle_window(600), None);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                // The client dies mid-window; the connection ending under the
                // server is part of what is being exercised.
                let _ = serve_connection(stream, handler).await;
            });

            let mut child = Command::new(env!("CARGO_BIN_EXE_tally"))
                .arg("--socket")
                .arg(&socket)
                .args([
                    "query",
                    "log",
                    "--flow-run",
                    "00000000-0000-4000-8000-000000000045",
                ])
                .env_remove("TALLY_JOB_ID")
                .env_remove("TALLY_JOB_TOKEN")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();

            let mut reader = BufReader::new(child.stdout.take().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).await.unwrap();
            assert!(first.contains("crm/t000"), "{first:?}");
            drop(reader);

            let output = child.wait_with_output().await.unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            assert!(
                !stderr.contains("panicked"),
                "a hung-up reader panicked the CLI:\n{stderr}"
            );
            // Not even the multi-page notice: the reader stopped reading, so
            // the command stops talking on both surfaces.
            assert!(stderr.is_empty(), "unexpected stderr:\n{stderr}");
            let signal = std::os::unix::process::ExitStatusExt::signal(&output.status);
            assert_eq!(
                (output.status.code(), signal),
                (Some(0), None),
                "a hung-up reader must end the command quietly: {:?}",
                output.status
            );
            server.await.unwrap();
        })
        .await;
}

/// #316 acceptance bullet 2's second half, which had no CLI-level coverage: an
/// item too large to *elide* — its bulk is structure, not text — is still a
/// hard failure, and the CLI must name it rather than passing the daemon's
/// opaque internal error through. Anything less and an operator cannot tell
/// this apart from a dead daemon.
#[tokio::test(flavor = "current_thread")]
async fn query_jobs_names_the_item_it_cannot_render_and_exits_nonzero() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    // Twenty thousand small objects: far past the 48 KiB cap, and with no
    // string leaf long enough for elision to reach.
    let structural = (0..20_000)
        .map(|index| serde_json::json!({"n": index}))
        .collect::<Vec<_>>();
    let envelope = serde_json::json!({
        "schemaVersion": 1,
        "protocolVersion": 5,
        "items": [{"anchor": "00000000-0000-4000-8000-000000000045", "rows": structural}],
        "nextCursor": null,
        "snapshot": {"createdAt": "2026-08-01T10:30:00.000Z"},
    });
    let handler = PagedLogHandler::new(envelope, None);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                serve_connection(stream, handler).await.unwrap();
            });
            let output = run_tally(
                &socket,
                &[
                    "query",
                    "jobs",
                    "--flow-run",
                    "00000000-0000-4000-8000-000000000045",
                ],
            )
            .await;
            let stderr = String::from_utf8(output.stderr.clone()).unwrap();
            assert!(
                !output.status.success(),
                "an unrenderable item must not be reported as success: {stderr}"
            );
            assert_eq!(output.status.code(), Some(1), "{stderr}");
            for expected in [
                "query.jobs",
                "could not render one item within the bounded response size",
                "eliding its largest text fields",
                "--limit 1",
            ] {
                assert!(
                    stderr.contains(expected),
                    "missing {expected:?} in:\n{stderr}"
                );
            }
            assert!(
                output.stdout.is_empty(),
                "a failed query printed a partial envelope: {:?}",
                String::from_utf8_lossy(&output.stdout)
            );
            server.await.unwrap();
        })
        .await;
}

/// #247 repair, at the surface an operator actually reads: a `--flow-run`
/// window that resolved to no member tasks must say so. Since #380 made
/// membership a durable admission fact, the commonest cause of a zero is a
/// mistyped or stale run ID -- but the notice must not close the question,
/// because a repaired or deleted ledger, a compacted-out idle run, and a
/// degraded admission all produce a zero for a run that really did admit work,
/// and a row-less node has no durable row to fall back on in any of them.
#[tokio::test(flavor = "current_thread")]
async fn an_empty_flow_run_window_says_the_run_resolved_to_no_members() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let envelope = serde_json::json!({
        "schemaVersion": 1,
        "protocolVersion": 5,
        "items": [],
        "nextCursor": null,
        "position": "log-v1:00000000000000000041:00000000000000000007",
        "flowRunTasks": 0,
        "snapshot": {"createdAt": "2026-08-01T10:30:00.000Z"},
    });
    let handler = PagedLogHandler::new(envelope, None);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                for _ in 0..2 {
                    let (stream, _) = listener.accept().await.unwrap();
                    serve_connection(stream, handler.clone()).await.unwrap();
                }
            });
            for args in [
                vec![
                    "query",
                    "log",
                    "--flow-run",
                    "00000000-0000-4000-8000-000000000045",
                ],
                // The single-page surface owes the reader the same warning.
                vec![
                    "query",
                    "log",
                    "--flow-run",
                    "00000000-0000-4000-8000-000000000045",
                    "--json",
                ],
            ] {
                let output = run_tally(&socket, &args).await;
                let stderr = String::from_utf8(output.stderr).unwrap();
                assert!(output.status.success(), "{args:?}: {stderr}");
                assert!(
                    stderr.contains("resolves to NO member tasks"),
                    "{args:?} presented an empty window as authoritative:\n{stderr}"
                );
                assert!(
                    stderr.contains("not evidence that the run is quiet"),
                    "{args:?} presented a zero as proof the run is idle:\n{stderr}"
                );
                assert!(
                    stderr.contains("membershipDegraded")
                        && stderr.contains("flow-membership.jsonl"),
                    "{args:?} did not name the states in which a zero is the daemon \
                     having lost membership rather than the run having none:\n{stderr}"
                );
            }
            server.await.unwrap();
        })
        .await;
}
