use std::ffi::OsStr;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tally_core::config::{Config, Priority};
use tally_core::journal::{
    parse_journal_json_line, EmitEvent, JournalEmitter, TallyEvent, JOURNAL_IDENTIFIER,
};
use tally_core::taskdb::EnqueueSource;
use tokio::process::Command;

#[path = "support/live.rs"]
mod live_support;

const JOURNALCTL: &str = "/run/current-system/sw/bin/journalctl";
const SYSTEMCTL: &str = "/run/current-system/sw/bin/systemctl";
const SYSTEMD_RUN: &str = "/run/current-system/sw/bin/systemd-run";
const CHILD_MODE: &str = "TALLY_LIVE_JOURNAL_CHILD";

struct UnitCleanup(String);

impl Drop for UnitCleanup {
    fn drop(&mut self) {
        for verb in ["stop", "reset-failed"] {
            let _ = std::process::Command::new(SYSTEMCTL)
                .args(["--user", verb, "--", &self.0])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn event(marker: &str, path: &str) -> EmitEvent {
    let mut event = EmitEvent::enqueued(marker, Priority::High, EnqueueSource::Manual);
    event.message = Some(format!("live-journal {path} {marker}"));
    event.job_id = Some(format!("job-{marker}"));
    event.pools = Some(vec!["worker-live".to_owned()]);
    event
}

fn configured_emitter(native: bool) -> JournalEmitter {
    let config: Config = serde_json::from_value(serde_json::json!({
        "journald": { "native": native }
    }))
    .expect("journald toggle must parse through the production config shape");
    JournalEmitter::from_config(&config.journald)
}

async fn systemctl(args: &[&OsStr]) -> std::process::Output {
    Command::new(SYSTEMCTL)
        .arg("--user")
        .args(args)
        .output()
        .await
        .expect("systemctl must exist on the designated NixOS worker")
}

async fn assert_collected(unit: &str) {
    for _ in 0..50 {
        let output = systemctl(&[
            OsStr::new("show"),
            OsStr::new("--property=LoadState"),
            OsStr::new("--value"),
            OsStr::new("--"),
            OsStr::new(unit),
        ])
        .await;
        if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "not-found"
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("transient unit {unit} was not collected");
}

async fn read_since(epoch_seconds: u64) -> std::process::Output {
    Command::new(JOURNALCTL)
        .args([
            "--user",
            "--no-pager",
            "-t",
            JOURNAL_IDENTIFIER,
            "-o",
            "json",
            "--since",
            &format!("@{epoch_seconds}"),
        ])
        .output()
        .await
        .expect("journalctl must exist on the designated NixOS worker")
}

#[tokio::test]
#[ignore = "requires an explicitly selected NixOS host with a user manager and journal socket"]
// The stdout line below is the payload under test, not operator output.
#[allow(clippy::disallowed_macros)]
async fn real_user_manager_journal_paths() {
    if let Ok(marker) = std::env::var(CHILD_MODE) {
        // libtest writes `test <name> ... ` without a newline before entering the
        // test. End that harness-only prefix so the emitter's stdout payload is
        // still the standalone line a daemon writes under StandardOutput=journal.
        println!();
        configured_emitter(false)
            .emit(event(&marker, "stdout"))
            .expect("stdout fallback emit must succeed in the transient unit");
        return;
    }

    let Some(_remote_host) = live_support::require_remote_host("real_user_manager_journal_paths")
    else {
        return;
    };

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let nonce = format!("{}-{}", std::process::id(), now.as_nanos());
    let native_marker = format!("live-native-{nonce}");
    let stdout_marker = format!("live-stdout-{nonce}");
    let unit = format!("tally-live-journal-{nonce}.service");
    let _cleanup = UnitCleanup(unit.clone());
    let since = now.as_secs().saturating_sub(2);

    let mut ignored_stdout = Vec::new();
    let expected_stdout_fields = event(&stdout_marker, "stdout")
        .into_fields()
        .expect("stdout event must validate before the transient run");
    let native_fields = configured_emitter(true)
        .emit_to(event(&native_marker, "native"), &mut ignored_stdout)
        .expect("native journald emit must succeed");
    assert!(ignored_stdout.is_empty());

    let child = Command::new(SYSTEMD_RUN)
        .args(["--user", "--wait", "--collect", "--quiet"])
        .arg(format!("--unit={unit}"))
        .args([
            "--property=Type=exec",
            "--property=SyslogIdentifier=tally",
            "--property=StandardOutput=journal",
            "--property=StandardError=journal",
            "--property=RuntimeMaxSec=15s",
            "--setenv",
        ])
        .arg(format!("{CHILD_MODE}={stdout_marker}"))
        .arg(std::env::current_exe().unwrap())
        .args([
            "real_user_manager_journal_paths",
            "--ignored",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .await
        .expect("systemd-run must exist on the designated NixOS worker");
    assert!(
        child.status.success(),
        "stdout fallback transient failed: stdout={} stderr={}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );

    let mut raw_native = false;
    let mut raw_stdout = false;
    let mut parsed_native = None;
    let mut parsed_stdout = None;
    for _ in 0..50 {
        let journal = read_since(since).await;
        assert!(
            journal.status.success(),
            "journalctl failed: {}",
            String::from_utf8_lossy(&journal.stderr)
        );
        for line in String::from_utf8_lossy(&journal.stdout).lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if value.get("TALLY_TASK_UUID").and_then(Value::as_str) == Some(native_marker.as_str())
            {
                raw_native = true;
            }
            if value
                .get("MESSAGE")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains(&stdout_marker))
                && value.get("TALLY_TASK_UUID").is_none()
            {
                raw_stdout = true;
            }
            if let Ok(Some(entry)) = parse_journal_json_line(line) {
                if entry.fields.task_uuid == native_marker {
                    parsed_native = Some(entry.fields);
                } else if entry.fields.task_uuid == stdout_marker {
                    parsed_stdout = Some(entry.fields);
                }
            }
        }
        if parsed_native.is_some() && parsed_stdout.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(
        raw_native,
        "native custom fields did not land at journal top level"
    );
    assert!(
        raw_stdout,
        "stdout fallback did not land as one JSON MESSAGE record"
    );
    let parsed_native = parsed_native.expect("native record was not queryable through journald");
    let parsed_stdout = parsed_stdout.expect("stdout record was not queryable through journald");
    assert_eq!(parsed_native, native_fields);
    assert_eq!(parsed_native.event, TallyEvent::Enqueued);
    assert_eq!(
        parsed_native.pools.as_deref(),
        Some(["worker-live".to_owned()].as_slice())
    );
    assert_eq!(parsed_stdout.event, TallyEvent::Enqueued);
    assert_eq!(
        parsed_stdout.pools.as_deref(),
        Some(["worker-live".to_owned()].as_slice())
    );
    assert_eq!(parsed_stdout, expected_stdout_fields);
    assert_collected(&unit).await;
}
