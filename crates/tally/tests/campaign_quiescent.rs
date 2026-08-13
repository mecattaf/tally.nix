use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tally_core::campaign_registry::{
    CampaignRegistration, CampaignRegistrationV4, CampaignRegistry, REGISTRY_SCHEMA_VERSION,
};

const WORKLIST: &str = "specs/night/tasks.json";

fn quiescent(state_dir: &Path, absent_socket: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tally"))
        .args(["campaign", "quiescent", "--state-dir"])
        .arg(state_dir)
        .env("TALLY_SOCKET", absent_socket)
        .env_remove("TALLY_JOB_ID")
        .env_remove("TALLY_JOB_TOKEN")
        .output()
        .unwrap()
}

fn arm_fixture(state_dir: &Path, fixture_dir: &Path) {
    fs::create_dir_all(fixture_dir).unwrap();
    let flow = fixture_dir.join("spec-build.js");
    let driver = fixture_dir.join("spec-build-driver");
    fs::write(&flow, "fixture flow\n").unwrap();
    fs::write(&driver, "fixture driver\n").unwrap();

    let authority = CampaignRegistrationV4 {
        schema_version: REGISTRY_SCHEMA_VERSION,
        registration_id: "0198a62b-41ee-7000-8000-000000000539".to_owned(),
        worklist_pattern: WORKLIST.to_owned(),
        code_repository: "acme/widgets".to_owned(),
        checkout: PathBuf::from("/srv/acme/widgets"),
        base_branch: "main".to_owned(),
        remote: "origin".to_owned(),
        armed_at: "2026-08-12T20:00:00Z".to_owned(),
        arm_serial: 1,
        approved_graph_digest: format!("sha256:{}", "a".repeat(64)),
        // SAFETY: `geteuid` has no preconditions and does not mutate process state.
        local_actor: format!("uid:{}", unsafe { libc::geteuid() }),
        allowed_actors: vec!["operator".to_owned()],
        last_observation: None,
        flow,
        driver,
        workspace_root: PathBuf::from("/var/lib/tally/campaigns"),
    };
    let mut registration = CampaignRegistration::new(authority, None);
    CampaignRegistry::open(state_dir)
        .unwrap()
        .write(&mut registration)
        .unwrap();
}

#[test]
fn quiescent_reads_both_exit_paths_from_the_registry_without_a_daemon() {
    let temporary = tempfile::tempdir().unwrap();
    let state_dir = temporary.path().join("state");
    let absent_socket = temporary.path().join("daemon-is-not-running.sock");

    let empty = quiescent(&state_dir, &absent_socket);
    assert_eq!(empty.status.code(), Some(0), "{empty:?}");
    assert!(empty.stdout.is_empty(), "{empty:?}");
    assert!(empty.stderr.is_empty(), "{empty:?}");

    arm_fixture(&state_dir, &temporary.path().join("fixture-assets"));
    let armed = quiescent(&state_dir, &absent_socket);
    assert_eq!(armed.status.code(), Some(1), "{armed:?}");
    assert!(armed.stdout.is_empty(), "{armed:?}");

    let stderr = String::from_utf8(armed.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 1, "{stderr:?}");
    let listing: Value = serde_json::from_str(stderr.trim_end()).unwrap();
    let registrations = listing.as_array().expect("listing must be a JSON array");
    assert_eq!(registrations.len(), 1, "{listing}");
    assert_eq!(registrations[0]["codeRepository"], "acme/widgets");
    assert_eq!(registrations[0]["worklistPattern"], WORKLIST);
}
