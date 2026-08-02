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
                        "protocolVersion": 4,
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
                    Ok(serde_json::json!({"schemaVersion": 1, "protocolVersion": 4}))
                }
                "query.run" => {
                    assert_eq!(
                        request.params.as_ref().unwrap()["id"],
                        "00000000-0000-4000-8000-000000000045"
                    );
                    Ok(serde_json::json!({
                        "schemaVersion": 1,
                        "protocolVersion": 4,
                        "flowRunId": "00000000-0000-4000-8000-000000000045",
                        "flowName": "spec-build",
                        "campaign": "crm",
                        "state": "running",
                        "counts": {"done": 1, "running": 1, "blocked": 0, "pending": 0},
                        "tasks": [],
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
                    assert_eq!(params["provenance"], false);
                    Ok(serde_json::json!({
                        "schemaVersion": 1,
                        "protocolVersion": 4,
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
                        "protocolVersion": 4,
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
                        "protocolVersion": 4,
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
                        "protocolVersion": 4,
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
                        "protocolVersion": 4,
                        "status": "ok",
                        "items": [{
                            "schemaVersion": 1,
                            "protocolVersion": 4,
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
                    "protocolVersion": 4,
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
                    "protocolVersion": 4,
                    "flowRunId": "00000000-0000-4000-8000-000000000262",
                    "flowName": "spec-build",
                    "campaign": "crm",
                    "repository": "mecattaf/tally.nix",
                    "state": "needs-attention",
                    "counts": {"done": 1, "running": 0, "blocked": 1, "pending": 0},
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
                        "attempt": 1, "leaseEpoch": 2, "timestamp": "2026-08-01T10:00:05Z"
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
                "query.log" => Ok(serde_json::json!({
                    "schemaVersion": 1,
                    "protocolVersion": 4,
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
                    "nextCursor": "\u{1b}[2Jpage-v1:log",
                    "snapshot": {}
                })),
                "query.run" => Ok(serde_json::json!({
                    "schemaVersion": 1,
                    "protocolVersion": 4,
                    "flowRunId": "\u{1b}[2J00000000-0000-4000-8000-000000000262",
                    "flowName": "\u{1b}[2Jspec-build",
                    "campaign": "c\u{1b}]0;pwned\u{7}rm",
                    "state": "needs-\u{1b}[31mattention",
                    "counts": {"done": 0, "running": 1, "blocked": 0, "pending": 0},
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
                "protocolVersion": 4,
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
    let success = Command::new(env!("CARGO_BIN_EXE_tally"))
        .args([
            "__record-unit-exit",
            "--record",
            record.to_str().unwrap(),
            "--unit",
            unit,
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
async fn query_v4_cli_forwards_all_durable_observability_commands() {
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
                assert_eq!(value["protocolVersion"], 4);
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
                // Failure-tail indentation survives; the six-space frame plus
                // the line's own four spaces.
                "\n          at gate.rs:1",
                // A hand-closed sub-issue is rendered above the board, not
                // buried in the pass projection.
                "!! ANOMALIES: 1 closed sub-issue(s) hold no merged proof",
                "https://example.test/issues/42",
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

#[tokio::test(flavor = "current_thread")]
async fn human_query_output_carries_no_daemon_sourced_terminal_control() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                for _ in 0..2 {
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
            // The pagination hint is an operator instruction on stderr and is
            // sanitized on the same terms as the rows.
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
    assert_eq!(valid_json["protocolVersion"], 4);
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
