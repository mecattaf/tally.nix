//! Issue #382: the exit recorder's accounting probe, exercised through the
//! real `__record-unit-exit` binary the same way `ExecStopPost` invokes it.
//!
//! This runs the compiled `tally` binary as a fresh child process for every
//! case specifically so the environment variables the recorder reads
//! (`INVOCATION_ID`, `SERVICE_RESULT`, `TALLY_ATTEMPT`, `TALLY_LEASE_EPOCH`)
//! are set on that one child rather than mutated on the shared test-process
//! environment, which `cargo test`'s default parallel threads would race.

use std::path::Path;
use std::process::Command;

use tally_core::executor::{read_exit_record, UNIT_EXIT_SCHEMA_VERSION};

#[path = "support/shell_program.rs"]
mod shell_program;

const UNIT: &str = "tally-job-00000000-0000-4000-8000-000000000001.service";
const EMPTY_CONFIG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/empty-config.json");

/// Install a fake `systemctl` through the immutable provider rather than
/// writing an executable and `chmod`ing it (#396): a program still open for
/// writing anywhere on the host cannot be `execve`d, and in a parallel test
/// binary a sibling thread's fork holds exactly such an fd until its own
/// `execve` closes it.
fn fake_systemctl(dir: &Path, name: &str, script: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    shell_program::install(&path, format!("#!/bin/sh\n{script}\n"));
    path
}

/// Issue #396: every caller of `shell_program::install` is immune to `ETXTBSY`
/// for one reason only — the file the kernel is asked to `execve` is a
/// checked-in fixture this process never opens. That is a property of the
/// installer, so it is pinned once, here, rather than once per caller.
///
/// It is deliberately not "the installed program runs". A program written and
/// `chmod +x`'d a microsecond earlier also runs, whenever no fork happens to be
/// holding it — which is precisely the race that red-gated an innocent sha and
/// never reproduced on a quiet host.
#[test]
fn an_installed_program_is_a_symlink_to_the_checked_in_provider_not_a_written_file() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let program = temp.path().join("probe");
    shell_program::install(&program, "#!/bin/sh\nexit 0\n");

    let installed = std::fs::symlink_metadata(&program).unwrap();
    assert!(
        installed.file_type().is_symlink(),
        "the exec target must be a symlink to the checked-in provider, not a file this \
         process wrote"
    );
    let target = std::fs::read_link(&program).unwrap();
    assert!(
        target.ends_with("test/fixtures/shell-command-provider"),
        "unexpected provider target {}",
        target.display()
    );
    assert!(
        !target.starts_with(temp.path()),
        "the exec target resolves inside the directory this test writes into, so it is a \
         file this process can hold open for writing: {}",
        target.display()
    );
    assert!(target.exists(), "{} is not checked in", target.display());

    let mut sidecar = program.clone().into_os_string();
    sidecar.push(".tally-test-script");
    assert_eq!(
        std::fs::metadata(std::path::PathBuf::from(sidecar))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0,
        "the file the installer writes must never be executable"
    );
}

fn record_unit_exit(temp: &Path, systemctl: &Path, record: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("__record-unit-exit")
        .args(["--config", EMPTY_CONFIG])
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
