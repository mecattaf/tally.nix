//! `tally migrate unit-exit-labels` is the command the startup refusal names.
//!
//! Recovery tells an operator to run exactly this, so it is exercised as an
//! operator does — through the real binary, against a state directory shaped
//! like one an older tally left behind.

use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};
use tally_core::executor::{
    write_exit_record, ExecutionIdentity, UnitExitRecord, Uuid, UNIT_EXIT_DIRECTORY,
    UNIT_EXIT_SCHEMA_VERSION,
};
use tally_core::taskdb::{write_enqueue_event_atomic, DurableEnqueueEvent, RowSeed};

/// A durable row and the `unit-exit` record the binary of the given vintage
/// would have written for it.
fn seed(
    state_dir: &Path,
    task_ref: Option<&str>,
    unit_of: fn(&ExecutionIdentity) -> String,
) -> Uuid {
    let uuid = Uuid::new_v4();
    let mut row = json!({
        "uuid": uuid,
        "description": "campaign task executed under the previous pin",
        "priority": "high",
        "source": "events-dir",
        "adapter": "shell",
        "pool": ["worker"],
        "leaseEpoch": 3,
        "argv": ["worker", "leaf"],
        "cwd": "/work",
    });
    if let Some(task_ref) = task_ref {
        row["orchestration"] = json!({
            "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
            "taskRef": task_ref,
        });
    }
    let row: RowSeed = serde_json::from_value(row).unwrap();
    let identity = ExecutionIdentity {
        job_id: uuid,
        task_uuid: Some(uuid),
        task_ref: row
            .orchestration
            .as_ref()
            .and_then(tally_core::provenance::Orchestration::task_ref),
    };

    let events_dir = state_dir.join("events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let event = DurableEnqueueEvent::new(row).unwrap();
    write_enqueue_event_atomic(&events_dir, &event).unwrap();

    write_exit_record(
        &state_dir
            .join(UNIT_EXIT_DIRECTORY)
            .join(format!("{uuid}.json")),
        &UnitExitRecord {
            schema_version: UNIT_EXIT_SCHEMA_VERSION,
            unit: unit_of(&identity),
            invocation_id: "5f3b1a2c4d6e7f8a".to_owned(),
            attempt: 1,
            lease_epoch: 3,
            service_result: "success".to_owned(),
            exit_code: Some("exited".to_owned()),
            exit_status: Some("0".to_owned()),
        },
    )
    .unwrap();
    uuid
}

fn migrate(state_dir: &Path, apply: bool) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tally"));
    command
        .arg("migrate")
        .arg("unit-exit-labels")
        .arg("--state-dir")
        .arg(state_dir);
    if apply {
        command.arg("--apply");
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "migrate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn recorded_unit(state_dir: &Path, uuid: Uuid) -> String {
    let bytes = std::fs::read(
        state_dir
            .join(UNIT_EXIT_DIRECTORY)
            .join(format!("{uuid}.json")),
    )
    .unwrap();
    serde_json::from_slice::<UnitExitRecord>(&bytes)
        .unwrap()
        .unit
}

#[test]
fn the_named_migration_plans_then_relabels_and_is_a_no_op_afterwards() {
    let temp = tempfile::tempdir().unwrap();
    let state_dir = temp.path();

    let labeled = seed(
        state_dir,
        Some("issue253-live/task-1"),
        ExecutionIdentity::pre_label_unit_name,
    );
    let plain = seed(state_dir, None, ExecutionIdentity::unit_name);
    let plain_unit_before = recorded_unit(state_dir, plain);

    let plan = migrate(state_dir, false);
    assert_eq!(plan["applied"], json!(false));
    assert_eq!(plan["labeledRows"], json!(1));
    assert_eq!(plan["rewritten"].as_array().unwrap().len(), 1);
    assert_eq!(plan["rewritten"][0]["uuid"], json!(labeled));
    assert_eq!(
        plan["rewritten"][0]["recordedUnit"],
        json!(format!("tally-job-{labeled}.service"))
    );
    assert_eq!(
        plan["rewritten"][0]["expectedUnit"],
        json!(format!("tally-job-issue253-live-task-1-{labeled}.service"))
    );
    assert_eq!(
        recorded_unit(state_dir, labeled),
        format!("tally-job-{labeled}.service"),
        "a plan-only run must not write"
    );

    let applied = migrate(state_dir, true);
    assert_eq!(applied["applied"], json!(true));
    assert_eq!(applied["rewritten"].as_array().unwrap().len(), 1);
    assert_eq!(
        recorded_unit(state_dir, labeled),
        format!("tally-job-issue253-live-task-1-{labeled}.service")
    );
    assert_eq!(
        recorded_unit(state_dir, plain),
        plain_unit_before,
        "a row with no taskRef is out of scope and untouched"
    );

    let again = migrate(state_dir, true);
    assert_eq!(again["rewritten"].as_array().unwrap().len(), 0);
    assert_eq!(again["alreadyLabeled"], json!(1));
    assert_eq!(again["skipped"].as_array().unwrap().len(), 0);
}

/// The evaluator's Finding 2, as a regression: a state directory that does not
/// exist used to report `labeledRows: 0` and exit 0, which is the failure where
/// an operator runs the documented command, sees a clean report, restarts, and
/// crash-loops again with no signal that they pointed at the wrong tree.
#[test]
fn a_state_directory_that_holds_no_durable_rows_is_refused_rather_than_reported_clean() {
    let temp = tempfile::tempdir().unwrap();

    for (label, dir) in [
        ("typo", temp.path().join("stat")),
        // A worker's layout: an exit record, and no durable rows anywhere.
        ("worker", temp.path().join("worker")),
    ] {
        if label == "worker" {
            std::fs::create_dir_all(dir.join(UNIT_EXIT_DIRECTORY)).unwrap();
        }
        let output = Command::new(env!("CARGO_BIN_EXE_tally"))
            .arg("migrate")
            .arg("unit-exit-labels")
            .arg("--state-dir")
            .arg(&dir)
            .arg("--apply")
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "{label}: a directory with no durable rows must not report clean; stdout was {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(output.stdout.is_empty(), "{label}: no report is emitted");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("is not a directory; nothing here can be migrated"),
            "{label}: {stderr}"
        );
    }
}

/// A remote-owned row reaches the operator through the real binary carrying the
/// path on the worker and both names — the facts the hand repair needs, since
/// no invocation of this command can perform it.
#[test]
fn a_remote_owned_row_reports_the_hand_repair_and_never_claims_to_have_made_it() {
    let temp = tempfile::tempdir().unwrap();
    let state_dir = temp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let uuid = Uuid::new_v4();
    let row: RowSeed = serde_json::from_value(json!({
        "uuid": uuid,
        "description": "campaign task dispatched to a worker",
        "priority": "high",
        "source": "events-dir",
        "adapter": "shell",
        "pool": ["worker"],
        "executor": "worker",
        "leaseEpoch": 3,
        "argv": ["worker", "leaf"],
        "cwd": "/work",
        "orchestration": {
            "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
            "taskRef": "issue253-live/task-1",
        },
    }))
    .unwrap();
    let events_dir = state_dir.join("events");
    std::fs::create_dir_all(&events_dir).unwrap();
    write_enqueue_event_atomic(&events_dir, &DurableEnqueueEvent::new(row).unwrap()).unwrap();

    let config = temp.path().join("config.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&json!({
            "executors": {
                "worker": {
                    "kind": "ssh",
                    "host": "worker.invalid",
                    "user": "tally-worker",
                    "sshProgram": "/bin/ssh",
                    "identityFile": "/key",
                    "knownHostsFile": "/known-hosts",
                    "program": "/bin/tally",
                    "stateDir": "/var/lib/tally-worker/state",
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("--config")
        .arg(&config)
        .arg("migrate")
        .arg("unit-exit-labels")
        .arg("--state-dir")
        .arg(&state_dir)
        .arg("--apply")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "migrate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["rewritten"].as_array().unwrap().len(), 0);
    let skipped = &report["skipped"][0];
    assert_eq!(skipped["executor"], json!("worker"));
    assert_eq!(
        skipped["recordPath"],
        json!(format!("/var/lib/tally-worker/state/unit-exit/{uuid}.json"))
    );
    assert_eq!(
        skipped["preLabelUnit"],
        json!(format!("tally-job-{uuid}.service"))
    );
    assert_eq!(
        skipped["expectedUnit"],
        json!(format!("tally-job-issue253-live-task-1-{uuid}.service"))
    );
    let reason = skipped["reason"].as_str().unwrap();
    assert!(
        !reason.contains("run this migration against"),
        "the retracted claim must be gone: {reason}"
    );
    assert!(reason.contains("cannot reach or repair"), "{reason}");
}
