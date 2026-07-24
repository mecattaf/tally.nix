use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test/fixtures/flows")
        .join(name)
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
fn flow_run_cli_derives_the_run_id_and_emits_jsonl_for_a_zero_node_script() {
    let output = Command::new(env!("CARGO_BIN_EXE_tally"))
        .args(["flow", "run"])
        .arg(fixture("valid.js"))
        .args([
            "--args",
            r#"{"task":"ship"}"#,
            "--max-nodes",
            "12",
            "--catalog",
        ])
        .arg(fixture("catalog.json"))
        .env("TALLY_TASK_UUID", "flow-run-fixture")
        .output()
        .unwrap();
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
    assert_eq!(events[1]["report"]["flowRunId"], "flow-run-fixture");
    assert_eq!(events[1]["report"]["finalValue"], "ship");
}
