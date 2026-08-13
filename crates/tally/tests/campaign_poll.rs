use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

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
