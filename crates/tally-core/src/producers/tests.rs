use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
#[cfg(target_os = "linux")]
use std::os::unix::process::ExitStatusExt;
use std::sync::{Arc, Barrier};

use chrono::TimeZone;
use tempfile::tempdir;

use super::*;

const STORE_A: &str = "/nix/store/00000000000000000000000000000000-output-a";
const STORE_B: &str = "/nix/store/11111111111111111111111111111111-output-b";

/// A receipt written before `duplicateAcknowledged` was retired still loads.
///
/// The struct denies unknown fields, so dropping the key outright would have
/// made every trigger receipt already on a real host unreadable; the field
/// stays declared and is read by nothing, and it is not written back.
#[test]
fn a_receipt_carrying_the_retired_duplicate_flag_still_loads_and_stops_being_written() {
    let stored = serde_json::json!({
        "schemaVersion": 1,
        "receiptId": "receipt-42",
        "producer": "github",
        "source": "search",
        "itemId": "I_widget_42",
        "eventId": "event-1",
        "triggerKind": "comment",
        "triggerActor": "contributor",
        "triggerTimestamp": "2026-07-20T12:30:00Z",
        "primaryDecision": "accepted",
        "primaryAcknowledged": true,
        "duplicateAcknowledged": true,
        "duplicateCount": 3,
    });
    let receipt: super::gh_decision::GhTriggerReceipt =
        serde_json::from_value(stored).expect("a pre-retirement receipt must still load");
    assert_eq!(receipt.duplicate_count, 3);
    assert!(receipt.primary_acknowledged);
    let rewritten = serde_json::to_value(&receipt).unwrap();
    assert!(rewritten.get("duplicateAcknowledged").is_none());
}

fn enqueue(command: &str) -> ProducerEnqueue {
    ProducerEnqueue {
        argv: vec![command.to_owned()],
        adapter: "shell".to_owned(),
        cwd: None,
        workspace: None,
        adapter_options: AdapterJobOptions::default(),
        gate_manifest: None,
        brief: None,
        pools: vec!["slot".to_owned()],
        executor: None,
        priority: Priority::Low,
        dedup_key: None,
        evidence: vec!["exit:0".to_owned()],
        evidence_class: None,
        manifest_hash: None,
        consumption_estimate: None,
        runtime_max_sec: None,
        no_enqueue: false,
        credentials: BTreeMap::new(),
    }
}

fn registry(watch_path: &Path) -> BTreeMap<String, ProducerConfig> {
    let mut attest = enqueue("assess-return");
    attest.no_enqueue = true;
    BTreeMap::from([
        (
            "daily".to_owned(),
            ProducerConfig::Calendar(CalendarProducer {
                credentials: BTreeMap::new(),
                on_calendar: "daily".to_owned(),
                enqueue: ProducerEnqueue {
                    dedup_key: Some("daily-%Y%m%d".to_owned()),
                    ..enqueue("calendar-job")
                },
            }),
        ),
        (
            "drop".to_owned(),
            ProducerConfig::EventsDir(EventsDirProducer {
                credentials: BTreeMap::new(),
                poll_interval_sec: 60,
            }),
        ),
        (
            "github".to_owned(),
            ProducerConfig::Gh(GhProducer {
                credentials: BTreeMap::new(),
                enable: true,
                sources: vec![
                    GhSource::Notifications(GhSourceConstraints {
                        repo: Some("acme/widgets".to_owned()),
                        ..GhSourceConstraints::default()
                    }),
                    GhSource::Search(GhSourceConstraints {
                        repo: Some("acme/widgets".to_owned()),
                        ..GhSourceConstraints::default()
                    }),
                ],
                triggers: GhTriggers {
                    mentions: vec!["@tally-bot please run".to_owned()],
                    assignments: vec!["tally-bot".to_owned()],
                    ..GhTriggers::default()
                },
                actor_exclude: "self".to_owned(),
                allow_self_triggered: false,
                allowed_actors: Vec::new(),
                poll_interval_sec: 60,
                post_receipt: true,
                post_evidence: true,
                post_failure_evidence: false,
                post_failure_stderr: false,
                post_gate_summary: false,
                request_review: false,
                reviewers: Vec::new(),
                close_on_acceptance: false,
                never_mutate: false,
                close_on_pass: Some(true),
                enqueue: enqueue("gh-job"),
            }),
        ),
        (
            "effects".to_owned(),
            ProducerConfig::BuildEffect(BuildEffectProducer {
                credentials: BTreeMap::new(),
                watch: BuildEffectWatch::Jsonl,
                path: watch_path.to_owned(),
                on_key: enqueue("effect-job"),
            }),
        ),
        (
            "health".to_owned(),
            ProducerConfig::PoolReachability(Box::new(PoolReachabilityProducer {
                credentials: BTreeMap::new(),
                probe_pool: "slot".to_owned(),
                interval_sec: 30,
                hysteresis: 3,
                on_lost: Some(enqueue("pool-lost")),
                on_return: Some(enqueue("pool-return")),
                on_return_attest: Some(attest),
            })),
        ),
    ])
}

fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 20, 12, 30, 0)
        .single()
        .unwrap()
}

#[cfg(target_os = "linux")]
fn process_is_alive(pid: i32) -> bool {
    // SAFETY: signal zero performs existence/permission checking only.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(target_os = "linux")]
fn wait_for_process_exit(pid: i32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while process_is_alive(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    !process_is_alive(pid)
}

#[cfg(target_os = "linux")]
#[test]
fn gh_timeout_kills_the_process_group_and_drains_readers() {
    let temp = tempdir().unwrap();
    let descendant_pid = temp.path().join("descendant-pid");
    let gh = temp.path().join("fake-gh");
    crate::test_support::install_shell_program(
        &gh,
        format!(
            "#!/bin/sh\n\
                 trap '' HUP INT TERM\n\
                 sleep 300 >&2 &\n\
                 printf '%s' \"$!\" > '{}'\n\
                 while :; do sleep 300; done\n",
            descendant_pid.display()
        ),
    );

    let started = Instant::now();
    let error =
        run_gh_bounded_with_timeout(&gh, &[], None, Duration::from_millis(500)).unwrap_err();
    let elapsed = started.elapsed();
    assert!(error.contains("exceeded the 0.5 second timeout"), "{error}");
    assert!(
        elapsed < Duration::from_secs(2),
        "gh timeout cleanup took {elapsed:?}"
    );
    let descendant_pid = std::fs::read_to_string(descendant_pid)
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert!(
        wait_for_process_exit(descendant_pid, Duration::from_secs(2)),
        "gh descendant {descendant_pid} survived process-group cleanup"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn gh_parent_death_helper() {
    let Some(program) = std::env::var_os("TALLY_TEST_GH_PARENT_DEATH_PROGRAM") else {
        return;
    };
    let _ = run_gh_bounded_with_timeout(Path::new(&program), &[], None, Duration::from_secs(30));
}

#[cfg(target_os = "linux")]
#[test]
fn interactive_cancellation_still_terminates_gh() {
    let temp = tempdir().unwrap();
    let gh_pid_path = temp.path().join("gh-pid");
    let gh = temp.path().join("fake-gh");
    crate::test_support::install_shell_program(
        &gh,
        format!(
            "#!/bin/sh\n\
                 printf '%s' \"$$\" > '{}'\n\
                 exec sleep 300\n",
            gh_pid_path.display()
        ),
    );
    let mut helper = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "producers::tests::gh_parent_death_helper",
            "--nocapture",
        ])
        .env("TALLY_TEST_GH_PARENT_DEATH_PROGRAM", &gh)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let startup_deadline = Instant::now() + Duration::from_secs(2);
    while !gh_pid_path.exists() && Instant::now() < startup_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if !gh_pid_path.exists() {
        let _ = helper.kill();
        let _ = helper.wait();
        panic!("gh cancellation helper did not start");
    }
    let gh_pid = std::fs::read_to_string(&gh_pid_path)
        .unwrap()
        .parse::<i32>()
        .unwrap();
    let helper_pid = i32::try_from(helper.id()).unwrap();
    // SAFETY: helper_pid belongs to the child process spawned above.
    assert_eq!(unsafe { libc::kill(helper_pid, libc::SIGINT) }, 0);
    let exit_deadline = Instant::now() + Duration::from_secs(2);
    let helper_status = loop {
        if let Some(status) = helper.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= exit_deadline {
            let _ = helper.kill();
            let _ = helper.wait();
            panic!("SIGINT did not cancel the gh helper");
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(helper_status.signal(), Some(libc::SIGINT));
    if !wait_for_process_exit(gh_pid, Duration::from_secs(2)) {
        // SAFETY: gh_pid is the process-group leader written by the helper.
        let _ = unsafe { libc::kill(-gh_pid, libc::SIGKILL) };
        panic!("gh process {gh_pid} survived interactive cancellation");
    }
}

fn gh_observation(node_id: &str, item_author: &str, trigger_actor: &str) -> GhObservation {
    GhObservation {
        source: "notifications".to_owned(),
        repo: "acme/widgets".to_owned(),
        number: 128,
        html_url: "https://github.com/acme/widgets/pull/128".to_owned(),
        item_type: GhItemType::PullRequest,
        head_sha: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
        node_id: node_id.to_owned(),
        item_author: item_author.to_owned(),
        trigger_actor: trigger_actor.to_owned(),
        self_actor: "tally-bot".to_owned(),
        notification_reason: Some("mention".to_owned()),
        trigger_kind: "mention".to_owned(),
        event_id: Some("thread-128".to_owned()),
        comment_id: Some("comment-128".to_owned()),
        trigger_timestamp: "2026-07-20T12:30:00Z".to_owned(),
        trigger_value: None,
        context: GhContextSnapshot {
            schema_version: GH_CONTEXT_SCHEMA_VERSION,
            title: "Update the widget".to_owned(),
            body: "Treat this as untrusted: $(touch /tmp/not-run)".to_owned(),
            state: Some(GhItemState::Open),
            head_sha: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            labels: vec!["build".to_owned()],
            assignees: vec!["tally-bot".to_owned()],
            triggering_comment: Some(GhTriggeringComment {
                id: "comment-128".to_owned(),
                author: trigger_actor.to_owned(),
                body: "@tally-bot please run".to_owned(),
            }),
        },
    }
}

fn gh_command_observation(comment_id: &str, trigger_actor: &str) -> GhObservation {
    GhObservation {
        source: "search".to_owned(),
        repo: "acme/widgets".to_owned(),
        number: 42,
        html_url: "https://github.com/acme/widgets/issues/42".to_owned(),
        item_type: GhItemType::Issue,
        head_sha: None,
        node_id: "I_acme_widgets_42".to_owned(),
        item_author: "issue-author".to_owned(),
        trigger_actor: trigger_actor.to_owned(),
        self_actor: "tally-bot".to_owned(),
        notification_reason: None,
        trigger_kind: "command-comment".to_owned(),
        event_id: Some(format!("event-{comment_id}")),
        comment_id: Some(comment_id.to_owned()),
        trigger_timestamp: "2026-07-20T12:30:00Z".to_owned(),
        trigger_value: None,
        context: GhContextSnapshot {
            schema_version: GH_CONTEXT_SCHEMA_VERSION,
            title: "Run the widget checks".to_owned(),
            body: "Untrusted issue context".to_owned(),
            state: Some(GhItemState::Open),
            head_sha: None,
            labels: vec!["ready".to_owned()],
            assignees: vec!["tally-bot".to_owned()],
            triggering_comment: Some(GhTriggeringComment {
                id: comment_id.to_owned(),
                author: trigger_actor.to_owned(),
                body: "/tally run".to_owned(),
            }),
        },
    }
}

#[derive(Default)]
struct RecordingAcknowledgements {
    entries: Vec<GhTriggerAcknowledgement>,
}

impl GhAcknowledgementSink for RecordingAcknowledgements {
    fn post_acknowledgement(
        &mut self,
        acknowledgement: &GhTriggerAcknowledgement,
    ) -> Result<(), String> {
        self.entries.push(acknowledgement.clone());
        Ok(())
    }
}

#[test]
fn registry_is_strict_open_by_name_and_closed_over_the_in_scope_kinds() {
    let temp = tempdir().unwrap();
    let registry = registry(&temp.path().join("effects.jsonl"));
    validate_registry(
        &registry,
        &BTreeSet::from(["slot".to_owned()]),
        &BTreeSet::from(["shell".to_owned()]),
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(
        registry
            .values()
            .map(ProducerConfig::kind)
            .collect::<BTreeSet<_>>(),
        IN_SCOPE_PRODUCER_KINDS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
    );

    assert!(serde_json::from_value::<ProducerConfig>(serde_json::json!({
        "kind": "r2",
        "enqueue": {"argv": ["x"], "pool": "slot"}
    }))
    .is_err());
    assert!(serde_json::from_value::<ProducerConfig>(serde_json::json!({
        "kind": "calendar",
        "onCalendar": "daily",
        "pool": "producer-owned-is-forbidden",
        "enqueue": {"argv": ["x"], "pool": "slot"}
    }))
    .is_err());
    assert!(matches!(
        serde_json::from_value::<ProducerObservation>(serde_json::json!({
            "kind": "gh",
            "source": "notifications",
            "nodeId": "PR-1",
            "triggerActor": "contributor",
            "selfActor": "tally-bot"
        }))
        .unwrap(),
        ProducerObservation::Gh(observation)
            if observation.node_id.as_deref() == Some("PR-1")
                && observation.self_actor.as_deref() == Some("tally-bot")
    ));

    let mut invalid_attest = registry.clone();
    let ProducerConfig::PoolReachability(health) = invalid_attest.get_mut("health").unwrap() else {
        unreachable!()
    };
    health.on_return_attest.as_mut().unwrap().no_enqueue = false;
    assert!(validate_registry(
        &invalid_attest,
        &BTreeSet::from(["slot".to_owned()]),
        &BTreeSet::from(["shell".to_owned()]),
        &BTreeSet::new(),
    )
    .unwrap_err()
    .to_string()
    .contains("noEnqueue=true"));

    let mut duplicate_reachability = registry.clone();
    let duplicate = duplicate_reachability.get("health").unwrap().clone();
    duplicate_reachability.insert("health-backup".to_owned(), duplicate);
    assert!(validate_registry(
        &duplicate_reachability,
        &BTreeSet::from(["slot".to_owned()]),
        &BTreeSet::from(["shell".to_owned()]),
        &BTreeSet::new(),
    )
    .unwrap_err()
    .to_string()
    .contains("both own probePool"));

    for invalid_name in [".hidden", "-option"] {
        let mut invalid_names = registry.clone();
        invalid_names.insert(
            invalid_name.to_owned(),
            invalid_names.get("daily").unwrap().clone(),
        );
        assert!(validate_registry(
            &invalid_names,
            &BTreeSet::from(["slot".to_owned()]),
            &BTreeSet::from(["shell".to_owned()]),
            &BTreeSet::new(),
        )
        .unwrap_err()
        .to_string()
        .contains("invalid producer configuration"));
    }

    let mut relative_credential = registry;
    let ProducerConfig::Calendar(calendar) = relative_credential.get_mut("daily").unwrap() else {
        unreachable!()
    };
    calendar
        .enqueue
        .credentials
        .insert("token".to_owned(), PathBuf::from("relative/token"));
    assert!(validate_registry(
        &relative_credential,
        &BTreeSet::from(["slot".to_owned()]),
        &BTreeSet::from(["shell".to_owned()]),
        &BTreeSet::new(),
    )
    .unwrap_err()
    .to_string()
    .contains("must be absolute"));

    let mut invalid_strftime = relative_credential;
    let ProducerConfig::Calendar(calendar) = invalid_strftime.get_mut("daily").unwrap() else {
        unreachable!()
    };
    calendar.enqueue.credentials.clear();
    calendar.enqueue.dedup_key = Some("daily-%Q".to_owned());
    assert!(validate_registry(
        &invalid_strftime,
        &BTreeSet::from(["slot".to_owned()]),
        &BTreeSet::from(["shell".to_owned()]),
        &BTreeSet::new(),
    )
    .unwrap_err()
    .to_string()
    .contains("strftime"));

    let mut invalid_close = invalid_strftime;
    let ProducerConfig::Calendar(calendar) = invalid_close.get_mut("daily").unwrap() else {
        unreachable!()
    };
    calendar.enqueue.dedup_key = None;
    let ProducerConfig::Gh(github) = invalid_close.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.post_evidence = false;
    github.close_on_pass = Some(true);
    assert!(validate_registry(
        &invalid_close,
        &BTreeSet::from(["slot".to_owned()]),
        &BTreeSet::from(["shell".to_owned()]),
        &BTreeSet::new(),
    )
    .unwrap_err()
    .to_string()
    .contains("closeOnPass=true requires postEvidence=true"));

    let mut invalid_failure_stderr = invalid_close;
    let ProducerConfig::Gh(github) = invalid_failure_stderr.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.post_evidence = true;
    github.post_failure_stderr = true;
    assert!(validate_registry(
        &invalid_failure_stderr,
        &BTreeSet::from(["slot".to_owned()]),
        &BTreeSet::from(["shell".to_owned()]),
        &BTreeSet::new(),
    )
    .unwrap_err()
    .to_string()
    .contains("postFailureStderr=true requires postFailureEvidence=true"));

    // A switch that claims to request a human review has to name the humans.
    let mut invalid_reviewers = invalid_failure_stderr;
    let ProducerConfig::Gh(github) = invalid_reviewers.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.post_failure_stderr = false;
    github.request_review = true;
    assert!(validate_registry(
        &invalid_reviewers,
        &BTreeSet::from(["slot".to_owned()]),
        &BTreeSet::from(["shell".to_owned()]),
        &BTreeSet::new(),
    )
    .unwrap_err()
    .to_string()
    .contains("requestReview=true requires a non-empty reviewers list"));

    let ProducerConfig::Gh(github) = invalid_reviewers.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.reviewers = vec!["not a login".to_owned()];
    assert!(validate_registry(
        &invalid_reviewers,
        &BTreeSet::from(["slot".to_owned()]),
        &BTreeSet::from(["shell".to_owned()]),
        &BTreeSet::new(),
    )
    .unwrap_err()
    .to_string()
    .contains("is not a GitHub login"));

    let ProducerConfig::Gh(github) = invalid_reviewers.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.reviewers = vec!["octocat".to_owned(), "octocat".to_owned()];
    assert!(validate_registry(
        &invalid_reviewers,
        &BTreeSet::from(["slot".to_owned()]),
        &BTreeSet::from(["shell".to_owned()]),
        &BTreeSet::new(),
    )
    .unwrap_err()
    .to_string()
    .contains("repeats reviewers entry"));

    let ProducerConfig::Gh(github) = invalid_reviewers.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.reviewers = vec!["octocat".to_owned()];
    validate_registry(
        &invalid_reviewers,
        &BTreeSet::from(["slot".to_owned()]),
        &BTreeSet::from(["shell".to_owned()]),
        &BTreeSet::new(),
    )
    .unwrap();
}

#[test]
fn serialized_github_config_treats_an_absent_close_on_pass_as_off() {
    let config = |close_on_pass: Option<bool>| {
        let mut value = serde_json::json!({
            "kind": "gh",
            "enable": true,
            "sources": [{"notifications": {"repo": "acme/widgets"}}],
            "triggers": {"assignments": ["tally-bot"]},
            "postEvidence": true,
            "enqueue": {"argv": ["gh-job"], "pool": "slot"}
        });
        if let Some(close_on_pass) = close_on_pass {
            value["closeOnPass"] = Value::Bool(close_on_pass);
        }
        let ProducerConfig::Gh(config) = serde_json::from_value::<ProducerConfig>(value).unwrap()
        else {
            unreachable!()
        };
        config
    };

    // Absent no longer inherits `postEvidence`: closing is its own opt-in,
    // and this configuration has evidence posting on.
    let absent = config(None);
    assert!(!absent.post_failure_evidence);
    assert!(!absent.post_failure_stderr);
    assert!(absent.post_evidence);
    assert_eq!(absent.close_on_pass, None);
    assert!(!absent.close_on_pass());
    let comment_only = config(Some(false));
    assert_eq!(comment_only.close_on_pass, Some(false));
    assert!(!comment_only.close_on_pass());
    let closing = config(Some(true));
    assert!(closing.close_on_pass());
}

#[test]
fn github_search_queries_are_derived_only_from_declared_scopes() {
    let scoped = GhSourceConstraints {
        repo: Some("agency-agency/spec".to_owned()),
        labels: vec!["agency:codex-ready".to_owned()],
        state: Some(GhItemState::Open),
        assignee: Some("tally-bot".to_owned()),
        kinds: vec![GhSourceItemKind::Issue],
        query: Some("draft:false".to_owned()),
        ..GhSourceConstraints::default()
    };
    assert_eq!(
            gh_search_queries(&scoped),
            ["repo:agency-agency/spec label:\"agency:codex-ready\" state:open assignee:\"tally-bot\" is:issue draft:false"]
        );

    let query_without_identity = GhSourceConstraints {
        query: Some("state:open".to_owned()),
        ..GhSourceConstraints::default()
    };
    assert!(gh_search_queries(&query_without_identity).is_empty());
}

#[test]
fn github_explicit_comment_assignment_and_label_triggers_are_classified_exactly() {
    let triggers = GhTriggers {
        command_comments: vec!["/tally run".to_owned()],
        mentions: vec!["@tally-bot run".to_owned()],
        assignments: vec!["tally-bot".to_owned()],
        labels: vec!["tally:run".to_owned()],
    };
    let command = gh_command_observation("command", "maintainer");
    assert!(gh_trigger_matches(&triggers, &command));

    let mut mention = command.clone();
    mention.trigger_kind = "mention".to_owned();
    mention.context.triggering_comment.as_mut().unwrap().body = "@tally-bot run".to_owned();
    assert!(gh_trigger_matches(&triggers, &mention));

    let assignment = configured_gh_event(
        &serde_json::json!({
            "id": 42,
            "event": "assigned",
            "actor": {"login": "maintainer"},
            "assignee": {"login": "tally-bot"}
        }),
        &triggers,
    )
    .unwrap();
    assert_eq!(assignment.id, "42");
    assert_eq!(assignment.kind, "assignment");
    assert_eq!(assignment.actor, "maintainer");
    assert_eq!(assignment.value, "tally-bot");

    let label = configured_gh_event(
        &serde_json::json!({
            "node_id": "LE_label_43",
            "event": "labeled",
            "actor": {"login": "maintainer"},
            "label": {"name": "tally:run"}
        }),
        &triggers,
    )
    .unwrap();
    assert_eq!(label.id, "LE_label_43");
    assert_eq!(label.kind, "label");
    assert_eq!(label.value, "tally:run");

    assert!(configured_gh_event(
        &serde_json::json!({
            "id": 44,
            "event": "labeled",
            "actor": {"login": "maintainer"},
            "label": {"name": "unconfigured"}
        }),
        &triggers,
    )
    .is_none());
}

#[test]
fn github_remaining_source_constraints_are_fail_closed() {
    let constraints = GhSourceConstraints {
        owners: vec!["acme".to_owned()],
        assignee: Some("tally-bot".to_owned()),
        kinds: vec![GhSourceItemKind::Issue],
        notification_reasons: vec!["mention".to_owned()],
        item_allowlist: vec!["https://github.com/acme/widgets/issues/42".to_owned()],
        ..GhSourceConstraints::default()
    };
    let mut matching = gh_command_observation("constraints", "maintainer");
    matching.source = "notifications".to_owned();
    matching.notification_reason = Some("mention".to_owned());
    assert_eq!(gh_source_constraints_reason(&constraints, &matching), None);

    let mut wrong_item = matching.clone();
    wrong_item.html_url = "https://github.com/acme/widgets/issues/43".to_owned();
    assert_eq!(
        gh_source_constraints_reason(&constraints, &wrong_item),
        Some(GhFilterReason::ItemNotAllowlisted)
    );
    let mut wrong_assignee = matching.clone();
    wrong_assignee.context.assignees.clear();
    assert_eq!(
        gh_source_constraints_reason(&constraints, &wrong_assignee),
        Some(GhFilterReason::AssigneeMismatch)
    );
    let mut wrong_kind = matching.clone();
    wrong_kind.item_type = GhItemType::PullRequest;
    assert_eq!(
        gh_source_constraints_reason(&constraints, &wrong_kind),
        Some(GhFilterReason::ItemKindMismatch)
    );
    let mut wrong_reason = matching;
    wrong_reason.notification_reason = Some("subscribed".to_owned());
    assert_eq!(
        gh_source_constraints_reason(&constraints, &wrong_reason),
        Some(GhFilterReason::NotificationReasonMismatch)
    );
}

#[test]
fn producer_multi_pool_validation_rejects_empty_duplicate_and_unknown_sets() {
    let temp = tempdir().unwrap();
    let error_for = |requested: Vec<String>| {
        let mut registry = registry(&temp.path().join("effects.jsonl"));
        let ProducerConfig::Calendar(calendar) = registry.get_mut("daily").unwrap() else {
            unreachable!()
        };
        calendar.enqueue.pools = requested;
        validate_registry(
            &registry,
            &BTreeSet::from(["slot".to_owned()]),
            &BTreeSet::from(["shell".to_owned()]),
            &BTreeSet::new(),
        )
        .unwrap_err()
        .to_string()
    };

    assert!(error_for(Vec::new()).contains("at least one"));
    assert!(error_for(vec!["slot".to_owned(), "slot".to_owned()]).contains("duplicate"));
    assert!(error_for(vec!["slot".to_owned(), "missing".to_owned()])
        .contains("references unknown pool \"missing\""));
}

#[test]
fn calendar_emits_a_direct_payload_with_strftime_dedup_and_credentials() {
    let temp = tempdir().unwrap();
    let mut registry = registry(&temp.path().join("effects.jsonl"));
    let ProducerConfig::Calendar(calendar) = registry.get_mut("daily").unwrap() else {
        unreachable!()
    };
    calendar.enqueue.credentials.insert(
        "token".to_owned(),
        PathBuf::from("/run/credentials/calendar-token"),
    );
    calendar.enqueue.brief = Some(serde_json::json!({"task": "nightly"}));
    let engine = ProducerEngine::new(
        &registry,
        temp.path().join("events"),
        temp.path().join("state"),
        temp.path(),
    );
    let EmitOutcome::Emitted(path) = engine.emit_calendar("daily", fixed_now()).unwrap() else {
        panic!("calendar did not emit")
    };
    let payload: EnqueuePayload = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(payload.source, Some(EnqueueSource::Calendar));
    assert_eq!(
        payload.pools.as_deref(),
        Some(["slot".to_owned()].as_slice())
    );
    assert_eq!(payload.adapter.as_deref(), Some("shell"));
    assert_eq!(payload.dedup_key.as_deref(), Some("daily-20260720"));
    assert_eq!(
        payload.credentials["token"],
        PathBuf::from("/run/credentials/calendar-token")
    );
    let brief_path = payload.brief_path.as_ref().unwrap();
    assert!(brief_path.starts_with(temp.path().join("briefs")));
    assert!(!temp.path().join("state/briefs").exists());
    assert_eq!(
        crate::brief::PreparedBrief::from_path(brief_path)
            .unwrap()
            .document(),
        &serde_json::json!({"task": "nightly"})
    );
}

#[test]
fn github_origin_templates_render_into_literal_fields_without_a_shell() {
    let temp = tempdir().unwrap();
    let marker = temp.path().join("must-not-exist");
    let mut registry = registry(&temp.path().join("effects.jsonl"));
    let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.enqueue.argv = vec![
        "review".to_owned(),
        "${gh.url}".to_owned(),
        "${gh.headSha}".to_owned(),
        format!("$(touch {})", marker.display()),
    ];
    let large_campaign_config = "repository and gate configuration ".repeat(40_000);
    assert!(large_campaign_config.len() > MAX_INGRESS_BYTES as usize);
    github.enqueue.brief = Some(serde_json::json!({
        "issue": {
            "url": "${gh.url}",
            "number": "${gh.number}",
        },
        "configPayload": large_campaign_config,
        "runId": "${gh.eventId}",
    }));
    github.enqueue.cwd = Some(PathBuf::from("/worktrees/${repoName}"));
    validate_registry(
        &registry,
        &BTreeSet::from(["slot".to_owned()]),
        &BTreeSet::from(["shell".to_owned()]),
        &BTreeSet::new(),
    )
    .unwrap();

    let engine = ProducerEngine::new(
        &registry,
        temp.path().join("events"),
        temp.path().join("state"),
        temp.path(),
    );
    let observation = gh_observation("PR_template", "author", "contributor");
    let EmitOutcome::Emitted(path) = engine.emit_gh("github", &observation, fixed_now()).unwrap()
    else {
        panic!("GitHub observation did not emit")
    };
    assert!(std::fs::metadata(&path).unwrap().len() < MAX_INGRESS_BYTES);
    let payload: EnqueuePayload = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let argv = payload.argv.as_ref().unwrap();
    assert_eq!(
        argv.as_slice(),
        &[
            "review".to_owned(),
            "https://github.com/acme/widgets/pull/128".to_owned(),
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            format!("$(touch {})", marker.display()),
        ]
    );
    assert!(argv.iter().all(|argument| argument.len() < 64 * 1024));
    assert!(argv
        .iter()
        .all(|argument| !argument.contains("repository and gate configuration")));
    assert!(payload.brief.is_none());
    let brief_path = payload.brief_path.as_ref().unwrap();
    assert!(brief_path.starts_with(temp.path().join("briefs")));
    assert!(!temp.path().join("state/briefs").exists());
    let prepared = crate::brief::PreparedBrief::from_path(brief_path).unwrap();
    assert_eq!(
        prepared.document(),
        &serde_json::json!({
            "issue": {
                "url": "https://github.com/acme/widgets/pull/128",
                "number": "128",
            },
            "configPayload": large_campaign_config,
            "runId": "thread-128",
        })
    );
    assert_eq!(
        payload.cwd.as_deref(),
        Some(Path::new("/worktrees/widgets"))
    );
    assert!(!marker.exists());

    let mut unknown = registry.clone();
    let ProducerConfig::Gh(github) = unknown.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.enqueue.argv = vec!["review".to_owned()];
    github.enqueue.brief = Some(serde_json::json!({"body": "${gh.body}"}));
    assert!(validate_registry(
        &unknown,
        &BTreeSet::from(["slot".to_owned()]),
        &BTreeSet::from(["shell".to_owned()]),
        &BTreeSet::new(),
    )
    .unwrap_err()
    .to_string()
    .contains("unknown placeholder"));

    let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    github
        .triggers
        .command_comments
        .push("/tally run".to_owned());
    let issue = gh_command_observation("missing-head", "contributor");
    let missing_engine = ProducerEngine::new(
        &registry,
        temp.path().join("missing-events"),
        temp.path().join("missing-state"),
        temp.path(),
    );
    let error = missing_engine
        .emit_gh("github", &issue, fixed_now())
        .unwrap_err()
        .to_string();
    assert!(error.contains("gh.headSha"), "{error}");
}

#[test]
fn rendered_producer_brief_enforces_the_canonical_size_limit() {
    let temp = tempdir().unwrap();
    let mut registry = registry(&temp.path().join("effects.jsonl"));
    let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.enqueue.brief = Some(serde_json::json!({
        // The template itself is small enough to configure, but rendering a
        // bounded 4 KiB origin field repeatedly crosses MAX_BRIEF_BYTES.
        "expanded": "${gh.triggerValue}".repeat(5_000),
    }));
    validate_registry(
        &registry,
        &BTreeSet::from(["slot".to_owned()]),
        &BTreeSet::from(["shell".to_owned()]),
        &BTreeSet::new(),
    )
    .unwrap();
    let engine = ProducerEngine::new(
        &registry,
        temp.path().join("events"),
        temp.path().join("state"),
        temp.path(),
    );
    let mut observation = gh_observation("PR_large_render", "author", "contributor");
    observation.trigger_value = Some("x".repeat(MAX_GH_ORIGIN_FIELD_BYTES));
    let error = engine
        .emit_gh("github", &observation, fixed_now())
        .unwrap_err()
        .to_string();
    assert!(error.contains("rendered producer brief exceeds"), "{error}");
    assert!(!temp.path().join("briefs").exists());
}

struct RecordingMutation {
    comments: Vec<GhCompletedMutation>,
    reviews: Vec<GhCompletedMutation>,
    closes: Vec<GhCompletedMutation>,
    item_open: bool,
}

impl Default for RecordingMutation {
    fn default() -> Self {
        Self {
            comments: Vec::new(),
            reviews: Vec::new(),
            closes: Vec::new(),
            item_open: true,
        }
    }
}

impl GhMutationSink for RecordingMutation {
    fn post_evidence(&mut self, mutation: &GhCompletedMutation) -> Result<(), String> {
        self.comments.push(mutation.clone());
        Ok(())
    }

    fn request_reviews(&mut self, mutation: &GhCompletedMutation) -> Result<(), String> {
        self.reviews.push(mutation.clone());
        Ok(())
    }

    fn close_item(&mut self, mutation: &GhCompletedMutation) -> Result<(), String> {
        self.closes.push(mutation.clone());
        self.item_open = false;
        Ok(())
    }
}

#[test]
fn storage_warning_receipt_is_idempotent_and_never_closes_the_campaign_issue() {
    let temp = tempdir().unwrap();
    let registry = registry(&temp.path().join("effects.jsonl"));
    let ProducerConfig::Gh(github) = registry.get("github").unwrap() else {
        unreachable!()
    };
    let origin = gh_origin(
        "github",
        github,
        &gh_observation("PR_storage", "author", "contributor"),
    );
    let engine = ProducerEngine::new(
        &registry,
        temp.path().join("events"),
        temp.path().join("state"),
        temp.path(),
    );
    let warning = crate::storage::ActiveStorageWarning {
        warning_sequence: 7,
        store: "dataDir".to_owned(),
        level: crate::storage::BudgetLevel::Hard,
        size_bytes: 200,
        threshold_bytes: 100,
        pressures: vec![crate::storage::StoragePressure {
            resource: crate::storage::StoragePressureResource::AllocatedBytes,
            observed_bytes: 200,
            threshold_bytes: 100,
            recovery_bytes: 90,
        }],
        message: "hard budget crossed".to_owned(),
    };
    let mut sink = RecordingMutation::default();
    assert!(engine
        .post_storage_warning_once(&origin, &warning, &mut sink)
        .unwrap());
    assert!(!engine
        .post_storage_warning_once(&origin, &warning, &mut sink)
        .unwrap());
    assert_eq!(sink.comments.len(), 1);
    assert!(sink.closes.is_empty());
    assert!(sink.item_open);
    assert_eq!(
        sink.comments[0].evidence.as_ref().unwrap()["kind"],
        "storage-budget-warning"
    );
}

fn semantic_completion(
    gate_status: GateSummaryStatus,
    acceptance_status: AcceptanceStatus,
) -> SemanticCompletion {
    SemanticCompletion {
        schema_version: crate::completion::GATE_MANIFEST_SCHEMA_VERSION,
        execution: crate::completion::ExecutionFact::exited(0),
        gates: GateSummary {
            status: gate_status,
            artifact: Some(serde_json::json!({"commit": "abc"})),
            gates: Vec::new(),
            missing_required_gate_ids: if gate_status == GateSummaryStatus::Fail {
                vec!["live".to_owned()]
            } else {
                Vec::new()
            },
            manifest_error: None,
        },
        acceptance: AcceptanceFact {
            status: acceptance_status,
            policy: crate::completion::AcceptancePolicy::ExecutionAndGates,
            reason: "test policy result".to_owned(),
        },
    }
}

#[test]
fn github_gate_failure_and_not_run_remain_open_and_never_mutate_wins() {
    let temp = tempdir().unwrap();
    let mut registry = registry(&temp.path().join("effects.jsonl"));
    let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.post_gate_summary = true;
    github.request_review = true;
    github.reviewers = vec!["octocat".to_owned()];
    github.close_on_acceptance = true;
    github.close_on_pass = Some(true);
    let observation = gh_observation("PR_policy", "author", "contributor");
    let origin = gh_origin("github", github, &observation);
    let engine = ProducerEngine::new(
        &registry,
        temp.path().join("events"),
        temp.path().join("state"),
        temp.path(),
    );

    for completion in [
        semantic_completion(GateSummaryStatus::Fail, AcceptanceStatus::Rejected),
        semantic_completion(GateSummaryStatus::NotRun, AcceptanceStatus::Pending),
    ] {
        let mut sink = RecordingMutation::default();
        assert!(engine
            .complete_gh_with_completion(
                &origin,
                Verdict::Pass,
                Some(serde_json::json!({"witnessSeq": 28})),
                Some(completion),
                &mut sink,
            )
            .unwrap());
        assert_eq!(sink.comments.len(), 1);
        assert!(sink.comments[0].request_review);
        // The boolean is provenance; the review request is its own mutation.
        assert_eq!(sink.reviews.len(), 1);
        assert_eq!(sink.reviews[0].reviewers, ["octocat"]);
        assert!(sink.closes.is_empty());
        assert!(sink.item_open);
    }

    let mut accepted_sink = RecordingMutation::default();
    assert!(engine
        .complete_gh_with_completion(
            &origin,
            Verdict::Pass,
            Some(serde_json::json!({"witnessSeq": 29})),
            Some(semantic_completion(
                GateSummaryStatus::Pass,
                AcceptanceStatus::Accepted,
            )),
            &mut accepted_sink,
        )
        .unwrap());
    assert_eq!(accepted_sink.closes.len(), 1);
    assert!(!accepted_sink.comments[0].request_review);
    assert!(accepted_sink.reviews.is_empty());

    let mut inert_registry = registry;
    let ProducerConfig::Gh(github) = inert_registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.never_mutate = true;
    let inert_engine = ProducerEngine::new(
        &inert_registry,
        temp.path().join("inert-events"),
        temp.path().join("inert-state"),
        temp.path(),
    );
    let mut inert_sink = RecordingMutation::default();
    assert!(!inert_engine
        .complete_gh_with_completion(
            &origin,
            Verdict::Pass,
            None,
            Some(semantic_completion(
                GateSummaryStatus::Pass,
                AcceptanceStatus::Accepted,
            )),
            &mut inert_sink,
        )
        .unwrap());
    assert!(inert_sink.comments.is_empty());
    assert!(inert_sink.closes.is_empty());
}

#[test]
fn github_enforces_sources_trigger_actor_policy_and_completion_mutations() {
    let temp = tempdir().unwrap();
    let registry = registry(&temp.path().join("effects.jsonl"));
    let engine = ProducerEngine::new(
        &registry,
        temp.path().join("events"),
        temp.path().join("state"),
        temp.path(),
    );
    let external = gh_observation("PR_kwABC128", "issue-author", "contributor");
    let EmitOutcome::Emitted(path) = engine.emit_gh("github", &external, fixed_now()).unwrap()
    else {
        panic!("GitHub observation did not emit")
    };
    let payload: EnqueuePayload = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(payload.source, Some(EnqueueSource::Gh));
    assert_eq!(payload.gh_trigger_actor.as_deref(), Some("contributor"));
    assert_eq!(payload.gh_self_actor.as_deref(), Some("tally-bot"));
    let origin = payload.gh_origin.clone().unwrap();
    assert_eq!(
        payload.dedup_key.as_deref(),
        Some(gh_trigger_dedup_key(&origin).unwrap().as_str())
    );
    assert_eq!(origin.producer, "github");
    assert_eq!(origin.source, "notifications");
    assert_eq!(origin.node_id, "PR_kwABC128");
    assert_eq!(origin.item_author, "issue-author");
    assert_eq!(origin.trigger_actor, "contributor");
    assert_eq!(
        engine.emit_gh("github", &external, fixed_now()).unwrap(),
        EmitOutcome::Duplicate
    );

    let own = GhObservation {
        trigger_actor: "tally-bot".to_owned(),
        context: GhContextSnapshot {
            triggering_comment: Some(GhTriggeringComment {
                author: "tally-bot".to_owned(),
                ..external.context.triggering_comment.clone().unwrap()
            }),
            ..external.context.clone()
        },
        ..external.clone()
    };
    assert_eq!(
        engine.emit_gh("github", &own, fixed_now()).unwrap(),
        EmitOutcome::Filtered {
            reason: GhFilterReason::SelfTriggerDisabled
        }
    );
    let wrong_source = GhObservation {
        source: "unconfigured".to_owned(),
        ..external.clone()
    };
    assert_eq!(
        engine
            .emit_gh("github", &wrong_source, fixed_now())
            .unwrap(),
        EmitOutcome::Filtered {
            reason: GhFilterReason::SourceNotConfigured
        }
    );

    let mut mutations = RecordingMutation::default();
    assert!(!engine
        .complete_gh(
            &origin,
            Verdict::Failed,
            Some(serde_json::json!({
                "witnessSeq": 3,
                "stderrTail": "actionable failure\n",
                "stderrTruncated": false,
            })),
            &mut mutations,
        )
        .unwrap());
    assert!(mutations.comments.is_empty());
    assert!(mutations.closes.is_empty());
    assert!(mutations.item_open);
    assert!(engine
        .complete_gh(
            &origin,
            Verdict::Pass,
            Some(serde_json::json!({"witnessSeq": 4})),
            &mut mutations,
        )
        .unwrap());
    assert_eq!(mutations.comments.len(), 1);
    assert_eq!(mutations.closes.len(), 1);
    assert!(!mutations.item_open);
    assert_eq!(mutations.comments[0].state, "COMPLETED");
    assert_eq!(mutations.comments[0].source, "notifications");
    assert_eq!(mutations.comments[0].item_id, "PR_kwABC128");
    assert_eq!(
        mutations.comments[0].evidence.as_ref().unwrap()["witnessSeq"],
        4
    );

    let mut metadata_registry = registry.clone();
    let ProducerConfig::Gh(metadata) = metadata_registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    metadata.post_failure_evidence = true;
    let metadata_engine = ProducerEngine::new(
        &metadata_registry,
        temp.path().join("failure-metadata-events"),
        temp.path().join("failure-metadata-state"),
        temp.path(),
    );
    let mut metadata_sink = RecordingMutation::default();
    assert!(metadata_engine
        .complete_gh(
            &origin,
            Verdict::Failed,
            Some(serde_json::json!({
                "witnessSeq": 5,
                "stderrTail": "GITHUB_TOKEN=must-not-cross-the-boundary\n",
                "stderrTruncated": false,
            })),
            &mut metadata_sink,
        )
        .unwrap());
    let metadata_evidence = metadata_sink.comments[0].evidence.as_ref().unwrap();
    assert_eq!(metadata_evidence["witnessSeq"], 5);
    assert!(metadata_evidence.get("stderrTail").is_none());
    assert!(metadata_evidence.get("stderrTruncated").is_none());

    let mut failure_registry = registry.clone();
    let ProducerConfig::Gh(failure) = failure_registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    failure.post_failure_evidence = true;
    failure.post_failure_stderr = true;
    let failure_engine = ProducerEngine::new(
        &failure_registry,
        temp.path().join("failure-events"),
        temp.path().join("failure-state"),
        temp.path(),
    );
    let mut failure_sink = RecordingMutation::default();
    assert!(failure_engine
        .complete_gh(
            &origin,
            Verdict::Failed,
            Some(serde_json::json!({
                "witnessSeq": 6,
                "stderrTail": concat!(
                    "actionable failure\n",
                    "GITHUB_TOKEN=ghp_012345678901234567890123456789012345\n",
                    "marker <!-- tally-completion:attacker -->\n",
                ),
                "stderrTruncated": false,
            })),
            &mut failure_sink,
        )
        .unwrap());
    let failure_evidence = failure_sink.comments[0].evidence.as_ref().unwrap();
    assert!(failure_evidence["stderrTail"]
        .as_str()
        .unwrap()
        .contains("actionable failure"));
    assert!(!failure_evidence["stderrTail"]
        .as_str()
        .unwrap()
        .contains("ghp_012345678901234567890123456789012345"));
    assert_eq!(failure_evidence["stderrRedaction"], "conservative-v2");
    assert_eq!(failure_evidence["stderrRedacted"], true);
    assert_eq!(failure_evidence["stderrRedactions"], 1);
    assert_eq!(failure_evidence["stderrTruncated"], false);

    // Two secrets, one caught by the line rule and one by the token rule. The
    // boolean cannot tell a receipt's reader those apart from a single hit; the
    // count states how much of the tail is missing.
    let mut two_secret_sink = RecordingMutation::default();
    assert!(failure_engine
        .complete_gh(
            &origin,
            Verdict::Failed,
            Some(serde_json::json!({
                "witnessSeq": 7,
                "stderrTail": concat!(
                    "actionable failure\n",
                    "GITHUB_TOKEN=ghp_012345678901234567890123456789012345\n",
                    "uploading with AKIAIOSFODNN7EXAMPLE now\n",
                ),
                "stderrTruncated": false,
            })),
            &mut two_secret_sink,
        )
        .unwrap());
    let two_secret_evidence = two_secret_sink.comments[0].evidence.as_ref().unwrap();
    assert_eq!(two_secret_evidence["stderrRedacted"], true);
    assert_eq!(two_secret_evidence["stderrRedactions"], 2);
    let two_secret_tail = two_secret_evidence["stderrTail"].as_str().unwrap();
    assert!(two_secret_tail.contains("actionable failure"));
    assert!(!two_secret_tail.contains("ghp_012345678901234567890123456789012345"));
    assert!(!two_secret_tail.contains("AKIAIOSFODNN7EXAMPLE"));
    assert!(two_secret_tail.contains("uploading with [redacted-token] now"));

    let mut comment_only_registry = registry.clone();
    let ProducerConfig::Gh(comment_only) = comment_only_registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    comment_only.close_on_pass = Some(false);
    let comment_only_engine = ProducerEngine::new(
        &comment_only_registry,
        temp.path().join("comment-only-events"),
        temp.path().join("comment-only-state"),
        temp.path(),
    );
    let mut comment_only_sink = RecordingMutation::default();
    assert!(comment_only_engine
        .complete_gh(
            &origin,
            Verdict::Pass,
            Some(serde_json::json!({"witnessSeq": 5})),
            &mut comment_only_sink,
        )
        .unwrap());
    assert_eq!(comment_only_sink.comments.len(), 1);
    assert!(comment_only_sink.closes.is_empty());
    assert!(comment_only_sink.item_open);

    let gh = temp.path().join("fake-gh");
    let requests = temp.path().join("gh-requests.jsonl");
    let calls = temp.path().join("gh-calls");
    let commented = temp.path().join("gh-commented");
    let failed_close = temp.path().join("gh-failed-close");
    let completion_id = "task-1:attempt-1:witness-5";
    let remote_key = stable_key(&["gh-remote-completion", completion_id]);
    crate::test_support::install_shell_program(
            &gh,
            format!(
                concat!(
                    "#!/bin/sh\n",
                    "[ \"$1 $2 $3 $4\" = 'api graphql --input -' ] || exit 91\n",
                    "request=$(cat)\n",
                    "printf '%s\\n' \"$request\" >> '{}'\n",
                    "printf x >> '{}'\n",
                    "case \"$request\" in\n",
                    "  *TallyCompletionState*)\n",
                    "    if test -e '{}'; then comments='[{{\"id\":\"IC_1\",\"body\":\"<!-- tally-completion:{} -->\"}}]'; else comments='[]'; fi\n",
                    "    printf '{{\"data\":{{\"node\":{{\"__typename\":\"PullRequest\",\"state\":\"OPEN\",\"comments\":{{\"nodes\":%s,\"pageInfo\":{{\"hasNextPage\":false,\"endCursor\":null}}}}}}}}}}' \"$comments\"\n",
                    "    ;;\n",
                    "  *TallyCompletionComment*) touch '{}'; printf '{{\"data\":{{\"addComment\":{{}}}}}}' ;;\n",
                    "  *TallyStickyComment*) printf '{{\"data\":{{\"updateIssueComment\":{{}}}}}}' ;;\n",
                    "  *TallyCompletionPullRequest*)\n",
                    "    if test ! -e '{}'; then touch '{}'; printf close-failed >&2; exit 92; fi\n",
                    "    printf '{{\"data\":{{\"closePullRequest\":{{}}}}}}'\n",
                    "    ;;\n",
                    "  *) exit 93 ;;\n",
                    "esac\n"
                ),
                requests.display(),
                calls.display(),
                commented.display(),
                remote_key,
                commented.display(),
                failed_close.display(),
                failed_close.display(),
            ),
        );
    let mut cli = GhCliMutationSink::with_program(&gh);
    let trailer_evidence = serde_json::json!({
        "taskUuid": "00000000-0000-4000-8000-000000000049",
        "witnessSeq": 5,
        "adapter": "codex",
        "model": "gpt-5.6-codex",
    });
    assert!(engine
        .complete_gh_once(
            &origin,
            completion_id,
            Verdict::Reused,
            Some(trailer_evidence.clone()),
            &mut cli,
        )
        .unwrap_err()
        .to_string()
        .contains("close-failed"));
    assert!(engine
        .complete_gh_once(
            &origin,
            completion_id,
            Verdict::Reused,
            Some(trailer_evidence.clone()),
            &mut cli,
        )
        .unwrap());
    assert!(!engine
        .complete_gh_once(
            &origin,
            completion_id,
            Verdict::Reused,
            Some(trailer_evidence),
            &mut cli,
        )
        .unwrap());
    // Eight calls: scan + create + scan + failed close, then scan + edit +
    // scan + close. This sink stores no comment ids, so the retry recovers
    // through the marker scan and publishes into the comment it finds rather
    // than adopting it silently; the third call is stopped by the durable
    // completion marker before it reaches the forge at all.
    assert_eq!(std::fs::read(&calls).unwrap(), b"xxxxxxxx");
    let requests = std::fs::read_to_string(requests)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["query"]
                .as_str()
                .unwrap()
                .contains("TallyCompletionComment"))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["query"]
                .as_str()
                .unwrap()
                .contains("TallyCompletionPullRequest"))
            .count(),
        2
    );
    let comment = requests
        .iter()
        .find(|request| {
            request["query"]
                .as_str()
                .unwrap()
                .contains("TallyCompletionComment")
        })
        .unwrap();
    assert_eq!(comment["variables"]["itemId"], "PR_kwABC128");
    assert!(comment["variables"]["body"]
        .as_str()
        .unwrap()
        .contains("witnessSeq"));
    assert!(comment["variables"]["body"]
            .as_str()
            .unwrap()
            .ends_with(
                "\n\nAssisted-by: codex:gpt-5.6-codex (tally:00000000-0000-4000-8000-000000000049 witness:5)"
            ));
}

#[test]
fn github_self_trigger_permission_is_separate_from_the_external_actor_allowlist() {
    let temp = tempdir().unwrap();
    let mut registry = registry(&temp.path().join("effects.jsonl"));
    let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.allow_self_triggered = true;
    github.allowed_actors = vec!["operator".to_owned()];
    let engine = ProducerEngine::new(
        &registry,
        temp.path().join("events"),
        temp.path().join("state"),
        temp.path(),
    );

    let allowed = gh_observation("I_self_authored", "tally-bot", "tally-bot");
    assert!(matches!(
        engine.emit_gh("github", &allowed, fixed_now()).unwrap(),
        EmitOutcome::Emitted(_)
    ));

    let external_allowed = gh_observation("I_operator", "tally-bot", "operator");
    assert!(matches!(
        engine
            .emit_gh("github", &external_allowed, fixed_now())
            .unwrap(),
        EmitOutcome::Emitted(_)
    ));

    let rejected = gh_observation("I_self_authored", "tally-bot", "untrusted-user");
    assert_eq!(
        engine.emit_gh("github", &rejected, fixed_now()).unwrap(),
        EmitOutcome::Filtered {
            reason: GhFilterReason::TriggerActorNotAllowed
        }
    );
}

#[test]
fn github_trigger_acknowledgement_marker_is_stable_and_remote_idempotent() {
    let temp = tempdir().unwrap();
    let gh = temp.path().join("fake-gh-ack");
    let requests = temp.path().join("ack-requests.jsonl");
    let marker = temp.path().join("ack-marker");
    crate::test_support::install_shell_program(
            &gh,
            format!(
                concat!(
                    "#!/bin/sh\n",
                    "[ \"$1 $2 $3 $4\" = 'api graphql --input -' ] || exit 91\n",
                    "request=$(cat)\n",
                    "printf '%s\\n' \"$request\" >> '{}'\n",
                    "case \"$request\" in\n",
                    "  *TallyCompletionState*)\n",
                    "    if test -e '{}'; then comments='[{{\"id\":\"IC_1\",\"body\":\"<!-- tally-trigger:receipt-42:accepted -->\"}}]'; else comments='[]'; fi\n",
                    "    printf '{{\"data\":{{\"node\":{{\"__typename\":\"Issue\",\"state\":\"OPEN\",\"comments\":{{\"nodes\":%s,\"pageInfo\":{{\"hasNextPage\":false,\"endCursor\":null}}}}}}}}}}' \"$comments\"\n",
                    "    ;;\n",
                    "  *TallyCompletionComment*) touch '{}'; printf '{{\"data\":{{\"addComment\":{{}}}}}}' ;;\n",
                    "  *TallyStickyComment*) printf '{{\"data\":{{\"updateIssueComment\":{{}}}}}}' ;;\n",
                    "  *) exit 92 ;;\n",
                    "esac\n"
                ),
                requests.display(),
                marker.display(),
                marker.display(),
            ),
        );
    File::open(&gh).unwrap().sync_all().unwrap();
    sync_directory(temp.path()).unwrap();
    let acknowledgement = GhTriggerAcknowledgement {
        schema_version: 1,
        producer: "github".to_owned(),
        receipt_id: "receipt-42".to_owned(),
        item_id: "I_widget_42".to_owned(),
        decision: GhDecisionStatus::Accepted,
        rule: None,
        task_uuid: Some("00000000-0000-5000-8000-000000000042".to_owned()),
        status_pointer: Some(
            "tally query log --task 00000000-0000-5000-8000-000000000042".to_owned(),
        ),
    };
    let mut sink = GhCliAcknowledgementSink::with_program(&gh);
    sink.post_acknowledgement(&acknowledgement).unwrap();
    sink.post_acknowledgement(&acknowledgement).unwrap();

    let requests = std::fs::read_to_string(requests)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let comments = requests
        .iter()
        .filter(|request| {
            request["query"]
                .as_str()
                .unwrap()
                .contains("TallyCompletionComment")
        })
        .collect::<Vec<_>>();
    assert_eq!(comments.len(), 1);
    let body = comments[0]["variables"]["body"].as_str().unwrap();
    assert!(body.contains("<!-- tally-trigger:receipt-42:accepted -->"));
    assert!(body.contains("00000000-0000-5000-8000-000000000042"));
    assert!(body.contains("tally query log --task"));
}

/// A scripted GraphQL transport that keeps the comment thread on disk: one
/// `<node id>.body` file per comment, holding the exact body that was
/// published to it. A test can seed a legacy comment, delete one behind the
/// sink's back, restart the sink, or refuse edits the way a secondary rate
/// limit does, and then read back both the operations issued and what the
/// thread actually says.
///
/// Three control files, all outside the `*.body` set: `refuse-edits` makes
/// `updateIssueComment` fail while the comment stays present, `hide-ids`
/// serves matched comments without a node id, and `pull-request` makes the
/// item resolve as a `PullRequest` instead of an `Issue`.
const SCRIPTED_GRAPHQL: &str = r#"#!/bin/sh
[ "$1 $2 $3 $4" = 'api graphql --input -' ] || exit 91
request=$(cat)
printf '%s\n' "$request" >> '@REQUESTS@'
body=$(printf '%s' "$request" | sed -n 's/.*"body":"\(.*\)"}}$/\1/p')
if [ -e '@THREAD@/pull-request' ]; then
  typename=PullRequest
else
  typename=Issue
fi
case "$request" in
  *TallyCompletionState*)
    nodes=''
    separator=''
    for comment in '@THREAD@'/*.body; do
      [ -f "$comment" ] || continue
      id=${comment##*/}
      id=${id%.body}
      if [ -e '@THREAD@/hide-ids' ]; then
        node="{\"body\":\"$(cat "$comment")\"}"
      else
        node="{\"id\":\"$id\",\"body\":\"$(cat "$comment")\"}"
      fi
      nodes="$nodes$separator$node"
      separator=','
    done
    printf '{"data":{"node":{"__typename":"%s","state":"OPEN","comments":{"nodes":[%s],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}' "$typename" "$nodes"
    ;;
  *TallyItemState*)
    printf '{"data":{"node":{"__typename":"%s","state":"OPEN"}}}' "$typename"
    ;;
  *TallyReviewerId*)
    login=$(printf '%s' "$request" | sed -n 's/.*"login":"\([^"]*\)".*/\1/p')
    if [ "$login" = "ghost" ]; then
      printf '{"data":{"user":null}}'
    else
      printf '{"data":{"user":{"id":"U_%s"}}}' "$login"
    fi
    ;;
  *TallyRequestReviews*)
    printf '%s' "$request" | sed -n 's/.*"userIds":\(\[[^]]*\]\).*/\1/p' \
      >> '@THREAD@/requested-reviews'
    printf '{"data":{"requestReviews":{"pullRequest":{"id":"PR_widget_42"}}}}'
    ;;
  *TallyCompletionComment*)
    sequence=$(cat '@THREAD@/sequence' 2>/dev/null || printf 1)
    printf '%s' "$body" > "@THREAD@/IC_$sequence.body"
    printf '%s' "$((sequence + 1))" > '@THREAD@/sequence'
    printf '{"data":{"addComment":{"commentEdge":{"node":{"id":"IC_%s"}}}}}' "$sequence"
    ;;
  *TallyStickyComment*)
    id=$(printf '%s' "$request" | sed -n 's/.*"commentId":"\([^"]*\)".*/\1/p')
    if [ ! -f "@THREAD@/$id.body" ]; then
      printf '{"errors":[{"message":"Could not resolve to a node"}]}'
      exit 0
    fi
    if [ -e '@THREAD@/refuse-edits' ]; then
      printf '{"errors":[{"message":"You have exceeded a secondary rate limit"}]}'
      exit 0
    fi
    printf '%s' "$body" > "@THREAD@/$id.body"
    printf '{"data":{"updateIssueComment":{"issueComment":{"id":"%s"}}}}' "$id"
    ;;
  *) exit 92 ;;
esac
"#;

fn install_scripted_graphql(gh: &Path, requests: &Path, thread: &Path) {
    std::fs::create_dir_all(thread).unwrap();
    crate::test_support::install_shell_program(
        gh,
        SCRIPTED_GRAPHQL
            .replace("@REQUESTS@", &requests.display().to_string())
            .replace("@THREAD@", &thread.display().to_string()),
    );
    File::open(gh).unwrap().sync_all().unwrap();
    sync_directory(gh.parent().unwrap()).unwrap();
}

/// The GraphQL operations the sink issued, in order, since the last drain.
fn scripted_operations(requests: &Path) -> Vec<String> {
    let recorded = std::fs::read_to_string(requests).unwrap_or_default();
    let _ = std::fs::remove_file(requests);
    recorded
        .lines()
        .map(|line| {
            let request: Value = serde_json::from_str(line).unwrap();
            let query = request["query"].as_str().unwrap().to_owned();
            [
                "TallyCompletionState",
                "TallyItemState",
                "TallyCompletionComment",
                "TallyStickyComment",
                "TallyReviewerId",
                "TallyRequestReviews",
            ]
            .into_iter()
            .find(|operation| query.contains(operation))
            .unwrap_or_else(|| panic!("unexpected scripted GraphQL operation {query:?}"))
            .to_owned()
        })
        .collect()
}

/// What the thread actually says on one comment, decoded from the transport's
/// stored JSON string.
fn thread_body(thread: &Path, comment_id: &str) -> String {
    let stored = std::fs::read_to_string(thread.join(format!("{comment_id}.body"))).unwrap();
    serde_json::from_str::<String>(&format!("\"{stored}\"")).unwrap()
}

fn thread_comments(thread: &Path) -> Vec<String> {
    let mut comments = std::fs::read_dir(thread)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension() == Some("body".as_ref()))
        .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    comments.sort();
    comments
}

fn stored_sticky_comments(state_dir: &Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(state_dir.join("producers/gh-comments")) else {
        return Vec::new();
    };
    let mut stored = entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension() == Some("json".as_ref()))
        .map(|path| {
            let record: GhStickyComment =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert_eq!(record.schema_version, 1);
            assert_eq!(record.producer, "github");
            assert_eq!(record.item_id, "I_widget_42");
            (record.logical_id, record.comment_id)
        })
        .collect::<Vec<_>>();
    stored.sort();
    stored
}

fn trigger_acknowledgement(
    receipt_id: &str,
    decision: GhDecisionStatus,
) -> GhTriggerAcknowledgement {
    GhTriggerAcknowledgement {
        schema_version: 1,
        producer: "github".to_owned(),
        receipt_id: receipt_id.to_owned(),
        item_id: "I_widget_42".to_owned(),
        decision,
        rule: (decision == GhDecisionStatus::Filtered)
            .then_some(GhFilterReason::TriggerActorNotAllowed),
        task_uuid: Some("00000000-0000-5000-8000-000000000042".to_owned()),
        status_pointer: Some(
            "tally query log --task 00000000-0000-5000-8000-000000000042".to_owned(),
        ),
    }
}

/// #245: a producer restart re-scans every historical trigger, and each
/// re-scan resolves to `Duplicate`. The old exact-marker check missed the
/// `:accepted` marker already on the thread and published one "already
/// recorded" comment per trigger.
#[test]
fn github_duplicate_trigger_acknowledgement_is_never_published() {
    let temp = tempdir().unwrap();
    let gh = temp.path().join("fake-gh");
    let requests = temp.path().join("requests.jsonl");
    let thread = temp.path().join("thread");
    let state_dir = temp.path().join("state");
    install_scripted_graphql(&gh, &requests, &thread);
    std::fs::write(
        thread.join("IC_7.body"),
        "<!-- tally-trigger:receipt-42:accepted -->",
    )
    .unwrap();

    // The suppression lives at the decision point now, so a duplicate is never
    // built into an acknowledgement and never dispatched. A sink handed one
    // anyway says so loudly instead of dropping it: silence here is what let a
    // second sink re-introduce the public duplicate by default.
    let mut sink = GhCliAcknowledgementSink::with_program(&gh).with_state_dir(&state_dir);
    let refused = sink
        .post_acknowledgement(&trigger_acknowledgement(
            "receipt-42",
            GhDecisionStatus::Duplicate,
        ))
        .unwrap_err();
    assert!(refused.contains("non-terminal"), "{refused}");
    assert_eq!(scripted_operations(&requests), Vec::<String>::new());
    assert_eq!(thread_comments(&thread), ["IC_7"]);

    // Not even on a thread that carries no marker at all.
    std::fs::remove_file(thread.join("IC_7.body")).unwrap();
    assert!(sink
        .post_acknowledgement(&trigger_acknowledgement(
            "receipt-42",
            GhDecisionStatus::Duplicate,
        ))
        .is_err());
    assert_eq!(scripted_operations(&requests), Vec::<String>::new());
    assert_eq!(thread_comments(&thread), Vec::<String>::new());

    // A legacy marker satisfies the completion check whatever decision wrote
    // it: the check matches the receipt id, not the decision suffix. The
    // comment it found is published into, not merely adopted.
    std::fs::write(
        thread.join("IC_7.body"),
        "<!-- tally-trigger:receipt-42:filtered -->",
    )
    .unwrap();
    sink.post_acknowledgement(&trigger_acknowledgement(
        "receipt-42",
        GhDecisionStatus::Accepted,
    ))
    .unwrap();
    assert_eq!(
        scripted_operations(&requests),
        ["TallyCompletionState", "TallyStickyComment"]
    );
    assert_eq!(thread_comments(&thread), ["IC_7"]);
    assert!(thread_body(&thread, "IC_7").contains("Tally accepted this trigger."));
    assert_eq!(
        stored_sticky_comments(&state_dir),
        [("receipt-42".to_owned(), "IC_7".to_owned())]
    );
}

/// §9.1.3: the receipt is one sticky comment. It is created once, remembered
/// durably, and edited in place afterwards; a lost id recovers through the
/// marker scan instead of duplicating the comment.
#[test]
fn github_receipt_comment_upserts_in_place_across_a_producer_restart() {
    let temp = tempdir().unwrap();
    let gh = temp.path().join("fake-gh");
    let requests = temp.path().join("requests.jsonl");
    let thread = temp.path().join("thread");
    let state_dir = temp.path().join("state");
    install_scripted_graphql(&gh, &requests, &thread);
    let accepted = trigger_acknowledgement("receipt-42", GhDecisionStatus::Accepted);

    let mut sink = GhCliAcknowledgementSink::with_program(&gh).with_state_dir(&state_dir);
    sink.post_acknowledgement(&accepted).unwrap();
    assert_eq!(
        scripted_operations(&requests),
        ["TallyCompletionState", "TallyCompletionComment"]
    );
    assert_eq!(thread_comments(&thread), ["IC_1"]);
    assert!(thread_body(&thread, "IC_1").contains("Tally accepted this trigger."));
    assert_eq!(
        stored_sticky_comments(&state_dir),
        [("receipt-42".to_owned(), "IC_1".to_owned())]
    );

    // A filtered receipt on the same thread is its own sticky comment.
    sink.post_acknowledgement(&trigger_acknowledgement(
        "receipt-43",
        GhDecisionStatus::Filtered,
    ))
    .unwrap();
    assert_eq!(
        scripted_operations(&requests),
        ["TallyCompletionState", "TallyCompletionComment"]
    );
    assert_eq!(thread_comments(&thread), ["IC_1", "IC_2"]);

    // The id is durable, not process memory: a restarted producer edits the
    // comment it created before, and never paginates the thread again.
    // One call, not two: re-reading the item's state after the edit landed
    // gated nothing and made the realistic case -- a campaign issue well under
    // one page of comments -- cost more API than the thread scan it replaced.
    let mut restarted = GhCliAcknowledgementSink::with_program(&gh).with_state_dir(&state_dir);
    restarted.post_acknowledgement(&accepted).unwrap();
    assert_eq!(scripted_operations(&requests), ["TallyStickyComment"]);
    assert_eq!(thread_comments(&thread), ["IC_1", "IC_2"]);

    // State loss with a marker still on the thread: recover, publish into the
    // comment already there, and create nothing.
    std::fs::remove_dir_all(state_dir.join("producers/gh-comments")).unwrap();
    restarted.post_acknowledgement(&accepted).unwrap();
    assert_eq!(
        scripted_operations(&requests),
        ["TallyCompletionState", "TallyStickyComment"]
    );
    assert_eq!(thread_comments(&thread), ["IC_1", "IC_2"]);
    // Only the receipt that was re-posted re-adopts its comment; the other
    // stays recoverable through its marker.
    assert_eq!(
        stored_sticky_comments(&state_dir),
        [("receipt-42".to_owned(), "IC_1".to_owned())]
    );

    // A remembered comment that no longer exists must not wedge the sink: it
    // forgets the id, recovers through the scan, and creates one fresh comment.
    std::fs::remove_file(thread.join("IC_1.body")).unwrap();
    restarted.post_acknowledgement(&accepted).unwrap();
    assert_eq!(
        scripted_operations(&requests),
        [
            "TallyStickyComment",
            "TallyCompletionState",
            "TallyCompletionComment"
        ]
    );
    assert_eq!(thread_comments(&thread), ["IC_2", "IC_3"]);
    assert_eq!(
        stored_sticky_comments(&state_dir),
        [("receipt-42".to_owned(), "IC_3".to_owned())]
    );
}

/// `requestReview` used to serialize `"requestReview":true` into the machine
/// completion comment and stop there: the producer's whole mutation vocabulary
/// was comment / closeIssue / closePullRequest, so no review was ever
/// requested and no human was ever notified. A pull request now receives
/// GitHub's own `requestReviews` mutation, once, with the configured logins
/// resolved to user ids.
#[test]
fn request_review_sends_a_real_review_request_for_a_pull_request() {
    let temp = tempdir().unwrap();
    let gh = temp.path().join("fake-gh");
    let requests = temp.path().join("requests.jsonl");
    let thread = temp.path().join("thread");
    let state_dir = temp.path().join("state");
    install_scripted_graphql(&gh, &requests, &thread);
    std::fs::write(thread.join("pull-request"), "").unwrap();

    let mut registry = registry(&temp.path().join("effects.jsonl"));
    let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.request_review = true;
    github.reviewers = vec!["octocat".to_owned(), "hubot".to_owned()];
    github.close_on_pass = Some(false);
    let observation = gh_observation("PR_widget_42", "author", "contributor");
    let origin = gh_origin("github", github, &observation);
    let engine = ProducerEngine::new(
        &registry,
        temp.path().join("events"),
        state_dir.clone(),
        temp.path(),
    );
    let mut sink = GhCliMutationSink::with_program(&gh).with_state_dir(&state_dir);

    assert!(engine
        .complete_gh_once(
            &origin,
            "task-1:attempt-1:witness-5",
            Verdict::Pass,
            Some(serde_json::json!({"witnessSeq": 5})),
            &mut sink,
        )
        .unwrap());
    assert_eq!(
        scripted_operations(&requests),
        [
            "TallyCompletionState",
            "TallyCompletionComment",
            "TallyItemState",
            "TallyReviewerId",
            "TallyReviewerId",
            "TallyRequestReviews",
        ]
    );
    assert_eq!(
        std::fs::read_to_string(thread.join("requested-reviews")).unwrap(),
        "[\"U_octocat\",\"U_hubot\"]"
    );

    // A replay of the same completion is refused by the durable marker, so the
    // reviewers are asked exactly once.
    assert!(!engine
        .complete_gh_once(
            &origin,
            "task-1:attempt-1:witness-5",
            Verdict::Pass,
            Some(serde_json::json!({"witnessSeq": 5})),
            &mut sink,
        )
        .unwrap());
    assert_eq!(scripted_operations(&requests), Vec::<String>::new());
    assert_eq!(
        std::fs::read_to_string(thread.join("requested-reviews")).unwrap(),
        "[\"U_octocat\",\"U_hubot\"]"
    );
}

/// An issue has no review concept, so the notification has to be the mention
/// itself: one fresh comment, marker-guarded so a re-publication of the same
/// completion neither repeats it nor silently edits the ping away.
#[test]
fn request_review_mentions_the_reviewers_once_on_an_issue() {
    let temp = tempdir().unwrap();
    let gh = temp.path().join("fake-gh");
    let requests = temp.path().join("requests.jsonl");
    let thread = temp.path().join("thread");
    let state_dir = temp.path().join("state");
    install_scripted_graphql(&gh, &requests, &thread);

    let mutation = GhCompletedMutation {
        producer: "github".to_owned(),
        source: "search".to_owned(),
        item_id: "I_widget_42".to_owned(),
        completion_id: Some("task-1:attempt-1:witness-5".to_owned()),
        state: "COMPLETED".to_owned(),
        evidence: None,
        gate_summary: None,
        acceptance: None,
        request_review: true,
        reviewers: vec!["octocat".to_owned()],
        assisted_by: None,
    };

    let mut sink = GhCliMutationSink::with_program(&gh).with_state_dir(&state_dir);
    sink.request_reviews(&mutation).unwrap();
    assert_eq!(
        scripted_operations(&requests),
        [
            "TallyItemState",
            "TallyCompletionState",
            "TallyCompletionComment"
        ]
    );
    assert_eq!(thread_comments(&thread), ["IC_1"]);
    let body = thread_body(&thread, "IC_1");
    assert!(body.contains("<!-- tally-review-request:"), "{body}");
    assert!(body.contains("@octocat"), "{body}");

    // Replayed: the marker is on the thread, so nothing new is published and
    // the existing comment is left exactly as it is.
    sink.request_reviews(&mutation).unwrap();
    assert_eq!(
        scripted_operations(&requests),
        ["TallyItemState", "TallyCompletionState"]
    );
    assert_eq!(thread_comments(&thread), ["IC_1"]);
    assert_eq!(thread_body(&thread, "IC_1"), body);

    // A login that does not resolve is a configuration error the operator has
    // to see, not a reviewer to drop quietly.
    std::fs::write(thread.join("pull-request"), "").unwrap();
    let error = sink
        .request_reviews(&GhCompletedMutation {
            reviewers: vec!["ghost".to_owned()],
            ..mutation
        })
        .unwrap_err();
    assert!(error.contains("does not resolve to a user"), "{error}");
}

/// `closeOnPass` unset used to fall back to `postEvidence`, so a producer that
/// only published evidence also closed the item. Absent now means off.
#[test]
fn unset_close_on_pass_posts_evidence_and_closes_nothing() {
    let temp = tempdir().unwrap();
    let mut registry = registry(&temp.path().join("effects.jsonl"));
    let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    assert!(github.post_evidence);
    github.close_on_pass = None;
    let observation = gh_observation("PR_unset_close", "author", "contributor");
    let origin = gh_origin("github", github, &observation);
    let engine = ProducerEngine::new(
        &registry,
        temp.path().join("events"),
        temp.path().join("state"),
        temp.path(),
    );

    let mut sink = RecordingMutation::default();
    assert!(engine
        .complete_gh(
            &origin,
            Verdict::Pass,
            Some(serde_json::json!({"witnessSeq": 5})),
            &mut sink,
        )
        .unwrap());
    assert_eq!(sink.comments.len(), 1);
    assert!(sink.closes.is_empty(), "evidence posting must not close");
    assert!(sink.item_open);
}

/// The same primitive carries completion evidence: one comment per completion
/// id, edited in place when the same completion is published again.
#[test]
fn github_completion_evidence_upserts_the_comment_it_already_created() {
    let temp = tempdir().unwrap();
    let gh = temp.path().join("fake-gh");
    let requests = temp.path().join("requests.jsonl");
    let thread = temp.path().join("thread");
    let state_dir = temp.path().join("state");
    install_scripted_graphql(&gh, &requests, &thread);
    let mutation = GhCompletedMutation {
        producer: "github".to_owned(),
        source: "search".to_owned(),
        item_id: "I_widget_42".to_owned(),
        completion_id: Some("task-1:attempt-1:witness-5".to_owned()),
        state: "COMPLETED".to_owned(),
        evidence: Some(serde_json::json!({"witnessSeq": 5})),
        gate_summary: None,
        acceptance: None,
        request_review: false,
        reviewers: Vec::new(),
        assisted_by: None,
    };

    let mut sink = GhCliMutationSink::with_program(&gh).with_state_dir(&state_dir);
    sink.post_evidence(&mutation).unwrap();
    assert_eq!(
        scripted_operations(&requests),
        ["TallyCompletionState", "TallyCompletionComment"]
    );
    assert_eq!(thread_comments(&thread), ["IC_1"]);
    assert_eq!(
        stored_sticky_comments(&state_dir),
        [("task-1:attempt-1:witness-5".to_owned(), "IC_1".to_owned())]
    );

    // A sticky re-publication is exactly one round trip. The state assertion
    // rides the thread scan the create and adopt paths already run; buying a
    // second query for it here doubled the cost of the case the sticky path
    // exists to make cheaper.
    let mut republished = GhCliMutationSink::with_program(&gh).with_state_dir(&state_dir);
    republished.post_evidence(&mutation).unwrap();
    assert_eq!(scripted_operations(&requests), ["TallyStickyComment"]);
    assert_eq!(thread_comments(&thread), ["IC_1"]);
}

/// A forge that refuses the edit — a secondary rate limit, a 502, a comment
/// locked by an org setting — must not be reported as a successful
/// publication. Adopting the comment and returning `Ok(())` would leave a
/// stale public body behind and destroy the reason.
#[test]
fn github_refused_sticky_edit_fails_the_publication_instead_of_reporting_success() {
    let temp = tempdir().unwrap();
    let gh = temp.path().join("fake-gh");
    let requests = temp.path().join("requests.jsonl");
    let thread = temp.path().join("thread");
    let state_dir = temp.path().join("state");
    install_scripted_graphql(&gh, &requests, &thread);
    let published = |note: &str| GhCompletedMutation {
        producer: "github".to_owned(),
        source: "search".to_owned(),
        item_id: "I_widget_42".to_owned(),
        completion_id: Some("task-1:attempt-1:witness-5".to_owned()),
        state: "COMPLETED".to_owned(),
        evidence: Some(serde_json::json!({"witnessSeq": 5, "note": note})),
        gate_summary: None,
        acceptance: None,
        request_review: false,
        reviewers: Vec::new(),
        assisted_by: None,
    };

    let mut sink = GhCliMutationSink::with_program(&gh).with_state_dir(&state_dir);
    sink.post_evidence(&published("first")).unwrap();
    assert_eq!(
        scripted_operations(&requests),
        ["TallyCompletionState", "TallyCompletionComment"]
    );

    // The remembered comment is still there, but the forge refuses the edit.
    std::fs::write(thread.join("refuse-edits"), "").unwrap();
    let error = sink.post_evidence(&published("second")).unwrap_err();
    assert!(error.contains("secondary rate limit"), "{error}");
    assert_eq!(
        scripted_operations(&requests),
        ["TallyStickyComment", "TallyCompletionState"]
    );
    assert_eq!(thread_comments(&thread), ["IC_1"]);
    assert!(thread_body(&thread, "IC_1").contains("first"));
    assert!(!thread_body(&thread, "IC_1").contains("second"));

    // It stays loud on the next attempt rather than silently converging on a
    // stale comment, and it never creates a duplicate to escape the refusal.
    let error = sink.post_evidence(&published("second")).unwrap_err();
    assert!(error.contains("secondary rate limit"), "{error}");
    assert_eq!(
        scripted_operations(&requests),
        ["TallyCompletionState", "TallyStickyComment"]
    );
    assert_eq!(thread_comments(&thread), ["IC_1"]);

    // Once the forge accepts writes again, the pending body is published into
    // the comment that was already there.
    std::fs::remove_file(thread.join("refuse-edits")).unwrap();
    sink.post_evidence(&published("second")).unwrap();
    assert_eq!(
        scripted_operations(&requests),
        ["TallyCompletionState", "TallyStickyComment"]
    );
    assert_eq!(thread_comments(&thread), ["IC_1"]);
    assert!(thread_body(&thread, "IC_1").contains("second"));
    assert_eq!(
        stored_sticky_comments(&state_dir),
        [("task-1:attempt-1:witness-5".to_owned(), "IC_1".to_owned())]
    );
}

/// The marker is on the thread but the comment carrying it has no node id to
/// edit. Creating a second comment would duplicate it and returning `Ok(())`
/// would report a publication that never happened, so the sink refuses.
#[test]
fn github_matched_comment_without_a_node_id_refuses_to_publish() {
    let temp = tempdir().unwrap();
    let gh = temp.path().join("fake-gh");
    let requests = temp.path().join("requests.jsonl");
    let thread = temp.path().join("thread");
    let state_dir = temp.path().join("state");
    install_scripted_graphql(&gh, &requests, &thread);
    std::fs::write(
        thread.join("IC_7.body"),
        "<!-- tally-trigger:receipt-42:accepted -->",
    )
    .unwrap();
    std::fs::write(thread.join("hide-ids"), "").unwrap();

    let mut sink = GhCliAcknowledgementSink::with_program(&gh).with_state_dir(&state_dir);
    let error = sink
        .post_acknowledgement(&trigger_acknowledgement(
            "receipt-42",
            GhDecisionStatus::Accepted,
        ))
        .unwrap_err();
    assert!(error.contains("refusing to publish a duplicate"), "{error}");
    assert_eq!(scripted_operations(&requests), ["TallyCompletionState"]);
    assert_eq!(thread_comments(&thread), ["IC_7"]);
    assert_eq!(stored_sticky_comments(&state_dir), Vec::new());
}

#[test]
fn github_issue_21_repo_label_and_state_scope_admits_only_the_exact_match() {
    let temp = tempdir().unwrap();
    let mut registry = registry(&temp.path().join("effects.jsonl"));
    let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.sources = vec![GhSource::Search(GhSourceConstraints {
        repo: Some("agency-agency/spec".to_owned()),
        labels: vec!["agency:codex-ready".to_owned()],
        state: Some(GhItemState::Open),
        ..GhSourceConstraints::default()
    })];
    github.triggers = GhTriggers {
        command_comments: vec!["/tally run".to_owned()],
        ..GhTriggers::default()
    };
    github.allowed_actors = vec!["contributor".to_owned()];
    let engine = ProducerEngine::new(
        &registry,
        temp.path().join("events"),
        temp.path().join("state"),
        temp.path(),
    );

    let mut matching = gh_command_observation("issue-21-comment", "contributor");
    matching.repo = "agency-agency/spec".to_owned();
    matching.number = 21;
    matching.html_url = "https://github.com/agency-agency/spec/issues/21".to_owned();
    matching.node_id = "I_agency_spec_21".to_owned();
    matching.context.labels = vec!["agency:codex-ready".to_owned()];
    assert!(matches!(
        engine.emit_gh("github", &matching, fixed_now()).unwrap(),
        EmitOutcome::Emitted(_)
    ));

    let mut wrong_repo = matching.clone();
    wrong_repo.repo = "agency-agency/other".to_owned();
    wrong_repo.html_url = "https://github.com/agency-agency/other/issues/21".to_owned();
    assert_eq!(
        engine.emit_gh("github", &wrong_repo, fixed_now()).unwrap(),
        EmitOutcome::Filtered {
            reason: GhFilterReason::RepositoryNotAllowed
        }
    );

    let mut wrong_label = matching.clone();
    wrong_label.context.labels = vec!["agency:triage".to_owned()];
    assert_eq!(
        engine.emit_gh("github", &wrong_label, fixed_now()).unwrap(),
        EmitOutcome::Filtered {
            reason: GhFilterReason::LabelMismatch
        }
    );

    let mut wrong_state = matching.clone();
    wrong_state.context.state = Some(GhItemState::Closed);
    assert_eq!(
        engine.emit_gh("github", &wrong_state, fixed_now()).unwrap(),
        EmitOutcome::Filtered {
            reason: GhFilterReason::StateMismatch
        }
    );

    let mut unscoped_registry = registry.clone();
    let ProducerConfig::Gh(unscoped) = unscoped_registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    unscoped.sources = vec![GhSource::Search(GhSourceConstraints::default())];
    let unscoped_engine = ProducerEngine::new(
        &unscoped_registry,
        temp.path().join("unscoped-events"),
        temp.path().join("unscoped-state"),
        temp.path(),
    );
    assert_eq!(
        unscoped_engine
            .emit_gh("github", &matching, fixed_now())
            .unwrap(),
        EmitOutcome::Filtered {
            reason: GhFilterReason::SourceUnconstrained
        }
    );
}

#[test]
fn github_cross_source_replay_prefers_the_source_whose_scope_matches() {
    let temp = tempdir().unwrap();
    let mut registry = registry(&temp.path().join("effects.jsonl"));
    let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.sources = vec![
        GhSource::Notifications(GhSourceConstraints {
            repo: Some("acme/widgets".to_owned()),
            labels: vec!["notification-only".to_owned()],
            ..GhSourceConstraints::default()
        }),
        GhSource::Search(GhSourceConstraints {
            repo: Some("acme/widgets".to_owned()),
            labels: vec!["ready".to_owned()],
            ..GhSourceConstraints::default()
        }),
    ];
    let search = gh_command_observation("cross-source-comment", "contributor");
    let notification = GhObservation {
        source: "notifications".to_owned(),
        notification_reason: Some("mention".to_owned()),
        event_id: Some("notification-42".to_owned()),
        ..search.clone()
    };
    let mut candidates = vec![
        GhIntakeCandidate::Observation(Box::new(notification)),
        GhIntakeCandidate::Observation(Box::new(search)),
    ];
    normalize_gh_candidates(github, &mut candidates);
    assert_eq!(candidates.len(), 1);
    let GhIntakeCandidate::Observation(observation) = &candidates[0] else {
        panic!("expected the matching concrete observation");
    };
    assert_eq!(observation.source, "search");
}

#[test]
fn github_comment_receipts_ack_one_accept_one_duplicate_and_a_later_job() {
    let temp = tempdir().unwrap();
    let mut registry = registry(&temp.path().join("effects.jsonl"));
    let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.sources = vec![GhSource::Search(GhSourceConstraints {
        repo: Some("acme/widgets".to_owned()),
        ..GhSourceConstraints::default()
    })];
    github.triggers = GhTriggers {
        command_comments: vec!["/tally run".to_owned()],
        ..GhTriggers::default()
    };
    github.allowed_actors = vec!["contributor".to_owned()];
    let events = temp.path().join("events");
    let state = temp.path().join("state");
    let engine = ProducerEngine::new(&registry, &events, &state, temp.path());
    let first_observation = gh_command_observation("comment-1", "contributor");
    let mut acknowledgements = RecordingAcknowledgements::default();

    let first = engine
        .admit_gh_observation(
            "github",
            &first_observation,
            fixed_now(),
            &mut acknowledgements,
        )
        .unwrap();
    assert_eq!(first.decision, GhDecisionStatus::Accepted);
    let first_task = first.task_uuid.clone().unwrap();
    assert_eq!(acknowledgements.entries.len(), 1);
    assert_eq!(
        acknowledgements.entries[0].decision,
        GhDecisionStatus::Accepted
    );
    assert_eq!(
        acknowledgements.entries[0].task_uuid.as_deref(),
        Some(first_task.as_str())
    );

    let duplicate = engine
        .admit_gh_observation(
            "github",
            &first_observation,
            fixed_now(),
            &mut acknowledgements,
        )
        .unwrap();
    assert_eq!(duplicate.decision, GhDecisionStatus::Duplicate);
    assert_eq!(
        duplicate.existing_task.as_deref(),
        Some(first_task.as_str())
    );
    // A duplicate reaches no sink at all. The suppression used to live in the
    // one production sink, which meant the decision point still built and
    // dispatched an acknowledgement nobody was allowed to publish, and any
    // second sink re-introduced the #245 public duplicate by default.
    assert_eq!(acknowledgements.entries.len(), 1);

    let mut later_observation = gh_command_observation("comment-2", "contributor");
    later_observation.trigger_timestamp = "2026-07-20T12:35:00Z".to_owned();
    let later = engine
        .admit_gh_observation(
            "github",
            &later_observation,
            fixed_now(),
            &mut acknowledgements,
        )
        .unwrap();
    assert_eq!(later.decision, GhDecisionStatus::Accepted);
    assert_ne!(later.task_uuid.as_deref(), Some(first_task.as_str()));
    assert_eq!(acknowledgements.entries.len(), 2);
    assert_eq!(
        acknowledgements.entries[1].decision,
        GhDecisionStatus::Accepted
    );
    assert_eq!(
        std::fs::read_dir(&events)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry
                .file_name()
                .to_string_lossy()
                .ends_with(INGRESS_SUFFIX))
            .count(),
        2
    );

    let third_replay = engine
        .admit_gh_observation(
            "github",
            &first_observation,
            fixed_now(),
            &mut acknowledgements,
        )
        .unwrap();
    assert_eq!(third_replay.decision, GhDecisionStatus::Duplicate);
    assert_eq!(
        acknowledgements.entries.len(),
        2,
        "no replay of an already-recorded trigger ever reaches a sink"
    );
}

#[test]
fn github_event_receipt_rejects_a_mutated_value_under_the_same_identity() {
    let temp = tempdir().unwrap();
    let mut registry = registry(&temp.path().join("effects.jsonl"));
    let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.sources = vec![GhSource::Search(GhSourceConstraints {
        repo: Some("acme/widgets".to_owned()),
        ..GhSourceConstraints::default()
    })];
    github.triggers = GhTriggers {
        assignments: vec!["tally-bot".to_owned()],
        ..GhTriggers::default()
    };
    github.allowed_actors = vec!["maintainer".to_owned()];
    let engine = ProducerEngine::new(
        &registry,
        temp.path().join("events"),
        temp.path().join("state"),
        temp.path(),
    );
    let mut observation = gh_command_observation("event-1", "maintainer");
    observation.trigger_kind = "assignment".to_owned();
    observation.event_id = Some("event-1".to_owned());
    observation.comment_id = None;
    observation.trigger_value = Some("tally-bot".to_owned());
    observation.context.triggering_comment = None;
    let mut acknowledgements = RecordingAcknowledgements::default();

    let accepted = engine
        .admit_gh_observation("github", &observation, fixed_now(), &mut acknowledgements)
        .unwrap();
    assert_eq!(accepted.decision, GhDecisionStatus::Accepted);

    let mut mutated = observation;
    mutated.trigger_value = Some("different-bot".to_owned());
    let error = engine
        .admit_gh_observation("github", &mutated, fixed_now(), &mut acknowledgements)
        .unwrap_err();
    assert!(error.to_string().contains("does not match the observation"));
    assert_eq!(acknowledgements.entries.len(), 1);
}

#[test]
fn github_unauthorized_command_records_rule_acknowledges_and_never_enqueues() {
    let temp = tempdir().unwrap();
    let mut registry = registry(&temp.path().join("effects.jsonl"));
    let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.sources = vec![GhSource::Search(GhSourceConstraints {
        repo: Some("acme/widgets".to_owned()),
        ..GhSourceConstraints::default()
    })];
    github.triggers = GhTriggers {
        command_comments: vec!["/tally run".to_owned()],
        ..GhTriggers::default()
    };
    github.allowed_actors = vec!["maintainer".to_owned()];
    let events = temp.path().join("events");
    let state = temp.path().join("state");
    let engine = ProducerEngine::new(&registry, &events, &state, temp.path());
    let observation = gh_command_observation("unauthorized-comment", "outsider");
    let mut acknowledgements = RecordingAcknowledgements::default();

    let decision = engine
        .admit_gh_observation("github", &observation, fixed_now(), &mut acknowledgements)
        .unwrap();
    assert_eq!(decision.decision, GhDecisionStatus::Filtered);
    assert_eq!(decision.rule, Some(GhFilterReason::TriggerActorNotAllowed));
    assert!(!events.exists() || std::fs::read_dir(&events).unwrap().next().is_none());
    assert_eq!(acknowledgements.entries.len(), 1);
    assert_eq!(
        acknowledgements.entries[0].decision,
        GhDecisionStatus::Filtered
    );
    assert_eq!(
        acknowledgements.entries[0].rule,
        Some(GhFilterReason::TriggerActorNotAllowed)
    );
    assert!(acknowledgements.entries[0].task_uuid.is_none());

    let receipt_id = decision.receipt_id.unwrap();
    let receipt: Value = serde_json::from_slice(
        &std::fs::read(
            state
                .join("producers/gh-triggers")
                .join(format!("{receipt_id}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["primaryDecision"], "filtered");
    assert_eq!(receipt["rule"], "trigger-actor-not-allowed");
    assert_eq!(receipt["primaryAcknowledged"], true);
}

#[test]
fn github_cli_paginates_notifications_until_a_short_page() {
    let temp = tempdir().unwrap();
    let mut registry = registry(&temp.path().join("effects.jsonl"));
    let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.sources = vec![GhSource::Notifications(GhSourceConstraints {
        repo: Some("acme/repo".to_owned()),
        ..GhSourceConstraints::default()
    })];
    github.triggers = GhTriggers {
        mentions: vec!["@tally-bot please run".to_owned()],
        ..GhTriggers::default()
    };
    github.allowed_actors = vec!["contributor".to_owned()];
    let gh = temp.path().join("fake-gh-pages");
    let calls = temp.path().join("gh-page-calls");
    crate::test_support::install_shell_program(
            &gh,
            format!(
                concat!(
                    "#!/bin/sh\n",
                    "printf '%s\\n' \"$*\" >> '{}'\n",
                    "case \"$*\" in\n",
                    "  'api user') printf '{{\"login\":\"tally-bot\"}}' ;;\n",
                    "  'api --method GET notifications -f all=false -f participating=false -f per_page=100')\n",
                    "    printf '['\n",
                    "    i=1\n",
                    "    while [ \"$i\" -le 100 ]; do\n",
                    "      [ \"$i\" -eq 1 ] || printf ','\n",
                    "      printf '{{\"id\":\"skip-%s\",\"subject\":{{\"type\":\"CheckSuite\"}}}}' \"$i\"\n",
                    "      i=$((i + 1))\n",
                    "    done\n",
                    "    printf ']'\n",
                    "    ;;\n",
                    "  'api --method GET notifications -f all=false -f participating=false -f per_page=100 -f page=2')\n",
                    "    printf '[{{\"id\":\"N101\",\"reason\":\"mention\",\"updated_at\":\"2026-07-20T12:00:00Z\",\"repository\":{{\"full_name\":\"acme/repo\"}},\"subject\":{{\"type\":\"Issue\",\"url\":\"https://api.github.com/repos/acme/repo/issues/101\",\"latest_comment_url\":\"https://api.github.com/repos/acme/repo/issues/comments/1101\"}}}}]'\n",
                    "    ;;\n",
                    "  'api /repos/acme/repo/issues/101') printf '{{\"node_id\":\"I_node_101\",\"number\":101,\"html_url\":\"https://github.com/acme/repo/issues/101\",\"title\":\"Paged issue\",\"body\":null,\"state\":\"open\",\"user\":{{\"login\":\"issue-author\"}},\"labels\":[],\"assignees\":[]}}' ;;\n",
                    "  'api /repos/acme/repo/issues/comments/1101') printf '{{\"id\":1101,\"body\":\"@tally-bot please run\",\"created_at\":\"2026-07-20T12:00:00Z\",\"updated_at\":\"2026-07-20T12:00:00Z\",\"user\":{{\"login\":\"contributor\"}}}}' ;;\n",
                    "  *) exit 91 ;;\n",
                    "esac\n"
                ),
                calls.display(),
            ),
        );

    let candidates = GhCliIntake::with_program(gh).poll(github).unwrap();
    assert_eq!(candidates.len(), 1);
    let GhIntakeCandidate::Observation(observation) = &candidates[0] else {
        panic!("expected a hydrated second-page notification");
    };
    assert_eq!(observation.number, 101);
    assert_eq!(observation.comment_id.as_deref(), Some("1101"));
    let calls = std::fs::read_to_string(calls).unwrap();
    assert!(calls.contains("per_page=100 -f page=2"));
    assert_eq!(calls.lines().count(), 5);
}

#[test]
fn github_cli_poll_requires_exact_trigger_actor_and_deduplicates_event_ids() {
    let temp = tempdir().unwrap();
    let mut registry = registry(&temp.path().join("effects.jsonl"));
    let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    github.sources = vec![GhSource::Notifications(GhSourceConstraints {
        repo: Some("acme/repo".to_owned()),
        ..GhSourceConstraints::default()
    })];
    github.triggers = GhTriggers {
        mentions: vec!["@tally-bot please run".to_owned()],
        ..GhTriggers::default()
    };
    github.allowed_actors = vec!["contributor".to_owned()];
    let events = temp.path().join("events");
    let state = temp.path().join("state");
    let gh = temp.path().join("fake-gh-intake");
    let calls = temp.path().join("gh-intake-calls");
    crate::test_support::install_shell_program(
            &gh,
            format!(
                concat!(
                    "#!/bin/sh\n",
                    "printf '%s\\n' \"$*\" >> '{}'\n",
                    "case \"$*\" in\n",
                    "  'api user') printf '{{\"login\":\"tally-bot\"}}' ;;\n",
                    "  'api --method GET notifications -f all=false -f participating=false -f per_page=100')\n",
                    "    printf '[{{\"id\":\"N1\",\"reason\":\"mention\",\"updated_at\":\"2026-07-20T12:00:00Z\",\"repository\":{{\"full_name\":\"acme/repo\"}},\"subject\":{{\"type\":\"Issue\",\"url\":\"https://api.github.com/repos/acme/repo/issues/1\",\"latest_comment_url\":\"https://api.github.com/repos/acme/repo/issues/comments/101\"}}}},{{\"id\":\"N2\",\"reason\":\"subscribed\",\"updated_at\":\"2026-07-20T12:10:00Z\",\"repository\":{{\"full_name\":\"acme/repo\"}},\"subject\":{{\"type\":\"Issue\",\"url\":\"https://api.github.com/repos/acme/repo/issues/2\",\"latest_comment_url\":\"https://api.github.com/repos/acme/repo/issues/comments/202\"}}}}]' ;;\n",
                    "  'api /repos/acme/repo/issues/1') printf '{{\"node_id\":\"I_node_1\",\"number\":1,\"html_url\":\"https://github.com/acme/repo/issues/1\",\"title\":\"Issue one\",\"body\":\"untrusted issue body\",\"state\":\"open\",\"user\":{{\"login\":\"tally-bot\"}},\"labels\":[{{\"name\":\"bug\"}}],\"assignees\":[{{\"login\":\"tally-bot\"}}]}}' ;;\n",
                    "  'api /repos/acme/repo/issues/comments/101') printf '{{\"id\":101,\"body\":\"@tally-bot please run\",\"created_at\":\"2026-07-20T12:00:00Z\",\"updated_at\":\"2026-07-20T12:00:00Z\",\"user\":{{\"login\":\"contributor\"}}}}' ;;\n",
                    "  'api /repos/acme/repo/issues/2') printf '{{\"node_id\":\"I_node_2\",\"number\":2,\"html_url\":\"https://github.com/acme/repo/issues/2\",\"title\":\"Issue two\",\"body\":null,\"state\":\"open\",\"user\":{{\"login\":\"issue-author\"}},\"labels\":[],\"assignees\":[]}}' ;;\n",
                    "  'api /repos/acme/repo/issues/comments/202') printf '{{\"id\":202,\"body\":\"older unrelated comment\",\"created_at\":\"2026-07-20T11:00:00Z\",\"updated_at\":\"2026-07-20T11:00:00Z\",\"user\":{{\"login\":\"other\"}}}}' ;;\n",
                    "  'api /repos/acme/repo/issues/2/events?per_page=100') printf '[]' ;;\n",
                    "  *) exit 91 ;;\n",
                    "esac\n"
                ),
                calls.display(),
            ),
        );
    let intake = GhCliIntake::with_program(&gh);
    let engine = ProducerEngine::new(&registry, &events, &state, temp.path());
    let first = engine.poll_gh("github", &intake, fixed_now()).unwrap();
    assert_eq!(
        first
            .iter()
            .filter(|outcome| matches!(outcome, EmitOutcome::Emitted(_)))
            .count(),
        1
    );
    assert_eq!(
        first
            .iter()
            .filter(|outcome| {
                matches!(
                    outcome,
                    EmitOutcome::Filtered {
                        reason: GhFilterReason::TriggerActorUnavailable
                    }
                )
            })
            .count(),
        1
    );
    assert!(!first.iter().any(|outcome| {
        matches!(
            outcome,
            EmitOutcome::Filtered {
                reason: GhFilterReason::SelfTriggerDisabled
            }
        )
    }));
    let emitted = first
        .iter()
        .find_map(|outcome| match outcome {
            EmitOutcome::Emitted(path) => Some(path),
            _ => None,
        })
        .unwrap();
    let payload: EnqueuePayload = serde_json::from_slice(&std::fs::read(emitted).unwrap()).unwrap();
    let origin = payload.gh_origin.unwrap();
    assert_eq!(origin.item_author, "tally-bot");
    assert_eq!(origin.trigger_actor, "contributor");
    assert_eq!(origin.comment_id.as_deref(), Some("101"));
    assert_eq!(
        origin.context.unwrap().triggering_comment.unwrap().author,
        "contributor"
    );
    let second = engine.poll_gh("github", &intake, fixed_now()).unwrap();
    assert_eq!(
        second
            .iter()
            .filter(|outcome| matches!(outcome, EmitOutcome::Duplicate))
            .count(),
        1
    );
    assert_eq!(
        second
            .iter()
            .filter(|outcome| {
                matches!(
                    outcome,
                    EmitOutcome::Filtered {
                        reason: GhFilterReason::TriggerActorUnavailable
                    }
                )
            })
            .count(),
        1
    );
    assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 14);

    let mut disabled_registry = registry.clone();
    let ProducerConfig::Gh(disabled) = disabled_registry.get_mut("github").unwrap() else {
        unreachable!()
    };
    disabled.enable = false;
    assert!(
        ProducerEngine::new(&disabled_registry, &events, &state, temp.path())
            .poll_gh(
                "github",
                &GhCliIntake::with_program(temp.path().join("absent-gh")),
                fixed_now(),
            )
            .unwrap()
            .is_empty()
    );

    let malformed_gh = temp.path().join("malformed-gh-intake");
    crate::test_support::install_shell_program(
            &malformed_gh,
            concat!(
                "#!/bin/sh\n",
                "case \"$*\" in\n",
                "  'api user') printf '{\"login\":\"tally-bot\"}' ;;\n",
                "  'api --method GET notifications -f all=false -f participating=false -f per_page=100') printf '[{\"id\":\"N9\",\"updated_at\":\"2026-07-20T12:00:00Z\",\"repository\":{\"full_name\":\"acme/repo\"},\"subject\":{\"type\":\"Issue\",\"url\":\"https://api.github.com/repos/acme/repo/issues/9\"}}]' ;;\n",
                "  'api /repos/acme/repo/issues/9') printf '{\"user\":{\"login\":\"contributor\"}}' ;;\n",
                "  'api /repos/acme/repo/issues/9/events?per_page=100') printf '[]' ;;\n",
                "  *) exit 91 ;;\n",
                "esac\n",
            ),
        );
    assert!(engine
        .poll_gh(
            "github",
            &GhCliIntake::with_program(malformed_gh),
            fixed_now(),
        )
        .unwrap_err()
        .to_string()
        .contains("omitted number"));
}

#[test]
fn build_effect_is_atomic_single_flight_per_store_path() {
    let temp = tempdir().unwrap();
    let registry = Arc::new(registry(&temp.path().join("effects.jsonl")));
    let events = temp.path().join("events");
    let state = temp.path().join("state");
    let barrier = Arc::new(Barrier::new(2));
    let outcomes = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..2 {
            let registry = registry.clone();
            let events = events.clone();
            let state = state.clone();
            let barrier = barrier.clone();
            handles.push(scope.spawn(move || {
                barrier.wait();
                ProducerEngine::new(&registry, events, state.clone(), state)
                    .emit_build_effect("effects", Path::new(STORE_A), fixed_now())
                    .unwrap()
            }));
        }
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, EmitOutcome::Emitted(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, EmitOutcome::Duplicate))
            .count(),
        1
    );
    let emitted = outcomes
        .into_iter()
        .find_map(|outcome| match outcome {
            EmitOutcome::Emitted(path) => Some(path),
            _ => None,
        })
        .unwrap();
    let payload: EnqueuePayload =
        serde_json::from_slice(&std::fs::read(&emitted).unwrap()).unwrap();
    assert_eq!(payload.source, Some(EnqueueSource::BuildEffect));
    assert_eq!(
        payload.dedup_key.as_deref(),
        Some(format!("build-effect:effects:{STORE_A}").as_str())
    );

    let claims = claim_ingress_files(&events).unwrap();
    assert_eq!(claims.len(), 1);
    archive_ingress_claim(&events, &claims[0], true).unwrap();
    assert_eq!(
        ProducerEngine::new(&registry, &events, &state, temp.path())
            .emit_build_effect("effects", Path::new(STORE_A), fixed_now())
            .unwrap(),
        EmitOutcome::Duplicate
    );
    assert!(ProducerEngine::new(&registry, &events, &state, temp.path())
        .emit_build_effect("effects", Path::new(STORE_B), fixed_now())
        .is_ok());
}

#[test]
fn build_effect_scanners_cover_all_bounded_watch_shapes() {
    let temp = tempdir().unwrap();
    let roots = temp.path().join("roots");
    std::fs::create_dir(&roots).unwrap();
    std::os::unix::fs::symlink(STORE_B, roots.join("b-root")).unwrap();
    std::os::unix::fs::symlink(STORE_A, roots.join("a-root")).unwrap();
    assert_eq!(
        scan_store_paths(BuildEffectWatch::GcRootsDir, &roots).unwrap(),
        [PathBuf::from(STORE_A), PathBuf::from(STORE_B)]
    );

    let jsonl = temp.path().join("effects.jsonl");
    std::fs::write(
        &jsonl,
        format!(
            "{}\n{}\n",
            serde_json::to_string(STORE_B).unwrap(),
            serde_json::json!({"outputs": [STORE_A, STORE_B]})
        ),
    )
    .unwrap();
    assert_eq!(
        scan_store_paths(BuildEffectWatch::Jsonl, &jsonl).unwrap(),
        [PathBuf::from(STORE_A), PathBuf::from(STORE_B)]
    );

    let hook = temp.path().join("post-build");
    std::fs::write(&hook, format!("{STORE_B} {STORE_A}\n")).unwrap();
    assert_eq!(
        scan_store_paths(BuildEffectWatch::PostBuildHook, &hook).unwrap(),
        [PathBuf::from(STORE_A), PathBuf::from(STORE_B)]
    );
}

#[test]
fn fleet_conformance_network_blip_and_true_vanish_are_distinguished_by_hysteresis() {
    let temp = tempdir().unwrap();
    let registry = registry(&temp.path().join("effects.jsonl"));
    let events = temp.path().join("events");
    let state = temp.path().join("state");
    let engine = ProducerEngine::new(&registry, &events, &state, temp.path());

    let blip = engine
        .observe_reachability("health", false, fixed_now())
        .unwrap();
    assert_eq!(blip.stable, ReachabilityStable::Reachable);
    assert_eq!(blip.transition, None);
    assert!(blip.emitted.is_empty());
    let recovered_blip = engine
        .observe_reachability("health", true, fixed_now())
        .unwrap();
    assert_eq!(recovered_blip.stable, ReachabilityStable::Reachable);
    assert_eq!(recovered_blip.transition, None);
    assert!(recovered_blip.emitted.is_empty());

    for failed in 1..=2 {
        let outcome = engine
            .observe_reachability("health", false, fixed_now())
            .unwrap();
        assert_eq!(outcome.stable, ReachabilityStable::Reachable);
        assert_eq!(outcome.transition, None, "failed probe {failed}");
        assert!(outcome.emitted.is_empty());
    }
    let lost = engine
        .observe_reachability("health", false, fixed_now())
        .unwrap();
    assert_eq!(lost.stable, ReachabilityStable::Lost);
    assert_eq!(lost.transition, Some(ReachabilityTransition::Lost));
    assert_eq!(lost.generation, 1);
    assert_eq!(lost.emitted.len(), 1);
    let pending = engine
        .observe_reachability("health", false, fixed_now())
        .unwrap();
    assert_eq!(pending.transition, Some(ReachabilityTransition::Lost));
    assert_eq!(pending.generation, lost.generation);
    assert!(pending.emitted.is_empty());
    for success in 1..=3 {
        let still_pending = engine
            .observe_reachability("health", true, fixed_now())
            .unwrap();
        assert_eq!(still_pending.stable, ReachabilityStable::Lost);
        assert_eq!(
            still_pending.transition,
            Some(ReachabilityTransition::Lost),
            "opposite probe {success} must not overwrite an unacknowledged loss"
        );
        assert_eq!(still_pending.generation, lost.generation);
        assert!(still_pending.emitted.is_empty());
    }
    engine
        .validate_reachability_transition("health", ReachabilityTransition::Lost, lost.generation)
        .unwrap();
    engine
        .acknowledge_reachability_transition("health", lost.generation)
        .unwrap();

    for success in 1..=2 {
        let outcome = engine
            .observe_reachability("health", true, fixed_now())
            .unwrap();
        assert_eq!(outcome.stable, ReachabilityStable::Lost);
        assert_eq!(outcome.transition, None, "successful probe {success}");
        assert!(outcome.emitted.is_empty());
    }
    let returned = engine
        .observe_reachability("health", true, fixed_now())
        .unwrap();
    assert_eq!(returned.stable, ReachabilityStable::Reachable);
    assert_eq!(returned.transition, Some(ReachabilityTransition::Returned));
    assert_eq!(returned.generation, 2);
    assert_eq!(returned.emitted.len(), 2);
    let payloads = returned
        .emitted
        .iter()
        .map(|path| {
            serde_json::from_slice::<EnqueuePayload>(&std::fs::read(path).unwrap()).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        payloads.iter().filter(|payload| payload.no_enqueue).count(),
        1
    );
    assert_eq!(
        payloads
            .iter()
            .filter(|payload| !payload.no_enqueue)
            .count(),
        1
    );
    assert!(payloads
        .iter()
        .all(|payload| payload.source == Some(EnqueueSource::PoolReachability)));
    assert_eq!(
        engine.confirmed_pool_returns().unwrap(),
        BTreeSet::from(["slot".to_owned()])
    );
    engine
        .acknowledge_reachability_transition("health", returned.generation)
        .unwrap();

    let reopened = ProducerEngine::new(&registry, &events, &state, temp.path());
    let stable = reopened
        .observe_reachability("health", true, fixed_now())
        .unwrap();
    assert_eq!(stable.stable, ReachabilityStable::Reachable);
    assert_eq!(stable.transition, None);
    assert!(stable.emitted.is_empty());

    let mut rebound_registry = registry.clone();
    let ProducerConfig::PoolReachability(rebound) = rebound_registry.get_mut("health").unwrap()
    else {
        unreachable!()
    };
    rebound.probe_pool = "different-slot".to_owned();
    let rebound = ProducerEngine::new(&rebound_registry, &events, &state, temp.path());
    assert!(rebound
        .confirmed_pool_returns()
        .unwrap_err()
        .to_string()
        .contains("not bound"));
}

#[test]
fn ingress_claims_are_atomic_recoverable_and_nofollow() {
    let temp = tempdir().unwrap();
    let events = temp.path().join("events");
    std::fs::create_dir(&events).unwrap();
    let payload = enqueue("from-file")
        .payload(EnqueueSource::EventsDir, Some("events"), fixed_now(), None)
        .unwrap();
    std::fs::write(
        events.join("valid.json"),
        serde_json::to_vec(&payload).unwrap(),
    )
    .unwrap();
    std::fs::write(events.join("internal.enqueue.json"), b"not ingress").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", events.join("hostile.json")).unwrap();
    let fifo = events.join("hostile-fifo.json");
    let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    let overlong = format!("{}.json", "a".repeat(MAX_CLAIMABLE_NAME_BYTES));
    std::fs::write(
        events.join(&overlong),
        serde_json::to_vec(&payload).unwrap(),
    )
    .unwrap();

    let claims = claim_ingress_files(&events).unwrap();
    assert_eq!(claims.len(), 3);
    assert!(!events.join("valid.json").exists());
    assert!(events.join("internal.enqueue.json").exists());
    assert!(!events.join(&overlong).exists());
    assert!(std::fs::read_dir(events.join("rejected"))
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().starts_with("overlong-")));
    std::fs::write(
        events.join(&overlong),
        serde_json::to_vec(&payload).unwrap(),
    )
    .unwrap();
    let resumed = claim_ingress_files(&events).unwrap();
    assert_eq!(resumed, claims);
    assert!(!events.join(&overlong).exists());
    assert_eq!(
        std::fs::read_dir(events.join("rejected"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("overlong-"))
            .count(),
        2
    );

    for claim in claims {
        if claim.original_name == "valid.json" {
            let decoded = read_ingress_payload(&claim).unwrap();
            assert_eq!(decoded, payload);
            std::fs::write(events.join("done/valid.json"), b"prior archive").unwrap();
            let archived = archive_ingress_claim(&events, &claim, true).unwrap();
            assert_eq!(archived, events.join("done/valid.json.1"));
            assert_eq!(
                std::fs::read(events.join("done/valid.json")).unwrap(),
                b"prior archive"
            );
        } else if claim.original_name == "hostile.json" {
            assert!(read_ingress_payload(&claim).is_err());
            let archived = archive_ingress_claim(&events, &claim, false).unwrap();
            assert_eq!(archived, events.join("rejected/hostile.json"));
            assert!(std::fs::symlink_metadata(archived)
                .unwrap()
                .file_type()
                .is_symlink());
        } else {
            assert_eq!(claim.original_name, "hostile-fifo.json");
            assert!(read_ingress_payload(&claim).is_err());
            let archived = archive_ingress_claim(&events, &claim, false).unwrap();
            assert_eq!(archived, events.join("rejected/hostile-fifo.json"));
            assert!(std::fs::symlink_metadata(archived)
                .unwrap()
                .file_type()
                .is_fifo());
        }
    }
    assert!(claim_ingress_files(&events).unwrap().is_empty());
}
