use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;
use tally_core::config::Priority;
use tally_core::taskdb::{EnqueueSource, RowSeed, TaskDb};
use tally_core::witness::LaborClass;
use taskchampion::{Status, Uuid};

#[tokio::test(flavor = "current_thread")]
async fn stock_taskwarrior_reads_the_in_process_replica() {
    let temp = tempfile::tempdir().unwrap();
    let uuid = Uuid::new_v4();
    let taskdata_dir;
    {
        let mut db = TaskDb::open(temp.path()).await.unwrap();
        taskdata_dir = db.taskdata_dir().to_owned();
        let seed = RowSeed {
            uuid,
            description: "stock viewer compatibility".to_owned(),
            priority: Priority::Medium,
            source: EnqueueSource::Manual,
            adapter: "shell".to_owned(),
            pools: vec!["build-slot".to_owned()],
            executor: None,
            model: None,
            cwd: Some(PathBuf::from("/tmp")),
            workspace: None,
            adapter_options: Default::default(),
            gate_manifest: None,
            resumed_from: None,
            dedup_key: Some("stock-viewer-1".to_owned()),
            payload_hash: None,
            brief_hash: None,
            orchestration: None,
            session_ref: None,
            final_message: None,
            lease_epoch: 1,
            attempt: 1,
            argv: vec!["true".to_owned()],
            evidence: Vec::new(),
            parent_uuid: None,
            consumption_estimate: None,
            runtime_max_sec: None,
            no_enqueue: false,
            credentials: BTreeMap::new(),
            origin: None,
            gh_origin: None,
            related_trigger: None,
            evidence_class: None,
            manifest_hash: None,
        };
        let prepared = db
            .prepare_row(seed, Status::Pending, LaborClass::Fresh)
            .await
            .unwrap();
        db.commit_prepared([prepared]).await.unwrap();
    }

    let output = Command::new("task")
        .env("TASKRC", "/dev/null")
        .arg(format!("rc.data.location:{}", taskdata_dir.display()))
        .arg(uuid.to_string())
        .arg("export")
        .output()
        .expect("the test environment must provide stock Taskwarrior 3");
    assert!(
        output.status.success(),
        "task export failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["uuid"], uuid.to_string());
    assert_eq!(rows[0]["description"], "stock viewer compatibility");
    assert_eq!(rows[0]["dedup_key"], "stock-viewer-1");
    assert_eq!(rows[0]["pool"], "build-slot");
    assert_eq!(rows[0]["argv_json"], r#"["true"]"#);
}
