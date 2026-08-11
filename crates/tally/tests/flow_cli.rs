use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tally_core::config::{
    Config, FlowRegistration, PoolConfig, PoolPredicate, ResourceKind, WindowedConsumptionPredicate,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test/fixtures/flows")
        .join(name)
}

fn write_config(root: &Path) -> PathBuf {
    let path = root.join("config.json");
    fs::write(&path, serde_json::to_vec(&Config::default()).unwrap()).unwrap();
    path
}

fn serve_empty_flow_history(
    socket: &Path,
    runner_task_uuid: Option<&str>,
) -> thread::JoinHandle<()> {
    let listener = UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let runner_task_uuid = runner_task_uuid.map(str::to_owned);
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("flow history server did not accept a client: {error}"),
            }
        };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        // Optional runner-identity lookup, then the lineage question, then the
        // durable-row scan.
        let request_count = usize::from(runner_task_uuid.is_some()) + 2;
        for _ in 0..request_count {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "query.job" => {
                    let runner_task_uuid = runner_task_uuid.as_deref().unwrap();
                    assert_eq!(request["params"]["id"], runner_task_uuid);
                    json!({
                        "schemaVersion": 1,
                        "protocolVersion": 5,
                        "job": {
                            "taskUuid": runner_task_uuid,
                            "source": "manual"
                        }
                    })
                }
                // Every run is asked whether a durable rollover retired it,
                // before its rows are scanned. These fixtures never supersede.
                "query.lineage" => json!({
                    "schemaVersion": 1,
                    "flowRunId": request["params"]["flowRun"],
                    "superseded": false,
                    "chain": [request["params"]["flowRun"]],
                    "currentFlowRunId": request["params"]["flowRun"]
                }),
                "query.jobs" => json!({
                    "schemaVersion": 1,
                    "protocolVersion": 5,
                    "items": [],
                    "nextCursor": null,
                    "snapshot": {
                        "createdAt": "2026-07-25T00:00:00Z",
                        "cursor": null,
                        "history": {
                            "earliestCursor": null,
                            "latestCursor": null,
                            "records": 0
                        },
                        "witnessHead": {
                            "seq": 0,
                            "hash": "sha256:genesis"
                        }
                    }
                }),
                method => panic!("unexpected flow history request: {method}"),
            };
            serde_json::to_writer(&mut stream, &json!({"id": request["id"], "result": result}))
                .unwrap();
            stream.write_all(b"\n").unwrap();
        }
    })
}

#[test]
fn flow_check_cli_accepts_valid_and_rejects_the_eval_fixture_matrix() {
    let valid = Command::new(env!("CARGO_BIN_EXE_tally"))
        .args(["flow", "check"])
        .arg(fixture("valid.js"))
        .args(["--args", r#"{"task":"ship"}"#, "--catalog"])
        .arg(fixture("catalog.json"))
        .output()
        .unwrap();
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );
    let meta: Value = serde_json::from_slice(&valid.stdout).unwrap();
    assert_eq!(meta["name"], "fixture-valid");
    assert_eq!(meta["selectors"], serde_json::json!(["pooled-fast"]));

    let temp = tempfile::tempdir().unwrap();
    let args_path = temp.path().join("flow-args.json");
    fs::write(&args_path, br#"{"task":"from-path"}"#).unwrap();
    let from_path = Command::new(env!("CARGO_BIN_EXE_tally"))
        .args(["flow", "check"])
        .arg(fixture("valid.js"))
        .arg("--args-path")
        .arg(&args_path)
        .arg("--catalog")
        .arg(fixture("catalog.json"))
        .output()
        .unwrap();
    assert!(
        from_path.status.success(),
        "{}",
        String::from_utf8_lossy(&from_path.stderr)
    );
    let relative_link = temp.path().join("args-link.json");
    std::os::unix::fs::symlink(&args_path, &relative_link).unwrap();
    let from_relative_link = Command::new(env!("CARGO_BIN_EXE_tally"))
        .current_dir(temp.path())
        .args(["flow", "check"])
        .arg(fixture("valid.js"))
        .args(["--args-path", "args-link.json", "--catalog"])
        .arg(fixture("catalog.json"))
        .output()
        .unwrap();
    assert!(
        from_relative_link.status.success(),
        "{}",
        String::from_utf8_lossy(&from_relative_link.stderr)
    );

    let drv = Command::new(env!("CARGO_BIN_EXE_tally"))
        .args(["flow", "check"])
        .arg(fixture("valid-drv.js"))
        .output()
        .unwrap();
    assert!(
        drv.status.success(),
        "{}",
        String::from_utf8_lossy(&drv.stderr)
    );
    let drv_meta: Value = serde_json::from_slice(&drv.stdout).unwrap();
    assert_eq!(drv_meta["pools"], json!([]));

    for (name, code) in [
        ("nonliteral-meta.js", "meta-nonliteral"),
        ("banned-global.js", "determinism-violation"),
        ("undeclared-pool.js", "undeclared-pool"),
        ("bad-args-schema.js", "args-schema-invalid"),
    ] {
        let rejected = Command::new(env!("CARGO_BIN_EXE_tally"))
            .args(["flow", "check"])
            .arg(fixture(name))
            .output()
            .unwrap();
        assert!(
            !rejected.status.success(),
            "{name} was unexpectedly accepted"
        );
        let stderr = String::from_utf8_lossy(&rejected.stderr);
        assert!(stderr.contains(code), "{name}: {stderr}");
        assert!(stderr.contains("\"line\":"), "{name}: {stderr}");
        assert!(stderr.contains("\"column\":"), "{name}: {stderr}");
    }
}

#[test]
fn flow_render_cli_is_static_mermaid_with_check_level_failures() {
    let rendered = Command::new(env!("CARGO_BIN_EXE_tally"))
        .args(["flow", "render"])
        .arg(fixture("valid-drv.js"))
        .output()
        .unwrap();
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    assert!(rendered.stderr.is_empty());
    let mermaid = String::from_utf8(rendered.stdout).unwrap();
    assert!(mermaid.starts_with("flowchart TD\n"), "{mermaid}");
    assert!(
        mermaid.contains("line 9<br/>drv · pools: build"),
        "{mermaid}"
    );

    let rejected = Command::new(env!("CARGO_BIN_EXE_tally"))
        .args(["flow", "render"])
        .arg(fixture("banned-global.js"))
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(10));
    assert!(rejected.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("FlowDeterminismError"), "{stderr}");
    assert!(stderr.contains("determinism-violation"), "{stderr}");
}

#[test]
fn flow_check_cli_rejects_configured_windowed_consumption_pools() {
    let temp = tempfile::tempdir().unwrap();
    let config_path = temp.path().join("config.json");
    let mut config = Config::default();
    config.pools.insert(
        "worker-gpu".to_owned(),
        PoolConfig {
            resource: Some(ResourceKind::Budget),
            predicate: PoolPredicate::WindowedConsumption(WindowedConsumptionPredicate {
                window_sec: 18_000,
                consumption_cap: 100,
            }),
            ..PoolConfig::default()
        },
    );
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

    let rejected = Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("--config")
        .arg(&config_path)
        .args(["flow", "check"])
        .arg(fixture("valid.js"))
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("FlowPoolError"), "{stderr}");
    assert!(stderr.contains("windowed-consumption-excluded"), "{stderr}");
    assert!(stderr.contains("worker-gpu"), "{stderr}");
    assert!(stderr.contains("priorities"), "{stderr}");
}

#[test]
fn flow_run_rejects_a_registered_workload_mutex_without_an_admitted_parent() {
    let temp = tempfile::tempdir().unwrap();
    let config_path = temp.path().join("config.json");
    let script = fixture("valid.js");
    let mut config = Config::default();
    config.pools.insert(
        "monthly-review".to_owned(),
        PoolConfig {
            resource: Some(ResourceKind::Mutex),
            capacity: 1,
            ..PoolConfig::default()
        },
    );
    config.flows.insert(
        "monthly-review".to_owned(),
        FlowRegistration {
            script: script.canonicalize().unwrap(),
            workload_mutex: Some("monthly-review".to_owned()),
        },
    );
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("--config")
        .arg(&config_path)
        .arg("--socket")
        .arg(temp.path().join("absent.sock"))
        .args(["flow", "run"])
        .arg(&script)
        .args([
            "--args",
            r#"{"task":"ship"}"#,
            "--max-nodes",
            "12",
            "--flow-run-id",
            "00000000-0000-4000-8000-000000000051",
            "--catalog",
        ])
        .arg(fixture("catalog.json"))
        .env_remove("TALLY_TASK_UUID")
        .env_remove("TALLY_JOB_ID")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("FlowStartupError"), "{stderr}");
    assert!(
        stderr.contains("workload-mutex-parent-required"),
        "{stderr}"
    );
    assert!(stderr.contains("monthly-review"), "{stderr}");
    assert!(stderr.contains("admitted parent"), "{stderr}");
    assert!(!stderr.contains("daemon socket"), "{stderr}");
}

#[test]
fn flow_run_allows_a_registered_flow_without_a_workload_mutex() {
    let temp = tempfile::tempdir().unwrap();
    let config_path = temp.path().join("config.json");
    let socket = temp.path().join("tally.sock");
    let script = fixture("valid.js");
    let mut config = Config::default();
    config.flows.insert(
        "manual-review".to_owned(),
        FlowRegistration {
            script: script.canonicalize().unwrap(),
            workload_mutex: None,
        },
    );
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let server = serve_empty_flow_history(&socket, None);

    let output = Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("--config")
        .arg(&config_path)
        .arg("--socket")
        .arg(&socket)
        .args(["flow", "run"])
        .arg(&script)
        .args([
            "--args",
            r#"{"task":"manual"}"#,
            "--max-nodes",
            "12",
            "--flow-run-id",
            "00000000-0000-4000-8000-000000000052",
            "--catalog",
        ])
        .arg(fixture("catalog.json"))
        .env_remove("TALLY_TASK_UUID")
        .env_remove("TALLY_JOB_ID")
        .output()
        .unwrap();
    server.join().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn flow_run_captures_identity_and_reads_args_from_inherited_brief() {
    let temp = tempfile::tempdir().unwrap();
    let config = write_config(temp.path());
    let socket = temp.path().join("tally.sock");
    let args_path = temp.path().join("runner-args.json");
    let prepared = tally_core::brief::PreparedBrief::from_value(json!({
        "marker": "issue-250",
        "configBlob": "x".repeat(256 * 1024),
    }))
    .unwrap();
    fs::write(&args_path, prepared.canonical()).unwrap();
    let server = serve_empty_flow_history(&socket, Some("00000000-0000-4000-8000-000000000048"));
    let output = Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("--config")
        .arg(&config)
        .arg("--socket")
        .arg(&socket)
        .args(["flow", "run"])
        .arg(fixture("args-from-brief.js"))
        .args(["--args-from-brief", "--max-nodes", "12"])
        .env("TALLY_TASK_UUID", "00000000-0000-4000-8000-000000000048")
        .env("TALLY_JOB_ID", "00000000-0000-4000-8000-000000000048")
        .env(
            "TALLY_JOB_TOKEN",
            "abababababababababababababababababababababababababababababababab",
        )
        .env("TALLY_BRIEF", &args_path)
        .env("TALLY_BRIEF_HASH", prepared.hash())
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines = String::from_utf8(output.stdout).unwrap();
    let events = lines
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["type"], "flow-completed");
    assert_eq!(events[1]["type"], "flow-report");
    assert_eq!(
        events[1]["report"]["flowRunId"],
        "00000000-0000-4000-8000-000000000048"
    );
    assert_eq!(
        events[1]["report"]["finalValue"],
        json!({"marker": "issue-250", "configBytes": 256 * 1024})
    );

    let rejected = Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("--config")
        .arg(&config)
        .args(["flow", "run"])
        .arg(fixture("args-from-brief.js"))
        .args(["--args-from-brief", "--max-nodes", "12"])
        .env("TALLY_TASK_UUID", "00000000-0000-4000-8000-000000000048")
        .env("TALLY_JOB_ID", "00000000-0000-4000-8000-000000000048")
        .env("TALLY_BRIEF", &args_path)
        .env("TALLY_BRIEF_HASH", format!("sha256:{}", "0".repeat(64)))
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("TALLY_BRIEF_HASH"));
}

#[test]
fn flow_run_script_failure_has_a_distinguished_exit_and_structured_capture_event() {
    let temp = tempfile::tempdir().unwrap();
    let config = write_config(temp.path());
    let socket = temp.path().join("tally.sock");
    let server = serve_empty_flow_history(&socket, None);
    let output = Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("--config")
        .arg(&config)
        .arg("--socket")
        .arg(&socket)
        .args(["flow", "run"])
        .arg(fixture("banned-global.js"))
        .args([
            "--args",
            "{}",
            "--max-nodes",
            "12",
            "--flow-run-id",
            "00000000-0000-4000-8000-000000000049",
        ])
        .env_remove("TALLY_TASK_UUID")
        .env_remove("TALLY_JOB_ID")
        .env_remove("TALLY_JOB_TOKEN")
        .output()
        .unwrap();
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(10));
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["type"], "flow-failed");
    assert_eq!(events[0]["error"]["code"], "determinism-violation");
    assert_eq!(events[0]["error"]["ordinal"], 0);
    assert!(events[0]["error"]["location"]["line"].as_u64().unwrap() > 1);
}

#[test]
fn flow_run_without_config_names_the_missing_default_before_connecting() {
    let temp = tempfile::tempdir().unwrap();
    let config_home = temp.path().join("empty-config-home");
    fs::create_dir_all(&config_home).unwrap();
    let expected = config_home.join("tally/config.json");

    let output = Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("--socket")
        .arg(temp.path().join("absent.sock"))
        .args(["flow", "run"])
        .arg(fixture("valid.js"))
        .args([
            "--args",
            r#"{"task":"ship"}"#,
            "--max-nodes",
            "12",
            "--flow-run-id",
            "00000000-0000-4000-8000-000000000050",
            "--catalog",
        ])
        .arg(fixture("catalog.json"))
        .env("XDG_CONFIG_HOME", &config_home)
        .env_remove("HOME")
        .env_remove("TALLY_TASK_UUID")
        .env_remove("TALLY_JOB_ID")
        .env_remove("TALLY_JOB_TOKEN")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot read config"), "{stderr}");
    assert!(stderr.contains(&expected.display().to_string()), "{stderr}");
    assert!(!stderr.contains("daemon socket"), "{stderr}");
}
