//! #434 — what the CLI is allowed to say when the daemon stops answering.
//!
//! The incident these tests are written from: on 2026-08-07 two adapter smokes
//! whose daemon-side verdicts were exit 0 and witness-emitted PASS were reported
//! as **failures**, because their `query.job` read timed out during a daemon
//! stall (#431). A false negative from the estate's own preflight tool costs
//! diagnosis time and poisons the operator's model of what is broken.
//!
//! The stall is injected rather than reproduced: a socket that speaks the wire
//! protocol, answers what the client needs to get as far as the result read,
//! and then never answers that read. That is deterministic and is the shape the
//! client sees during a real stall.

use std::collections::BTreeMap;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant};

use serde_json::Value;
use tally_client::{RequestFrame, WireError};
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
use tally_core::wire::{serve_connection, RpcHandler};
use tokio::net::UnixListener;
use tokio::process::Command;
use tokio::sync::watch;
use tokio::task::{JoinHandle, LocalSet};

#[path = "support/shell_program.rs"]
mod shell_program;

/// A daemon that admits work and then never answers a read. `queue.enqueue`
/// returns the terminal object a waited enqueue returns, so the client reaches
/// its result read with a passing execution verdict in hand — which is exactly
/// the state the 2026-08-07 smokes were in when they were reported as failed.
#[derive(Clone, Copy)]
struct StallingHandler {
    task_uuid: &'static str,
}

impl RpcHandler for StallingHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            match request.method.as_str() {
                "queue.enqueue" => Ok(serde_json::json!({
                    "task_uuid": self.task_uuid,
                    "job_id": self.task_uuid,
                    "verdict": "pass",
                    "exit_code": 0,
                    "attempt": 1,
                    "lease_epoch": 1,
                })),
                // The stall. Long enough that no deadline under test can
                // outlive it, and the connection stays open throughout, so the
                // client sees a deadline rather than a closed socket.
                _ => {
                    tokio::time::sleep(Duration::from_secs(600)).await;
                    Ok(serde_json::json!({}))
                }
            }
        })
    }
}

async fn serve_stalled(listener: UnixListener, handler: StallingHandler) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        tokio::task::spawn_local(async move {
            let _ = serve_connection(stream, handler).await;
        });
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

struct AbsentUnitProbe;

impl LocalUnitProbe for AbsentUnitProbe {
    fn inspect(&self, unit: &str, _paths: &ExecutionPaths) -> Result<LocalUnitFact, ExecutorError> {
        Ok(LocalUnitFact::absent(unit))
    }
}

fn daemon_paths(root: &Path) -> DaemonPaths {
    DaemonPaths {
        socket: root.join("run/tally.sock"),
        state_dir: root.join("state"),
        data_dir: root.join("data"),
    }
}

fn stall_config(root: &Path) -> Config {
    let structured = root.join("structured-adapter");
    shell_program::install(
        &structured,
        concat!(
            "#!/bin/sh\n",
            "printf '%s\\n' 'TALLY_SESSION=smoke-session' 'TALLY_FINAL_MESSAGE=ok'\n",
        ),
    );
    let capture = |pattern: &str| ScrapeCapture {
        stream: ScrapeStream::Stdout,
        mode: ScrapeMode::Regex,
        pattern: pattern.to_owned(),
        fields: Default::default(),
    };
    Config {
        pools: BTreeMap::from([(
            "stock".to_owned(),
            PoolConfig {
                resource: Some(ResourceKind::BuildSlot),
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
                auto_pool_return: true,
                auto_resource_return: false,
                auto_bounded_requeue: false,
            },
            max_attempts: 2,
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
    let state_home = config.parent().unwrap().join("xdg-state");
    Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("--config")
        .arg(config)
        .arg("--socket")
        .arg(socket)
        .args(args)
        .env("XDG_STATE_HOME", &state_home)
        .env_remove("TALLY_JOB_ID")
        .env_remove("TALLY_JOB_TOKEN")
        .env_remove("TALLY_RPC_TIMEOUT_SEC")
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

/// Acceptance 1 and 2 of #434 together: a timed-out result read is reported as
/// VERDICT-UNAVAILABLE with its own exit code rather than as adapter failure,
/// and the knob that bounds that read is the operator's `--rpc-timeout-sec`.
#[tokio::test(flavor = "current_thread")]
async fn a_timed_out_smoke_result_read_is_unavailable_not_a_failure() {
    LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let config = stall_config(temp.path());
            let config_path = temp.path().join("config.json");
            std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            std::fs::create_dir_all(temp.path().join("run")).unwrap();
            let socket = temp.path().join("run/tally.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            tokio::task::spawn_local(serve_stalled(
                listener,
                StallingHandler {
                    task_uuid: "00000000-0000-4000-8000-0000000004aa",
                },
            ));

            let started = Instant::now();
            let output = run_tally(
                &config_path,
                &socket,
                &[
                    "--rpc-timeout-sec",
                    "1",
                    "adapter",
                    "smoke",
                    "structured",
                    "--pool",
                    "stock",
                ],
            )
            .await;
            let elapsed = started.elapsed();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

            // Never 1. That is the whole finding: exit 1 says "the adapter
            // failed" about a run whose execution verdict was pass.
            assert_eq!(
                output.status.code(),
                Some(5),
                "stdout:\n{}\nstderr:\n{stderr}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(
                stderr.contains("could not read its verdict") && stderr.contains("#431"),
                "{stderr}"
            );

            // The receipt is still printed. An operator who gets no verdict
            // must still get the task identity and the deadline that bounded
            // the read, or the tool has told them nothing.
            let result = parse_stdout(&output);
            assert_eq!(result["verdictState"], "VERDICT-UNAVAILABLE");
            assert_eq!(result["captureStatus"], "unavailable");
            assert_eq!(result["verdict"], "pass");
            assert_eq!(result["rpcTimeoutSec"], 1);
            assert_eq!(result["taskUuid"], "00000000-0000-4000-8000-0000000004aa");

            // The knob demonstrably reaches the result read: the private
            // 10-second capture-projection constant used to bound it, and a run
            // that returned in about a second cannot have used that one.
            assert!(
                elapsed < Duration::from_secs(9),
                "the --rpc-timeout-sec value did not reach the query.job read: {elapsed:?}"
            );
        })
        .await;
}

/// The other two thirds of the three-way split, against a real daemon: a smoke
/// that passes says PASS, and a smoke whose adapter fails says FAIL. Without
/// these, "never renders a timeout as failure" could be satisfied by a tool
/// that never says failure at all.
#[tokio::test(flavor = "current_thread")]
async fn a_read_verdict_is_pass_or_fail_and_says_which() {
    LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let paths = daemon_paths(temp.path());
            let mut config = stall_config(temp.path());
            let failing = temp.path().join("failing-adapter");
            shell_program::install(&failing, "#!/bin/sh\nprintf 'nope\\n' >&2\nexit 1\n");
            config.adapters.insert(
                "failing".to_owned(),
                AdapterConfig {
                    argv: vec![failing.display().to_string()],
                    ..AdapterConfig::default()
                },
            );
            let config_path = temp.path().join("config.json");
            std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let daemon = start_daemon(&paths, config).await;

            let passing = run_tally(
                &config_path,
                &paths.socket,
                &["adapter", "smoke", "structured", "--pool", "stock"],
            )
            .await;
            assert_eq!(
                passing.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&passing.stderr)
            );
            assert_eq!(parse_stdout(&passing)["verdictState"], "PASS");

            let failed = run_tally(
                &config_path,
                &paths.socket,
                &["adapter", "smoke", "failing", "--pool", "stock"],
            )
            .await;
            assert_eq!(failed.status.code(), Some(1));
            assert_eq!(parse_stdout(&failed)["verdictState"], "FAIL");

            daemon.stop().await;
        })
        .await;
}

/// Acceptance 3: `query run` answers from disk when the daemon does not answer
/// at all, and the durable answer is the same answer — not a degraded shape
/// with different task identities.
#[tokio::test(flavor = "current_thread")]
async fn query_run_falls_back_to_a_labelled_durable_view_that_agrees_with_the_live_one() {
    LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let paths = daemon_paths(temp.path());
            let config = stall_config(temp.path());
            let config_path = temp.path().join("config.json");
            std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let daemon = start_daemon(&paths, config).await;

            let admitted = run_tally(
                &config_path,
                &paths.socket,
                &[
                    "enqueue",
                    "--pool",
                    "stock",
                    "--adapter",
                    "structured",
                    "--wait",
                    "--",
                    "smoke",
                ],
            )
            .await;
            assert_eq!(
                admitted.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&admitted.stderr)
            );
            let task_uuid = parse_stdout(&admitted)["task_uuid"]
                .as_str()
                .unwrap()
                .to_owned();

            let live = run_tally(
                &config_path,
                &paths.socket,
                &["query", "run", &task_uuid, "--json"],
            )
            .await;
            assert_eq!(
                live.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&live.stderr)
            );
            let live = parse_stdout(&live);
            assert_eq!(live["view"], "live");

            daemon.stop().await;

            // A socket that accepts and never answers. The daemon process is
            // gone, so this stands in for the state the client actually sees
            // during a stall: connected, and no reply.
            let stalled_socket = temp.path().join("run/stalled.sock");
            let listener = UnixListener::bind(&stalled_socket).unwrap();
            tokio::task::spawn_local(serve_stalled(
                listener,
                StallingHandler {
                    task_uuid: "00000000-0000-4000-8000-0000000004bb",
                },
            ));

            let fallen_back = run_tally(
                &config_path,
                &stalled_socket,
                &[
                    "--rpc-timeout-sec",
                    "1",
                    "query",
                    "run",
                    &task_uuid,
                    "--json",
                    "--state-dir",
                    paths.state_dir.to_str().unwrap(),
                    "--data-dir",
                    paths.data_dir.to_str().unwrap(),
                ],
            )
            .await;
            let stderr = String::from_utf8_lossy(&fallen_back.stderr).into_owned();
            assert_eq!(fallen_back.status.code(), Some(0), "{stderr}");
            assert!(
                stderr.contains("Falling back to the durable-state view"),
                "{stderr}"
            );
            let durable = parse_stdout(&fallen_back);

            // Labelled, in the payload, not only on stderr: a consumer that
            // reads stdout must be able to tell that this is not live.
            assert_eq!(durable["view"], "durable-state");
            assert_eq!(durable["live"], false);
            assert!(durable["caveats"]
                .as_array()
                .unwrap()
                .iter()
                .any(|caveat| caveat.as_str().unwrap().contains("may be stale")));

            // And it is the same run: same identity, same task set, same
            // terminal verdicts. A fallback that answered about a different
            // shape would be worse than no fallback.
            assert_eq!(durable["flowRunId"], live["flowRunId"]);
            let task_ids = |view: &Value| {
                view.get("tasks")
                    .or_else(|| view.get("items"))
                    .and_then(Value::as_array)
                    .unwrap()
                    .iter()
                    .map(|task| task["taskUuid"].clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(task_ids(&durable), task_ids(&live));
            assert_eq!(durable["counts"], live["counts"]);

            // The same view is available without a daemon in the loop at all.
            let explicit = run_tally(
                &config_path,
                &stalled_socket,
                &[
                    "query",
                    "run",
                    &task_uuid,
                    "--json",
                    "--durable",
                    "--state-dir",
                    paths.state_dir.to_str().unwrap(),
                    "--data-dir",
                    paths.data_dir.to_str().unwrap(),
                ],
            )
            .await;
            assert_eq!(
                explicit.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&explicit.stderr)
            );
            assert_eq!(parse_stdout(&explicit)["view"], "durable-state");

            // #434 (eval F1). The deployment this surface exists for: the
            // operator can read the daemon's data and cannot write it. The
            // view must still *render* — it used to probe the membership
            // ledger for appendability and die with an I/O error that was
            // itself false, on the automatic fallback path as well as this
            // one.
            let membership = paths.data_dir.join("flow-membership.jsonl");
            if !membership.exists() {
                std::fs::write(&membership, "").unwrap();
            }
            std::fs::set_permissions(&membership, std::fs::Permissions::from_mode(0o444)).unwrap();
            let read_only = run_tally(
                &config_path,
                &stalled_socket,
                &[
                    "query",
                    "run",
                    &task_uuid,
                    "--json",
                    "--durable",
                    "--state-dir",
                    paths.state_dir.to_str().unwrap(),
                    "--data-dir",
                    paths.data_dir.to_str().unwrap(),
                ],
            )
            .await;
            assert_eq!(
                read_only.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&read_only.stderr)
            );
            let read_only = parse_stdout(&read_only);
            assert_eq!(read_only["view"], "durable-state");
            assert_eq!(read_only["flowRunId"], live["flowRunId"]);
            assert_eq!(task_ids(&read_only), task_ids(&live));
            std::fs::set_permissions(&membership, std::fs::Permissions::from_mode(0o644)).unwrap();
        })
        .await;
}
