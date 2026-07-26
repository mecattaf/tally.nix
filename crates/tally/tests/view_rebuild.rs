use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use tally_client::RpcClient;
use tally_core::adapters::AdapterConfig;
use tally_core::config::{
    CoResidencyPredicate, Config, JournaldConfig, PoolConfig, PoolPredicate, ResourceKind,
};
use tally_core::daemon::{Daemon, DaemonError, DaemonPaths, DaemonSettings};
use tally_core::evidence::RetryPolicy;
use tally_core::executor::{
    ExecutionPaths, Executor, ExecutorError, LocalUnitFact, LocalUnitProbe, UnitLimits,
};
use tally_core::recovery::RecoveryPolicy;
use tally_core::taskdb::{
    write_enqueue_event_atomic, AdmissionOrigin, DurableEnqueueEvent, EnqueueSource, RowSeed,
    TaskDb, CURRENT_ROW_VERSION, TASKDATA_DIRECTORY,
};
use tally_core::witness::{LaborClass, Verdict, WitnessBody, WitnessLedger};
use tally_core::Priority;
use tokio::process::Command;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

struct AbsentUnitProbe;

impl LocalUnitProbe for AbsentUnitProbe {
    fn inspect(&self, unit: &str, _paths: &ExecutionPaths) -> Result<LocalUnitFact, ExecutorError> {
        Ok(LocalUnitFact::absent(unit))
    }
}

struct RunningDaemon {
    shutdown: watch::Sender<bool>,
    task: JoinHandle<Result<(), DaemonError>>,
}

impl RunningDaemon {
    async fn stop(self) {
        self.shutdown.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(10), self.task)
            .await
            .expect("daemon shutdown timed out")
            .expect("daemon task panicked")
            .expect("daemon shutdown failed");
    }
}

fn paths(root: &Path) -> DaemonPaths {
    DaemonPaths {
        socket: root.join("run/tally.sock"),
        state_dir: root.join("xdg-state/tally"),
        data_dir: root.join("data"),
    }
}

fn config() -> Config {
    Config {
        pools: BTreeMap::from([(
            "slot".to_owned(),
            PoolConfig {
                resource: ResourceKind::BuildSlot,
                capacity: 1,
                predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                ..PoolConfig::default()
            },
        )]),
        adapters: BTreeMap::from([("shell".to_owned(), AdapterConfig::default())]),
        journald: JournaldConfig { native: false },
        ..Config::default()
    }
}

fn settings() -> DaemonSettings {
    DaemonSettings {
        unit_limits: UnitLimits {
            cpu_weight: 100,
            memory_max_bytes: 64 * 1024 * 1024,
        },
        yield_grace: Duration::from_secs(1),
        recovery_policy: RecoveryPolicy {
            retry: RetryPolicy {
                auto_pool_return: false,
                auto_resource_return: false,
                auto_bounded_requeue: false,
            },
            max_attempts: 2,
        },
    }
}

fn row(uuid: Uuid) -> RowSeed {
    RowSeed {
        row_version: CURRENT_ROW_VERSION,
        uuid,
        description: "rebuild golden row".to_owned(),
        priority: Priority::High,
        source: EnqueueSource::Manual,
        adapter: "shell".to_owned(),
        pools: vec!["slot".to_owned()],
        executor: None,
        model: None,
        cwd: None,
        workspace: None,
        adapter_options: Default::default(),
        gate_manifest: None,
        resumed_from: None,
        dedup_key: Some("view:golden".to_owned()),
        payload_hash: None,
        brief_hash: None,
        orchestration: None,
        session_ref: None,
        final_message: None,
        lease_epoch: 1,
        attempt: 1,
        argv: vec!["true".to_owned()],
        evidence: vec!["exit:0".to_owned()],
        drv: None,
        parent_uuid: None,
        consumption_estimate: None,
        runtime_max_sec: None,
        no_enqueue: false,
        credentials: BTreeMap::new(),
        origin: Some(AdmissionOrigin::direct(EnqueueSource::Manual)),
        gh_origin: None,
        related_trigger: None,
        evidence_class: None,
        manifest_hash: None,
    }
}

fn seed_durable_facts(paths: &DaemonPaths, uuid: Uuid) {
    let event = DurableEnqueueEvent::new(row(uuid)).unwrap();
    write_enqueue_event_atomic(&paths.events_dir(), &event).unwrap();
    let mut ledger = WitnessLedger::open(paths.witness_path()).unwrap();
    let record = ledger
        .append(WitnessBody {
            task_uuid: Some(uuid.to_string()),
            transition_timestamp: "2026-07-26T18:30:00.000Z".to_owned(),
            verdict: Verdict::Pass,
            exit_code: 0,
            artifact_content_hash: None,
            store_paths: None,
            drv: None,
            gpu_seconds: None,
            wall_clock: 0.25,
            attempt: 1,
            lease_epoch: 1,
            dedup_key: Some("view:golden".to_owned()),
            payload_hash: None,
            brief_hash: None,
            origin: event.row.origin.unwrap(),
            orchestration: None,
            labor_class: LaborClass::Fresh,
            trace_ref: None,
            pools: vec!["slot".to_owned()],
            executor: None,
            host_id: None,
            charge: None,
            model: None,
            evidence_class: None,
            manifest_hash: None,
            completion: None,
            result_revision: None,
            authorship: None,
        })
        .unwrap();
    assert_eq!(record.seq, 1);
}

async fn start_daemon(paths: &DaemonPaths) -> RunningDaemon {
    let executor = Executor::new(&paths.state_dir, env!("CARGO_BIN_EXE_tally"))
        .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
        .with_unit_probe(AbsentUnitProbe);
    let daemon = Daemon::open_with_executor(config(), paths.clone(), settings(), executor)
        .await
        .unwrap();
    let (shutdown, receiver) = watch::channel(false);
    let task = tokio::task::spawn_local(daemon.run_until(receiver));
    RunningDaemon { shutdown, task }
}

async fn query_golden(socket: &Path) -> Value {
    let result = RpcClient::connect(socket)
        .await
        .unwrap()
        .call("query.jobs", Some(json!({"limit": 100})))
        .await
        .unwrap();
    let projected = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            json!({
                "taskUuid": item["taskUuid"],
                "rowStatus": item["rowStatus"],
                "terminalVerdict": item["terminalVerdict"],
                "terminalAttempt": item["terminalAttempt"],
                "currentAttempt": item["currentAttempt"],
                "leaseEpoch": item["leaseEpoch"],
                "laborClass": item["laborClass"],
            })
        })
        .collect::<Vec<_>>();
    Value::Array(projected)
}

async fn run_rebuild(root: &Path, data_dir: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tally"))
        .args(["view", "rebuild", "--data-dir"])
        .arg(data_dir)
        .arg("--yes")
        .env("XDG_STATE_HOME", root.join("xdg-state"))
        .output()
        .await
        .unwrap()
}

fn taskdata_archives(data_dir: &Path) -> Vec<PathBuf> {
    let mut archives = fs::read_dir(data_dir)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("taskdata.pre-rebuild-"))
                .then_some(path)
        })
        .collect::<Vec<_>>();
    archives.sort();
    archives
}

#[tokio::test(flavor = "current_thread")]
async fn view_rebuild_archives_and_reconstructs_the_query_golden() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let paths = paths(temp.path());
            let uuid = Uuid::parse_str("00000000-0000-4000-8000-000000000084").unwrap();
            seed_durable_facts(&paths, uuid);

            let daemon = start_daemon(&paths).await;
            let before_query = query_golden(&paths.socket).await;
            assert_eq!(
                before_query,
                json!([{
                    "taskUuid": uuid,
                    "rowStatus": "completed",
                    "terminalVerdict": "pass",
                    "terminalAttempt": 1,
                    "currentAttempt": 1,
                    "leaseEpoch": 1,
                    "laborClass": "fresh",
                }])
            );

            let locked = run_rebuild(temp.path(), &paths.data_dir).await;
            assert!(!locked.status.success());
            let locked_stderr = String::from_utf8(locked.stderr).unwrap();
            assert!(
                locked_stderr.contains(&paths.state_dir.join("daemon.lock").display().to_string())
            );
            assert!(locked_stderr.contains("while the daemon lock is held"));
            assert!(taskdata_archives(&paths.data_dir).is_empty());

            daemon.stop().await;
            let before_rows = {
                let mut db = TaskDb::open_read_only(&paths.data_dir.join(TASKDATA_DIRECTORY))
                    .await
                    .unwrap();
                db.all_rows().await.unwrap()
            };

            let rebuilt = run_rebuild(temp.path(), &paths.data_dir).await;
            assert!(
                rebuilt.status.success(),
                "{}",
                String::from_utf8_lossy(&rebuilt.stderr)
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&rebuilt.stdout).unwrap(),
                json!({
                    "rebuilt": true,
                    "rows": 1,
                    "witnessRecords": 1,
                })
            );
            let archives = taskdata_archives(&paths.data_dir);
            assert_eq!(archives.len(), 1);
            assert!(archives[0].join("taskchampion.sqlite3").is_file());

            let after_rows = {
                let mut db = TaskDb::open_read_only(&paths.data_dir.join(TASKDATA_DIRECTORY))
                    .await
                    .unwrap();
                db.all_rows().await.unwrap()
            };
            assert_eq!(after_rows, before_rows);

            let restarted = start_daemon(&paths).await;
            let after_query = query_golden(&paths.socket).await;
            assert_eq!(after_query, before_query);
            restarted.stop().await;

            let ledger = fs::read_to_string(paths.witness_path()).unwrap();
            assert_eq!(ledger.lines().count(), 1);
            assert!(ledger.contains("\"schemaVersion\":2"));
        })
        .await;
}
