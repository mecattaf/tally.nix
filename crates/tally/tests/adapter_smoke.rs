use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tally_core::adapters::{AdapterConfig, ScrapeCapture, ScrapeMode, ScrapeStream};
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
use tally_core::witness::read_verified_records;
use tokio::process::Command;
use tokio::sync::watch;
use tokio::task::{JoinHandle, LocalSet};

#[path = "support/shell_program.rs"]
mod shell_program;

const PRE_OUTPUT_FAILURE: &str =
    "Not inside a trusted directory and --skip-git-repo-check was not specified.";

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

fn smoke_config(root: &Path) -> Config {
    let structured = root.join("structured-adapter");
    shell_program::install(
        &structured,
        "#!/bin/sh\nprintf '%s\\n' 'TALLY_SESSION=smoke-session' 'TALLY_FINAL_MESSAGE=ok'\n",
    );
    let failing = root.join("pre-output-failure-adapter");
    shell_program::install(
        &failing,
        format!("#!/bin/sh\nprintf '%s\\n' '{PRE_OUTPUT_FAILURE}' >&2\nexit 1\n"),
    );
    let capture = |pattern: &str| ScrapeCapture {
        stream: ScrapeStream::Stdout,
        mode: ScrapeMode::Regex,
        pattern: pattern.to_owned(),
    };
    Config {
        pools: BTreeMap::from([(
            "stock".to_owned(),
            PoolConfig {
                resource: ResourceKind::BuildSlot,
                capacity: 1,
                predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                ..PoolConfig::default()
            },
        )]),
        adapters: BTreeMap::from([
            ("shell".to_owned(), AdapterConfig::default()),
            (
                "structured".to_owned(),
                AdapterConfig {
                    argv: vec![structured.display().to_string()],
                    scrape: BTreeMap::from([
                        ("sessionRef".to_owned(), capture("^TALLY_SESSION=(.*)$")),
                        (
                            "finalMessage".to_owned(),
                            capture("^TALLY_FINAL_MESSAGE=(.*)$"),
                        ),
                    ]),
                    ..AdapterConfig::default()
                },
            ),
            (
                "pre-output-failure".to_owned(),
                AdapterConfig {
                    argv: vec![failing.display().to_string()],
                    ..AdapterConfig::default()
                },
            ),
        ]),
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

async fn start_daemon(paths: &DaemonPaths, config: Config) -> RunningDaemon {
    let executor = Executor::new(&paths.state_dir, PathBuf::from(env!("CARGO_BIN_EXE_tally")))
        .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
        .with_direct_fallback()
        .with_unit_probe(AbsentUnitProbe);
    let daemon = Daemon::open_with_executor(config, paths.clone(), settings(), executor)
        .await
        .unwrap();
    let (shutdown, receiver) = watch::channel(false);
    let task = tokio::task::spawn_local(daemon.run_until(receiver));
    RunningDaemon { shutdown, task }
}

async fn run_tally(config: &Path, socket: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("--config")
        .arg(config)
        .arg("--socket")
        .arg(socket)
        .args(args)
        .env_remove("TALLY_JOB_ID")
        .env_remove("TALLY_JOB_TOKEN")
        .output()
        .await
        .unwrap()
}

fn parse_stdout(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid JSON stdout ({error}):\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[tokio::test(flavor = "current_thread")]
async fn smoke_runs_real_jobs_parses_declared_captures_and_surfaces_pre_output_stderr() {
    LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let paths = daemon_paths(temp.path());
            let config = smoke_config(temp.path());
            let config_path = temp.path().join("config.json");
            std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let daemon = start_daemon(&paths, config).await;

            let shell = run_tally(
                &config_path,
                &paths.socket,
                &[
                    "adapter",
                    "smoke",
                    "shell",
                    "--cwd",
                    temp.path().to_str().unwrap(),
                ],
            )
            .await;
            assert_eq!(shell.status.code(), Some(0));
            let shell = parse_stdout(&shell);
            assert_eq!(shell["diagnostic"], "adapter-smoke");
            assert_eq!(shell["pool"], "stock");
            assert_eq!(shell["verdict"], "pass");
            assert_eq!(shell["captureStatus"], "not-declared");

            let structured = run_tally(
                &config_path,
                &paths.socket,
                &[
                    "adapter",
                    "smoke",
                    "structured",
                    "--pool",
                    "stock",
                    "--cwd",
                    temp.path().to_str().unwrap(),
                ],
            )
            .await;
            assert_eq!(
                structured.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&structured.stderr)
            );
            let structured = parse_stdout(&structured);
            assert_eq!(structured["captureStatus"], "verified");
            assert_eq!(structured["captures"]["sessionRef"], "smoke-session");
            assert_eq!(structured["captures"]["finalMessage"], "ok");

            let failure = run_tally(
                &config_path,
                &paths.socket,
                &[
                    "adapter",
                    "smoke",
                    "pre-output-failure",
                    "--pool",
                    "stock",
                    "--cwd",
                    temp.path().to_str().unwrap(),
                ],
            )
            .await;
            assert_eq!(failure.status.code(), Some(1));
            let failure_json = parse_stdout(&failure);
            assert_eq!(failure_json["verdict"], "failed");
            assert_eq!(failure_json["captureStatus"], "not-checked");
            let failure_stderr = String::from_utf8_lossy(&failure.stderr);
            assert!(
                failure_stderr.contains("captured stderr:"),
                "{failure_stderr}"
            );
            assert!(
                failure_stderr.contains(PRE_OUTPUT_FAILURE),
                "{failure_stderr}"
            );

            let task_uuid = failure_json["taskUuid"].as_str().unwrap();
            let reconstructed = run_tally(
                &config_path,
                &paths.socket,
                &["queue", "await-job", task_uuid],
            )
            .await;
            assert_eq!(reconstructed.status.code(), Some(0));
            assert_eq!(
                parse_stdout(&reconstructed)["stderr_excerpt"],
                PRE_OUTPUT_FAILURE.to_owned() + "\n"
            );

            daemon.stop().await;
            let (report, records) = read_verified_records(&paths.witness_path()).unwrap();
            assert!(report.ok);
            assert_eq!(records.len(), 3);
            for record in records {
                let marker = record.evidence_class.as_ref().unwrap();
                assert_eq!(marker["kind"], "adapter-smoke");
                assert!(marker["label"]
                    .as_str()
                    .unwrap()
                    .starts_with("adapter-smoke:"));
            }
        })
        .await;
}
