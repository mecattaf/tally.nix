use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use serde_json::Value;
use tally_core::wire::{serve_connection, RequestFrame, RpcHandler, WireError};
use tokio::net::UnixListener;
use tokio::process::Command;

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
                method => Err(WireError::invalid(format!("unexpected method {method}"))),
            }
        })
    }
}

async fn run_tally(socket: &Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tally"));
    command.arg("--socket").arg(socket).args(args);
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
                    serve_connection(stream, &CliHandler).await.unwrap();
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
async fn cli_forwards_opaque_evidence_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                serve_connection(stream, &CliHandler).await.unwrap();
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
                    serve_connection(stream, &CliHandler).await.unwrap();
                }
            });
            let enqueued = run_tally(
                &socket,
                &["enqueue", "--pool", "zeta", "--pool", "slot", "--", "true"],
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
