//! Issue #382: the exit recorder's accounting probe, exercised through the
//! real `__record-unit-exit` binary the same way `ExecStopPost` invokes it.
//!
//! This runs the compiled `tally` binary as a fresh child process for every
//! case specifically so the environment variables the recorder reads
//! (`INVOCATION_ID`, `SERVICE_RESULT`, `TALLY_ATTEMPT`, `TALLY_LEASE_EPOCH`)
//! are set on that one child rather than mutated on the shared test-process
//! environment, which `cargo test`'s default parallel threads would race.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tally_core::executor::{read_exit_record, UNIT_EXIT_SCHEMA_VERSION};

const UNIT: &str = "tally-job-00000000-0000-4000-8000-000000000001.service";

fn fake_systemctl(dir: &Path, name: &str, script: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn record_unit_exit(temp: &Path, systemctl: &Path, record: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("__record-unit-exit")
        .arg("--record")
        .arg(record)
        .arg("--unit")
        .arg(UNIT)
        .arg("--systemctl")
        .arg(systemctl)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("INVOCATION_ID", "5f3b1a2c4d6e7f8a")
        .env("SERVICE_RESULT", "success")
        .env("TALLY_ATTEMPT", "1")
        .env("TALLY_LEASE_EPOCH", "7")
        .env("EXIT_CODE", "exited")
        .env("EXIT_STATUS", "0")
        .current_dir(temp)
        .output()
        .unwrap()
}

#[test]
fn a_successful_accounting_probe_is_embedded_in_the_exit_record() {
    let temp = tempfile::tempdir().unwrap();
    let systemctl = fake_systemctl(
        temp.path(),
        "fake-systemctl-ok",
        r#"
if [ "$1" = "--user" ] && [ "$2" = "show" ]; then
    echo "CPUUsageNSec=1500000000"
    echo "ExecMainStartTimestampMonotonic=1000000"
    echo "ExecMainExitTimestampMonotonic=3500000"
    exit 0
fi
exit 1
"#,
    );
    let record_path = temp.path().join("exit.json");
    let output = record_unit_exit(temp.path(), &systemctl, &record_path);
    assert!(
        output.status.success(),
        "recorder failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record = read_exit_record(&record_path, UNIT).unwrap();
    let accounting = record
        .accounting
        .expect("a successful probe embeds an accounting sample");
    assert_eq!(accounting.cpu_usage_nsec, Some(1_500_000_000));
    assert_eq!(accounting.exec_main_start_monotonic_usec, Some(1_000_000));
    assert_eq!(accounting.exec_main_exit_monotonic_usec, Some(3_500_000));
    assert_eq!(accounting.cpu_seconds(), Some(1.5));
    assert_eq!(accounting.wall_seconds(), Some(2.5));
}

#[test]
fn a_failed_accounting_probe_never_blocks_the_exit_record_and_logs_the_fact() {
    let temp = tempfile::tempdir().unwrap();
    let systemctl = fake_systemctl(
        temp.path(),
        "fake-systemctl-fail",
        r#"
echo "unit is gone" >&2
exit 1
"#,
    );
    let record_path = temp.path().join("exit.json");
    let output = record_unit_exit(temp.path(), &systemctl, &record_path);
    assert!(
        output.status.success(),
        "a failed accounting probe must not fail the exit record: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unit accounting probe failed"),
        "expected a logged fact about the failed probe, got: {stderr}"
    );
    let record = read_exit_record(&record_path, UNIT).unwrap();
    assert_eq!(
        record.accounting, None,
        "a failed probe is a typed absence, never a fabricated zero"
    );
    assert_eq!(record.schema_version, UNIT_EXIT_SCHEMA_VERSION);
}

#[test]
fn a_missing_systemctl_binary_never_blocks_the_exit_record() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("does-not-exist");
    let record_path = temp.path().join("exit.json");
    let output = record_unit_exit(temp.path(), &missing, &record_path);
    assert!(
        output.status.success(),
        "a missing systemctl binary must not fail the exit record: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record = read_exit_record(&record_path, UNIT).unwrap();
    assert_eq!(record.accounting, None);
}

#[test]
fn accounting_reads_not_set_properties_as_a_typed_absence_not_an_error() {
    let temp = tempfile::tempdir().unwrap();
    let systemctl = fake_systemctl(
        temp.path(),
        "fake-systemctl-not-set",
        r#"
if [ "$1" = "--user" ] && [ "$2" = "show" ]; then
    echo "CPUUsageNSec=[not set]"
    echo "ExecMainStartTimestampMonotonic=1000000"
    echo "ExecMainExitTimestampMonotonic=3500000"
    exit 0
fi
exit 1
"#,
    );
    let record_path = temp.path().join("exit.json");
    let output = record_unit_exit(temp.path(), &systemctl, &record_path);
    assert!(output.status.success());
    let record = read_exit_record(&record_path, UNIT).unwrap();
    let accounting = record.accounting.expect("probe succeeded overall");
    assert_eq!(accounting.cpu_usage_nsec, None);
    assert_eq!(accounting.cpu_seconds(), None);
    assert_eq!(accounting.wall_seconds(), Some(2.5));
}
