use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tally_core::adapters::AdapterHardening;
use tally_core::config::Priority;
use tally_core::exec_attestation::ExecAttestationContext;
use tally_core::executor::{
    ExecutionBackend, ExecutionIdentity, ExecutionRequest, ExecutionTermination, Executor,
    UnitLimits, Uuid,
};
use tokio::process::Command;

#[path = "support/live.rs"]
mod live_support;

const BASH: &str = "/run/current-system/sw/bin/bash";
const JOURNALCTL: &str = "/run/current-system/sw/bin/journalctl";
const SYSTEMCTL: &str = "/run/current-system/sw/bin/systemctl";
const SYSTEMD_RUN: &str = "/run/current-system/sw/bin/systemd-run";

struct UnitCleanup {
    units: Vec<String>,
}

impl UnitCleanup {
    fn new() -> Self {
        Self { units: Vec::new() }
    }

    fn track(&mut self, unit: String) {
        self.units.push(unit);
    }
}

impl Drop for UnitCleanup {
    fn drop(&mut self) {
        for unit in &self.units {
            let _ = std::process::Command::new(SYSTEMCTL)
                .args(["--user", "stop", "--", unit])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = std::process::Command::new(SYSTEMCTL)
                .args(["--user", "reset-failed", "--", unit])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn request(argv: Vec<String>) -> ExecutionRequest {
    ExecutionRequest {
        identity: ExecutionIdentity {
            job_id: Uuid::new_v4(),
            task_uuid: Some(Uuid::new_v4()),
        },
        parent: Some(Uuid::new_v4()),
        pools: vec!["worker-live".to_owned()],
        lease_epoch: 19,
        attempt: 1,
        priority: Priority::High,
        no_enqueue: true,
        argv,
        yield_hook: None,
        tally_socket: None,
        environment: BTreeMap::new(),
        gh_origin: None,
        brief_hash: None,
        brief_path: None,
        brief_document: None,
        cwd: None,
        workspace: None,
        gate_manifest: None,
        git_ai: None,
        exec_attestation: None,
        hardening: Default::default(),
        extra_writable_paths: Vec::new(),
        credentials: BTreeMap::new(),
        limits: UnitLimits {
            cpu_weight: 250,
            memory_max_bytes: 512 * 1024 * 1024,
        },
        runtime_max_sec: Some(10),
    }
}

fn live_executor(state_dir: &Path) -> Executor {
    Executor::new(state_dir, env!("CARGO_BIN_EXE_tally"))
        .with_systemd_run(SYSTEMD_RUN)
        .with_systemctl(SYSTEMCTL)
}

async fn systemctl(args: &[&OsStr]) -> std::process::Output {
    Command::new(SYSTEMCTL)
        .arg("--user")
        .args(args)
        .output()
        .await
        .expect("systemctl must exist on the designated NixOS worker")
}

async fn wait_for_active(unit: &str) -> String {
    for _ in 0..50 {
        let output = systemctl(&[
            OsStr::new("show"),
            OsStr::new("--property=ActiveState"),
            OsStr::new("--property=CPUWeight"),
            OsStr::new("--property=MemoryMax"),
            OsStr::new("--property=RuntimeMaxUSec"),
            OsStr::new("--property=Environment"),
            OsStr::new("--property=UnsetEnvironment"),
            OsStr::new("--property=StandardOutput"),
            OsStr::new("--property=StandardError"),
            OsStr::new("--property=ExecStopPost"),
            OsStr::new("--property=LoadCredential"),
            OsStr::new("--"),
            OsStr::new(unit),
        ])
        .await;
        let properties = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() && properties.contains("ActiveState=active") {
            return properties;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("transient unit {unit} never became active");
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

fn assert_private_file(path: &Path) {
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600,
        "{} must be private",
        path.display()
    );
}

#[tokio::test]
#[ignore = "requires an explicitly selected NixOS host with a user manager"]
async fn real_user_manager_executor_smoke() {
    let Some(_remote_host) = live_support::require_remote_host("real_user_manager_executor_smoke")
    else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let state_dir = temp.path().join("state");
    std::fs::create_dir(&state_dir).unwrap();
    let credential = temp.path().join("credential-token");
    let credential_copy = temp.path().join("credential-copy");
    let secret = "tally-live-secret-value";
    std::fs::write(&credential, secret).unwrap();
    std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&credential_copy, b"").unwrap();
    std::fs::set_permissions(&credential_copy, std::fs::Permissions::from_mode(0o600)).unwrap();

    let executor = live_executor(&state_dir);
    let mut cleanup = UnitCleanup::new();
    let script = r#"
set -eu
printf 'pool=%s class=%s epoch=%s no_enqueue=%s credentials=%s\n' \
  "$TALLY_POOL" "$TALLY_CLASS" "$TALLY_LEASE_EPOCH" "$TALLY_NO_ENQUEUE" "$TALLY_CREDENTIALS"
printf 'stderr-line\n' >&2
credential=$(<"$CREDENTIALS_DIRECTORY/token")
test -n "$credential"
printf '%s' "$credential" > "$1"
printf 'credential-ok\n'
sleep 3
"#;
    let mut success = request(vec![
        BASH.to_owned(),
        "-c".to_owned(),
        script.to_owned(),
        "tally-live-credential".to_owned(),
        credential_copy.to_string_lossy().into_owned(),
    ]);
    success
        .credentials
        .insert("token".to_owned(), credential.clone());
    let success_unit = executor.unit_name(&success.identity);
    cleanup.track(success_unit.clone());

    let rendered_argv = executor
        .build_systemd_argv(&success)
        .unwrap()
        .into_iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered_argv.contains(&format!("LoadCredential=token:{}", credential.display())));
    assert!(!rendered_argv.contains(secret));

    let running_executor = executor.clone();
    let running = tokio::spawn(async move { running_executor.execute(success).await });
    let properties = wait_for_active(&success_unit).await;
    assert!(properties.contains("CPUWeight=250"), "{properties}");
    assert!(properties.contains("MemoryMax=536870912"), "{properties}");
    assert!(properties.contains("RuntimeMaxUSec="), "{properties}");
    assert!(properties.contains("RuntimeMaxUSec=10s"), "{properties}");
    for name in [
        "TALLY_JOB_ID",
        "TALLY_TASK_UUID",
        "TALLY_PARENT",
        "TALLY_POOL",
        "TALLY_LEASE_EPOCH",
        "TALLY_CLASS",
        "TALLY_NO_ENQUEUE",
        "TALLY_CREDENTIALS",
    ] {
        assert!(properties.contains(name), "missing {name} in {properties}");
    }
    assert!(!properties.contains(secret));

    let cat = systemctl(&[
        OsStr::new("cat"),
        OsStr::new("--"),
        OsStr::new(&success_unit),
    ])
    .await;
    assert!(
        cat.status.success(),
        "{}",
        String::from_utf8_lossy(&cat.stderr)
    );
    let transient_unit = String::from_utf8_lossy(&cat.stdout);
    assert!(transient_unit.contains(&format!("LoadCredential=token:{}", credential.display())));
    assert!(transient_unit.contains("StandardOutput=append:"));
    assert!(transient_unit.contains("StandardError=append:"));
    assert!(transient_unit.contains("ExecStopPost="));
    assert!(!transient_unit.contains(secret));

    let success = running.await.unwrap().unwrap();
    assert_eq!(success.backend, ExecutionBackend::Systemd);
    assert_eq!(success.termination, ExecutionTermination::Exited(0));
    assert_eq!(success.record.attempt, 1);
    assert_eq!(success.record.lease_epoch, 19);
    assert_eq!(success.record.service_result, "success");
    let stdout = std::fs::read_to_string(&success.paths.stdout).unwrap();
    let stderr = std::fs::read_to_string(&success.paths.stderr).unwrap();
    assert!(stdout.contains("pool=worker-live class=high epoch=19 no_enqueue=1"));
    assert!(stdout.contains("credentials=[\"token\"]"));
    assert!(stdout.contains("credential-ok"));
    assert_eq!(std::fs::read_to_string(&credential_copy).unwrap(), secret);
    assert_eq!(stderr, "stderr-line\n");
    assert!(!stdout.contains(secret));
    assert!(!stderr.contains(secret));
    let exit_json = std::fs::read_to_string(&success.paths.exit_record).unwrap();
    assert!(!exit_json.contains(secret));
    assert_private_file(&success.paths.stdout);
    assert_private_file(&success.paths.stderr);
    assert_private_file(&success.paths.exit_record);
    assert_private_file(&credential_copy);
    let journal = Command::new(JOURNALCTL)
        .arg("--user")
        .arg("--no-pager")
        .arg("--output=json")
        .arg(format!(
            "_SYSTEMD_INVOCATION_ID={}",
            success.record.invocation_id
        ))
        .output()
        .await
        .expect("journalctl must exist on the designated NixOS worker");
    assert!(journal.status.success());
    assert!(!String::from_utf8_lossy(&journal.stdout).contains(secret));
    assert!(!String::from_utf8_lossy(&journal.stderr).contains(secret));
    assert_collected(&success_unit).await;

    let mut timeout_request = request(vec![
        BASH.to_owned(),
        "-c".to_owned(),
        r#"
set -eu
test -z "${TALLY_TASK_UUID+x}"
test -z "${TALLY_PARENT+x}"
test -z "${TALLY_NO_ENQUEUE+x}"
test -z "${TALLY_CREDENTIALS+x}"
test -z "${CREDENTIALS_DIRECTORY+x}"
printf 'optional-environment-absent\n'
sleep 30
"#
        .to_owned(),
    ]);
    timeout_request.identity.task_uuid = None;
    timeout_request.parent = None;
    timeout_request.no_enqueue = false;
    let timeout_unit = executor.unit_name(&timeout_request.identity);
    cleanup.track(timeout_unit.clone());
    timeout_request.runtime_max_sec = Some(1);
    let timeout_executor = executor.clone();
    let timeout_running =
        tokio::spawn(async move { timeout_executor.execute(timeout_request).await });
    let timeout_properties = wait_for_active(&timeout_unit).await;
    for name in [
        "TALLY_TASK_UUID",
        "TALLY_PARENT",
        "TALLY_NO_ENQUEUE",
        "TALLY_CREDENTIALS",
        "CREDENTIALS_DIRECTORY",
    ] {
        assert!(
            timeout_properties.contains(name),
            "missing {name} from UnsetEnvironment in {timeout_properties}"
        );
    }
    let timeout = timeout_running.await.unwrap().unwrap();
    assert_eq!(timeout.backend, ExecutionBackend::Systemd);
    assert_eq!(timeout.termination, ExecutionTermination::RuntimeExceeded);
    assert_eq!(timeout.record.attempt, 1);
    assert_eq!(timeout.record.lease_epoch, 19);
    assert_eq!(timeout.record.service_result, "timeout");
    assert!(std::fs::read_to_string(&timeout.paths.stdout)
        .unwrap()
        .contains("optional-environment-absent"));
    assert_collected(&timeout_unit).await;

    let marker = temp.path().join("exit-127-count");
    let exit_127 = request(vec![
        BASH.to_owned(),
        "-c".to_owned(),
        "printf x >> \"$1\"; exit 127".to_owned(),
        "tally-live-exit-127".to_owned(),
        marker.to_string_lossy().into_owned(),
    ]);
    let exit_127_unit = executor.unit_name(&exit_127.identity);
    cleanup.track(exit_127_unit.clone());
    let exit_127 = executor.execute(exit_127).await.unwrap();
    assert_eq!(exit_127.backend, ExecutionBackend::Systemd);
    assert_eq!(exit_127.termination, ExecutionTermination::Exited(127));
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "x");
    assert_collected(&exit_127_unit).await;
}

#[tokio::test]
#[ignore = "requires an explicitly selected NixOS host with a user manager"]
async fn real_user_manager_hardening_presets_scope_writes() {
    let Some(_remote_host) =
        live_support::require_remote_host("real_user_manager_hardening_presets_scope_writes")
    else {
        return;
    };
    let home = std::env::var_os("HOME").expect("the selected live user must have HOME");
    let temp = tempfile::Builder::new()
        .prefix("tally-hardening-live-")
        .tempdir_in(home)
        .unwrap();

    for hardening in [AdapterHardening::Strict, AdapterHardening::Production] {
        let label = match hardening {
            AdapterHardening::Strict => "strict",
            AdapterHardening::Production => "production",
            _ => unreachable!(),
        };
        let root = temp.path().join(label);
        let state_dir = root.join("state");
        let allowed_dir = root.join("adapter-state");
        let allowed = allowed_dir.join("written");
        let forbidden = state_dir.join("forbidden");
        let other_capture = state_dir.join("capture/other-job.out");
        std::fs::create_dir_all(&allowed_dir).unwrap();
        std::fs::create_dir_all(other_capture.parent().unwrap()).unwrap();
        std::fs::write(&other_capture, b"untouched").unwrap();

        let executor = live_executor(&state_dir);
        let mut cleanup = UnitCleanup::new();
        let script = r#"
set -eu
printf allowed > "$1"
if printf forbidden > "$2" 2>/dev/null; then exit 70; fi
if printf replaced > "$3" 2>/dev/null; then exit 71; fi
printf 'hardening-ready\n'
sleep 3
"#;
        let mut hardened = request(vec![
            BASH.to_owned(),
            "-c".to_owned(),
            script.to_owned(),
            format!("tally-live-{label}"),
            allowed.to_string_lossy().into_owned(),
            forbidden.to_string_lossy().into_owned(),
            other_capture.to_string_lossy().into_owned(),
        ]);
        hardened.hardening = hardening;
        hardened.extra_writable_paths = vec![allowed_dir.clone()];
        hardened.exec_attestation = Some(ExecAttestationContext {
            adapter: "shell".to_owned(),
            executor: None,
            payload_hash: None,
            brief_hash: None,
            evidence: vec!["exit:0".to_owned()],
        });
        let unit = executor.unit_name(&hardened.identity);
        cleanup.track(unit.clone());
        let running_executor = executor.clone();
        let running = tokio::spawn(async move { running_executor.execute(hardened).await });
        wait_for_active(&unit).await;

        let properties = systemctl(&[
            OsStr::new("show"),
            OsStr::new("--property=ProtectHome"),
            OsStr::new("--property=ProtectSystem"),
            OsStr::new("--property=PrivateTmp"),
            OsStr::new("--property=NoNewPrivileges"),
            OsStr::new("--property=RestrictAddressFamilies"),
            OsStr::new("--property=PrivateDevices"),
            OsStr::new("--property=ProtectKernelTunables"),
            OsStr::new("--property=ProtectKernelModules"),
            OsStr::new("--property=ProtectKernelLogs"),
            OsStr::new("--property=ProtectControlGroups"),
            OsStr::new("--property=ProtectClock"),
            OsStr::new("--property=RestrictSUIDSGID"),
            OsStr::new("--property=LockPersonality"),
            OsStr::new("--property=RestrictRealtime"),
            OsStr::new("--property=SystemCallFilter"),
            OsStr::new("--property=CapabilityBoundingSet"),
            OsStr::new("--property=ProtectProc"),
            OsStr::new("--property=ReadWritePaths"),
            OsStr::new("--"),
            OsStr::new(&unit),
        ])
        .await;
        assert!(properties.status.success());
        let properties = String::from_utf8_lossy(&properties.stdout);
        for expected in [
            "ProtectHome=read-only",
            "ProtectSystem=strict",
            "PrivateTmp=yes",
            "NoNewPrivileges=yes",
        ] {
            assert!(
                properties.lines().any(|line| line == expected),
                "{properties}"
            );
        }
        assert!(properties.contains("AF_UNIX"), "{properties}");
        assert!(properties.contains("AF_INET"), "{properties}");
        assert!(properties.contains("AF_INET6"), "{properties}");
        let writable = properties
            .lines()
            .find_map(|line| line.strip_prefix("ReadWritePaths="))
            .expect("ReadWritePaths must be reported");
        assert!(!writable
            .split_whitespace()
            .any(|path| path == state_dir.to_str().unwrap()));
        assert!(
            writable.contains(allowed_dir.to_str().unwrap()),
            "{writable}"
        );
        assert!(
            !writable.contains(other_capture.to_str().unwrap()),
            "{writable}"
        );

        if hardening == AdapterHardening::Production {
            for expected in [
                "PrivateDevices=yes",
                "ProtectKernelTunables=yes",
                "ProtectKernelModules=yes",
                "ProtectKernelLogs=yes",
                "ProtectControlGroups=yes",
                "ProtectClock=yes",
                "RestrictSUIDSGID=yes",
                "LockPersonality=yes",
                "RestrictRealtime=yes",
                "CapabilityBoundingSet=",
                "ProtectProc=invisible",
            ] {
                assert!(
                    properties.lines().any(|line| line == expected),
                    "{properties}"
                );
            }
            assert!(
                properties
                    .lines()
                    .any(|line| line.starts_with("SystemCallFilter=") && line.len() > 17),
                "{properties}"
            );
        }

        let outcome = running.await.unwrap().unwrap();
        assert_eq!(outcome.backend, ExecutionBackend::Systemd);
        assert_eq!(outcome.termination, ExecutionTermination::Exited(0));
        assert_eq!(std::fs::read_to_string(&allowed).unwrap(), "allowed");
        assert!(!forbidden.exists());
        assert_eq!(
            std::fs::read_to_string(&other_capture).unwrap(),
            "untouched"
        );
        assert!(state_dir.join("exec-attestations.jsonl").exists());
        assert_collected(&unit).await;
    }
}
