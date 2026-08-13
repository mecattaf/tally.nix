use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::{json, Value};
use tally_client::{RequestFrame, WireError};
use tally_core::campaign_registry::{
    CampaignRegistration, CampaignRegistrationV3, CampaignRegistry, REGISTRY_SCHEMA_VERSION,
};
use tally_core::wire::{serve_connection, RpcHandler};
use tokio::net::UnixListener;
use tokio::process::Command;

const ISSUE_URL: &str = "local://acme/widgets/specs/night/tasks.json";
const CODE_REPOSITORY: &str = "acme/widgets";
const WORKLIST: &str = "specs/night/tasks.json";
const REGISTRATION_ID: &str = "0198a62b-41ee-7000-8000-000000000542";
const OBSERVATION: &str = "sha256:fixture-observation-b";
const OLD_RUN: &str = "00000000-0000-7000-8000-000000000510";
const LATEST_RUN: &str = "00000000-0000-7000-8000-000000000520";

fn write_registration(state_dir: &Path, fixture_dir: &Path) {
    std::fs::create_dir_all(fixture_dir).unwrap();
    let flow = fixture_dir.join("spec-build.js");
    let driver = fixture_dir.join("spec-build-driver");
    std::fs::write(&flow, "fixture flow\n").unwrap();
    std::fs::write(&driver, "fixture driver\n").unwrap();
    let authority = CampaignRegistrationV3 {
        schema_version: REGISTRY_SCHEMA_VERSION,
        registration_id: REGISTRATION_ID.to_owned(),
        worklist_pattern: WORKLIST.to_owned(),
        code_repository: CODE_REPOSITORY.to_owned(),
        armed_at: "2026-08-12T20:00:00Z".to_owned(),
        arm_serial: 1,
        approved_graph_digest: format!("sha256:{}", "a".repeat(64)),
        // SAFETY: `geteuid` has no preconditions and does not mutate process state.
        local_actor: format!("uid:{}", unsafe { libc::geteuid() }),
        allowed_actors: vec!["operator".to_owned()],
        last_observation: Some(OBSERVATION.to_owned()),
        flow,
        driver,
        workspace_root: PathBuf::from("/var/lib/tally/campaigns"),
    };
    CampaignRegistry::open(state_dir)
        .unwrap()
        .write(&mut CampaignRegistration::new(authority, None))
        .unwrap();
}

fn usage() -> Value {
    json!({
        "authority": "advisory-provider-capture",
        "provenance": "fixture",
        "composition": "fixture",
        "coverage": {
            "tasks": 2,
            "attemptsExpected": 2,
            "attemptsAttested": 2,
            "attemptsObserved": 2,
            "attemptsReported": 2,
            "ledgerVerified": true
        },
        "tokens": {
            "inputTokens": {"value": 250, "attempts": 2},
            "cacheReadTokens": {"value": 0, "attempts": 0},
            "cacheWriteTokens": {"value": 0, "attempts": 0},
            "outputTokens": {"value": 50, "attempts": 2},
            "reasoningTokens": {"value": 0, "attempts": 0},
            "freshInputTokens": {
                "value": 250,
                "attemptsComplete": 2,
                "attemptsPartial": 0
            },
            "totalTokens": {
                "value": 300,
                "attempts": 2,
                "source": "derived-from-components"
            }
        },
        "cost": {"attempts": 0, "basis": "fixture"},
        "isComplete": true,
        "caveats": []
    })
}

#[derive(Clone, Copy)]
struct CampaignStatusHandler;

impl RpcHandler for CampaignStatusHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method, "__campaign.status");
            let params = request.params.unwrap();
            assert_eq!(params["issueUrl"], ISSUE_URL);
            assert_eq!(params["registrationId"], REGISTRATION_ID);
            assert_eq!(params["latestObservation"], OBSERVATION);
            Ok(json!({
                "schemaVersion": 1,
                "protocolVersion": 5,
                "issueUrl": ISSUE_URL,
                "registered": true,
                "registrationId": REGISTRATION_ID,
                "latestObservation": OBSERVATION,
                "flowRunId": LATEST_RUN,
                "flowRuns": [OLD_RUN, LATEST_RUN],
                "state": "running",
                "flowName": "spec-build",
                "campaign": "fixture-campaign",
                "repository": "acme/widgets",
                "counts": {"done": 1, "running": 1, "blocked": 0, "pending": 0},
                "usage": usage(),
                "items": [],
                "tasks": [
                    {
                        "taskRef": "fixture-registration/task-a",
                        "title": "Completed task",
                        "status": "done",
                        "blockedBy": []
                    },
                    {
                        "taskRef": "fixture-registration/task-b",
                        "title": "Live task",
                        "status": "running",
                        "blockedBy": [],
                        "currentNode": "agent-task-b"
                    }
                ],
                "anomalies": [],
                "currentNodes": [],
                "failures": [],
                "snapshot": {}
            }))
        })
    }
}

#[derive(Clone, Copy)]
struct SupersededRunHandler;

impl RpcHandler for SupersededRunHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method, "query.run");
            assert_eq!(request.params.unwrap()["id"], OLD_RUN);
            Ok(json!({
                "schemaVersion": 1,
                "protocolVersion": 5,
                "flowRunId": OLD_RUN,
                "flowName": "spec-build",
                "campaign": "fixture-campaign",
                "state": "superseded",
                "campaignSupersededBy": {
                    "issueUrl": ISSUE_URL,
                    "latestFlowRunId": LATEST_RUN,
                    "latestObservation": OBSERVATION
                },
                "counts": {"done": 1, "running": 0, "blocked": 0, "pending": 0},
                "items": [],
                "currentNodes": [],
                "failures": [],
                "snapshot": {}
            }))
        })
    }
}

async fn run_tally(socket: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("--socket")
        .arg(socket)
        .args(args)
        .env_remove("TALLY_JOB_ID")
        .env_remove("TALLY_JOB_TOKEN")
        .output()
        .await
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn campaign_status_joins_fixture_registration_to_latest_lineage_and_one_usage_total() {
    let temporary = tempfile::tempdir().unwrap();
    let state_dir = temporary.path().join("state");
    write_registration(&state_dir, &temporary.path().join("assets"));
    let socket = temporary.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                serve_connection(stream, CampaignStatusHandler)
                    .await
                    .unwrap();
            });
            let output = run_tally(
                &socket,
                &[
                    "campaign",
                    "status",
                    CODE_REPOSITORY,
                    WORKLIST,
                    "--state-dir",
                    state_dir.to_str().unwrap(),
                ],
            )
            .await;
            assert!(output.status.success(), "{output:?}");
            let stdout = String::from_utf8(output.stdout).unwrap();
            for expected in [
                "Campaign fixture-campaign  running",
                "Registration: 0198a62b-41ee-7000-8000-000000000542 (armed)",
                "Observation: sha256:fixture-observation-b",
                "Latest flow run: 00000000-0000-7000-8000-000000000520 (2 passes)",
                "Campaign usage: 300 tokens, derived-from-components",
                "fixture-registration/task-a",
                "fixture-registration/task-b",
            ] {
                assert!(
                    stdout.contains(expected),
                    "missing {expected:?} in:\n{stdout}"
                );
            }
            assert_eq!(
                stdout
                    .lines()
                    .filter(|line| line.contains("usage:") && line.contains("tokens"))
                    .count(),
                1,
                "status must print one campaign total, not a per-run scatter:\n{stdout}"
            );
            server.await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn query_run_prints_the_latest_campaign_descendant_pointer() {
    let temporary = tempfile::tempdir().unwrap();
    let socket = temporary.path().join("tally.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = tokio::task::spawn_local(async move {
                let (stream, _) = listener.accept().await.unwrap();
                serve_connection(stream, SupersededRunHandler)
                    .await
                    .unwrap();
            });
            let output = run_tally(&socket, &["query", "run", OLD_RUN]).await;
            assert!(output.status.success(), "{output:?}");
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert!(
                stdout.contains(&format!(
                    "SUPERSEDED — campaign advanced; latest flow run {LATEST_RUN}"
                )),
                "{stdout}"
            );
            server.await.unwrap();
        })
        .await;
}
