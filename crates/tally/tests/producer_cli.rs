use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use serde_json::Value;

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
    std::fs::write(
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
    )
    .unwrap();
    std::fs::set_permissions(&fake_gh, std::fs::Permissions::from_mode(0o700)).unwrap();

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
    std::fs::write(
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
    )
    .unwrap();
    std::fs::set_permissions(&fake_gh, std::fs::Permissions::from_mode(0o700)).unwrap();

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
