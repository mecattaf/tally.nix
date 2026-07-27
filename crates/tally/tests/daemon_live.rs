use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tally_client::RpcClient;
use tally_core::adapters::{AdapterConfig, ScrapeCapture, ScrapeMode, ScrapeStream};
use tally_core::config::{
    CoResidencyPredicate, Config, JournaldConfig, PoolConfig, PoolPredicate, ResourceKind,
};
use tally_core::daemon::{Daemon, DaemonPaths, DaemonSettings};
use tally_core::evidence::RetryPolicy;
use tally_core::executor::UnitLimits;
use tally_core::recovery::RecoveryPolicy;
use tally_core::taskdb::read_acknowledged_events;
use tally_core::witness::read_verified_records;
use tokio::process::Command;
use tokio::sync::watch;
use tokio::task::LocalSet;

#[path = "support/live.rs"]
mod live_support;
#[path = "support/shell_program.rs"]
mod shell_program;

const BASH: &str = "/run/current-system/sw/bin/bash";

struct UnitCleanup(Vec<String>);

impl UnitCleanup {
    fn remember(&mut self, task_uuid: &str) {
        self.0.push(format!("tally-job-{task_uuid}.service"));
    }

    fn remember_unit(&mut self, unit: impl Into<String>) {
        self.0.push(unit.into());
    }
}

impl Drop for UnitCleanup {
    fn drop(&mut self) {
        for unit in &self.0 {
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "stop", "--", unit])
                .output();
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "reset-failed", "--", unit])
                .output();
        }
    }
}

fn config() -> Config {
    Config {
        pools: BTreeMap::from([(
            "soak-slot".to_owned(),
            PoolConfig {
                resource: ResourceKind::BuildSlot,
                predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                ..PoolConfig::default()
            },
        )]),
        enqueue: Default::default(),
        lease: Default::default(),
        adapters: BTreeMap::from([("shell".to_owned(), AdapterConfig::default())]),
        producers: BTreeMap::new(),
        executors: BTreeMap::new(),
        journald: JournaldConfig { native: false },
        ..Config::default()
    }
}

fn settings() -> DaemonSettings {
    DaemonSettings {
        unit_limits: UnitLimits {
            cpu_weight: 100,
            memory_max_bytes: 256 * 1024 * 1024,
        },
        yield_grace: Duration::from_secs(2),
        recovery_policy: RecoveryPolicy {
            retry: RetryPolicy {
                auto_pool_return: false,
                auto_resource_return: false,
                auto_bounded_requeue: false,
            },
            max_attempts: 1,
        },
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an explicitly selected NixOS host with a user manager"]
async fn real_type_notify_daemon_survives_watchdog_periods() {
    let Some(_remote_host) =
        live_support::require_remote_host("real_type_notify_daemon_survives_watchdog_periods")
    else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("run/tally.sock");
    let state = temp.path().join("state");
    let data = temp.path().join("data");
    let config_path = temp.path().join("config.json");
    fs::write(&config_path, serde_json::to_vec(&config()).unwrap()).unwrap();
    let unit = format!("tally-daemon-live-{}.service", std::process::id());
    let mut cleanup = UnitCleanup(Vec::new());
    cleanup.remember_unit(unit.clone());
    let output = Command::new("systemd-run")
        .args([
            "--user",
            "--collect",
            "--quiet",
            "--unit",
            unit.trim_end_matches(".service"),
            "--property=Type=notify",
            "--property=NotifyAccess=main",
            "--property=WatchdogSec=1s",
            "--property=Restart=on-failure",
            "--property=TimeoutStopSec=10s",
            "--",
            env!("CARGO_BIN_EXE_tally"),
            "--config",
        ])
        .arg(&config_path)
        .arg("--socket")
        .arg(&socket)
        .args([
            "daemon",
            "run",
            "--cpu-weight",
            "100",
            "--memory-max-bytes",
            "268435456",
            "--state-dir",
        ])
        .arg(&state)
        .arg("--data-dir")
        .arg(&data)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "systemd-run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let active = systemctl(&[
                OsStr::new("--user"),
                OsStr::new("is-active"),
                OsStr::new("--quiet"),
                OsStr::new("--"),
                OsStr::new(&unit),
            ])
            .await
            .status
            .success();
            if active && socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Type=notify daemon did not reach READY=1");

    tokio::time::sleep(Duration::from_secs(3)).await;
    let status = systemctl(&[
        OsStr::new("--user"),
        OsStr::new("show"),
        OsStr::new("--property=ActiveState"),
        OsStr::new("--property=SubState"),
        OsStr::new("--property=NRestarts"),
        OsStr::new("--property=WatchdogUSec"),
        OsStr::new("--property=WatchdogTimestampMonotonic"),
        OsStr::new("--"),
        OsStr::new(&unit),
    ])
    .await;
    assert!(status.status.success());
    let properties = String::from_utf8(status.stdout).unwrap();
    assert!(properties.contains("ActiveState=active"), "{properties}");
    assert!(properties.contains("SubState=running"), "{properties}");
    assert!(properties.contains("NRestarts=0"), "{properties}");
    assert!(properties.contains("WatchdogUSec=1s"), "{properties}");
    let watchdog_timestamp = properties
        .lines()
        .find_map(|line| line.strip_prefix("WatchdogTimestampMonotonic="))
        .unwrap();
    assert_ne!(watchdog_timestamp, "0", "{properties}");

    let client = RpcClient::connect(&socket).await.unwrap();
    let query = client
        .call("query.status", Some(serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(query["protocolVersion"], 4);
    drop(client);
    assert!(systemctl(&[
        OsStr::new("--user"),
        OsStr::new("stop"),
        OsStr::new("--"),
        OsStr::new(&unit),
    ])
    .await
    .status
    .success());
    assert_collected(&unit).await;
    assert!(!socket.exists());
}

async fn systemctl(args: &[&OsStr]) -> std::process::Output {
    Command::new("systemctl")
        .args(args)
        .output()
        .await
        .expect("systemctl must be installed on the designated worker")
}

async fn wait_active(unit: &str) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let output = systemctl(&[
                OsStr::new("--user"),
                OsStr::new("show"),
                OsStr::new("--property=ActiveState"),
                OsStr::new("--value"),
                OsStr::new("--"),
                OsStr::new(unit),
            ])
            .await;
            if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "active"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the first transient unit did not become active");
}

async fn assert_collected(unit: &str) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let output = systemctl(&[
                OsStr::new("--user"),
                OsStr::new("show"),
                OsStr::new("--property=LoadState"),
                OsStr::new("--value"),
                OsStr::new("--"),
                OsStr::new(unit),
            ])
            .await;
            if !output.status.success()
                || String::from_utf8_lossy(&output.stdout).trim() == "not-found"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("transient unit {unit} was not collected"));
}

async fn enqueue(client: &RpcClient, script: &str) -> serde_json::Value {
    client
        .call(
            "queue.enqueue",
            Some(serde_json::json!({
                "argv": [BASH, "-c", script],
                "pool": "soak-slot",
                "priority": "high",
                "adapter": "shell",
                "source": "manual",
                "evidence": ["exit:0"],
                "runtimeMaxSec": 15
            })),
        )
        .await
        .unwrap()
}

fn capture(state_dir: &Path, task_uuid: &str, stream: &str) -> PathBuf {
    state_dir
        .join("capture")
        .join(format!("{task_uuid}.{stream}"))
}

fn path_executable(name: &str) -> PathBuf {
    env::split_paths(&env::var_os("PATH").expect("PATH is set"))
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("{name} is available in the live test environment"))
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an explicitly selected NixOS host with a user manager"]
async fn real_user_manager_adapter_capture_scrape() {
    let Some(_remote_host) =
        live_support::require_remote_host("real_user_manager_adapter_capture_scrape")
    else {
        return;
    };
    let local = LocalSet::new();
    local
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let paths = DaemonPaths {
                socket: temp.path().join("run/tally.sock"),
                state_dir: temp.path().join("state"),
                data_dir: temp.path().join("data"),
            };
            let mut live_config = config();
            let harness = temp.path().join("checkpoint-harness");
            shell_program::install(
                &harness,
                concat!(
                    "#!/bin/sh\n",
                    "set -eu\n",
                    "hook_status=\"$(\"$LIVE_TALLY_BIN\" lease status)\"\n",
                    "exec \"$LIVE_JQ\" -cn --argjson hookStatus \"$hook_status\" --args ",
                    "'{session_id:\"live-session\",model:\"Live/Model.Exact\",usage:{input_tokens:12345},mode:env.LIVE_ADAPTER_MODE,hook:env.TALLY_YIELD_HOOK,socket:env.TALLY_SOCKET,hook_status:$hookStatus,workload:$ARGS.positional}' -- \"$@\"\n"
                ),
            );
            let jq = path_executable("jq");
            live_config.adapters.insert(
                "live-json".to_owned(),
                AdapterConfig {
                    argv: vec![harness.to_string_lossy().into_owned()],
                    resume: Some(vec![
                        "/run/current-system/sw/bin/echo".to_owned(),
                        "%<sessionRef>%".to_owned(),
                        "%<model>%".to_owned(),
                    ]),
                    scrape: BTreeMap::from([
                        (
                            "hook".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPath,
                                pattern: "$.hook".to_owned(),
                            },
                        ),
                        (
                            "hookStatus".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPath,
                                pattern: "$.hook_status".to_owned(),
                            },
                        ),
                        (
                            "mode".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPath,
                                pattern: "$.mode".to_owned(),
                            },
                        ),
                        (
                            "model".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPath,
                                pattern: "$..model".to_owned(),
                            },
                        ),
                        (
                            "socket".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPath,
                                pattern: "$.socket".to_owned(),
                            },
                        ),
                        (
                            "sessionRef".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPath,
                                pattern: "$..session_id".to_owned(),
                            },
                        ),
                        (
                            "usage".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPath,
                                pattern: "$..usage".to_owned(),
                            },
                        ),
                    ]),
                    trace: None,
                    yield_hook: Some(vec![
                        env!("CARGO_BIN_EXE_tally").to_owned(),
                        "lease".to_owned(),
                        "status".to_owned(),
                    ]),
                    env: BTreeMap::from([
                        ("LIVE_ADAPTER_MODE".to_owned(), "json".to_owned()),
                        (
                            "LIVE_TALLY_BIN".to_owned(),
                            env!("CARGO_BIN_EXE_tally").to_owned(),
                        ),
                        ("LIVE_JQ".to_owned(), jq.to_string_lossy().into_owned()),
                    ]),
                    launch: tally_core::adapters::AdapterLaunchConfig::default(),
                    hardening: Default::default(),
                    skill_bundle: None,
                    skill_revision: None,
                    extra_config: BTreeMap::from([(
                        "modelFlag".to_owned(),
                        serde_json::Value::String("--model".to_owned()),
                    )]),
                },
            );
            let daemon = Daemon::open(
                live_config,
                paths.clone(),
                settings(),
                PathBuf::from(env!("CARGO_BIN_EXE_tally")),
            )
            .await
            .unwrap();
            let (shutdown, shutdown_rx) = watch::channel(false);
            let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
            let client = RpcClient::connect(&paths.socket).await.unwrap();
            let admitted = client
                .call(
                    "queue.enqueue",
                    Some(serde_json::json!({
                        "argv": ["live-workload"],
                        "pool": "soak-slot",
                        "priority": "high",
                        "adapter": "live-json",
                        "source": "manual",
                        "evidence": ["exit:0"],
                        "consumptionEstimate": 2
                    })),
                )
                .await
                .unwrap();
            let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
            let mut cleanup = UnitCleanup(Vec::new());
            cleanup.remember(&task_uuid);
            let terminal = tokio::time::timeout(
                Duration::from_secs(20),
                client.call(
                    "queue.await_job",
                    Some(serde_json::json!({"task_uuid": task_uuid})),
                ),
            )
            .await
            .expect("live adapter job did not reach a terminal witness")
            .unwrap();
            assert_eq!(terminal["verdict"], "pass");

            tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    let status = client
                        .call("query.status", Some(serde_json::json!({})))
                        .await
                        .unwrap();
                    let projected = status["jobs"].as_array().unwrap().iter().any(|job| {
                        job["taskUuid"] == task_uuid
                            && job["sessionRef"] == "live-session"
                            && job["model"] == "Live/Model.Exact"
                    });
                    if projected && paths.data_dir.join("attestations.jsonl").exists() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("post-ack adapter scrape was not projected");

            shutdown.send(true).unwrap();
            tokio::time::timeout(Duration::from_secs(10), daemon_task)
                .await
                .expect("live adapter daemon did not shut down")
                .unwrap()
                .unwrap();
            drop(client);
            let output = fs::read_to_string(capture(&paths.state_dir, &task_uuid, "out")).unwrap();
            let output: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
            assert_eq!(output["workload"], serde_json::json!(["live-workload"]));
            assert_eq!(output["hook_status"]["held"], true);
            let line = fs::read_to_string(paths.data_dir.join("attestations.jsonl")).unwrap();
            let attestation: tally_core::witness::AttestationRecord =
                serde_json::from_str(line.lines().next().unwrap()).unwrap();
            assert_eq!(
                attestation.payload["captures"]["model"],
                "Live/Model.Exact"
            );
            assert_eq!(
                attestation.payload["captures"]["usage"]["input_tokens"],
                12345
            );
            assert_eq!(attestation.payload["captures"]["mode"], "json");
            assert_eq!(attestation.payload["captures"]["hookStatus"]["held"], true);
            assert_eq!(
                attestation.payload["captures"]["hook"],
                serde_json::to_string(&vec![
                    env!("CARGO_BIN_EXE_tally"),
                    "lease",
                    "status"
                ])
                .unwrap()
            );
            assert_eq!(
                attestation.payload["captures"]["socket"],
                paths.socket.to_string_lossy().as_ref()
            );
            let (_, witness) = read_verified_records(&paths.witness_path()).unwrap();
            assert_eq!(witness[0].gpu_seconds, None);
            assert_eq!(witness[0].charge, None);
            assert_collected(&format!("tally-job-{task_uuid}.service")).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an explicitly selected NixOS host with a user manager"]
async fn real_user_manager_daemon_contention_restart_soak() {
    let Some(_remote_host) =
        live_support::require_remote_host("real_user_manager_daemon_contention_restart_soak")
    else {
        return;
    };
    let local = LocalSet::new();
    local
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let paths = DaemonPaths {
                socket: temp.path().join("run/tally.sock"),
                state_dir: temp.path().join("state"),
                data_dir: temp.path().join("data"),
            };
            let recorder = PathBuf::from(env!("CARGO_BIN_EXE_tally"));
            let mut cleanup = UnitCleanup(Vec::new());

            let first = Daemon::open(config(), paths.clone(), settings(), recorder.clone())
                .await
                .unwrap();
            let (first_shutdown, first_shutdown_rx) = watch::channel(false);
            let first_task = tokio::task::spawn_local(first.run_until(first_shutdown_rx));
            let client = RpcClient::connect(&paths.socket).await.unwrap();

            let first = enqueue(
                &client,
                "printf 'first-start\\n'; sleep 3; printf 'first-done\\n'",
            )
            .await;
            let first_uuid = first["task_uuid"].as_str().unwrap().to_owned();
            cleanup.remember(&first_uuid);
            let first_unit = format!("tally-job-{first_uuid}.service");
            wait_active(&first_unit).await;

            let second = enqueue(&client, "printf 'second\\n'").await;
            let third = enqueue(&client, "printf 'third\\n'").await;
            let second_uuid = second["task_uuid"].as_str().unwrap().to_owned();
            let third_uuid = third["task_uuid"].as_str().unwrap().to_owned();
            cleanup.remember(&second_uuid);
            cleanup.remember(&third_uuid);
            assert_eq!(first["state"], "running");
            assert_eq!(second["state"], "queued");
            assert_eq!(third["state"], "queued");

            let status = client
                .call("query.status", Some(serde_json::json!({})))
                .await
                .unwrap();
            let states = status["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|job| job["state"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                states.iter().filter(|state| **state == "running").count(),
                1
            );
            assert_eq!(states.iter().filter(|state| **state == "queued").count(), 2);

            first_shutdown.send(true).unwrap();
            first_task.await.unwrap().unwrap();
            drop(client);
            tokio::task::yield_now().await;
            assert!(systemctl(&[
                OsStr::new("--user"),
                OsStr::new("is-active"),
                OsStr::new("--quiet"),
                OsStr::new("--"),
                OsStr::new(&first_unit),
            ])
            .await
            .status
            .success());

            let second_daemon = Daemon::open(config(), paths.clone(), settings(), recorder)
                .await
                .unwrap();
            let (second_shutdown, second_shutdown_rx) = watch::channel(false);
            let second_task = tokio::task::spawn_local(second_daemon.run_until(second_shutdown_rx));
            let restarted = RpcClient::connect(&paths.socket).await.unwrap();
            let mut results = Vec::new();
            for task_uuid in [&first_uuid, &second_uuid, &third_uuid] {
                results.push(
                    restarted
                        .call(
                            "queue.await_job",
                            Some(serde_json::json!({"task_uuid": task_uuid})),
                        )
                        .await
                        .unwrap(),
                );
            }
            assert!(results.iter().all(|result| result["verdict"] == "pass"));

            for task_uuid in [&first_uuid, &second_uuid, &third_uuid] {
                let late = tokio::time::timeout(
                    Duration::from_millis(150),
                    restarted.call(
                        "queue.await_job",
                        Some(serde_json::json!({"task_uuid": task_uuid})),
                    ),
                )
                .await
                .expect("late wait did not resolve immediately")
                .unwrap();
                assert_eq!(late["verdict"], "pass");
            }

            second_shutdown.send(true).unwrap();
            second_task.await.unwrap().unwrap();
            drop(restarted);
            tokio::task::yield_now().await;

            let (report, witness) = read_verified_records(&paths.witness_path()).unwrap();
            assert!(report.ok);
            assert_eq!(witness.len(), 3);
            assert_eq!(
                witness
                    .iter()
                    .filter_map(|record| record.task_uuid.clone())
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([first_uuid.clone(), second_uuid.clone(), third_uuid.clone(),])
            );
            assert_eq!(
                read_acknowledged_events(&paths.events_dir()).unwrap().len(),
                3
            );
            assert_eq!(
                fs::read_to_string(paths.state_dir.join("lease_epoch"))
                    .unwrap()
                    .trim(),
                "2"
            );
            assert_eq!(
                fs::read_to_string(capture(&paths.state_dir, &first_uuid, "out")).unwrap(),
                "first-start\nfirst-done\n"
            );
            assert_eq!(
                fs::read_to_string(capture(&paths.state_dir, &second_uuid, "out")).unwrap(),
                "second\n"
            );
            assert_eq!(
                fs::read_to_string(capture(&paths.state_dir, &third_uuid, "out")).unwrap(),
                "third\n"
            );
            for task_uuid in [&first_uuid, &second_uuid, &third_uuid] {
                assert!(fs::read(capture(&paths.state_dir, task_uuid, "err"))
                    .unwrap()
                    .is_empty());
                assert_collected(&format!("tally-job-{task_uuid}.service")).await;
            }
        })
        .await;
}
