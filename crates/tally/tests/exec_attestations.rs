use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use chrono::{SecondsFormat, Utc};
use serde_json::Value;
use tally_core::evidence::hash_artifact_file;
use tally_core::taskdb::{AdmissionOrigin, EnqueueSource};
use tally_core::witness::{
    compute_hash_value, current_host_id, AttestationRecord, LaborClass, Verdict, WitnessBody,
    WitnessLedger, GENESIS_PREV_HASH,
};

const FIRST: &str = "00000000-0000-4000-8000-000000000061";
const SECOND: &str = "00000000-0000-4000-8000-000000000062";

fn tally() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tally"))
}

fn run_attested(
    ledger: &Path,
    task_uuid: &str,
    artifact: &Path,
    payload_hash: &str,
) -> Output {
    tally()
        .args([
            "attest",
            "exec",
            "--task-uuid",
            task_uuid,
            "--attempt",
            "1",
            "--lease-epoch",
            "7",
            "--adapter",
            "shell",
            "--payload-hash",
            payload_hash,
            "--evidence",
            &format!("artifact:{}", artifact.display()),
            "--ledger",
            ledger.to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            "printf attested-result > \"$1\"",
            "tally-u6-child",
            artifact.to_str().unwrap(),
        ])
        .output()
        .unwrap()
}

fn append_canon(
    ledger: &mut WitnessLedger,
    task_uuid: &str,
    artifact: &Path,
    payload_hash: &str,
) {
    ledger
        .append(WitnessBody {
            task_uuid: Some(task_uuid.to_owned()),
            transition_timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            verdict: Verdict::Pass,
            exit_code: 0,
            artifact_content_hash: Some(hash_artifact_file(artifact).unwrap()),
            store_paths: None,
            drv: None,
            gpu_seconds: None,
            wall_clock: 0.1,
            attempt: 1,
            lease_epoch: 7,
            dedup_key: None,
            payload_hash: Some(payload_hash.to_owned()),
            brief_hash: None,
            origin: AdmissionOrigin::direct(EnqueueSource::Manual),
            orchestration: None,
            labor_class: LaborClass::Fresh,
            trace_ref: None,
            pools: vec!["test".to_owned()],
            executor: None,
            host_id: Some(current_host_id().unwrap()),
            charge: None,
            model: None,
            evidence_class: None,
            manifest_hash: None,
            completion: None,
            result_revision: None,
            authorship: None,
        })
        .unwrap();
}

fn compare(canon: &Path, attestations: &Path) -> Output {
    tally()
        .args([
            "witness",
            "compare",
            "--canon",
            canon.to_str().unwrap(),
            "--attestations",
            attestations.to_str().unwrap(),
            "--format",
            "json",
            "--strict",
        ])
        .output()
        .unwrap()
}

fn rewrite_self_consistent_divergence(source: &Path, destination: &Path) {
    let input = fs::read_to_string(source).unwrap();
    let mut previous_hash = GENESIS_PREV_HASH.to_owned();
    let mut output = String::new();
    for (index, line) in input.lines().enumerate() {
        let mut record: AttestationRecord = serde_json::from_str(line).unwrap();
        if index == 0 {
            record.payload["exitCode"] = Value::from(17);
        }
        record.prev_hash.clone_from(&previous_hash);
        record.hash.clear();
        let raw = serde_json::to_value(&record).unwrap();
        record.hash = compute_hash_value(&raw).unwrap();
        previous_hash.clone_from(&record.hash);
        output.push_str(&serde_json::to_string(&record).unwrap());
        output.push('\n');
    }
    fs::write(destination, output).unwrap();
}

#[test]
fn wrapper_and_compare_distinguish_chain_tamper_from_self_consistent_divergence() {
    let temp = tempfile::tempdir().unwrap();
    let exec_ledger = temp.path().join("exec-attestations.jsonl");
    let canon_path = temp.path().join("witness.jsonl");
    let first_artifact = temp.path().join("first.result");
    let second_artifact = temp.path().join("second.result");
    let first_payload = format!("sha256:{}", "a".repeat(64));
    let second_payload = format!("sha256:{}", "b".repeat(64));

    for output in [
        run_attested(&exec_ledger, FIRST, &first_artifact, &first_payload),
        run_attested(&exec_ledger, SECOND, &second_artifact, &second_payload),
    ] {
        assert!(
            output.status.success(),
            "wrapper stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut canon = WitnessLedger::open(&canon_path).unwrap();
    append_canon(&mut canon, FIRST, &first_artifact, &first_payload);
    append_canon(&mut canon, SECOND, &second_artifact, &second_payload);
    drop(canon);

    let unanimous = compare(&canon_path, &exec_ledger);
    assert!(
        unanimous.status.success(),
        "compare stderr: {}",
        String::from_utf8_lossy(&unanimous.stderr)
    );
    let report: Value = serde_json::from_slice(&unanimous.stdout).unwrap();
    assert_eq!(report["summary"]["compared"], 2);
    assert_eq!(report["summary"]["unanimous"], 2);
    assert_eq!(report["summary"]["diverged"], 0);
    assert_eq!(report["summary"]["unattested"], 0);
    assert_eq!(report["summary"]["orphans"], 0);
    assert_eq!(report["executions"][0]["witnessRef"], "witness:1");

    let tampered = temp.path().join("exec-attestations-tampered.jsonl");
    let bytes = fs::read(&exec_ledger).unwrap();
    let position = bytes.iter().position(|byte| *byte == b'e').unwrap();
    let mut bytes = bytes;
    bytes[position] = b'f';
    fs::write(&tampered, bytes).unwrap();
    assert_eq!(compare(&canon_path, &tampered).status.code(), Some(2));

    let divergent = temp.path().join("exec-attestations-divergent.jsonl");
    rewrite_self_consistent_divergence(&exec_ledger, &divergent);
    let divergence = compare(&canon_path, &divergent);
    assert_eq!(divergence.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&divergence.stdout).unwrap();
    assert_eq!(report["summary"]["diverged"], 1);
    assert!(report["executions"][0]["diffs"][0]
        .as_str()
        .unwrap()
        .contains("exitCode"));
}

#[test]
fn attestation_append_failure_never_changes_child_exit_propagation() {
    let temp = tempfile::tempdir().unwrap();
    let blocker = temp.path().join("not-a-directory");
    fs::write(&blocker, "block").unwrap();
    let ledger = blocker.join("exec-attestations.jsonl");

    for expected in [0, 23] {
        let output = tally()
            .args([
                "attest",
                "exec",
                "--task-uuid",
                FIRST,
                "--attempt",
                "1",
                "--lease-epoch",
                "7",
                "--ledger",
                ledger.to_str().unwrap(),
                "--",
                "/bin/sh",
                "-c",
                &format!("exit {expected}"),
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(expected));
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("execution attestation append failed"));
    }
}
