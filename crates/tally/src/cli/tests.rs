use std::io;

use super::*;

#[test]
fn clap_tree_is_consistent() {
    Opts::command().debug_assert();
}

#[test]
fn version_provenance_names_the_source_revision_beside_the_crate_version() {
    let rendered = Opts::command().render_version().to_string();
    assert!(
        rendered.contains(env!("CARGO_PKG_VERSION")),
        "--version must still name the workspace version: {rendered}"
    );
    assert!(
        rendered.contains(env!("TALLY_BUILD_REV")),
        "--version must name the build's source revision: {rendered}"
    );
}

#[test]
fn version_provenance_is_a_revision_or_the_unstamped_dev_fallback() {
    // The two shapes the build environment can produce, and nothing else: a
    // git revision (`self.rev`, or `self.dirtyRev`'s `<rev>-dirty`) when nix
    // stamped one, and the literal `dev` when no revision was passed. An empty
    // or free-form stamp would render a --version line that reads like
    // provenance without being any.
    let stamped = env!("TALLY_BUILD_REV");
    if stamped == "dev" {
        return;
    }
    let revision = stamped.strip_suffix("-dirty").unwrap_or(stamped);
    assert!(
        revision.len() >= 7
            && revision.len() <= 64
            && revision
                .chars()
                .all(|character| character.is_ascii_hexdigit()),
        "a stamped revision must be a hexadecimal object name: {stamped}"
    );
}

#[test]
fn rpc_timeout_flag_is_global() {
    let options =
        Opts::try_parse_from(["tally", "query", "pools", "--rpc-timeout-sec", "7"]).unwrap();
    assert_eq!(options.rpc_timeout_sec, Some(7));
}

#[test]
fn rebuild_is_a_top_level_offline_run_view() {
    let options = Opts::try_parse_from([
        "tally",
        "rebuild",
        "00000000-0000-4000-8000-000000000043",
        "--json",
        "--state-dir",
        "/var/lib/tally/state",
        "--data-dir",
        "/var/lib/tally/data",
    ])
    .unwrap();
    assert!(matches!(
        options.command,
        Some(Command::Rebuild(RebuildArgs {
            id,
            json: true,
            state_dir: Some(state_dir),
            data_dir: Some(data_dir),
        })) if id == "00000000-0000-4000-8000-000000000043"
            && state_dir == Path::new("/var/lib/tally/state")
            && data_dir == Path::new("/var/lib/tally/data")
    ));
}

#[test]
fn campaign_arm_takes_repository_and_committed_worklist_identity() {
    let options = Opts::try_parse_from([
        "tally",
        "campaign",
        "arm",
        "acme/widgets",
        "specs/night/tasks.json",
        "--no-enqueue",
    ])
    .unwrap();
    assert!(matches!(
        options.command,
        Some(Command::Campaign {
            command: CampaignCommand::Arm(CampaignArmArgs {
                code_repository,
                worklist_pattern,
                checkout: None,
                base_branch,
                remote,
                no_enqueue: true,
                ..
            })
        }) if code_repository == "acme/widgets"
            && worklist_pattern == "specs/night/tasks.json"
            && base_branch == "main"
            && remote == "origin"
    ));
    assert!(Opts::try_parse_from([
        "tally",
        "campaign",
        "arm",
        "https://github.com/acme/widgets/issues/42",
    ])
    .is_err());

    let options = Opts::try_parse_from([
        "tally",
        "campaign",
        "arm",
        "acme/widgets",
        "specs/night/tasks.json",
        "--checkout",
        "/srv/widgets",
        "--base-branch",
        "stable",
        "--remote",
        "upstream",
    ])
    .unwrap();
    assert!(matches!(
        options.command,
        Some(Command::Campaign {
            command: CampaignCommand::Arm(CampaignArmArgs {
                checkout: Some(checkout),
                base_branch,
                remote,
                ..
            })
        }) if checkout == Path::new("/srv/widgets")
            && base_branch == "stable"
            && remote == "upstream"
    ));
}

#[test]
fn campaign_steer_supports_task_scope_and_the_ssh_stdin_contract() {
    let options = Opts::try_parse_from([
        "tally",
        "campaign",
        "steer",
        "acme/widgets",
        "specs/night/tasks.json",
        "--task",
        "local-steering-source",
        "--message-file",
        "-",
        "--state-dir",
        "/var/lib/tally/state",
    ])
    .unwrap();
    assert!(matches!(
        options.command,
        Some(Command::Campaign {
            command: CampaignCommand::Steer(CampaignSteerArgs {
                code_repository,
                worklist_pattern,
                task: Some(task),
                message: None,
                message_file: Some(message_file),
                state_dir: Some(state_dir),
            })
        }) if code_repository == "acme/widgets"
            && worklist_pattern == "specs/night/tasks.json"
            && task == "local-steering-source"
            && message_file == Path::new("-")
            && state_dir == Path::new("/var/lib/tally/state")
    ));
    assert!(Opts::try_parse_from([
        "tally",
        "campaign",
        "steer",
        "acme/widgets",
        "specs/night/tasks.json",
    ])
    .is_err());
}

#[test]
fn campaign_inbox_reads_one_identity_and_nothing_else() {
    let options = Opts::try_parse_from([
        "tally",
        "campaign",
        "inbox",
        "acme/widgets",
        "specs/night/tasks.json",
        "--json",
        "--state-dir",
        "/var/lib/tally/state",
    ])
    .unwrap();
    assert!(matches!(
        options.command,
        Some(Command::Campaign {
            command: CampaignCommand::Inbox(CampaignInboxArgs {
                code_repository,
                worklist_pattern,
                json: true,
                state_dir: Some(state_dir),
            })
        }) if code_repository == "acme/widgets"
            && worklist_pattern == "specs/night/tasks.json"
            && state_dir == Path::new("/var/lib/tally/state")
    ));
    // The identity is the whole argument surface: the inbox is campaign
    // scoped, so half an identity addresses nothing.
    assert!(Opts::try_parse_from(["tally", "campaign", "inbox", "acme/widgets"]).is_err());
}

#[test]
fn campaign_resume_is_not_a_command() {
    let error = Opts::try_parse_from([
        "tally",
        "campaign",
        "resume",
        "acme/widgets",
        "specs/night/tasks.json",
    ])
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("unrecognized subcommand 'resume'"));
}

#[test]
fn campaign_status_accepts_the_campaign_identity_and_machine_output() {
    let options = Opts::try_parse_from([
        "tally",
        "campaign",
        "status",
        "acme/widgets",
        "specs/night/tasks.json",
        "--json",
        "--state-dir",
        "/var/lib/tally/state",
    ])
    .unwrap();
    assert!(matches!(
        options.command,
        Some(Command::Campaign {
            command: CampaignCommand::Status(CampaignStatusArgs {
                code_repository,
                worklist_pattern,
                json: true,
                state_dir: Some(state_dir),
            })
        }) if code_repository == "acme/widgets"
            && worklist_pattern == "specs/night/tasks.json"
            && state_dir == Path::new("/var/lib/tally/state")
    ));
}

#[test]
fn flow_run_jobs_lookup_conflicts_with_broad_archive_controls() {
    let flow_run = "00000000-0000-4000-8000-000000000415";
    assert!(Opts::try_parse_from(["tally", "query", "jobs", "--flow-run", flow_run]).is_ok());
    for archive_control in ["--archived", "--no-archived"] {
        let arguments = [
            "tally",
            "query",
            "jobs",
            "--flow-run",
            flow_run,
            archive_control,
        ];
        assert!(Opts::try_parse_from(arguments).is_err(), "{arguments:?}");
    }
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
    assert_eq!(exit_code("flow-run-superseded"), 20);
    // `SUPERSESSION_CODES` says of itself that `flow_error` maps exactly this
    // list to exit 20, and the exit map is a hand-written arm in another crate.
    // A sixth family member would otherwise inherit the fourteen-member details
    // contract and the doc's exit-20 promise while silently exiting 1.
    for code in tally_flow::SUPERSESSION_CODES {
        assert_eq!(exit_code(code), 20, "{code}");
    }
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
        "queue", "adapter", "witness", "lease", "daemon", "query", "flow",
    ] {
        assert!(help.contains(verb), "missing {verb} from help");
    }
    assert!(!help.contains("__record-unit-exit"));
}

#[test]
fn retired_top_level_spellings_are_refused_but_check_config_remains() {
    assert!(Opts::try_parse_from(["tally", "--mode", "daemon"]).is_err());
    assert!(Opts::try_parse_from(["tally", "enqueue", "--pool", "gpu", "--", "true"]).is_err());
    assert!(matches!(
        Opts::try_parse_from([
            "tally",
            "--mode",
            "check-config",
            "--config",
            "/etc/tally/config.json",
        ]),
        Ok(Opts {
            mode: Some(Mode::CheckConfig),
            command: None,
            ..
        })
    ));
}

#[test]
fn adapter_smoke_cli_defaults_and_overrides_are_stable() {
    let defaults = Opts::try_parse_from(["tally", "adapter", "smoke", "codex"]).unwrap();
    assert!(matches!(
        defaults.command,
        Some(Command::Adapter {
            command: AdapterCommand::Smoke(AdapterSmokeArgs {
                name,
                cwd: None,
                prompt: None,
                pool: None,
                sandbox: None,
                approval_policy: None,
                assert_commit: false,
                state_dir: None,
                probe_root: None,
            })
        }) if name == "codex"
    ));

    let overridden = Opts::try_parse_from([
        "tally",
        "adapter",
        "smoke",
        "pi",
        "--cwd",
        "worktree",
        "--prompt",
        "-answer briefly",
        "--pool",
        "agent-slot",
    ])
    .unwrap();
    assert!(matches!(
        overridden.command,
        Some(Command::Adapter {
            command: AdapterCommand::Smoke(AdapterSmokeArgs {
                name,
                cwd: Some(cwd),
                prompt: Some(prompt),
                pool: Some(pool),
                sandbox: None,
                approval_policy: None,
                assert_commit: false,
                state_dir: None,
                probe_root: None,
            })
        }) if name == "pi"
            && cwd == Path::new("worktree")
            && prompt == "-answer briefly"
            && pool == "agent-slot"
    ));

    let probing = Opts::try_parse_from([
        "tally",
        "adapter",
        "smoke",
        "codex",
        "--sandbox",
        "danger-full-access",
        "--approval-policy",
        "never",
        "--assert-commit",
        "--probe-root",
        "/var/lib/tally/campaigns",
    ])
    .unwrap();
    assert!(matches!(
        probing.command,
        Some(Command::Adapter {
            command: AdapterCommand::Smoke(AdapterSmokeArgs {
                name,
                cwd: None,
                prompt: None,
                pool: None,
                sandbox: Some(sandbox),
                approval_policy: Some(approval),
                assert_commit: true,
                state_dir: None,
                probe_root: Some(probe_root),
            })
        }) if name == "codex"
            && sandbox == "danger-full-access"
            && approval == "never"
            && probe_root == Path::new("/var/lib/tally/campaigns")
    ));

    // A probe root without a probe would silently do nothing.
    assert!(Opts::try_parse_from([
        "tally",
        "adapter",
        "smoke",
        "codex",
        "--probe-root",
        "/var/lib/tally/campaigns",
    ])
    .is_err());

    // A throwaway repository is the probe's whole point; naming another
    // directory would silently commit into it.
    assert!(Opts::try_parse_from([
        "tally",
        "adapter",
        "smoke",
        "codex",
        "--assert-commit",
        "--cwd",
        "worktree",
    ])
    .is_err());

    // `--state-dir` names the directory the default probe root derives from,
    // which is the same derivation `tally gc --state-dir` sweeps. Handing both
    // commands one directory is the only way a retained probe is ever reaped.
    let state_scoped = Opts::try_parse_from([
        "tally",
        "adapter",
        "smoke",
        "codex",
        "--assert-commit",
        "--state-dir",
        "/var/lib/tally/state",
    ])
    .unwrap();
    assert!(matches!(
        state_scoped.command,
        Some(Command::Adapter {
            command: AdapterCommand::Smoke(AdapterSmokeArgs {
                assert_commit: true,
                state_dir: Some(state_dir),
                probe_root: None,
                ..
            })
        }) if state_dir == Path::new("/var/lib/tally/state")
    ));

    // Naming both a state directory to derive the root from and the root
    // outright is a contradiction, not a precedence puzzle.
    assert!(Opts::try_parse_from([
        "tally",
        "adapter",
        "smoke",
        "codex",
        "--assert-commit",
        "--state-dir",
        "/var/lib/tally/state",
        "--probe-root",
        "/var/lib/tally/campaigns",
    ])
    .is_err());
}

#[test]
fn adapter_parity_cli_shape_and_its_hidden_probe_verbs_are_stable() {
    let defaults = Opts::try_parse_from(["tally", "adapter", "parity"]).unwrap();
    assert!(matches!(
        defaults.command,
        Some(Command::Adapter {
            command: AdapterCommand::Parity(AdapterParityArgs {
                adapter,
                pool: None,
                state_dir: None,
                probe_root: None,
            })
        }) if adapter == "shell"
    ));

    let overridden = Opts::try_parse_from([
        "tally",
        "adapter",
        "parity",
        "--adapter",
        "codex",
        "--pool",
        "build",
        "--state-dir",
        "/var/lib/tally/state",
    ])
    .unwrap();
    assert!(matches!(
        overridden.command,
        Some(Command::Adapter {
            command: AdapterCommand::Parity(AdapterParityArgs {
                adapter,
                pool: Some(pool),
                state_dir: Some(state_dir),
                probe_root: None,
            })
        }) if adapter == "codex"
            && pool == "build"
            && state_dir == Path::new("/var/lib/tally/state")
    ));

    // The site derivation has one home: naming both a state directory and a
    // probe root would leave `tally gc` sweeping one of them and the probe
    // writing the other.
    assert!(Opts::try_parse_from([
        "tally",
        "adapter",
        "parity",
        "--state-dir",
        "/var/lib/tally/state",
        "--probe-root",
        "/var/lib/tally/state/adapter-smoke",
    ])
    .is_err());

    // Both halves run the same hidden probe; the side is explicit on the wire
    // rather than inferred from which process spawned it.
    let laned = Opts::try_parse_from([
        "tally",
        "__parity-probe",
        "--side",
        "laned",
        "--out",
        "/tmp/parity-report.json",
    ])
    .unwrap();
    assert!(matches!(
        laned.command,
        Some(Command::ParityProbe(ParityProbeArgs {
            side: ProbeSide::Laned,
            out: Some(out),
        })) if out == Path::new("/tmp/parity-report.json")
    ));
    assert!(matches!(
        Opts::try_parse_from(["tally", "__parity-probe"])
            .unwrap()
            .command,
        Some(Command::ParityProbe(ParityProbeArgs {
            side: ProbeSide::Bare,
            out: None,
        }))
    ));
    assert!(matches!(
        Opts::try_parse_from(["tally", "__parity-fail"])
            .unwrap()
            .command,
        Some(Command::ParityFail)
    ));

    // Machinery, not surface: an operator asks for the comparison and never
    // for one half of it.
    let help = Opts::command().render_long_help().to_string();
    assert!(!help.contains("__parity-probe"));
    assert!(!help.contains("__parity-fail"));
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
        Some(Command::RecordUnitExit(RecordUnitExitArgs { record, unit, systemctl }))
            if record.as_path() == Path::new("/tmp/exit.json")
                && unit == "tally-job-example.service"
                && systemctl.as_path() == Path::new("systemctl")
    ));
}

#[test]
fn hidden_producer_dispatch_parses_a_typed_observation() {
    let options = Opts::try_parse_from([
        "tally",
        "--config",
        "/tmp/config.json",
        "__producer-dispatch",
        "daily",
        "--event",
        r#"{"kind":"calendar"}"#,
        "--state-dir",
        "/tmp/state",
        "--data-dir",
        "/tmp/data",
    ])
    .unwrap();
    assert!(matches!(
        options.command,
        Some(Command::ProducerDispatch(ProducerDispatchArgs {
            producer,
            state_dir: Some(state_dir),
            data_dir,
            ..
        })) if producer == "daily"
            && state_dir == Path::new("/tmp/state")
            && data_dir == Path::new("/tmp/data")
    ));
}

#[test]
fn hidden_producer_dispatch_requires_the_data_directory() {
    // A fallback that wrote briefs into `<stateDir>/briefs` is the retired
    // split layout the retention sweep now drains as legacy. A direct call must
    // name the daemon data directory like the generated units do, and clap must
    // say which flag is missing.
    let error = Opts::try_parse_from([
        "tally",
        "--config",
        "/tmp/config.json",
        "__producer-dispatch",
        "daily",
        "--event",
        r#"{"kind":"calendar"}"#,
        "--state-dir",
        "/tmp/state",
    ])
    .expect_err("__producer-dispatch must not fall back to the state directory");
    assert_eq!(
        error.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
    assert!(
        error.to_string().contains("--data-dir"),
        "error must name the missing flag: {error}"
    );
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
        (WireErrorCode::PreLaunchRefusal, 1),
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
fn queue_enqueue_accepts_direct_argv_or_invocation() {
    let direct = Opts::try_parse_from([
        "tally",
        "queue",
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
        "queue",
        "enqueue",
        "--pool",
        "gpu",
        "--dedup-key",
        "review:42",
        "--",
        "true",
    ])
    .unwrap();
    let Some(Command::Queue {
        command: QueueCommand::Enqueue(default),
    }) = default.command
    else {
        panic!("expected enqueue command");
    };
    assert_eq!(default.submission, CliSubmissionMode::Full);

    let legacy = Opts::try_parse_from([
        "tally",
        "queue",
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
    let Some(Command::Queue {
        command: QueueCommand::Enqueue(legacy),
    }) = legacy.command
    else {
        panic!("expected enqueue command");
    };
    assert_eq!(legacy.submission, CliSubmissionMode::Legacy);

    let invalid = Opts::try_parse_from([
        "tally",
        "queue",
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
fn flow_run_check_and_render_cli_shapes_match_the_declarative_contract() {
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
                args_path: None,
                catalog: Some(catalog),
            })
        }) if script == Path::new("/nix/store/example-flow.js")
            && args == json!({"task": "ship"})
            && catalog == Path::new("/nix/store/catalog.json")
    ));

    let render =
        Opts::try_parse_from(["tally", "flow", "render", "/nix/store/example-flow.js"]).unwrap();
    assert!(matches!(
        render.command,
        Some(Command::Flow {
            command: FlowCommand::Render(FlowRenderArgs { script })
        }) if script == Path::new("/nix/store/example-flow.js")
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

    let path_inputs = Opts::try_parse_from([
        "tally",
        "flow",
        "run",
        "/nix/store/example-flow.js",
        "--args-from-brief",
    ])
    .unwrap();
    assert!(matches!(
        path_inputs.command,
        Some(Command::Flow {
            command: FlowCommand::Run(FlowRunArgs {
                args: None,
                args_path: None,
                args_from_brief: true,
                ..
            })
        })
    ));

    assert!(Opts::try_parse_from([
        "tally",
        "flow",
        "run",
        "/nix/store/example-flow.js",
        "--args",
        "{}",
        "--args-path",
        "/tmp/args.json",
    ])
    .is_err());

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
        "queue",
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
    let Some(Command::Queue {
        command: QueueCommand::Enqueue(args),
    }) = options.command
    else {
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
        "queue",
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
    let Some(Command::Queue {
        command: QueueCommand::Enqueue(args),
    }) = scalar.command
    else {
        panic!("expected enqueue command");
    };
    assert_eq!(args.evidence_class, Some(Value::from(-1)));
    assert_eq!(args.manifest_hash.as_deref(), Some("-opaque-manifest"));
}

#[test]
fn enqueue_wave_three_options_and_public_continuation_parse_directly() {
    let options = Opts::try_parse_from([
        "tally",
        "queue",
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
    let Some(Command::Queue {
        command: QueueCommand::Enqueue(args),
    }) = options.command
    else {
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
fn related_trigger_fallback_parses_strictly() {
    let fallback = Opts::try_parse_from([
        "tally",
        "queue",
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
    let Some(Command::Queue {
        command: QueueCommand::Enqueue(args),
    }) = fallback.command
    else {
        panic!("expected enqueue command");
    };
    let related = args.related_trigger.unwrap();
    assert_eq!(related.producer, "github");
    assert_eq!(related.event_id, "comment-42");
    assert_eq!(related.receipt_id.as_deref(), Some("receipt-42"));
}
