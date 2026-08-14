use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tally_client::RpcClient;
use tally_core::adapters::AdapterConfig;
use tally_core::config::{
    CoResidencyPredicate, Config, JournaldConfig, PoolConfig, PoolPredicate, ResourceKind,
};
use tally_core::daemon::{
    Daemon, DaemonError, DaemonPaths, DaemonSettings, DEFAULT_MAX_CONNECTIONS,
};
use tally_core::evidence::RetryPolicy;
use tally_core::executor::{
    ExecutionPaths, Executor, ExecutorError, LocalUnitFact, LocalUnitProbe, UnitLimits,
};
use tally_core::recovery::RecoveryPolicy;
use tokio::process::Command;
use tokio::sync::watch;
use tokio::task::{JoinHandle, LocalSet};

#[path = "support/configured_tally.rs"]
mod configured_tally;

const EMPTY_CONFIG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/empty-config.json");

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

fn daemon_paths(root: &Path) -> DaemonPaths {
    DaemonPaths {
        socket: root.join("run/tally.sock"),
        state_dir: root.join("state"),
        data_dir: root.join("data"),
    }
}

fn config() -> Config {
    Config {
        pools: BTreeMap::from([(
            "slot".to_owned(),
            PoolConfig {
                resource: Some(ResourceKind::BuildSlot),
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
            max_attempts: 1,
        },
        max_connections: DEFAULT_MAX_CONNECTIONS,
    }
}

async fn start_daemon(paths: &DaemonPaths) -> RunningDaemon {
    let recorder = configured_tally::install(&paths.state_dir.join("configured-tally"));
    let executor = Executor::new(&paths.state_dir, recorder)
        .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
        .with_direct_fallback()
        .with_unit_probe(AbsentUnitProbe);
    let daemon = Daemon::open_with_executor(config(), paths.clone(), settings(), executor)
        .await
        .unwrap();
    let (shutdown, receiver) = watch::channel(false);
    let task = tokio::task::spawn_local(daemon.run_until(receiver));
    RunningDaemon { shutdown, task }
}

async fn enqueue(socket: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tally"))
        .args(["--config", EMPTY_CONFIG])
        .arg("--socket")
        .arg(socket)
        .arg("queue")
        .arg("enqueue")
        .args(args)
        .env_remove("TALLY_JOB_ID")
        .env_remove("TALLY_JOB_TOKEN")
        .output()
        .await
        .unwrap()
}

fn output_json(output: &std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn public_cli_defaults_keyed_enqueues_to_full_idempotency() {
    LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let paths = daemon_paths(temp.path());
            let daemon = start_daemon(&paths).await;
            let client = RpcClient::connect(&paths.socket).await.unwrap();
            client
                .call(
                    "queue.pause",
                    Some(serde_json::json!({"pool": "slot", "all": false})),
                )
                .await
                .unwrap();

            let first = enqueue(
                &paths.socket,
                &[
                    "--pool",
                    "slot",
                    "--dedup-key",
                    "public-cli:identical",
                    "--",
                    "true",
                ],
            );
            let second = enqueue(
                &paths.socket,
                &[
                    "--pool",
                    "slot",
                    "--dedup-key",
                    "public-cli:identical",
                    "--",
                    "true",
                ],
            );
            let (first, second) = tokio::join!(first, second);
            let first = output_json(&first);
            let second = output_json(&second);
            let dispositions = [
                first["disposition"].as_str(),
                second["disposition"].as_str(),
            ];
            assert!(dispositions.contains(&Some("created")));
            assert!(dispositions.contains(&Some("attached")));
            assert_eq!(first["task_uuid"], second["task_uuid"]);

            let conflict = enqueue(
                &paths.socket,
                &[
                    "--pool",
                    "slot",
                    "--dedup-key",
                    "public-cli:identical",
                    "--",
                    "false",
                ],
            )
            .await;
            assert_eq!(conflict.status.code(), Some(1));
            assert!(conflict.stdout.is_empty());
            let conflict_stderr = String::from_utf8_lossy(&conflict.stderr);
            assert!(
                conflict_stderr.contains("dedup-key-conflict"),
                "{conflict_stderr}"
            );

            let legacy_first = enqueue(
                &paths.socket,
                &[
                    "--pool",
                    "slot",
                    "--dedup-key",
                    "public-cli:legacy",
                    "--submission",
                    "legacy",
                    "--",
                    "true",
                ],
            )
            .await;
            let legacy_second = enqueue(
                &paths.socket,
                &[
                    "--pool",
                    "slot",
                    "--dedup-key",
                    "public-cli:legacy",
                    "--submission",
                    "legacy",
                    "--",
                    "false",
                ],
            )
            .await;
            let legacy_first = output_json(&legacy_first);
            let legacy_second = output_json(&legacy_second);
            assert_eq!(legacy_first["disposition"], "created");
            assert_eq!(legacy_second["disposition"], "created");
            assert_ne!(legacy_first["task_uuid"], legacy_second["task_uuid"]);

            daemon.stop().await;
        })
        .await;
}
