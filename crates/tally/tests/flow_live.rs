use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Stdio;
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
    read_exit_record, ExecutionPaths, Executor, ExecutorError, LocalUnitFact, LocalUnitProbe,
    LocalUnitState, UnitLimits,
};
use tally_core::recovery::RecoveryPolicy;
use tally_core::taskdb::{read_acknowledged_events, EnqueueSource};
use tally_core::witness::read_verified_records;
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::task::JoinHandle;

const CONCURRENT_RUN: &str = "00000000-0000-4000-8000-000000000501";
const KILLED_RUN: &str = "00000000-0000-4000-8000-000000000502";
const RESTARTED_RUN: &str = "00000000-0000-4000-8000-000000000503";
const DIVERGENT_RUN: &str = "00000000-0000-4000-8000-000000000504";

struct ExitFileProbe;

impl LocalUnitProbe for ExitFileProbe {
    fn inspect(&self, unit: &str, paths: &ExecutionPaths) -> Result<LocalUnitFact, ExecutorError> {
        if !paths.exit_record.exists() {
            return Ok(LocalUnitFact::absent(unit));
        }
        let record = read_exit_record(&paths.exit_record, unit)?;
        Ok(LocalUnitFact {
            unit: unit.to_owned(),
            loaded: false,
            state: LocalUnitState::Exited,
            invocation_id: Some(record.invocation_id.clone()),
            attempt: Some(record.attempt),
            lease_epoch: Some(record.lease_epoch),
            exit_record: Some(record),
        })
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

fn config() -> Config {
    let pool = |resource| PoolConfig {
        resource,
        capacity: 8,
        predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
        ..PoolConfig::default()
    };
    Config {
        pools: BTreeMap::from([
            ("flow".to_owned(), pool(ResourceKind::CpuSlot)),
            ("alpha".to_owned(), pool(ResourceKind::BuildSlot)),
            ("beta".to_owned(), pool(ResourceKind::BuildSlot)),
        ]),
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

fn paths(root: &Path) -> DaemonPaths {
    DaemonPaths {
        socket: root.join("run/tally.sock"),
        state_dir: root.join("state"),
        data_dir: root.join("data"),
    }
}

async fn start_daemon(paths: &DaemonPaths, config: Config) -> RunningDaemon {
    let executor = Executor::new(&paths.state_dir, env!("CARGO_BIN_EXE_tally"))
        .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
        .with_unit_probe(ExitFileProbe);
    let daemon = Daemon::open_with_executor(config, paths.clone(), settings(), executor)
        .await
        .unwrap();
    let (shutdown, receiver) = watch::channel(false);
    let task = tokio::task::spawn_local(daemon.run_until(receiver));
    RunningDaemon { shutdown, task }
}

fn six_node_source() -> &'static str {
    r#"
export const meta = {
  name: "fs5-six-node",
  description: "live heterogeneous runner integration",
  pools: ["alpha", "beta"],
  argsSchema: { type: "object", additionalProperties: false },
  selectors: [],
  maxNodes: 6
};

(async () => parallel([
  () => sh(["/bin/sh", "-c", "exit 0"], {
    pools: ["alpha"], priority: "high", evidence: ["exit:0"],
    env: { FLOW_KIND: "shell-a" }, label: "alpha-shell"
  }),
  () => sh(["/bin/sh", "-c", "test 2 -gt 1"], {
    pools: ["beta"], priority: "low", evidence: ["exit:0"], label: "beta-true"
  }),
  () => sh(["/bin/sh", "-c", "sleep 0.02"], {
    pools: ["alpha"], evidence: ["exit:0"], label: "alpha-delay"
  }),
  () => sh(["/bin/sh", "-c", "test \"$FLOW_KIND\" = shell-b"], {
    pools: ["beta"], evidence: ["exit:0"], env: { FLOW_KIND: "shell-b" },
    label: "beta-env"
  }),
  () => sh(["/bin/sh", "-c", ":"], {
    pools: ["alpha"], evidence: ["exit:0"], label: "alpha-true"
  }),
  () => sh(["/bin/sh", "-c", "exit 0"], {
    pools: ["beta"], priority: "medium", evidence: ["exit:0"], label: "beta-shell"
  })
]))()
"#
}

fn divergent_source() -> &'static str {
    r#"
export const meta = {
  name: "fs5-divergence",
  description: "live replay divergence",
  pools: ["alpha"],
  argsSchema: {
    type: "object",
    required: ["variant"],
    properties: { variant: { type: "string" } },
    additionalProperties: false
  },
  selectors: [],
  maxNodes: 1
};

(async () => sh(["/bin/sh", "-c", "exit 0", args.variant], {
  pools: ["alpha"],
  evidence: ["exit:0"],
  label: "variant-" + args.variant
}))()
"#
}

fn runner(
    config_path: &Path,
    socket: &Path,
    script: &Path,
    flow_run_id: &str,
    args: &str,
    max_nodes: u32,
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tally"));
    command
        .arg("--config")
        .arg(config_path)
        .arg("--socket")
        .arg(socket)
        .args(["flow", "run"])
        .arg(script)
        .arg("--args")
        .arg(args)
        .arg("--max-nodes")
        .arg(max_nodes.to_string())
        .arg("--flow-run-id")
        .arg(flow_run_id)
        .env_remove("TALLY_TASK_UUID")
        .env_remove("TALLY_JOB_ID")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn rpc(socket: &Path) -> RpcClient {
    RpcClient::connect(socket).await.unwrap()
}

async fn pause(client: &RpcClient, pool: &str) {
    client
        .call("queue.pause", Some(json!({"pool": pool, "all": false})))
        .await
        .unwrap();
}

async fn resume_all(client: &RpcClient) {
    client
        .call("queue.resume", Some(json!({"all": true})))
        .await
        .unwrap();
}

async fn flow_items(client: &RpcClient, flow_run_id: &str) -> Vec<Value> {
    client
        .call(
            "query.jobs",
            Some(json!({"flowRun": flow_run_id, "limit": 1000})),
        )
        .await
        .unwrap()["items"]
        .as_array()
        .unwrap()
        .clone()
}

async fn wait_for_flow_items(client: &RpcClient, flow_run_id: &str, expected: usize) -> Vec<Value> {
    for _ in 0..400 {
        let items = flow_items(client, flow_run_id).await;
        if items.len() == expected {
            return items;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("flow run {flow_run_id} did not reach {expected} durable rows");
}

async fn await_items(client: &RpcClient, items: &[Value]) {
    for item in items {
        let terminal = tokio::time::timeout(
            Duration::from_secs(20),
            client.call(
                "queue.await_job",
                Some(json!({"task_uuid": item["anchor"]})),
            ),
        )
        .await
        .expect("node wait timed out")
        .unwrap();
        assert_eq!(terminal["verdict"], "pass", "{terminal}");
    }
}

async fn runner_output(child: Child) -> std::process::Output {
    tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("flow runner timed out")
        .unwrap()
}

fn capture(paths: &DaemonPaths, task_uuid: &str) -> String {
    let stdout = fs::read_to_string(
        paths
            .state_dir
            .join("capture")
            .join(format!("{task_uuid}.out")),
    )
    .unwrap_or_default();
    let stderr = fs::read_to_string(
        paths
            .state_dir
            .join("capture")
            .join(format!("{task_uuid}.err")),
    )
    .unwrap_or_default();
    format!("stdout:\n{stdout}\nstderr:\n{stderr}")
}

fn assert_six_unique_rows(paths: &DaemonPaths, flow_run_id: &str) {
    let events = read_acknowledged_events(&paths.events_dir()).unwrap();
    let rows = events
        .iter()
        .filter(|event| {
            event
                .row
                .orchestration
                .as_ref()
                .is_some_and(|orchestration| orchestration.flow_run_id() == flow_run_id)
        })
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 6, "one durable enqueue event per ordinal");
    let mut ordinals = rows
        .iter()
        .map(|event| {
            event.row.orchestration.as_ref().unwrap().as_value()["nodeOrdinal"]
                .as_u64()
                .unwrap()
        })
        .collect::<Vec<_>>();
    ordinals.sort_unstable();
    assert_eq!(ordinals, [0, 1, 2, 3, 4, 5]);

    let (report, records) = read_verified_records(&paths.witness_path()).unwrap();
    assert!(report.ok);
    let witnessed = records
        .iter()
        .filter(|record| {
            record
                .orchestration
                .as_ref()
                .is_some_and(|orchestration| orchestration.flow_run_id() == flow_run_id)
        })
        .collect::<Vec<_>>();
    assert_eq!(witnessed.len(), 6, "one terminal witness per ordinal");
}

#[tokio::test(flavor = "current_thread")]
async fn fs5_live_acceptance_matrix() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let paths = paths(temp.path());
            let config = config();
            let config_path = temp.path().join("config.json");
            fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let six_node_script = temp.path().join("six-node.js");
            fs::write(&six_node_script, six_node_source()).unwrap();
            let divergent_script = temp.path().join("divergent.js");
            fs::write(&divergent_script, divergent_source()).unwrap();

            let mut daemon = start_daemon(&paths, config.clone()).await;
            let mut client = rpc(&paths.socket).await;

            // A flow runner is itself an ordinary daemon job. Its task UUID becomes
            // flowRunId, and every child carries real ancestry and orchestration.
            let parent = client
                .call(
                    "queue.enqueue",
                    Some(json!({
                        "argv": [
                            env!("CARGO_BIN_EXE_tally"),
                            "--config", config_path,
                            "--socket", paths.socket,
                            "flow", "run", six_node_script,
                            "--args", "{}",
                            "--max-nodes", "6"
                        ],
                        "pool": ["flow"],
                        "adapter": "shell",
                        "source": "manual",
                        "dedupKey": "fs5-runner-as-job",
                        "evidence": ["exit:0"],
                        "noEnqueue": false,
                        "wait": false
                    })),
                )
                .await
                .unwrap();
            let parent_uuid = parent["task_uuid"].as_str().unwrap().to_owned();
            let parent_terminal = tokio::time::timeout(
                Duration::from_secs(30),
                client.call("queue.await_job", Some(json!({"task_uuid": parent_uuid}))),
            )
            .await
            .expect("runner-as-job timed out")
            .unwrap();
            assert_eq!(
                parent_terminal["verdict"],
                "pass",
                "{}",
                capture(&paths, &parent_uuid)
            );
            let parent_children = wait_for_flow_items(&client, &parent_uuid, 6).await;
            for child in &parent_children {
                assert_eq!(child["source"], "orchestrator");
                assert_eq!(child["parentTaskUuid"], parent_uuid);
                assert_eq!(child["orchestration"]["flowRunId"], child["parentTaskUuid"]);
                assert_eq!(child["noEnqueue"], true);
            }
            assert_six_unique_rows(&paths, &parent_uuid);

            // Two concurrent runners race every ordinal while work is paused. The
            // kernel creates one row and returns attach to the other runner.
            pause(&client, "alpha").await;
            pause(&client, "beta").await;
            let first = runner(
                &config_path,
                &paths.socket,
                &six_node_script,
                CONCURRENT_RUN,
                "{}",
                6,
            )
            .spawn()
            .unwrap();
            let second = runner(
                &config_path,
                &paths.socket,
                &six_node_script,
                CONCURRENT_RUN,
                "{}",
                6,
            )
            .spawn()
            .unwrap();
            wait_for_flow_items(&client, CONCURRENT_RUN, 6).await;
            resume_all(&client).await;
            let (first, second) = tokio::join!(runner_output(first), runner_output(second));
            assert!(
                first.status.success(),
                "{}",
                String::from_utf8_lossy(&first.stderr)
            );
            assert!(
                second.status.success(),
                "{}",
                String::from_utf8_lossy(&second.stderr)
            );
            assert!(String::from_utf8_lossy(&first.stdout).contains("\"type\":\"flow-report\""));
            assert!(String::from_utf8_lossy(&second.stdout).contains("\"type\":\"flow-report\""));
            assert_eq!(flow_items(&client, CONCURRENT_RUN).await.len(), 6);
            assert_six_unique_rows(&paths, CONCURRENT_RUN);

            // SIGKILL loses only the stateless runner. The six durable child rows
            // finish, and retry collapses the whole prefix without a second row.
            pause(&client, "alpha").await;
            pause(&client, "beta").await;
            let mut killed = runner(
                &config_path,
                &paths.socket,
                &six_node_script,
                KILLED_RUN,
                "{}",
                6,
            )
            .spawn()
            .unwrap();
            let killed_items = wait_for_flow_items(&client, KILLED_RUN, 6).await;
            killed.kill().await.unwrap();
            resume_all(&client).await;
            await_items(&client, &killed_items).await;
            let replay = runner(
                &config_path,
                &paths.socket,
                &six_node_script,
                KILLED_RUN,
                "{}",
                6,
            )
            .spawn()
            .unwrap();
            let replay = runner_output(replay).await;
            assert!(
                replay.status.success(),
                "{}",
                String::from_utf8_lossy(&replay.stderr)
            );
            assert_six_unique_rows(&paths, KILLED_RUN);

            // A daemon epoch change tears down all six outstanding awaits. The
            // runner reconnects one replacement client, re-awaits, and completes.
            pause(&client, "alpha").await;
            pause(&client, "beta").await;
            let restarted = runner(
                &config_path,
                &paths.socket,
                &six_node_script,
                RESTARTED_RUN,
                "{}",
                6,
            )
            .spawn()
            .unwrap();
            wait_for_flow_items(&client, RESTARTED_RUN, 6).await;
            drop(client);
            daemon.stop().await;
            daemon = start_daemon(&paths, config.clone()).await;
            let restarted = runner_output(restarted).await;
            assert!(
                restarted.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&restarted.stdout),
                String::from_utf8_lossy(&restarted.stderr)
            );
            client = rpc(&paths.socket).await;
            assert_eq!(flow_items(&client, RESTARTED_RUN).await.len(), 6);
            assert_six_unique_rows(&paths, RESTARTED_RUN);

            // The same ordinal with changed args reaches the live row's kernel
            // conflict and is surfaced as replay-divergence with both identities.
            pause(&client, "alpha").await;
            let mut original = runner(
                &config_path,
                &paths.socket,
                &divergent_script,
                DIVERGENT_RUN,
                r#"{"variant":"recorded"}"#,
                1,
            )
            .spawn()
            .unwrap();
            let original_items = wait_for_flow_items(&client, DIVERGENT_RUN, 1).await;
            original.kill().await.unwrap();
            let divergent = runner(
                &config_path,
                &paths.socket,
                &divergent_script,
                DIVERGENT_RUN,
                r#"{"variant":"expected"}"#,
                1,
            )
            .spawn()
            .unwrap();
            let divergent = runner_output(divergent).await;
            assert_eq!(divergent.status.code(), Some(20));
            let failed = String::from_utf8(divergent.stdout).unwrap();
            let event = failed
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .find(|event| event["type"] == "flow-failed")
                .unwrap();
            assert_eq!(event["error"]["code"], "replay-divergence");
            assert_eq!(event["error"]["ordinal"], 0);
            assert!(event["error"]["details"]["expectedHash"]
                .as_str()
                .unwrap()
                .starts_with("sha256:"));
            assert!(event["error"]["details"]["recordedHash"]
                .as_str()
                .unwrap()
                .starts_with("sha256:"));
            assert_eq!(
                event["error"]["details"]["expectedLabel"],
                "variant-expected"
            );
            assert_eq!(
                event["error"]["details"]["recordedLabel"],
                "variant-recorded"
            );
            assert_eq!(flow_items(&client, DIVERGENT_RUN).await.len(), 1);
            resume_all(&client).await;
            await_items(&client, &original_items).await;

            // Script identity is inspected from durable rows before re-execution.
            fs::write(
                &divergent_script,
                format!("{}\n// edited generation\n", divergent_source()),
            )
            .unwrap();
            let edited = runner(
                &config_path,
                &paths.socket,
                &divergent_script,
                DIVERGENT_RUN,
                r#"{"variant":"recorded"}"#,
                1,
            )
            .spawn()
            .unwrap();
            let edited = runner_output(edited).await;
            assert_eq!(edited.status.code(), Some(20));
            assert!(String::from_utf8_lossy(&edited.stdout).contains("script-changed-mid-run"));
            assert_eq!(flow_items(&client, DIVERGENT_RUN).await.len(), 1);

            let events = read_acknowledged_events(&paths.events_dir()).unwrap();
            let parent_event = events
                .iter()
                .find(|event| event.row.uuid.to_string() == parent_uuid)
                .unwrap();
            assert_eq!(parent_event.row.source, EnqueueSource::Manual);
            let child_events = events
                .iter()
                .filter(|event| {
                    event
                        .row
                        .parent_uuid
                        .map(|uuid| uuid.to_string())
                        .as_deref()
                        == Some(&parent_uuid)
                })
                .collect::<Vec<_>>();
            assert_eq!(child_events.len(), 6);
            assert!(child_events
                .iter()
                .all(|event| event.row.source == EnqueueSource::Orchestrator));

            daemon.stop().await;
        })
        .await;
}
