use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tally_core::campaign_registry::{
    CampaignRegistration, CampaignRegistrationV2, CampaignRegistry, REGISTRY_SCHEMA_VERSION,
};

#[path = "support/shell_program.rs"]
mod shell_program;

fn arm_fixture(
    state_dir: &Path,
    assets: &Path,
    registration_id: &str,
    repository: &str,
    issue_number: u64,
) {
    fs::create_dir_all(assets).unwrap();
    let flow = assets.join(format!("{issue_number}-flow.js"));
    let driver = assets.join(format!("{issue_number}-driver"));
    fs::write(&flow, "fixture flow\n").unwrap();
    fs::write(&driver, "fixture driver\n").unwrap();
    let issue_url = format!("https://github.com/{repository}/issues/{issue_number}");
    let mut registration = CampaignRegistration::new(
        CampaignRegistrationV2 {
            schema_version: REGISTRY_SCHEMA_VERSION,
            registration_id: registration_id.to_owned(),
            issue_url,
            repository: repository.to_owned(),
            issue_number,
            armed_at: "2026-08-12T20:00:00Z".to_owned(),
            arm_serial: 1,
            approved_graph_digest: format!("sha256:{}", "a".repeat(64)),
            authenticated_actor: "operator".to_owned(),
            allowed_actors: vec!["operator".to_owned()],
            allow_test_local_forge: false,
            sub_issue_walk: true,
            last_observation: None,
            last_forge_observation: None,
            flow,
            driver,
            workspace_root: PathBuf::from("/var/lib/tally/campaigns"),
        },
        None,
    );
    CampaignRegistry::open(state_dir)
        .unwrap()
        .write(&mut registration)
        .unwrap();
}

#[test]
fn two_closed_campaigns_emit_two_attributed_completion_events_and_prune_cleanly() {
    let temporary = tempfile::tempdir().unwrap();
    let state_dir = temporary.path().join("state");
    let assets = temporary.path().join("assets");
    let first_id = "0198f000-0000-7000-8000-000000000011";
    let second_id = "0198f000-0000-7000-8000-000000000012";
    arm_fixture(&state_dir, &assets, first_id, "acme/one", 1);
    arm_fixture(&state_dir, &assets, second_id, "acme/two", 2);

    let fake_gh = temporary.path().join("gh");
    shell_program::install(
        &fake_gh,
        concat!(
            "#!/bin/sh\n",
            "case \"$*\" in\n",
            "  'api repos/acme/one/issues/1') printf '%s\\n' ",
            "'{\"number\":1,\"state\":\"closed\",\"html_url\":\"https://github.com/acme/one/issues/1\",\"updated_at\":\"2026-08-12T20:01:00Z\",\"user\":{\"login\":\"operator\"}}' ;;\n",
            "  'api repos/acme/two/issues/2') printf '%s\\n' ",
            "'{\"number\":2,\"state\":\"closed\",\"html_url\":\"https://github.com/acme/two/issues/2\",\"updated_at\":\"2026-08-12T20:02:00Z\",\"user\":{\"login\":\"operator\"}}' ;;\n",
            "  *) echo \"unexpected gh call: $*\" >&2; exit 91 ;;\n",
            "esac\n",
        ),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_tally"))
        .args(["campaign", "poll", "--once", "--state-dir"])
        .arg(&state_dir)
        .env("TALLY_GH_PROGRAM", &fake_gh)
        .env("TALLY_SOCKET", temporary.path().join("absent.sock"))
        .env("HOME", temporary.path())
        .env_remove("TALLY_JOB_ID")
        .env_remove("TALLY_JOB_TOKEN")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");

    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2, "{events:?}");
    let by_id = events
        .into_iter()
        .map(|event| (event["registrationId"].as_str().unwrap().to_owned(), event))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(
        by_id[first_id]["issueUrl"],
        "https://github.com/acme/one/issues/1"
    );
    assert_eq!(
        by_id[second_id]["issueUrl"],
        "https://github.com/acme/two/issues/2"
    );
    for event in by_id.values() {
        assert_eq!(event["schemaVersion"], 1);
        assert_eq!(event["status"], "complete");
        assert_eq!(event["action"], "pruned");
        assert!(event.get("detail").is_none());
    }

    assert!(
        CampaignRegistry::open(&state_dir)
            .unwrap()
            .registrations()
            .unwrap()
            .is_empty(),
        "closed canonical masters must leave no live registrations"
    );
}

#[test]
fn an_already_queued_reconcile_decodes_a_closed_master_as_complete() {
    let temporary = tempfile::tempdir().unwrap();
    let checkout = temporary.path().join("checkout");
    fs::create_dir(&checkout).unwrap();
    let driver = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../drivers/spec_build_driver.py");
    let script = r#"
import importlib.util
import json
import subprocess
import sys

spec = importlib.util.spec_from_file_location("spec_build_driver", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
checkout = sys.argv[2]
revision = "a" * 40
manifest = {
    "schemaVersion": 1,
    "name": "closed-fixture",
    "repository": {
        "checkout": checkout,
        "baseBranch": "main",
        "remote": "origin",
        "forge": "github",
    },
    "maxTasks": 100,
    "maxParallel": 1,
    "driverRuntimeMaxSec": 900,
    "runtimeMaxSec": None,
    "pool": "campaign",
    "mergeMethod": "squash",
    "gitAiBinding": "off",
    "gitAiAwaitSec": 60,
    "agent": {
        "adapter": "codex",
        "argv": ["codex", "exec"],
        "priority": "medium",
        "runtimeMaxSec": None,
        "approvalPolicy": "never",
        "sandboxPolicy": "danger-full-access",
        "diagnosisSandboxPolicy": "workspace-write",
        "model": None,
    },
    "steward": None,
    "gates": [{
        "kind": "forbidPaths",
        "id": "scope",
        "forbidPaths": ["*.db"],
        "runtimeMaxSec": 30,
    }],
    "tasks": [{
        "id": "task-a",
        "kind": "implementation",
        "issue": 2,
        "dependencies": [],
        "conflictDomains": [],
        "argv": None,
        "runtimeMaxSec": None,
    }],
}
tasks = [{"number": 2, "title": "Task A", "body": "Implement task A."}]
graph = {
    "manifest": manifest,
    "tasks": tasks,
    "executableDigest": module.canonical_sha256({"manifest": manifest, "tasks": tasks}),
}
master = {
    "number": 1,
    "state": "closed",
    "html_url": "https://github.com/acme/one/issues/1",
    "body": "closed campaign",
}
responses = iter([master])
module.github_json = lambda *args, **kwargs: next(responses)

def fake_git(_checkout, *args, **kwargs):
    stdout = revision + "\n" if args[:2] == ("rev-parse", "--verify") else ".git\n"
    return subprocess.CompletedProcess(["git", *args], 0, stdout, "")

module.git = fake_git
result = module.action_reconcile({
    "repository": "acme/one",
    "issue": {"number": "1", "url": "https://github.com/acme/one/issues/1"},
    "worklist": {"kind": "github-issue", "graphDigest": graph["executableDigest"]},
    "armedManifest": manifest,
    "campaignGraph": graph,
})
print(json.dumps(result, sort_keys=True))
"#;

    let output = Command::new("python3")
        .args(["-c", script])
        .arg(&driver)
        .arg(&checkout)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["complete"], true);
    assert_eq!(result["remaining"], serde_json::json!([]));
    assert_eq!(result["frontier"], serde_json::json!([]));
    assert_eq!(result["closingSummary"], Value::Null);
    assert_eq!(result["config"]["campaign"], "closed-fixture");
}
