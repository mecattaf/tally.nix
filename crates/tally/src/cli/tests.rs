use std::io;

use super::*;

#[test]
fn clap_tree_is_consistent() {
    Opts::command().debug_assert();
}

#[test]
fn rpc_timeout_flag_is_global() {
    let options =
        Opts::try_parse_from(["tally", "query", "pools", "--rpc-timeout-sec", "7"]).unwrap();
    assert_eq!(options.rpc_timeout_sec, Some(7));
}

#[test]
fn rpc_timeout_selection_prefers_flag_then_environment_then_default() {
    assert_eq!(
        resolve_rpc_timeout(Some(7), Some(OsStr::new("invalid"))).unwrap(),
        Duration::from_secs(7)
    );
    assert_eq!(
        resolve_rpc_timeout(None, Some(OsStr::new("9"))).unwrap(),
        Duration::from_secs(9)
    );
    assert_eq!(
        resolve_rpc_timeout(None, None).unwrap(),
        Duration::from_secs(DEFAULT_RPC_TIMEOUT_SEC)
    );
}

#[test]
fn rpc_timeout_selection_rejects_zero_and_invalid_environment_values() {
    assert!(resolve_rpc_timeout(Some(0), None).is_err());
    assert!(resolve_rpc_timeout(None, Some(OsStr::new("0"))).is_err());
    assert!(resolve_rpc_timeout(None, Some(OsStr::new("not-a-number"))).is_err());
}

#[test]
fn authorship_verifier_cli_selects_an_exact_witness_lane() {
    let options = Opts::try_parse_from([
        "tally",
        "witness",
        "verify-authorship",
        "--ledger",
        "/tmp/witness.jsonl",
        "--repository",
        "/tmp/repository",
        "--task",
        "00000000-0000-4000-8000-000000000053",
        "--attempt",
        "2",
        "--lease-epoch",
        "7",
        "--format",
        "json",
    ])
    .unwrap();
    assert!(matches!(
        options.command,
        Some(Command::Witness {
            command: WitnessCommand::VerifyAuthorship {
                ledger: Some(ledger),
                repository,
                task,
                attempt: Some(2),
                lease_epoch: Some(7),
                format: WitnessVerifyFormat::Json,
            }
        }) if ledger == Path::new("/tmp/witness.jsonl")
            && repository == Path::new("/tmp/repository")
            && task == "00000000-0000-4000-8000-000000000053"
    ));
}

#[test]
fn explicit_client_config_controls_the_transport_limit() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config.json");
    std::fs::write(
        &config,
        concat!(
            r#"{"maxFrameBytes":20971520,"agingThresholdSec":3600,"pools":{"slot":{"#,
            r#""credentials":{"token":"/run/credentials/slot-token"}}}}"#
        ),
    )
    .unwrap();
    assert_eq!(
        client_max_frame_bytes(Some(&config)).unwrap(),
        20 * 1024 * 1024
    );
    assert_eq!(
        load_client_config(Some(&config)).unwrap().pools["slot"].credentials["token"],
        PathBuf::from("/run/credentials/slot-token")
    );
    assert!(client_max_frame_bytes(Some(&temp.path().join("missing.json"))).is_err());
}

#[test]
fn runner_identity_is_all_or_nothing_and_uuid_typed() {
    assert_eq!(
        captured_runner_identity(None, None, None).unwrap(),
        RunnerIdentity::default()
    );
    let identity = captured_runner_identity(
        Some("00000000-0000-4000-8000-000000000071".to_owned()),
        Some("00000000-0000-4000-8000-000000000072".to_owned()),
        Some("ab".repeat(32)),
    )
    .unwrap();
    assert_eq!(
        identity.task_uuid.as_deref(),
        Some("00000000-0000-4000-8000-000000000071")
    );
    assert_eq!(
        identity.job_token.as_deref(),
        Some("ab".repeat(32).as_str())
    );
    assert_eq!(
        captured_runner_identity(
            Some("00000000-0000-4000-8000-000000000071".to_owned()),
            None,
            None
        )
        .unwrap_err()
        .code,
        "runner-identity-incomplete"
    );
    assert_eq!(
        captured_runner_identity(
            Some("not-a-uuid".to_owned()),
            Some("also-bad".to_owned()),
            None
        )
        .unwrap_err()
        .code,
        "runner-identity-invalid"
    );
}

#[test]
fn flow_failure_taxonomy_has_distinguished_exit_codes() {
    let exit_code = |code| {
        error_exit_code(&flow_error(FlowError::new(
            "FlowTestError",
            code,
            "fixture",
        )))
    };
    assert_eq!(exit_code("script-syntax"), 10);
    assert_eq!(exit_code("script-evaluation"), 10);
    assert_eq!(exit_code("determinism-violation"), 10);
    assert_eq!(exit_code("replay-divergence"), 20);
    assert_eq!(exit_code("script-changed-mid-run"), 20);
    assert_eq!(exit_code("args-changed-mid-run"), 20);
    assert_eq!(exit_code("catalog-changed-mid-run"), 20);
    assert_eq!(exit_code("flow-run-id-missing"), 2);
    assert_eq!(exit_code("runner-identity-incomplete"), 2);
    assert_eq!(exit_code("workload-mutex-parent-required"), 2);
    assert_eq!(exit_code("flow-cancelled"), 4);
    assert_eq!(exit_code("terminal-failure"), 1);
}

#[test]
fn full_top_level_surface_is_visible() {
    let help = Opts::command().render_long_help().to_string();
    for verb in [
        "enqueue", "queue", "producer", "witness", "lease", "daemon", "query", "flow",
    ] {
        assert!(help.contains(verb), "missing {verb} from help");
    }
    assert!(!help.contains("__record-unit-exit"));
}

#[test]
fn hidden_exit_recorder_command_parses() {
    let options = Opts::try_parse_from([
        "tally",
        "__record-unit-exit",
        "--record",
        "/tmp/exit.json",
        "--unit",
        "tally-job-example.service",
    ])
    .unwrap();
    assert!(matches!(
        options.command,
        Some(Command::RecordUnitExit(RecordUnitExitArgs { record, unit }))
            if record.as_path() == Path::new("/tmp/exit.json")
                && unit == "tally-job-example.service"
    ));
}

#[test]
fn hidden_producer_dispatch_parses_a_typed_observation() {
    let options = Opts::try_parse_from([
        "tally",
        "--config",
        "/tmp/config.json",
        "__producer-dispatch",
        "health",
        "--event",
        r#"{"kind":"pool-reachability","reachable":false}"#,
        "--state-dir",
        "/tmp/state",
    ])
    .unwrap();
    assert!(matches!(
        options.command,
        Some(Command::ProducerDispatch(ProducerDispatchArgs {
            producer,
            state_dir: Some(state_dir),
            ..
        })) if producer == "health" && state_dir == Path::new("/tmp/state")
    ));
}

#[test]
fn frozen_transport_exit_codes_are_stable() {
    let unreachable = anyhow::Error::new(WireIoError::Unreachable {
        path: PathBuf::from("/missing"),
        source: io::Error::from(io::ErrorKind::NotFound),
    });
    assert_eq!(error_exit_code(&unreachable), 3);
    let rearm_exhausted = anyhow::Error::new(WireIoError::RearmDeadlineExceeded {
        method: "queue.await_job".to_owned(),
        path: PathBuf::from("/missing"),
        window: Duration::from_secs(60),
    });
    assert_eq!(error_exit_code(&rearm_exhausted), 3);
    for (wire_code, exit_code) in [
        (WireErrorCode::InvalidParams, 2),
        (WireErrorCode::NotFound, 4),
        (WireErrorCode::Internal, 1),
    ] {
        let error = anyhow::Error::new(WireIoError::Rpc(wire_code, "failure".to_owned(), None));
        assert_eq!(error_exit_code(&error), exit_code);
    }
}

#[test]
fn waited_verdict_exit_codes_are_stable() {
    assert_eq!(verdict_exit_code("pass"), 0);
    assert_eq!(verdict_exit_code("reused"), 0);
    assert_eq!(verdict_exit_code("clean-exit-no-artifact"), 3);
    assert_eq!(verdict_exit_code("cancelled"), 4);
    for verdict in ["failed", "pool-vanished", "preempted", "runtime-exceeded"] {
        assert_eq!(verdict_exit_code(verdict), 1);
    }
}

#[test]
fn waited_signal_exit_is_never_success() {
    let waited = json!({"exit_code": -9});
    assert!(waited.get("verdict").is_none());
    assert_eq!(waited_exit_code(&waited), 1);
}

#[test]
fn enqueue_accepts_direct_argv_or_invocation() {
    let direct = Opts::try_parse_from([
        "tally",
        "enqueue",
        "--pool",
        "gpu",
        "--",
        "cmd",
        "two words",
    ]);
    assert!(direct.is_ok());
    let invocation = Opts::try_parse_from([
        "tally",
        "queue",
        "enqueue",
        "--pool",
        "gpu",
        "--invocation",
        "cmd 'two words'",
    ]);
    assert!(invocation.is_ok());
}

#[test]
fn enqueue_submission_mode_defaults_to_full_and_accepts_legacy() {
    let default = Opts::try_parse_from([
        "tally",
        "enqueue",
        "--pool",
        "gpu",
        "--dedup-key",
        "review:42",
        "--",
        "true",
    ])
    .unwrap();
    let Some(Command::Enqueue(default)) = default.command else {
        panic!("expected enqueue command");
    };
    assert_eq!(default.submission, CliSubmissionMode::Full);

    let legacy = Opts::try_parse_from([
        "tally",
        "enqueue",
        "--pool",
        "gpu",
        "--dedup-key",
        "review:42",
        "--submission",
        "legacy",
        "--",
        "true",
    ])
    .unwrap();
    let Some(Command::Enqueue(legacy)) = legacy.command else {
        panic!("expected enqueue command");
    };
    assert_eq!(legacy.submission, CliSubmissionMode::Legacy);

    let invalid = Opts::try_parse_from([
        "tally",
        "enqueue",
        "--pool",
        "gpu",
        "--submission",
        "other",
        "--",
        "true",
    ]);
    assert!(invalid.is_err());
}

#[test]
fn flow_run_and_check_cli_shapes_match_the_declarative_contract() {
    let check = Opts::try_parse_from([
        "tally",
        "flow",
        "check",
        "/nix/store/example-flow.js",
        "--args",
        r#"{"task":"ship"}"#,
        "--catalog",
        "/nix/store/catalog.json",
    ])
    .unwrap();
    assert!(matches!(
        check.command,
        Some(Command::Flow {
            command: FlowCommand::Check(FlowCheckArgs {
                script,
                args: Some(args),
                catalog: Some(catalog),
            })
        }) if script == Path::new("/nix/store/example-flow.js")
            && args == json!({"task": "ship"})
            && catalog == Path::new("/nix/store/catalog.json")
    ));

    let run = Opts::try_parse_from([
        "tally",
        "flow",
        "run",
        "/nix/store/example-flow.js",
        "--args",
        r#"{"task":"ship"}"#,
        "--max-nodes",
        "200",
        "--flow-run-id",
        "run-47",
        "--rpc-call-deadline-sec",
        "7200",
    ])
    .unwrap();
    assert!(matches!(
        run.command,
        Some(Command::Flow {
            command: FlowCommand::Run(FlowRunArgs {
                flow_run_id: Some(flow_run_id),
                max_nodes: 200,
                rpc_call_deadline_sec: Some(7200),
                ..
            })
        }) if flow_run_id == "run-47"
    ));

    let cancel = Opts::try_parse_from([
        "tally",
        "flow",
        "cancel",
        "00000000-0000-4000-8000-000000000145",
    ])
    .unwrap();
    assert!(matches!(
        cancel.command,
        Some(Command::Flow {
            command: FlowCommand::Cancel(FlowCancelArgs { flow_run_id })
        }) if flow_run_id == "00000000-0000-4000-8000-000000000145"
    ));
}

#[test]
fn enqueue_accepts_opaque_evidence_metadata_flags() {
    let evidence_class = r#"{"arbitrary":[true,7,{"nested":null}]}"#;
    let options = Opts::try_parse_from([
        "tally",
        "enqueue",
        "--pool",
        "gpu",
        "--evidence-class",
        evidence_class,
        "--manifest-hash",
        "deliberately-not-validated://manifest value",
        "--",
        "true",
    ])
    .unwrap();
    let Some(Command::Enqueue(args)) = options.command else {
        panic!("expected enqueue command");
    };
    assert_eq!(
        args.evidence_class,
        Some(serde_json::from_str(evidence_class).unwrap())
    );
    assert_eq!(
        args.manifest_hash.as_deref(),
        Some("deliberately-not-validated://manifest value")
    );

    let scalar = Opts::try_parse_from([
        "tally",
        "enqueue",
        "--pool",
        "gpu",
        "--evidence-class",
        "-1",
        "--manifest-hash",
        "-opaque-manifest",
        "--",
        "true",
    ])
    .unwrap();
    let Some(Command::Enqueue(args)) = scalar.command else {
        panic!("expected enqueue command");
    };
    assert_eq!(args.evidence_class, Some(Value::from(-1)));
    assert_eq!(args.manifest_hash.as_deref(), Some("-opaque-manifest"));
}

#[test]
fn enqueue_wave_three_options_and_public_continuation_parse_directly() {
    let options = Opts::try_parse_from([
        "tally",
        "enqueue",
        "--pool",
        "build",
        "--adapter",
        "codex",
        "--cwd",
        "/worktrees/tally",
        "--env",
        "NO_COLOR=1",
        "--pre-prompt-arg",
        "--dangerously-bypass-approvals-and-sandbox",
        "--approval-policy",
        "never",
        "--sandbox-policy",
        "danger-full-access",
        "--model",
        "gpt-5-codex",
        "--effort",
        "high",
        "--workspace-repo",
        "mecattaf/tally.nix",
        "--workspace-base-rev",
        "origin/main",
        "--workspace-branch",
        "wave-3-ergonomics",
        "--workspace-worktree",
        "/worktrees/tally",
        "--gate-manifest",
        "/worktrees/tally/.tally/gates.json",
        "--required-gate",
        "tests",
        "--acceptance-policy",
        "execution-and-gates",
        "--",
        "implement issue 28",
    ])
    .unwrap();
    let Some(Command::Enqueue(args)) = options.command else {
        panic!("expected enqueue command");
    };
    assert_eq!(
        args.pre_prompt_argv,
        ["--dangerously-bypass-approvals-and-sandbox"]
    );
    assert_eq!(args.environment, [("NO_COLOR".to_owned(), "1".to_owned())]);
    assert_eq!(args.workspace_repo.as_deref(), Some("mecattaf/tally.nix"));
    assert_eq!(args.required_gate_ids, ["tests"]);

    let continuation = Opts::try_parse_from([
        "tally",
        "queue",
        "continue",
        "00000000-0000-4000-8000-000000000028",
        "--wait",
        "--",
        "address review",
    ])
    .unwrap();
    assert!(matches!(
        continuation.command,
        Some(Command::Queue {
            command: QueueCommand::Continue {
                job,
                wait: true,
                argv,
            }
        }) if job == "00000000-0000-4000-8000-000000000028"
            && argv == ["address review"]
    ));

    let retry = Opts::try_parse_from([
        "tally",
        "queue",
        "retry",
        "00000000-0000-4000-8000-000000000028",
    ])
    .unwrap();
    assert!(matches!(
        retry.command,
        Some(Command::Queue {
            command: QueueCommand::Retry { job }
        }) if job == "00000000-0000-4000-8000-000000000028"
    ));
}

#[test]
fn producer_diagnostics_and_related_trigger_fallback_parse_strictly() {
    let test = Opts::try_parse_from([
        "tally",
        "producer",
        "test",
        "github",
        "--item",
        "https://github.com/acme/widgets/issues/42",
        "--event",
        "command-comment",
        "--actor",
        "maintainer",
        "--no-enqueue",
    ])
    .unwrap();
    assert!(matches!(
        test.command,
        Some(Command::Producer {
            command: ProducerCommand::Test {
                name,
                event: GhDiagnosticEvent::CommandComment,
                no_enqueue: true,
                promote: false,
                ..
            }
        }) if name == "github"
    ));

    let fallback = Opts::try_parse_from([
        "tally",
        "enqueue",
        "--pool",
        "gpu",
        "--source",
        "orchestrator",
        "--related-trigger",
        r#"{"producer":"github","eventId":"comment-42","outcome":"filtered","receiptId":"receipt-42"}"#,
        "--",
        "true",
    ])
    .unwrap();
    let Some(Command::Enqueue(args)) = fallback.command else {
        panic!("expected enqueue command");
    };
    let related = args.related_trigger.unwrap();
    assert_eq!(related.producer, "github");
    assert_eq!(related.event_id, "comment-42");
    assert_eq!(related.receipt_id.as_deref(), Some("receipt-42"));
}
