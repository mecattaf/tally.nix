use std::process::Command;

use serde_json::Value;

#[path = "support/shell_program.rs"]
mod shell_program;

#[test]
fn poll_once_no_enqueue_in_scratch_leaves_live_ingress_untouched() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&serde_json::json!({
            "pools": {"slot": {"resource": "build-slot"}},
            "adapters": {"shell": {"argv": []}},
            "producers": {
                "github": {
                    "kind": "gh",
                    "enable": true,
                    "sources": [{"notifications": {"repo": "acme/widgets"}}],
                    "triggers": {"commandComments": ["/tally run"]},
                    "allowedActors": ["contributor"],
                    "enqueue": {
                        "argv": ["run-widget-checks", "--fixed-option"],
                        "pool": "slot"
                    }
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let fake_gh = temp.path().join("gh");
    shell_program::install(
        &fake_gh,
        concat!(
            "#!/bin/sh\n",
            "case \"$*\" in\n",
            "  'api user') printf '{\"login\":\"tally-bot\"}' ;;\n",
            "  'api --method GET notifications -f all=false -f participating=false -f per_page=100')\n",
            "    printf '[{\"id\":\"notification-42\",\"reason\":\"subscribed\",\"updated_at\":\"2026-07-20T12:30:00Z\",\"repository\":{\"full_name\":\"acme/widgets\"},\"subject\":{\"type\":\"Issue\",\"url\":\"https://api.github.com/repos/acme/widgets/issues/42\",\"latest_comment_url\":\"https://api.github.com/repos/acme/widgets/issues/comments/4242\"}}]' ;;\n",
            "  'api /repos/acme/widgets/issues/42')\n",
            "    printf '{\"node_id\":\"I_widget_42\",\"number\":42,\"html_url\":\"https://github.com/acme/widgets/issues/42\",\"title\":\"Run checks\",\"body\":\"untrusted context\",\"state\":\"open\",\"user\":{\"login\":\"issue-author\"},\"labels\":[{\"name\":\"ready\"}],\"assignees\":[]}' ;;\n",
            "  'api /repos/acme/widgets/issues/comments/4242')\n",
            "    printf '{\"id\":4242,\"body\":\"/tally run\",\"created_at\":\"2026-07-20T12:30:00Z\",\"updated_at\":\"2026-07-20T12:30:00Z\",\"user\":{\"login\":\"contributor\"}}' ;;\n",
            "  *) exit 91 ;;\n",
            "esac\n",
        ),
    );

    let live_state = temp.path().join("live-state");
    let live_events = live_state.join("events");
    std::fs::create_dir_all(&live_events).unwrap();
    let sentinel = live_events.join("existing.producer.json");
    std::fs::write(&sentinel, b"live-ingress-sentinel\n").unwrap();
    let scratch_state = temp.path().join("scratch-state");

    let output = Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("--config")
        .arg(&config)
        .args([
            "producer",
            "poll",
            "github",
            "--once",
            "--no-enqueue",
            "--state-dir",
        ])
        .arg(&scratch_state)
        .env("PATH", temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let decisions: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(decisions.as_array().unwrap().len(), 1);
    assert_eq!(decisions[0]["decision"], "would-enqueue");
    assert_eq!(
        decisions[0]["enqueue"]["argv"],
        serde_json::json!(["run-widget-checks", "--fixed-option"])
    );
    assert_eq!(decisions[0]["candidate"]["commentId"], "4242");
    assert_eq!(decisions[0]["enqueue"]["context"]["repo"], "acme/widgets");

    assert!(!scratch_state.exists(), "dry-run created scratch state");
    assert_eq!(
        std::fs::read(&sentinel).unwrap(),
        b"live-ingress-sentinel\n"
    );
    assert_eq!(std::fs::read_dir(&live_events).unwrap().count(), 1);
}

#[test]
fn one_shot_test_is_read_only_and_reports_the_resolved_synthetic_trigger() {
    let temp = tempfile::tempdir().unwrap();
    let item_url = "https://github.com/acme/widgets/issues/42";
    let config = temp.path().join("config.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&serde_json::json!({
            "pools": {"slot": {"resource": "build-slot"}},
            "adapters": {"shell": {"argv": []}},
            "producers": {
                "github": {
                    "kind": "gh",
                    "enable": true,
                    "sources": [{"search": {
                        "repo": "acme/widgets",
                        "itemAllowlist": [item_url]
                    }}],
                    "triggers": {"mentions": ["@tally-bot run"]},
                    "allowedActors": ["maintainer"],
                    "enqueue": {"argv": ["run-widget-checks"], "pool": "slot"}
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let fake_gh = temp.path().join("gh");
    shell_program::install(
        &fake_gh,
        concat!(
            "#!/bin/sh\n",
            "case \"$*\" in\n",
            "  'api user') printf '{\"login\":\"tally-bot\"}' ;;\n",
            "  'api /repos/acme/widgets/issues/42')\n",
            "    printf '{\"node_id\":\"I_widget_42\",\"number\":42,\"html_url\":\"https://github.com/acme/widgets/issues/42\",\"title\":\"Run checks\",\"body\":\"untrusted context\",\"state\":\"open\",\"user\":{\"login\":\"issue-author\"},\"labels\":[],\"assignees\":[]}' ;;\n",
            "  *) exit 91 ;;\n",
            "esac\n",
        ),
    );

    let live_state = temp.path().join("live-state");
    let live_events = live_state.join("events");
    std::fs::create_dir_all(&live_events).unwrap();
    let sentinel = live_events.join("existing.producer.json");
    std::fs::write(&sentinel, b"live-ingress-sentinel\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("--config")
        .arg(&config)
        .args([
            "producer",
            "test",
            "github",
            "--item",
            item_url,
            "--event",
            "mention",
            "--actor",
            "maintainer",
            "--no-enqueue",
            "--state-dir",
        ])
        .arg(&live_state)
        .env("PATH", temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let decision: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(decision["decision"], "would-enqueue");
    assert_eq!(decision["candidate"]["source"], "search");
    assert_eq!(decision["candidate"]["triggerKind"], "mention");
    assert_eq!(decision["candidate"]["triggerActor"], "maintainer");
    assert!(decision["candidate"]["eventId"]
        .as_str()
        .unwrap()
        .starts_with("diagnostic-"));
    assert_eq!(
        decision["enqueue"]["context"]["context"]["triggeringComment"]["body"],
        "@tally-bot run"
    );
    assert_eq!(
        std::fs::read(&sentinel).unwrap(),
        b"live-ingress-sentinel\n"
    );
    assert_eq!(std::fs::read_dir(&live_events).unwrap().count(), 1);
    assert!(!live_state.join("producers/gh-triggers").exists());
}

/// The orphaned-projection report names this command, so the command has to
/// answer for real — including when the configuration no longer mentions the
/// producer at all, which is the only situation in which it is ever run.
#[test]
fn orphaned_lists_every_recorded_projection_without_a_configuration() {
    use std::collections::BTreeMap;

    use tally_core::producers::{
        OrphanedProjection, OrphanedProjectionKind, ProducerEngine,
        ORPHANED_PROJECTION_SCHEMA_VERSION,
    };
    use tally_core::witness::Verdict;

    let temp = tempfile::tempdir().unwrap();
    let state_dir = temp.path().join("state");
    // The registry is empty exactly as it is after a retired campaign's
    // producer block is deleted.
    let registry = BTreeMap::new();
    let engine = ProducerEngine::new(
        &registry,
        state_dir.join("events"),
        &state_dir,
        temp.path().join("data"),
    );
    for (index, task) in [
        "1514ece1-0000-4000-8000-000000000001",
        "996a384d-0000-4000-8000-000000000002",
    ]
    .into_iter()
    .enumerate()
    {
        assert!(engine
            .record_orphaned_projection(&OrphanedProjection {
                schema_version: ORPHANED_PROJECTION_SCHEMA_VERSION,
                kind: OrphanedProjectionKind::Completion,
                producer: "campaign-crm".to_owned(),
                source: "notifications".to_owned(),
                item_id: format!("I_retired_{index}"),
                completion_id: format!("{task}:1:{index}"),
                task_uuid: Some(task.to_owned()),
                verdict: Some(Verdict::Pass),
                observed_at: "2026-08-03T09:00:00.000Z".to_owned(),
                detail: "unknown producer \"campaign-crm\"".to_owned(),
            })
            .unwrap());
    }

    let output = Command::new(env!("CARGO_BIN_EXE_tally"))
        .args(["producer", "orphaned", "--state-dir"])
        .arg(&state_dir)
        // No `--config`: the point of the command is that the configuration
        // has stopped naming the producer.
        .env("HOME", temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(listed["count"], 2);
    assert_eq!(listed["stateDir"], state_dir.to_str().unwrap());
    let projections = listed["projections"].as_array().unwrap();
    assert_eq!(projections.len(), 2);
    assert_eq!(projections[0]["producer"], "campaign-crm");
    assert_eq!(projections[0]["kind"], "completion");
    assert_eq!(projections[0]["itemId"], "I_retired_0");
    assert_eq!(
        projections[0]["taskUuid"],
        "1514ece1-0000-4000-8000-000000000001"
    );
    assert_eq!(projections[0]["verdict"], "pass");
    assert_eq!(projections[1]["itemId"], "I_retired_1");

    // An untouched estate answers the same question with an empty set rather
    // than an error about a missing directory.
    let output = Command::new(env!("CARGO_BIN_EXE_tally"))
        .args(["producer", "orphaned", "--state-dir"])
        .arg(temp.path().join("never-used"))
        .env("HOME", temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(listed["count"], 0);
    assert_eq!(listed["projections"].as_array().unwrap().len(), 0);
}
