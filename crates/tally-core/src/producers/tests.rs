use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;

use chrono::TimeZone;
use tempfile::tempdir;

use super::*;

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

fn registry() -> BTreeMap<String, ProducerConfig> {
    BTreeMap::from([
        (
            "daily".to_owned(),
            ProducerConfig::Calendar(Box::new(CalendarProducer {
                credentials: BTreeMap::new(),
                on_calendar: "daily".to_owned(),
                enqueue: ProducerEnqueue {
                    dedup_key: Some("daily-%Y%m%d".to_owned()),
                    ..enqueue("calendar-job")
                },
            })),
        ),
        (
            "drop".to_owned(),
            ProducerConfig::EventsDir(EventsDirProducer {
                credentials: BTreeMap::new(),
                poll_interval_sec: 60,
            }),
        ),
    ])
}

fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 20, 12, 30, 0)
        .single()
        .unwrap()
}

#[test]
fn registry_is_strict_open_by_name_and_closed_over_the_in_scope_kinds() {
    let registry = registry();
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
        "kind": "retired",
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

    let mut placeholder = invalid_strftime;
    let ProducerConfig::Calendar(calendar) = placeholder.get_mut("daily").unwrap() else {
        unreachable!()
    };
    calendar.enqueue.dedup_key = None;
    calendar.enqueue.argv = vec!["run-${trigger}".to_owned()];
    assert!(validate_registry(
        &placeholder,
        &BTreeSet::from(["slot".to_owned()]),
        &BTreeSet::from(["shell".to_owned()]),
        &BTreeSet::new(),
    )
    .unwrap_err()
    .to_string()
    .contains("placeholders are not supported"));
}

#[test]
fn producer_multi_pool_validation_rejects_empty_duplicate_and_unknown_sets() {
    let error_for = |requested: Vec<String>| {
        let mut registry = registry();
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
    let mut registry = registry();
    let ProducerConfig::Calendar(calendar) = registry.get_mut("daily").unwrap() else {
        unreachable!()
    };
    calendar.enqueue.credentials.insert(
        "token".to_owned(),
        PathBuf::from("/run/credentials/calendar-token"),
    );
    calendar.enqueue.brief = Some(serde_json::json!({"task": "nightly"}));
    let engine = ProducerEngine::new(&registry, temp.path().join("events"), temp.path());
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
    assert_eq!(
        crate::brief::PreparedBrief::from_path(brief_path)
            .unwrap()
            .document(),
        &serde_json::json!({"task": "nightly"})
    );
}

#[test]
fn ingress_claims_are_atomic_recoverable_and_nofollow() {
    let temp = tempdir().unwrap();
    let events = temp.path().join("events");
    std::fs::create_dir(&events).unwrap();
    let payload = enqueue("from-file")
        .payload(EnqueueSource::EventsDir, Some("events"), fixed_now())
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

    for claim in claims {
        if claim.original_name == "valid.json" {
            let decoded = read_ingress_payload(&claim).unwrap();
            assert_eq!(decoded, payload);
            std::fs::write(events.join("done/valid.json"), b"prior archive").unwrap();
            let archived = archive_ingress_claim(&events, &claim, true).unwrap();
            assert_eq!(archived, events.join("done/valid.json.1"));
        } else if claim.original_name == "hostile.json" {
            assert!(read_ingress_payload(&claim).is_err());
            let archived = archive_ingress_claim(&events, &claim, false).unwrap();
            assert!(std::fs::symlink_metadata(archived)
                .unwrap()
                .file_type()
                .is_symlink());
        } else {
            assert_eq!(claim.original_name, "hostile-fifo.json");
            assert!(read_ingress_payload(&claim).is_err());
            let archived = archive_ingress_claim(&events, &claim, false).unwrap();
            assert!(std::fs::symlink_metadata(archived)
                .unwrap()
                .file_type()
                .is_fifo());
        }
    }
    assert!(claim_ingress_files(&events).unwrap().is_empty());
}
