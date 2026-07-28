use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tally_client::{RequestFrame, WireError};
use tally_core::wire::{serve_connection, RpcHandler};
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

async fn run_tally(socket: &Path, args: &[&str]) -> std::process::Output {
    run_tally_with_job_id(socket, args, None).await
}

async fn run_tally_with_job_id(
    socket: &Path,
    args: &[&str],
    job_id: Option<&str>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tally"));
    command
        .arg("--socket")
        .arg(socket)
        .args(args)
        .env_remove("TALLY_JOB_ID");
    if let Some(job_id) = job_id {
        command.env("TALLY_JOB_ID", job_id);
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
                for _ in 0..7 {
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
