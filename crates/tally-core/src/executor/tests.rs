use std::ffi::CString;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;

use super::*;
use crate::taskdb::{
    GhContextSnapshot, GhItemState, GhItemType, GH_CONTEXT_SCHEMA_VERSION, GH_ORIGIN_SCHEMA_VERSION,
};

fn uuid(value: &str) -> Uuid {
    Uuid::parse_str(value).unwrap()
}

fn request() -> ExecutionRequest {
    ExecutionRequest {
        identity: ExecutionIdentity {
            job_id: uuid("00000000-0000-4000-8000-000000000001"),
            task_uuid: Some(uuid("00000000-0000-4000-8000-000000000002")),
            task_ref: None,
        },
        parent: Some(uuid("00000000-0000-4000-8000-000000000003")),
        pools: vec!["gpu".to_owned()],
        lease_epoch: 7,
        attempt: 1,
        priority: Priority::High,
        no_enqueue: true,
        argv: vec![
            "/bin/leaf".to_owned(),
            "two words".to_owned(),
            "$HOME".to_owned(),
            "$(touch /tmp/nope);%n".to_owned(),
            "--option-looking".to_owned(),
        ],
        yield_hook: Some(vec![
            "tally".to_owned(),
            "lease".to_owned(),
            "status".to_owned(),
        ]),
        tally_socket: Some("/run/user/1000/tally.sock".to_owned()),
        job_token: Some("ab".repeat(32)),
        environment: BTreeMap::from([("ADAPTER_COLOR".to_owned(), "never".to_owned())]),
        gh_origin: None,
        brief_hash: None,
        brief_path: None,
        brief_document: None,
        cwd: Some(PathBuf::from("/work tree")),
        workspace: None,
        gate_manifest: None,
        git_ai: None,
        exec_attestation: None,
        hardening: AdapterHardening::None,
        extra_writable_paths: Vec::new(),
        credentials: BTreeMap::from([
            ("alpha".to_owned(), PathBuf::from("/run/keys/alpha")),
            ("zeta".to_owned(), PathBuf::from("/run/keys/zeta")),
        ]),
        limits: UnitLimits {
            cpu_weight: 250,
            memory_max_bytes: 1_073_741_824,
        },
        runtime_max_sec: Some(30),
    }
}

fn git_ai_execution() -> GitAiExecution {
    GitAiExecution {
        config: crate::config::GitAiConfig {
            enable: true,
            mode: crate::config::GitAiMode::Advisory,
            await_timeout_sec: 60,
            global_await_ok: true,
        },
        attributes: BTreeMap::from([
            ("adapter".to_owned(), "codex".to_owned()),
            ("attempt".to_owned(), "1".to_owned()),
            ("leaseEpoch".to_owned(), "7".to_owned()),
            (
                "taskUuid".to_owned(),
                "00000000-0000-4000-8000-000000000002".to_owned(),
            ),
        ]),
        expected_session: None,
        expected_model: None,
    }
}

fn gh_origin(item_type: GhItemType) -> GhOrigin {
    GhOrigin {
        schema_version: GH_ORIGIN_SCHEMA_VERSION,
        producer: "github".to_owned(),
        source: "notifications".to_owned(),
        repo: "acme/widgets".to_owned(),
        number: 77,
        html_url: match item_type {
            GhItemType::Issue => "https://github.com/acme/widgets/issues/77",
            GhItemType::PullRequest => "https://github.com/acme/widgets/pull/77",
        }
        .to_owned(),
        item_type: Some(item_type),
        head_sha: (item_type == GhItemType::PullRequest)
            .then(|| "7777777777777777777777777777777777777777".to_owned()),
        node_id: "I_kwDO_origin".to_owned(),
        item_author: "issue-author".to_owned(),
        trigger_actor: "trusted-maintainer".to_owned(),
        self_actor: "tally-bot".to_owned(),
        notification_reason: Some("mention".to_owned()),
        trigger_kind: "assignment".to_owned(),
        event_id: Some("notification-77".to_owned()),
        comment_id: None,
        trigger_timestamp: Some("2026-07-20T12:30:00Z".to_owned()),
        trigger_value: Some("tally-bot".to_owned()),
        context: Some(GhContextSnapshot {
            schema_version: GH_CONTEXT_SCHEMA_VERSION,
            title: "Untrusted title".to_owned(),
            body: "$(touch /tmp/must-not-run); ${SECRET}".to_owned(),
            state: Some(GhItemState::Open),
            head_sha: (item_type == GhItemType::PullRequest)
                .then(|| "7777777777777777777777777777777777777777".to_owned()),
            labels: vec!["build".to_owned()],
            assignees: vec!["tally-bot".to_owned()],
            triggering_comment: None,
        }),
        actor_exclude: "self".to_owned(),
        allow_self_triggered: false,
        allowed_actors: vec!["trusted-maintainer".to_owned()],
    }
}

#[derive(Debug, Clone, Copy)]
struct AbsentProbe;

impl LocalUnitProbe for AbsentProbe {
    fn inspect(&self, unit: &str, _paths: &ExecutionPaths) -> Result<LocalUnitFact, ExecutorError> {
        Ok(LocalUnitFact::absent(unit))
    }
}

fn executor(state_dir: &Path) -> Executor {
    Executor::new(state_dir, "/nix/store/example/bin/tally").with_unit_probe(AbsentProbe)
}

fn strings(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect()
}

fn ssh_config() -> SshExecutorConfig {
    SshExecutorConfig {
        host: "worker.example".to_owned(),
        user: "tally-worker".to_owned(),
        port: 2222,
        ssh_program: PathBuf::from("/run/current-system/sw/bin/ssh"),
        identity_file: PathBuf::from("/run/credentials/tally-worker-key"),
        known_hosts_file: PathBuf::from("/etc/tally/worker-known-hosts"),
        program: PathBuf::from("/run/current-system/sw/bin/tally"),
        state_dir: PathBuf::from("/var/lib/tally-remote"),
        connect_timeout_sec: 3,
        server_alive_interval_sec: 2,
        server_alive_count_max: 2,
        retry_interval_ms: 10,
    }
}

#[derive(Clone)]
struct ScriptedRemoteTransport {
    calls: Arc<Mutex<Vec<RemoteExecutorRequest>>>,
    replies:
        Arc<Mutex<std::collections::VecDeque<Result<RemoteExecutorReply, RemoteTransportError>>>>,
}

impl ScriptedRemoteTransport {
    fn new(
        replies: impl IntoIterator<Item = Result<RemoteExecutorReply, RemoteTransportError>>,
    ) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            replies: Arc::new(Mutex::new(replies.into_iter().collect())),
        }
    }
}

impl RemoteTransport for ScriptedRemoteTransport {
    fn call<'a>(
        &'a self,
        _config: &'a SshExecutorConfig,
        request: RemoteExecutorRequest,
    ) -> Pin<Box<dyn Future<Output = Result<RemoteExecutorReply, RemoteTransportError>> + Send + 'a>>
    {
        let calls = self.calls.clone();
        let replies = self.replies.clone();
        Box::pin(async move {
            calls.lock().unwrap().push(request);
            replies.lock().unwrap().pop_front().unwrap_or_else(|| {
                Err(RemoteTransportError {
                    detail: "scripted remote replies exhausted".to_owned(),
                })
            })
        })
    }
}

fn remote_executor(state_dir: &Path, transport: ScriptedRemoteTransport) -> Executor {
    Executor::new(state_dir, "/nix/store/example/bin/tally")
        .with_remote_executors(BTreeMap::from([(
            "worker".to_owned(),
            ExecutionTargetConfig::Ssh(ssh_config()),
        )]))
        .with_remote_transport(transport)
}

fn remote_completion(request: &ExecutionRequest, stdout: &[u8]) -> RemoteCompletion {
    let unit = format!("tally-job-{}.service", request.identity.unit_uuid());
    let record = UnitExitRecord {
        accounting: None,
        schema_version: UNIT_EXIT_SCHEMA_VERSION,
        unit: unit.clone(),
        invocation_id: "remote-invocation".to_owned(),
        attempt: request.attempt,
        lease_epoch: request.lease_epoch,
        service_result: "success".to_owned(),
        exit_code: Some("exited".to_owned()),
        exit_status: Some("0".to_owned()),
    };
    let evidence = parse_evidence_specs(&["exit:0".to_owned()]).unwrap();
    RemoteCompletion {
        unit,
        record,
        termination: ExecutionTermination::Exited(0),
        capture: RemoteCapture {
            attempt: request.attempt,
            lease_epoch: request.lease_epoch,
            stdout_base64: Some(encode_base64(stdout)),
            stderr_base64: Some(encode_base64(b"")),
            error: None,
        },
        evidence_gate: Some(run_evidence_gate(RunOutcome {
            exit_code: 0,
            wall_clock_seconds: 1.0,
            evidence: &evidence,
        })),
        semantic_completion: None,
        result_revision: None,
        authorship: None,
        authorship_sessions: None,
        host_id: Some("worker.example".to_owned()),
    }
}

#[test]
fn ssh_transport_is_fixed_and_never_contains_workload_argv() {
    let config = ssh_config();
    let args = strings(&build_ssh_argv(&config));
    assert_eq!(
        &args[args.len() - 4..],
        [
            "--",
            "tally-worker@worker.example",
            "/run/current-system/sw/bin/tally",
            "__remote-executor",
        ]
    );
    for required in [
        "BatchMode=yes",
        "PasswordAuthentication=no",
        "KbdInteractiveAuthentication=no",
        "IdentitiesOnly=yes",
        "IdentityAgent=none",
        "StrictHostKeyChecking=yes",
        "UserKnownHostsFile=/etc/tally/worker-known-hosts",
        "GlobalKnownHostsFile=/dev/null",
        "ClearAllForwardings=yes",
        "ForwardAgent=no",
        "ForwardX11=no",
        "ProxyCommand=none",
    ] {
        assert!(args.contains(&required.to_owned()), "missing {required}");
    }
    for workload_argument in &request().argv {
        assert!(
            !args.contains(workload_argument),
            "workload argv leaked into the SSH command: {workload_argument:?}"
        );
    }
}

#[test]
fn remote_capture_base64_is_canonical_and_bounded() {
    for bytes in [
        b"".as_slice(),
        b"f".as_slice(),
        b"fo".as_slice(),
        b"foo".as_slice(),
        &[0, 1, 2, 253, 254, 255],
    ] {
        let encoded = encode_base64(bytes);
        assert_eq!(decode_base64(&encoded).unwrap(), bytes);
    }
    for invalid in ["A===", "Zh==", "Zm9=", "Zm=v", "!!!!", "Zg==AAAA"] {
        assert!(decode_base64(invalid).is_err(), "accepted {invalid:?}");
    }
}

#[tokio::test]
async fn transport_loss_retries_the_same_ensure_without_relaunching() {
    let temp = tempfile::tempdir().unwrap();
    let request = request();
    let completion = remote_completion(&request, b"completed remotely\n");
    let transport = ScriptedRemoteTransport::new([
        Err(RemoteTransportError {
            detail: "connection reset after dispatch".to_owned(),
        }),
        Ok(RemoteExecutorReply::Ok {
            protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
            result: Box::new(RemoteExecutorResult::Completion(Box::new(
                completion.clone(),
            ))),
        }),
    ]);
    let calls = transport.calls.clone();
    let executor = remote_executor(temp.path(), transport);
    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        executor.execute_on(Some("worker"), request.clone(), vec!["exit:0".to_owned()]),
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(outcome.backend, ExecutionBackend::Remote);
    assert_eq!(outcome.record, completion.record);
    assert_eq!(
        std::fs::read(outcome.paths.stdout).unwrap(),
        b"completed remotely\n"
    );
    assert!(outcome.evidence_gate.unwrap().passed);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
    assert!(matches!(calls[0], RemoteExecutorRequest::Ensure { .. }));
}

#[tokio::test]
async fn durable_launch_marker_blocks_replay_after_worker_loss() {
    let temp = tempfile::tempdir().unwrap();
    let request = request();
    let executor = executor(temp.path());
    let paths = executor.paths(&request.identity);
    write_capture_generation(
        &paths.capture_generation,
        CaptureGeneration {
            attempt: request.attempt,
            lease_epoch: request.lease_epoch,
        },
    )
    .unwrap();

    let error = executor.execute(request.clone()).await.unwrap_err();
    assert!(matches!(
        error,
        ExecutorError::IndeterminatePriorLaunch {
            attempt,
            lease_epoch,
            ..
        } if attempt == request.attempt && lease_epoch == request.lease_epoch
    ));
    assert!(!paths.stdout.exists());
    assert!(!paths.stderr.exists());
    assert!(!paths.exit_record.exists());
}

#[tokio::test]
async fn restart_probe_and_adoption_survive_worker_loss() {
    let temp = tempfile::tempdir().unwrap();
    let request = request();
    let completion = remote_completion(&request, b"adopted\n");
    let fact = LocalUnitFact {
        unit: completion.unit.clone(),
        loaded: true,
        state: LocalUnitState::Running,
        invocation_id: Some(completion.record.invocation_id.clone()),
        attempt: Some(request.attempt),
        lease_epoch: Some(request.lease_epoch),
        exit_record: None,
    };
    let transport = ScriptedRemoteTransport::new([
        Ok(RemoteExecutorReply::Ok {
            protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
            result: Box::new(RemoteExecutorResult::Fact(fact.clone())),
        }),
        Err(RemoteTransportError {
            detail: "worker temporarily offline".to_owned(),
        }),
        Ok(RemoteExecutorReply::Ok {
            protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
            result: Box::new(RemoteExecutorResult::Completion(Box::new(completion))),
        }),
    ]);
    let calls = transport.calls.clone();
    let executor = remote_executor(temp.path(), transport);

    assert_eq!(
        executor
            .inspect_identity_on(Some("worker"), &request.identity)
            .await
            .unwrap(),
        fact
    );
    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        executor.adopt_on(
            Some("worker"),
            request,
            "remote-invocation",
            vec!["exit:0".to_owned()],
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(outcome.backend, ExecutionBackend::Remote);

    let calls = calls.lock().unwrap();
    assert!(matches!(calls[0], RemoteExecutorRequest::Probe { .. }));
    assert_eq!(calls[1], calls[2]);
    assert!(matches!(calls[1], RemoteExecutorRequest::Adopt { .. }));
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, RemoteExecutorRequest::Ensure { .. })),
        "restart adoption must never issue a fresh launch"
    );
}

#[tokio::test]
async fn malformed_remote_completion_is_a_fail_closed_protocol_error() {
    let temp = tempfile::tempdir().unwrap();
    let request = request();
    let mut completion = remote_completion(&request, b"");
    completion.capture.attempt += 1;
    let transport = ScriptedRemoteTransport::new([Ok(RemoteExecutorReply::Ok {
        protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
        result: Box::new(RemoteExecutorResult::Completion(Box::new(completion))),
    })]);
    let error = remote_executor(temp.path(), transport)
        .execute_on(Some("worker"), request, vec!["exit:0".to_owned()])
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ExecutorError::RemoteProtocol { executor, .. } if executor == "worker"
    ));
}

#[tokio::test]
async fn remote_reclaim_retries_the_exact_invocation_and_generation() {
    let temp = tempfile::tempdir().unwrap();
    let request = request();
    let transport = ScriptedRemoteTransport::new([
        Err(RemoteTransportError {
            detail: "worker disappeared during stop".to_owned(),
        }),
        Ok(RemoteExecutorReply::Ok {
            protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
            result: Box::new(RemoteExecutorResult::Reclaimed(RemoteCapture {
                attempt: request.attempt,
                lease_epoch: request.lease_epoch,
                stdout_base64: Some(String::new()),
                stderr_base64: Some(String::new()),
                error: None,
            })),
        }),
    ]);
    let calls = transport.calls.clone();
    let executor = remote_executor(temp.path(), transport);
    tokio::time::timeout(
        Duration::from_secs(1),
        executor.reclaim_identity_exact_on(
            Some("worker"),
            &request.identity,
            Some("remote-invocation"),
            request.attempt,
            request.lease_epoch,
        ),
    )
    .await
    .unwrap()
    .unwrap();

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
    assert!(matches!(
        &calls[0],
        RemoteExecutorRequest::Reclaim {
            expected_invocation_id: Some(invocation_id),
            attempt: 1,
            lease_epoch: 7,
            ..
        } if invocation_id == "remote-invocation"
    ));
}

#[test]
fn worker_reclaim_pins_observed_generation_before_stopping() {
    let request = request();
    let unit = format!("tally-job-{}.service", request.identity.unit_uuid());
    let mut fact = LocalUnitFact {
        unit,
        loaded: true,
        state: LocalUnitState::Running,
        invocation_id: Some("observed-invocation".to_owned()),
        attempt: Some(request.attempt),
        lease_epoch: Some(request.lease_epoch),
        exit_record: None,
    };
    assert_eq!(
        pin_remote_reclaim(&fact, None, request.attempt, request.lease_epoch).unwrap(),
        Some("observed-invocation".to_owned())
    );
    assert!(matches!(
        pin_remote_reclaim(
            &fact,
            Some("replacement-invocation"),
            request.attempt,
            request.lease_epoch,
        ),
        Err(ExecutorError::AdoptedInvocationMismatch { .. })
    ));
    fact.attempt = Some(request.attempt + 1);
    assert!(matches!(
        pin_remote_reclaim(&fact, None, request.attempt, request.lease_epoch),
        Err(ExecutorError::AdoptedGenerationMismatch { .. })
    ));
}

#[test]
fn systemd_argv_is_direct_stable_and_complete() {
    let request = request();
    let args = strings(
        &executor(Path::new("/state tree"))
            .build_systemd_argv(&request)
            .unwrap(),
    );
    assert_eq!(
        &args[..7],
        [
            "--user",
            "--wait",
            "--collect",
            "--unit",
            "tally-job-00000000-0000-4000-8000-000000000002",
            "--quiet",
            "--expand-environment=no",
        ]
    );
    for property in [
        "Type=exec",
        "CPUWeight=250",
        "MemoryMax=1073741824",
        "RuntimeMaxSec=30s",
        "StandardOutput=append:/state tree/capture/00000000-0000-4000-8000-000000000002.out",
        "StandardError=append:/state tree/capture/00000000-0000-4000-8000-000000000002.adapter.err",
        "LoadCredential=alpha:/run/keys/alpha",
        "LoadCredential=zeta:/run/keys/zeta",
    ] {
        assert!(args.windows(2).any(|pair| pair == ["--property", property]));
    }
    let exec_stop = args
        .windows(2)
        .find(|pair| pair[0] == "--property" && pair[1].starts_with("ExecStopPost="))
        .unwrap();
    assert!(exec_stop[1].starts_with("ExecStopPost=:"));
    assert!(exec_stop[1].contains("__record-unit-exit"));
    assert!(exec_stop[1].contains("/state tree/unit-exit/"));
    for environment in [
        "ADAPTER_COLOR=never",
        "TALLY_JOB_ID=00000000-0000-4000-8000-000000000001",
        "TALLY_TASK_UUID=00000000-0000-4000-8000-000000000002",
        "TALLY_PARENT=00000000-0000-4000-8000-000000000003",
        "TALLY_POOL=gpu",
        "TALLY_LEASE_EPOCH=7",
        "TALLY_ATTEMPT=1",
        "TALLY_CLASS=high",
        "TALLY_NO_ENQUEUE=1",
        "TALLY_CREDENTIALS=[\"alpha\",\"zeta\"]",
        "TALLY_YIELD_HOOK=[\"tally\",\"lease\",\"status\"]",
        "TALLY_SOCKET=/run/user/1000/tally.sock",
        "TALLY_JOB_TOKEN=abababababababababababababababababababababababababababababababab",
    ] {
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--setenv", environment]));
    }
    let separator = args.iter().rposition(|argument| argument == "--").unwrap();
    assert_eq!(&args[separator + 1..], request.argv);
    let joined = args.join("\n");
    for forbidden in ["DeviceMemoryMax", "Delegate=", "dmem", "servingSlice"] {
        assert!(!joined.contains(forbidden));
    }
}

#[test]
fn campaign_task_ref_names_the_unit_captures_gate_and_child_environment() {
    let mut request = request();
    request.identity.task_ref = Some(TaskRef::new("crm/t07").unwrap());
    let executor = executor(Path::new("/state"));
    let uuid = request.identity.unit_uuid();

    assert_eq!(
        executor.unit_name(&request.identity),
        format!("tally-job-crm-t07-{uuid}.service")
    );
    let paths = executor.paths(&request.identity);
    assert_eq!(
        paths.stdout,
        PathBuf::from(format!("/state/capture/{uuid}.t07.out"))
    );
    assert_eq!(
        paths.stderr,
        PathBuf::from(format!("/state/capture/{uuid}.t07.adapter.err"))
    );
    assert_eq!(
        paths.failure_stderr,
        PathBuf::from(format!("/state/capture/{uuid}.t07.err"))
    );
    assert_eq!(
        executor
            .default_gate_manifest_on(None, &request.identity, request.attempt)
            .unwrap()
            .path,
        PathBuf::from(format!("/state/capture/{uuid}.t07.attempt-1.gates.json"))
    );

    let args = strings(&executor.build_systemd_argv(&request).unwrap());
    assert!(args.contains(&format!("tally-job-crm-t07-{uuid}")));
    assert!(args.contains(&"TALLY_TASK_REF=crm/t07".to_owned()));
}

#[test]
fn exec_attestation_wrapper_is_argv_safe_and_preserves_the_exact_child() {
    let mut request = request();
    let child = request.argv.clone();
    request.exec_attestation = Some(ExecAttestationContext {
        adapter: "codex".to_owned(),
        executor: Some("worker-1".to_owned()),
        payload_hash: Some(format!("sha256:{}", "a".repeat(64))),
        brief_hash: Some(format!("sha256:{}", "b".repeat(64))),
        evidence: vec![
            "exit:0".to_owned(),
            "artifact:/work tree/result.json".to_owned(),
        ],
    });
    let args = strings(
        &executor(Path::new("/state tree"))
            .build_systemd_argv(&request)
            .unwrap(),
    );
    let systemd_separator = args.iter().position(|argument| argument == "--").unwrap();
    assert_eq!(
        &args[systemd_separator + 1..systemd_separator + 4],
        ["/nix/store/example/bin/tally", "attest", "exec"]
    );
    for pair in [
        ["--task-uuid", "00000000-0000-4000-8000-000000000002"],
        ["--attempt", "1"],
        ["--lease-epoch", "7"],
        ["--adapter", "codex"],
        ["--executor", "worker-1"],
        [
            "--payload-hash",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ],
        [
            "--brief-hash",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ],
        ["--ledger", "/state tree/exec-attestations.jsonl"],
    ] {
        assert!(args.windows(2).any(|window| window == pair));
    }
    assert!(args
        .windows(2)
        .any(|window| { window == ["--evidence", "artifact:/work tree/result.json"] }));
    let child_separator = args.iter().rposition(|argument| argument == "--").unwrap();
    assert!(child_separator > systemd_separator);
    assert_eq!(&args[child_separator + 1..], child);
}

#[test]
fn hardening_preset_names_stamp_only_the_normative_property_bundles() {
    let executor = executor(Path::new("/state tree"));
    let properties = |request: &ExecutionRequest| {
        let mut args = Vec::new();
        executor
            .push_hardening_properties(&mut args, request)
            .unwrap();
        strings(&args)
            .chunks_exact(2)
            .map(|pair| {
                assert_eq!(pair[0], "--property");
                pair[1].clone()
            })
            .collect::<Vec<_>>()
    };

    let mut strict = request();
    strict.hardening = AdapterHardening::Strict;
    strict.workspace = Some(WorkspaceMetadata {
        repo: "acme/widgets".to_owned(),
        base_rev: "origin/main".to_owned(),
        branch: "tally/work".to_owned(),
        worktree_path: PathBuf::from("/work tree"),
    });
    assert_eq!(
        properties(&strict),
        [
            "ProtectHome=read-only",
            "PrivateTmp=yes",
            "ProtectSystem=strict",
            "NoNewPrivileges=yes",
            "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6",
            "ReadWritePaths=\"/work tree\" \"/state tree/unit-exit\" \"/state tree/capture/00000000-0000-4000-8000-000000000002.out\" \"/state tree/capture/00000000-0000-4000-8000-000000000002.adapter.err\"",
        ]
    );

    let mut production = strict.clone();
    production.hardening = AdapterHardening::Production;
    assert_eq!(
        properties(&production),
        [
            "ProtectHome=read-only",
            "PrivateTmp=yes",
            "ProtectSystem=strict",
            "NoNewPrivileges=yes",
            "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6",
            "PrivateDevices=yes",
            "ProtectKernelTunables=yes",
            "ProtectKernelModules=yes",
            "ProtectKernelLogs=yes",
            "ProtectControlGroups=yes",
            "ProtectClock=yes",
            "RestrictSUIDSGID=yes",
            "LockPersonality=yes",
            "RestrictRealtime=yes",
            "SystemCallFilter=@system-service",
            "CapabilityBoundingSet=",
            "ProtectProc=invisible",
            "ReadWritePaths=\"/work tree\" \"/state tree/unit-exit\" \"/state tree/capture/00000000-0000-4000-8000-000000000002.out\" \"/state tree/capture/00000000-0000-4000-8000-000000000002.adapter.err\"",
        ]
    );

    let mut workspace = request();
    workspace.hardening = AdapterHardening::Workspace;
    assert_eq!(
        properties(&workspace),
        ["PrivateTmp=yes", "ReadWritePaths=\"/state tree\""]
    );
    assert!(properties(&request()).is_empty());
}

#[test]
fn strict_writes_are_scoped_to_declared_execution_paths() {
    let executor = executor(Path::new("/state tree"));
    let mut strict = request();
    strict.hardening = AdapterHardening::Strict;
    strict.exec_attestation = Some(ExecAttestationContext {
        adapter: "codex".to_owned(),
        executor: None,
        payload_hash: None,
        brief_hash: None,
        evidence: vec!["exit:0".to_owned()],
    });
    strict.gh_origin = Some(gh_origin(GhItemType::Issue));
    strict.gate_manifest = Some(GateManifestSpec {
        path: PathBuf::from("/state tree/capture/gates.json"),
        required_gate_ids: vec!["tests".to_owned()],
        acceptance_policy: AcceptancePolicy::ExecutionAndGates,
    });
    strict.extra_writable_paths = vec![
        PathBuf::from("/home/agent/.codex"),
        PathBuf::from("/home/agent/.codex"),
    ];
    let args = strings(&executor.build_systemd_argv(&strict).unwrap());
    let writable = args
        .windows(2)
        .find(|pair| pair[0] == "--property" && pair[1].starts_with("ReadWritePaths="))
        .unwrap()[1]
        .clone();
    assert_eq!(
        writable,
        "ReadWritePaths=\"/state tree/unit-exit\" \"/state tree/capture/00000000-0000-4000-8000-000000000002.out\" \"/state tree/capture/00000000-0000-4000-8000-000000000002.adapter.err\" \"/state tree/exec-attestations.jsonl\" \"/state tree/github-context/00000000-0000-4000-8000-000000000002.json\" \"/state tree/capture/gates.json\" \"/home/agent/.codex\""
    );

    strict.extra_writable_paths = vec![PathBuf::from("relative/path")];
    assert!(matches!(
        executor.build_systemd_argv(&strict),
        Err(ExecutorError::InvalidRequest(detail))
            if detail == "extra writable path relative/path must be absolute"
    ));
}

#[test]
fn git_ai_hardening_grants_the_linked_worktree_common_git_directory() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repository");
    let worktree = temp.path().join("linked-worktree");
    std::fs::create_dir(&repository).unwrap();
    let git = |cwd: &Path, args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&repository, &["init", "-q"]);
    git(&repository, &["config", "user.name", "Tally Test"]);
    git(
        &repository,
        &["config", "user.email", "tally@example.invalid"],
    );
    std::fs::write(repository.join("file"), "initial\n").unwrap();
    git(&repository, &["add", "file"]);
    git(&repository, &["commit", "-q", "-m", "initial"]);
    git(
        &repository,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "tally-linked-test",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );

    let mut enabled = request();
    enabled.hardening = AdapterHardening::Strict;
    enabled.workspace = Some(WorkspaceMetadata {
        repo: "acme/widgets".to_owned(),
        base_rev: "HEAD".to_owned(),
        branch: "tally-linked-test".to_owned(),
        worktree_path: worktree.clone(),
    });
    enabled.git_ai = Some(git_ai_execution());
    let args = strings(
        &executor(&temp.path().join("state"))
            .build_systemd_argv(&enabled)
            .unwrap(),
    );
    let writable = args
        .windows(2)
        .find(|pair| pair[0] == "--property" && pair[1].starts_with("ReadWritePaths="))
        .unwrap()[1]
        .clone();
    assert!(writable.contains(worktree.to_str().unwrap()));
    assert!(writable.contains(repository.join(".git").to_str().unwrap()));
    assert!(writable.contains(".git/worktrees/linked-worktree"));
}

#[test]
fn gate_manifest_path_is_exported_or_scrubbed_and_defaults_per_target() {
    let local = executor(Path::new("/coordinator-state"));
    let mut declared = request();
    declared.gate_manifest = Some(GateManifestSpec {
        path: PathBuf::from("/work/gates.json"),
        required_gate_ids: Vec::new(),
        acceptance_policy: AcceptancePolicy::Manual,
    });
    let environment = execution_environment(&declared, None).unwrap();
    assert!(environment
        .iter()
        .any(|(name, value)| { name == "TALLY_GATE_MANIFEST" && value == "/work/gates.json" }));
    assert!(!environment_to_unset(&declared).contains(&"TALLY_GATE_MANIFEST"));
    assert!(environment_to_unset(&request()).contains(&"TALLY_GATE_MANIFEST"));

    let local_default = local
        .default_gate_manifest_on(None, &declared.identity, 3)
        .unwrap();
    assert_eq!(
        local_default.path,
        PathBuf::from(format!(
            "/coordinator-state/capture/{}.attempt-3.gates.json",
            declared.identity.unit_uuid()
        ))
    );
    assert!(local_default.required_gate_ids.is_empty());
    assert_eq!(local_default.acceptance_policy, AcceptancePolicy::Manual);

    let remote = local.with_remote_executors(BTreeMap::from([(
        "worker".to_owned(),
        ExecutionTargetConfig::Ssh(ssh_config()),
    )]));
    let remote_default = remote
        .default_gate_manifest_on(Some("worker"), &declared.identity, 4)
        .unwrap();
    assert_eq!(
        remote_default.path,
        PathBuf::from(format!(
            "/var/lib/tally-remote/capture/{}.attempt-4.gates.json",
            declared.identity.unit_uuid()
        ))
    );
}

#[test]
fn execution_environment_preserves_scalar_compatibility_and_encodes_multi_pool_sets() {
    let singleton = request();
    let singleton_environment = execution_environment(&singleton, None).unwrap();
    assert!(singleton_environment
        .iter()
        .any(|(name, value)| name == "TALLY_POOL" && value == "gpu"));

    let mut multi = request();
    multi.pools = vec!["alpha".to_owned(), "zeta".to_owned()];
    let multi_environment = execution_environment(&multi, None).unwrap();
    assert!(multi_environment
        .iter()
        .any(|(name, value)| { name == "TALLY_POOL" && value == r#"["alpha","zeta"]"# }));
}

#[test]
fn git_ai_custom_attributes_are_exact_and_disabled_integration_is_absent() {
    let mut enabled = request();
    enabled.environment.insert(
        "GIT_AI_CUSTOM_ATTRIBUTES".to_owned(),
        r#"{"spoofed":"value"}"#.to_owned(),
    );
    enabled.git_ai = Some(git_ai_execution());
    let environment = execution_environment(&enabled, None).unwrap();
    assert_eq!(
        environment
            .iter()
            .rev()
            .find(|(name, _)| name == "GIT_AI_CUSTOM_ATTRIBUTES")
            .map(|(_, value)| value.as_str()),
        Some(
            r#"{"adapter":"codex","attempt":"1","leaseEpoch":"7","taskUuid":"00000000-0000-4000-8000-000000000002"}"#
        )
    );
    assert!(!environment_to_unset(&enabled).contains(&"GIT_AI_CUSTOM_ATTRIBUTES"));

    let disabled = request();
    assert!(execution_environment(&disabled, None)
        .unwrap()
        .iter()
        .all(|(name, _)| name != "GIT_AI_CUSTOM_ATTRIBUTES"));
    assert!(environment_to_unset(&disabled).contains(&"GIT_AI_CUSTOM_ATTRIBUTES"));
    assert!(serde_json::to_value(&disabled)
        .unwrap()
        .get("gitAi")
        .is_none());
}

#[test]
fn private_git_ai_runtime_routes_only_the_job_to_its_control_and_trace_sockets() {
    let temp = tempfile::tempdir().unwrap();
    let state_dir = temp.path().join("state");
    let executor = executor(&state_dir);
    let mut enabled = request();
    enabled.git_ai = Some(git_ai_execution());
    let runtime = git_ai::private_daemon_paths(
        Path::new("/opt/dotfiles/bin/git-ai"),
        "1.6.17",
        &state_dir,
        "task-53:1:7",
        Path::new("/run/current-system/sw/bin/systemctl"),
    )
    .unwrap();
    let expected = runtime
        .child_environment()
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let args = strings(
        &executor
            .build_systemd_argv_with_git_ai(&enabled, Some(&runtime))
            .unwrap(),
    );
    for (name, value) in expected {
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--setenv" && pair[1] == format!("{name}={value}")),
            "job unit omitted {name}"
        );
    }
    std::mem::forget(runtime);
}

#[test]
fn transported_brief_materializes_privately_and_provisions_exact_path() {
    let temp = tempfile::tempdir().unwrap();
    let state_dir = temp.path().join("remote-state");
    let executor = executor(&state_dir);
    let document = serde_json::json!({
        "mission": "execute remotely",
        "acceptance": ["TALLY_BRIEF is durable"]
    });
    let prepared = PreparedBrief::from_value(document.clone()).unwrap();
    let mut request = request();
    request.brief_hash = Some(prepared.hash().to_owned());
    request.brief_document = Some(document);

    executor.materialize_brief(&mut request).unwrap();
    assert!(request.brief_document.is_none());
    let path = request.brief_path.as_ref().unwrap();
    assert_eq!(
        path,
        &brief::content_path(&state_dir, prepared.hash()).unwrap()
    );
    assert_eq!(
        brief::read_verified(path, prepared.hash()).unwrap(),
        prepared
    );
    assert_eq!(
        std::fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let environment = execution_environment(&request, None).unwrap();
    assert!(environment
        .iter()
        .any(|(name, value)| name == "TALLY_BRIEF" && value == &path.to_string_lossy()));
    assert!(environment
        .iter()
        .any(|(name, value)| name == "TALLY_BRIEF_HASH" && value == prepared.hash()));
}

#[test]
fn github_origin_materializes_private_context_and_exact_identity_environment() {
    let temp = tempfile::tempdir().unwrap();
    let state_dir = temp.path().join("state");
    let executor = executor(&state_dir);
    let mut request = request();
    request.gh_origin = Some(gh_origin(GhItemType::Issue));
    let original_argv = request.argv.clone();

    let context_path = executor.materialize_gh_context(&request).unwrap().unwrap();
    let environment = execution_environment(&request, Some(&context_path)).unwrap();
    let github = environment
        .iter()
        .filter(|(name, _)| name.starts_with("TALLY_GH_"))
        .cloned()
        .collect::<BTreeMap<_, _>>();
    assert_eq!(github.len(), GH_TALLY_ENVIRONMENT.len());
    assert_eq!(github["TALLY_GH_REPO"], "acme/widgets");
    assert_eq!(github["TALLY_GH_NUMBER"], "77");
    assert_eq!(
        github["TALLY_GH_URL"],
        "https://github.com/acme/widgets/issues/77"
    );
    assert_eq!(github["TALLY_GH_TYPE"], "issue");
    assert_eq!(github["TALLY_GH_HEAD_SHA"], "");
    assert_eq!(github["TALLY_GH_NODE_ID"], "I_kwDO_origin");
    assert_eq!(github["TALLY_GH_TRIGGER_KIND"], "assignment");
    assert_eq!(github["TALLY_GH_TRIGGER_ACTOR"], "trusted-maintainer");
    assert_eq!(github["TALLY_GH_EVENT_ID"], "notification-77");
    assert_eq!(github["TALLY_GH_COMMENT_ID"], "");
    assert_eq!(github["TALLY_GH_CONTEXT"], context_path.to_string_lossy());
    assert_eq!(request.argv, original_argv);
    assert!(github
        .values()
        .all(|value| !value.contains("touch /tmp/must-not-run")));

    let context: GhContextSnapshot =
        serde_json::from_slice(&std::fs::read(&context_path).unwrap()).unwrap();
    assert_eq!(context.schema_version, GH_CONTEXT_SCHEMA_VERSION);
    assert_eq!(context.body, "$(touch /tmp/must-not-run); ${SECRET}");
    assert_eq!(
        std::fs::metadata(&context_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(context_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    let mut pull_request = request;
    pull_request.gh_origin = Some(gh_origin(GhItemType::PullRequest));
    let pull_path = executor.gh_context_path(&pull_request.identity);
    let environment = execution_environment(&pull_request, Some(&pull_path)).unwrap();
    assert!(environment.iter().any(|(name, value)| {
        name == "TALLY_GH_HEAD_SHA" && value == "7777777777777777777777777777777777777777"
    }));
}

#[test]
fn jobs_without_github_origin_unset_every_github_identity_variable() {
    let request = request();
    let environment = execution_environment(&request, None).unwrap();
    assert!(environment
        .iter()
        .all(|(name, _)| !name.starts_with("TALLY_GH_")));
    let unset = environment_to_unset(&request);
    for name in GH_TALLY_ENVIRONMENT {
        assert!(unset.contains(&name), "missing unset for {name}");
    }
}

#[test]
fn rowless_identity_uses_job_uuid_and_optional_env_stays_absent() {
    let mut request = request();
    request.identity.task_uuid = None;
    request.parent = None;
    request.no_enqueue = false;
    request.credentials.clear();
    request.yield_hook = None;
    request.tally_socket = None;
    request.job_token = None;
    request.runtime_max_sec = None;
    request.cwd = None;
    let args = strings(
        &executor(Path::new("/state"))
            .build_systemd_argv(&request)
            .unwrap(),
    );
    assert!(args.contains(&"tally-job-00000000-0000-4000-8000-000000000001".to_owned()));
    let joined = args.join("\n");
    for absent in [
        "TALLY_TASK_UUID=",
        "TALLY_PARENT=",
        "TALLY_NO_ENQUEUE=",
        "TALLY_CREDENTIALS=",
        "TALLY_YIELD_HOOK=",
        "TALLY_SOCKET=",
        "TALLY_JOB_TOKEN=",
        "RuntimeMaxSec=",
        "LoadCredential=",
    ] {
        assert!(!joined.contains(absent));
    }
    let unset = args
        .windows(2)
        .find(|pair| pair[0] == "--property" && pair[1].starts_with("UnsetEnvironment="))
        .unwrap();
    let unset_names = unset[1].strip_prefix("UnsetEnvironment=").unwrap();
    for name in OPTIONAL_TALLY_ENVIRONMENT
        .into_iter()
        .chain(["CREDENTIALS_DIRECTORY"])
        .chain(GH_TALLY_ENVIRONMENT)
    {
        assert!(unset_names.split_whitespace().any(|word| word == name));
    }
}

#[test]
fn exec_stop_post_disables_environment_expansion() {
    let executor = Executor::new("/state", "/nix/store/$literal-path/bin/tally");
    let args = strings(&executor.build_systemd_argv(&request()).unwrap());
    let property = args
        .windows(2)
        .find(|pair| pair[0] == "--property" && pair[1].starts_with("ExecStopPost="))
        .unwrap();
    assert!(property[1].starts_with("ExecStopPost=:"));
    assert!(property[1].contains("$literal-path"));
    assert!(!property[1].contains("$$literal-path"));

    let specifier = Executor::new("/state", "/nix/store/%n/bin/tally");
    assert!(specifier.build_systemd_argv(&request()).is_err());
}

#[test]
fn invalid_limits_runtime_paths_and_credentials_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let executor = executor(temp.path());
    let mut invalid = request();
    invalid.limits.cpu_weight = 0;
    assert!(executor.build_systemd_argv(&invalid).is_err());
    invalid = request();
    invalid.limits.memory_max_bytes = 0;
    assert!(executor.build_systemd_argv(&invalid).is_err());
    invalid = request();
    invalid.limits.memory_max_bytes = u64::MAX;
    assert!(executor.build_systemd_argv(&invalid).is_err());
    invalid = request();
    invalid.runtime_max_sec = Some(0);
    assert!(executor.build_systemd_argv(&invalid).is_err());
    invalid = request();
    invalid.runtime_max_sec = Some(u64::MAX / 1_000_000);
    assert!(executor.build_systemd_argv(&invalid).is_err());
    invalid = request();
    invalid.cwd = Some(PathBuf::from("relative"));
    assert!(executor.build_systemd_argv(&invalid).is_err());
    invalid = request();
    invalid.cwd = Some(PathBuf::from("/work/%n"));
    assert!(executor.build_systemd_argv(&invalid).is_err());
    for token in ["a".repeat(63), "A".repeat(64), "g".repeat(64)] {
        invalid = request();
        invalid.job_token = Some(token);
        assert!(executor.build_systemd_argv(&invalid).is_err());
    }
    invalid = request();
    invalid.credentials = BTreeMap::from([("secret".to_owned(), PathBuf::from("/run/keys/%n"))]);
    assert!(executor.build_systemd_argv(&invalid).is_err());
    for name in ["", ".", "..", "slash/name", "colon:name", "space name"] {
        invalid = request();
        invalid.credentials = BTreeMap::from([(name.to_owned(), PathBuf::from("/secret"))]);
        assert!(executor.build_systemd_argv(&invalid).is_err(), "{name:?}");
    }
    invalid = request();
    invalid
        .credentials
        .insert("x".repeat(256), PathBuf::from("/secret"));
    assert!(executor.build_systemd_argv(&invalid).is_err());
}

#[test]
fn capture_files_truncate_and_exit_record_is_atomic_and_private() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
    let request = request();
    let paths = executor.prepare_paths(&request.identity).unwrap();
    std::fs::write(&paths.stdout, b"stale-tail").unwrap();
    std::fs::write(&paths.stderr, b"stale-error").unwrap();
    let paths = executor.prepare_paths(&request.identity).unwrap();
    assert_eq!(std::fs::read(&paths.stdout).unwrap(), b"");
    assert_eq!(std::fs::read(&paths.stderr).unwrap(), b"");
    assert!(!paths.failure_stderr.exists());
    assert_eq!(
        std::fs::metadata(&paths.stdout)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(paths.stdout.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    let environment = HashMap::from([
        ("INVOCATION_ID", "abc123".to_owned()),
        ("SERVICE_RESULT", "success".to_owned()),
        ("TALLY_ATTEMPT", "1".to_owned()),
        ("TALLY_LEASE_EPOCH", "7".to_owned()),
        ("EXIT_CODE", "exited".to_owned()),
        ("EXIT_STATUS", "0".to_owned()),
    ]);
    let unit = executor.unit_name(&request.identity);
    persist_exit_record(&paths.exit_record, &unit, &environment).unwrap();
    let record = read_exit_record(&paths.exit_record, &unit).unwrap();
    assert_eq!(record.invocation_id, "abc123");
    assert_eq!(
        std::fs::metadata(&paths.exit_record)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let leftovers = std::fs::read_dir(paths.exit_record.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn failure_capture_excerpt_is_a_bounded_private_tail() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
    let request = request();
    let paths = executor.prepare_paths(&request.identity).unwrap();
    write_capture_generation(
        &paths.capture_generation,
        CaptureGeneration {
            attempt: request.attempt,
            lease_epoch: request.lease_epoch,
        },
    )
    .unwrap();

    std::fs::write(&paths.stderr, b"short failure\n").unwrap();
    let excerpt = executor
        .persist_failure_stderr(&request.identity, request.attempt, request.lease_epoch)
        .unwrap()
        .unwrap();
    let failure_path = paths.failure_stderr.clone();
    assert_eq!(excerpt.text, "short failure\n");
    assert_eq!(std::fs::read(&failure_path).unwrap(), b"short failure\n");
    assert_eq!(
        std::fs::metadata(&failure_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        read_capture_excerpt(&failure_path).unwrap(),
        CaptureExcerpt {
            text: "short failure\n".to_owned(),
            truncated: false,
        }
    );

    let mut long = vec![b'x'; CAPTURE_EXCERPT_MAX_BYTES + 17];
    long.extend_from_slice(b"actionable tail\n");
    std::fs::write(&paths.stderr, long).unwrap();
    let excerpt = read_capture_excerpt(&paths.stderr).unwrap();
    assert!(excerpt.truncated);
    assert!(excerpt.text.len() <= CAPTURE_EXCERPT_MAX_BYTES);
    assert!(excerpt
        .text
        .starts_with("[... earlier captured stderr omitted ...]\n"));
    assert!(excerpt.text.ends_with("actionable tail\n"));
    let persisted = executor
        .persist_failure_stderr(&request.identity, request.attempt, request.lease_epoch)
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read(&failure_path).unwrap(),
        persisted.text.as_bytes()
    );
    assert!(std::fs::metadata(&failure_path).unwrap().len() <= CAPTURE_EXCERPT_MAX_BYTES as u64);

    let mut split_codepoint = "€".as_bytes().to_vec();
    split_codepoint.extend(std::iter::repeat_n(b'x', CAPTURE_EXCERPT_MAX_BYTES - 1));
    std::fs::write(&paths.stderr, split_codepoint).unwrap();
    let excerpt = read_capture_excerpt(&paths.stderr).unwrap();
    assert!(excerpt.truncated);
    assert!(!excerpt.text.contains('�'));
    assert!(excerpt.text.len() <= CAPTURE_EXCERPT_MAX_BYTES);

    let linked = temp.path().join("linked-capture");
    std::fs::hard_link(&paths.stderr, &linked).unwrap();
    assert!(read_capture_excerpt(&paths.stderr).is_err());
}

#[test]
fn raw_and_failure_stderr_archive_as_distinct_generation_files() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
    let request = request();
    let paths = executor.prepare_paths(&request.identity).unwrap();
    write_capture_generation(
        &paths.capture_generation,
        CaptureGeneration {
            attempt: request.attempt,
            lease_epoch: request.lease_epoch,
        },
    )
    .unwrap();
    let mut raw = vec![b'x'; CAPTURE_EXCERPT_MAX_BYTES + 17];
    raw.extend_from_slice(b"actionable failure\n");
    std::fs::write(&paths.stderr, &raw).unwrap();
    let failure = executor
        .persist_failure_stderr(&request.identity, request.attempt, request.lease_epoch)
        .unwrap()
        .unwrap();

    let retry = executor.prepare_paths(&request.identity).unwrap();
    assert!(!retry.failure_stderr.exists());
    let archived = executor
        .retained_capture_paths(&request.identity, request.attempt, request.lease_epoch)
        .unwrap()
        .unwrap();
    assert!(!archived.current);
    assert!(archived
        .stderr
        .ends_with("attempt-0000000001-epoch-00000000000000000007.adapter.err"));
    let archived_failure = archived.failure_stderr.unwrap();
    assert!(archived_failure.ends_with("attempt-0000000001-epoch-00000000000000000007.err"));
    assert_eq!(std::fs::read(archived.stderr).unwrap(), raw);
    assert_eq!(
        std::fs::read(archived_failure).unwrap(),
        failure.text.as_bytes()
    );
    assert!(failure.text.len() <= CAPTURE_EXCERPT_MAX_BYTES);
}

#[test]
fn stale_failure_persistence_cannot_mark_a_new_capture_generation_failed() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
    let request = request();
    let first = executor.prepare_paths(&request.identity).unwrap();
    write_capture_generation(
        &first.capture_generation,
        CaptureGeneration {
            attempt: 1,
            lease_epoch: 7,
        },
    )
    .unwrap();
    std::fs::write(&first.stderr, b"old failed attempt\n").unwrap();

    let second = executor.prepare_paths(&request.identity).unwrap();
    write_capture_generation(
        &second.capture_generation,
        CaptureGeneration {
            attempt: 2,
            lease_epoch: 8,
        },
    )
    .unwrap();
    std::fs::write(&second.stderr, b"healthy adapter chatter\n").unwrap();

    assert_eq!(
        executor
            .persist_failure_stderr(&request.identity, 1, 7)
            .unwrap(),
        None
    );
    assert!(!second.failure_stderr.exists());
}

#[test]
fn concurrent_retry_and_late_failure_persistence_never_leave_a_false_err_signal() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
    for _ in 0..16 {
        let mut request = request();
        let uuid = Uuid::new_v4();
        request.identity.job_id = uuid;
        request.identity.task_uuid = Some(uuid);
        let first = executor.prepare_paths(&request.identity).unwrap();
        write_capture_generation(
            &first.capture_generation,
            CaptureGeneration {
                attempt: 1,
                lease_epoch: 7,
            },
        )
        .unwrap();
        std::fs::write(&first.stderr, b"old failed attempt\n").unwrap();

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let late_executor = executor.clone();
        let late_identity = request.identity.clone();
        let late_barrier = barrier.clone();
        let late = std::thread::spawn(move || {
            late_barrier.wait();
            late_executor
                .persist_failure_stderr(&late_identity, 1, 7)
                .unwrap()
        });
        let retry_executor = executor.clone();
        let retry_identity = request.identity.clone();
        let retry = std::thread::spawn(move || {
            barrier.wait();
            let paths = retry_executor.prepare_paths(&retry_identity).unwrap();
            write_capture_generation(
                &paths.capture_generation,
                CaptureGeneration {
                    attempt: 2,
                    lease_epoch: 8,
                },
            )
            .unwrap();
            std::fs::write(&paths.stderr, b"healthy adapter chatter\n").unwrap();
            paths
        });
        late.join().unwrap();
        let current = retry.join().unwrap();
        assert!(executor
            .capture_generation_matches(&request.identity, 2, 8)
            .unwrap());
        assert!(!current.failure_stderr.exists());
    }
}

#[test]
fn capture_replacement_rejects_fifo_and_hardlink_truncation_attacks() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
    let request = request();
    let paths = executor.prepare_paths(&request.identity).unwrap();
    std::fs::remove_file(&paths.stdout).unwrap();
    std::fs::remove_file(&paths.stderr).unwrap();

    let fifo = CString::new(paths.stdout.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let victim = temp.path().join("must-not-truncate");
    std::fs::write(&victim, b"preserved").unwrap();
    std::fs::hard_link(&victim, &paths.stderr).unwrap();
    std::fs::hard_link(&victim, &paths.capture_generation).unwrap();

    let replaced = executor.prepare_paths(&request.identity).unwrap();
    write_capture_generation(
        &replaced.capture_generation,
        CaptureGeneration {
            attempt: request.attempt,
            lease_epoch: request.lease_epoch,
        },
    )
    .unwrap();

    assert_eq!(std::fs::read(&victim).unwrap(), b"preserved");
    assert_eq!(std::fs::read(&replaced.stdout).unwrap(), b"");
    assert_eq!(std::fs::read(&replaced.stderr).unwrap(), b"");
    for path in [
        &replaced.stdout,
        &replaced.stderr,
        &replaced.capture_generation,
    ] {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        assert!(metadata.file_type().is_file());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }
    assert_eq!(
        serde_json::from_slice::<CaptureGeneration>(
            &std::fs::read(&replaced.capture_generation).unwrap()
        )
        .unwrap(),
        CaptureGeneration {
            attempt: request.attempt,
            lease_epoch: request.lease_epoch,
        }
    );
}

#[test]
fn wave_5_red_case_two_attempt_provider_captures_are_distinct_and_queryable() {
    use crate::adapters::{AdapterConfig, AdapterTrace, ScrapeStream, TraceFraming};
    use crate::history::RetentionMetadata;
    use crate::query_v2::{QueryChainHead, QuerySnapshotMetadata};
    use crate::trace::{query_trace, TraceCapability, TraceLane};

    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
    let mut request = request();
    let task_ref = crate::provenance::TaskRef::new("crm/t07").unwrap();
    request.identity.task_ref = Some(task_ref.clone());
    let paths = executor.prepare_paths(&request.identity).unwrap();
    std::fs::write(&paths.stdout, b"{\"attempt\":1,\"message\":\"first\"}\n").unwrap();
    std::fs::write(&paths.stderr, b"first failure stderr\n").unwrap();
    write_capture_generation(
        &paths.capture_generation,
        CaptureGeneration {
            attempt: 1,
            lease_epoch: 7,
        },
    )
    .unwrap();
    let failure = executor
        .persist_failure_stderr(&request.identity, 1, 7)
        .unwrap()
        .unwrap();
    assert_eq!(failure.text, "first failure stderr\n");
    assert_eq!(
        std::fs::read(&paths.failure_stderr).unwrap(),
        b"first failure stderr\n"
    );

    let second = executor.prepare_paths(&request.identity).unwrap();
    let task_uuid = request.identity.task_uuid.unwrap().to_string();
    let archive = temp
        .path()
        .join("capture/archive")
        .join(format!("{task_uuid}.t07"));
    let archived_stdout = archive.join("attempt-0000000001-epoch-00000000000000000007.out");
    let archived_adapter_stderr =
        archive.join("attempt-0000000001-epoch-00000000000000000007.adapter.err");
    let archived_failure_stderr = archive.join("attempt-0000000001-epoch-00000000000000000007.err");
    assert_eq!(
        std::fs::read(&archived_stdout).unwrap(),
        b"{\"attempt\":1,\"message\":\"first\"}\n"
    );
    assert_eq!(
        std::fs::read(&archived_adapter_stderr).unwrap(),
        b"first failure stderr\n"
    );
    assert_eq!(
        std::fs::read(&archived_failure_stderr).unwrap(),
        b"first failure stderr\n"
    );
    let retained = executor
        .retained_capture_paths(&request.identity, 1, 7)
        .unwrap()
        .unwrap();
    assert_eq!(retained.stdout, archived_stdout);
    assert_eq!(retained.stderr, archived_adapter_stderr);
    assert_eq!(retained.failure_stderr, Some(archived_failure_stderr));
    assert!(!retained.current);

    std::fs::write(&second.stdout, b"{\"attempt\":2,\"message\":\"second\"}\n").unwrap();
    write_capture_generation(
        &second.capture_generation,
        CaptureGeneration {
            attempt: 2,
            lease_epoch: 8,
        },
    )
    .unwrap();

    let job_id = request.identity.job_id.to_string();
    let lanes = [
        TraceLane {
            task_uuid: task_uuid.clone(),
            task_ref: Some(task_ref.clone()),
            job_id: Some(job_id.clone()),
            attempt: 1,
            lease_epoch: 7,
            adapter: "codex".to_owned(),
            session_ref: Some("thread-1".to_owned()),
            running: false,
            remote: false,
        },
        TraceLane {
            task_uuid: task_uuid.clone(),
            task_ref: Some(task_ref.clone()),
            job_id: Some(job_id),
            attempt: 2,
            lease_epoch: 8,
            adapter: "codex".to_owned(),
            session_ref: Some("thread-2".to_owned()),
            running: false,
            remote: false,
        },
    ];
    let adapters = BTreeMap::from([(
        "codex".to_owned(),
        AdapterConfig {
            trace: Some(AdapterTrace {
                stream: ScrapeStream::Stdout,
                framing: TraceFraming::JsonLines,
            }),
            ..AdapterConfig::default()
        },
    )]);
    let result = query_trace(
        &task_uuid,
        None,
        &lanes,
        &adapters,
        &executor,
        QuerySnapshotMetadata {
            created_at: chrono::Utc::now().to_rfc3339(),
            cursor: None,
            history: RetentionMetadata {
                complete: true,
                policy: crate::history::LIFECYCLE_RETENTION_POLICY.to_owned(),
                earliest_cursor: None,
                latest_cursor: None,
                truncation_boundary: None,
                reason: None,
            },
            witness_head: QueryChainHead {
                seq: 0,
                hash: "genesis".to_owned(),
            },
        },
    )
    .unwrap();

    assert_eq!(
        result
            .generations
            .iter()
            .map(|generation| (
                generation.attempt,
                generation.lease_epoch,
                generation.capability
            ))
            .collect::<Vec<_>>(),
        vec![
            (1, 7, TraceCapability::Available),
            (2, 8, TraceCapability::Available)
        ]
    );
    assert!(result
        .generations
        .iter()
        .all(|generation| generation.task_ref.as_ref() == Some(&task_ref)));
    assert_eq!(
        result
            .items
            .iter()
            .map(|record| (record.attempt, record.raw.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (1, "{\"attempt\":1,\"message\":\"first\"}"),
            (2, "{\"attempt\":2,\"message\":\"second\"}")
        ]
    );
    assert!(result
        .items
        .iter()
        .all(|record| record.task_ref.as_ref() == Some(&task_ref)));
    let encoded = serde_json::to_value(result).unwrap();
    assert!(encoded["generations"]
        .as_array()
        .unwrap()
        .iter()
        .all(|generation| generation["taskRef"] == "crm/t07"));
    assert!(encoded["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|record| record["taskRef"] == "crm/t07"));
}

#[test]
fn duplicate_identity_is_reserved_before_capture_truncation() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
    let request = request();
    let first = executor.reserve(&request.identity).unwrap();
    assert!(matches!(
        executor.reserve(&request.identity),
        Err(ExecutorError::AlreadyRunning(_))
    ));
    drop(first);
    executor.reserve(&request.identity).unwrap();
}

#[derive(Debug, Clone)]
struct FactProbe(LocalUnitFact);

impl LocalUnitProbe for FactProbe {
    fn inspect(
        &self,
        _unit: &str,
        _paths: &ExecutionPaths,
    ) -> Result<LocalUnitFact, ExecutorError> {
        Ok(self.0.clone())
    }
}

#[derive(Debug, Clone)]
struct SequenceProbe(Arc<Mutex<std::collections::VecDeque<LocalUnitFact>>>);

impl LocalUnitProbe for SequenceProbe {
    fn inspect(
        &self,
        _unit: &str,
        _paths: &ExecutionPaths,
    ) -> Result<LocalUnitFact, ExecutorError> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ExecutorError::InvalidRequest("probe sequence exhausted".to_owned()))
    }
}

#[derive(Debug, Clone, Copy)]
struct FailingProbe;

impl LocalUnitProbe for FailingProbe {
    fn inspect(&self, unit: &str, _paths: &ExecutionPaths) -> Result<LocalUnitFact, ExecutorError> {
        Err(ExecutorError::UnitProbe {
            unit: unit.to_owned(),
            detail: "fake probe failure".to_owned(),
        })
    }
}

#[tokio::test]
async fn surviving_unit_and_probe_failure_stop_before_capture_truncation() {
    let temp = tempfile::tempdir().unwrap();
    let request = request();
    let base = executor(temp.path());
    let paths = base.prepare_paths(&request.identity).unwrap();
    std::fs::write(&paths.stdout, b"preserve-out").unwrap();
    std::fs::write(&paths.stderr, b"preserve-err").unwrap();
    let unit = base.unit_name(&request.identity);
    let running = LocalUnitFact {
        unit: unit.clone(),
        loaded: true,
        state: LocalUnitState::Running,
        invocation_id: Some("active-invocation".to_owned()),
        attempt: Some(request.attempt),
        lease_epoch: Some(request.lease_epoch),
        exit_record: None,
    };
    let guarded = Executor::new(temp.path(), "/nix/store/example/bin/tally")
        .with_unit_probe(FactProbe(running));
    assert!(matches!(
        guarded.execute(request.clone()).await,
        Err(ExecutorError::ExistingUnit {
            state: LocalUnitState::Running,
            ..
        })
    ));
    assert_eq!(std::fs::read(&paths.stdout).unwrap(), b"preserve-out");
    assert_eq!(std::fs::read(&paths.stderr).unwrap(), b"preserve-err");

    let failed =
        Executor::new(temp.path(), "/nix/store/example/bin/tally").with_unit_probe(FailingProbe);
    assert!(matches!(
        failed.execute(request).await,
        Err(ExecutorError::UnitProbe { .. })
    ));
    assert_eq!(std::fs::read(&paths.stdout).unwrap(), b"preserve-out");
    assert_eq!(std::fs::read(&paths.stderr).unwrap(), b"preserve-err");
}

#[tokio::test]
async fn matching_durable_exit_is_adopted_without_reexecution() {
    let temp = tempfile::tempdir().unwrap();
    let request = request();
    let base = executor(temp.path());
    let paths = base.prepare_paths(&request.identity).unwrap();
    std::fs::write(&paths.stdout, b"completed-once").unwrap();
    std::fs::remove_file(&paths.stderr).unwrap();
    std::fs::write(&paths.failure_stderr, b"legacy adapter stderr").unwrap();
    let unit = base.unit_name(&request.identity);
    let record = UnitExitRecord {
        accounting: None,
        schema_version: UNIT_EXIT_SCHEMA_VERSION,
        unit: unit.clone(),
        invocation_id: "completed-invocation".to_owned(),
        attempt: request.attempt,
        lease_epoch: request.lease_epoch,
        service_result: "success".to_owned(),
        exit_code: Some("exited".to_owned()),
        exit_status: Some("0".to_owned()),
    };
    let fact = LocalUnitFact {
        unit,
        loaded: false,
        state: LocalUnitState::Exited,
        invocation_id: Some(record.invocation_id.clone()),
        attempt: Some(record.attempt),
        lease_epoch: Some(record.lease_epoch),
        exit_record: Some(record.clone()),
    };
    let guarded =
        Executor::new(temp.path(), "/nix/store/example/bin/tally").with_unit_probe(FactProbe(fact));
    let outcome = guarded.execute(request).await.unwrap();
    assert_eq!(outcome.backend, ExecutionBackend::Adopted);
    assert_eq!(outcome.record, record);
    assert_eq!(std::fs::read(&paths.stdout).unwrap(), b"completed-once");
    assert_eq!(
        std::fs::read(&paths.stderr).unwrap(),
        b"legacy adapter stderr"
    );
    assert!(!paths.failure_stderr.exists());
}

#[tokio::test]
async fn recovered_absence_fails_closed_without_replay_or_capture_truncation() {
    let temp = tempfile::tempdir().unwrap();
    let request = request();
    let base = executor(temp.path());
    let paths = base.prepare_paths(&request.identity).unwrap();
    std::fs::write(&paths.stdout, b"retained-output").unwrap();
    std::fs::write(&paths.stderr, b"retained-error").unwrap();

    assert!(matches!(
        base.adopt(request, "recovered-invocation").await,
        Err(ExecutorError::AdoptedUnitUnavailable {
            state: LocalUnitState::Absent,
            ..
        })
    ));
    assert_eq!(std::fs::read(&paths.stdout).unwrap(), b"retained-output");
    assert_eq!(std::fs::read(&paths.stderr).unwrap(), b"retained-error");
}

#[tokio::test]
async fn adoption_waits_through_exit_record_visibility_race() {
    let temp = tempfile::tempdir().unwrap();
    let mut request = request();
    request.argv = vec![String::new(), "--raw-workload".to_owned()];
    let base = executor(temp.path());
    let paths = base.prepare_paths(&request.identity).unwrap();
    std::fs::write(&paths.stdout, b"retained-output").unwrap();
    let unit = base.unit_name(&request.identity);
    let record = UnitExitRecord {
        accounting: None,
        schema_version: UNIT_EXIT_SCHEMA_VERSION,
        unit: unit.clone(),
        invocation_id: "recovered-invocation".to_owned(),
        attempt: request.attempt,
        lease_epoch: request.lease_epoch,
        service_result: "success".to_owned(),
        exit_code: Some("exited".to_owned()),
        exit_status: Some("0".to_owned()),
    };
    let facts = std::collections::VecDeque::from([
        LocalUnitFact {
            unit: unit.clone(),
            loaded: true,
            state: LocalUnitState::InactiveWithoutRecord,
            invocation_id: Some(record.invocation_id.clone()),
            attempt: None,
            lease_epoch: None,
            exit_record: None,
        },
        LocalUnitFact {
            unit,
            loaded: false,
            state: LocalUnitState::Exited,
            invocation_id: Some(record.invocation_id.clone()),
            attempt: Some(record.attempt),
            lease_epoch: Some(record.lease_epoch),
            exit_record: Some(record.clone()),
        },
    ]);
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
        .with_unit_probe(SequenceProbe(Arc::new(Mutex::new(facts))));
    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        executor.adopt(request.clone(), "recovered-invocation"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(outcome.record, record);
    assert_eq!(std::fs::read(&paths.stdout).unwrap(), b"retained-output");

    let mut replacement = record;
    replacement.invocation_id = "replacement-invocation".to_owned();
    let replaced = Executor::new(temp.path(), "/nix/store/example/bin/tally").with_unit_probe(
        FactProbe(LocalUnitFact {
            unit: replacement.unit.clone(),
            loaded: false,
            state: LocalUnitState::Exited,
            invocation_id: Some(replacement.invocation_id.clone()),
            attempt: Some(replacement.attempt),
            lease_epoch: Some(replacement.lease_epoch),
            exit_record: Some(replacement),
        }),
    );
    assert!(matches!(
        replaced.adopt(request, "recovered-invocation").await,
        Err(ExecutorError::AdoptedInvocationMismatch { .. })
    ));
    assert_eq!(std::fs::read(&paths.stdout).unwrap(), b"retained-output");
}

#[tokio::test]
async fn loaded_prior_exit_blocks_represent_before_capture_truncation() {
    let temp = tempfile::tempdir().unwrap();
    let mut request = request();
    let base = executor(temp.path());
    let paths = base.prepare_paths(&request.identity).unwrap();
    std::fs::write(&paths.stdout, b"preserve-completed-out").unwrap();
    std::fs::write(&paths.stderr, b"preserve-completed-err").unwrap();
    let unit = base.unit_name(&request.identity);
    let record = UnitExitRecord {
        accounting: None,
        schema_version: UNIT_EXIT_SCHEMA_VERSION,
        unit: unit.clone(),
        invocation_id: "prior-invocation".to_owned(),
        attempt: request.attempt,
        lease_epoch: request.lease_epoch,
        service_result: "success".to_owned(),
        exit_code: Some("exited".to_owned()),
        exit_status: Some("0".to_owned()),
    };
    write_exit_record(&paths.exit_record, &record).unwrap();
    let exit_before = std::fs::read(&paths.exit_record).unwrap();
    let fact = LocalUnitFact {
        unit,
        loaded: true,
        state: LocalUnitState::Exited,
        invocation_id: Some(record.invocation_id.clone()),
        attempt: Some(record.attempt),
        lease_epoch: Some(record.lease_epoch),
        exit_record: Some(record),
    };
    request.attempt += 1;
    request.lease_epoch += 1;

    let guarded =
        Executor::new(temp.path(), "/nix/store/example/bin/tally").with_unit_probe(FactProbe(fact));
    assert!(matches!(
        guarded.execute(request).await,
        Err(ExecutorError::ExistingUnit {
            state: LocalUnitState::Exited,
            ..
        })
    ));
    assert_eq!(
        std::fs::read(&paths.stdout).unwrap(),
        b"preserve-completed-out"
    );
    assert_eq!(
        std::fs::read(&paths.stderr).unwrap(),
        b"preserve-completed-err"
    );
    assert_eq!(std::fs::read(&paths.exit_record).unwrap(), exit_before);
}

#[test]
fn systemd_probe_executes_user_show_and_correlates_rowless_exit() {
    let temp = tempfile::tempdir().unwrap();
    let mut request = request();
    request.identity.task_uuid = None;
    let probe_program = temp.path().join("fake-systemctl");
    let executor =
        Executor::new(temp.path(), "/nix/store/example/bin/tally").with_systemctl(&probe_program);
    let unit = executor.unit_name(&request.identity);
    let expected_script = format!(
        "#!/bin/sh\n\
         [ \"$#\" -eq 8 ] || exit 81\n\
         [ \"$1\" = --user ] || exit 82\n\
         [ \"$2\" = show ] || exit 83\n\
         [ \"$3\" = --property=LoadState ] || exit 84\n\
         [ \"$4\" = --property=ActiveState ] || exit 85\n\
         [ \"$5\" = --property=InvocationID ] || exit 86\n\
         [ \"$6\" = --property=Environment ] || exit 87\n\
         [ \"$7\" = -- ] || exit 88\n\
         [ \"$8\" = {unit} ] || exit 89\n\
         printf 'LoadState=not-found\\nActiveState=inactive\\nInvocationID=\\nEnvironment=\\n'\n"
    );
    crate::test_support::install_shell_program(&probe_program, expected_script);

    let absent = executor.inspect_identity(&request.identity).unwrap();
    assert_eq!(absent, LocalUnitFact::absent(&unit));
    assert!(unit.contains(request.identity.job_id.to_string().as_str()));

    let paths = executor.prepare_paths(&request.identity).unwrap();
    let record = UnitExitRecord {
        accounting: None,
        schema_version: UNIT_EXIT_SCHEMA_VERSION,
        unit: unit.clone(),
        invocation_id: "durable-invocation".to_owned(),
        attempt: request.attempt,
        lease_epoch: request.lease_epoch,
        service_result: "success".to_owned(),
        exit_code: Some("exited".to_owned()),
        exit_status: Some("0".to_owned()),
    };
    write_exit_record(&paths.exit_record, &record).unwrap();
    let loaded_script = format!(
        "#!/bin/sh\n\
         [ \"$#\" -eq 8 ] || exit 81\n\
         [ \"$8\" = {unit} ] || exit 89\n\
         printf 'LoadState=loaded\\nActiveState=inactive\\nInvocationID=durable-invocation\\nEnvironment=\\n'\n"
    );
    let loaded_probe_program = temp.path().join("fake-systemctl-loaded");
    crate::test_support::install_shell_program(&loaded_probe_program, loaded_script);
    let loaded_executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
        .with_systemctl(&loaded_probe_program);
    let exited = loaded_executor.inspect_identity(&request.identity).unwrap();
    assert!(exited.loaded);
    assert_eq!(exited.state, LocalUnitState::Exited);
    assert_eq!(exited.exit_record, Some(record));

    let failed_probe_program = temp.path().join("fake-systemctl-failed");
    crate::test_support::install_shell_program(&failed_probe_program, "#!/bin/sh\nexit 23\n");
    let failed_executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
        .with_systemctl(&failed_probe_program);
    assert!(matches!(
        failed_executor.inspect_identity(&request.identity),
        Err(ExecutorError::UnitProbe { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn async_unit_probe_keeps_current_thread_timers_live() {
    let temp = tempfile::tempdir().unwrap();
    let request = request();
    let systemctl = temp.path().join("slow-systemctl");
    crate::test_support::install_shell_program(
        &systemctl,
        "#!/bin/sh\nsleep 1\nprintf 'LoadState=not-found\\nActiveState=inactive\\nInvocationID=\\nEnvironment=\\n'\n",
    );
    let executor =
        Executor::new(temp.path(), "/nix/store/example/bin/tally").with_systemctl(systemctl);
    let probe = executor.inspect_identity_async(&request.identity);
    tokio::pin!(probe);
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        result = &mut probe => panic!("slow probe completed unexpectedly: {result:?}"),
    }
    assert_eq!(probe.await.unwrap().state, LocalUnitState::Absent);
}

#[tokio::test]
async fn hard_reclaim_stops_the_exact_running_unit_and_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let request = request();
    let control = temp.path().join("fake-systemctl-stop");
    let marker = temp.path().join("stopped-unit");
    let base = Executor::new(temp.path(), "/nix/store/example/bin/tally").with_systemctl(&control);
    let unit = base.unit_name(&request.identity);
    let script = format!(
        "#!/bin/sh\n\
         [ \"$#\" -eq 4 ] || exit 81\n\
         [ \"$1\" = --user ] || exit 82\n\
         [ \"$2\" = stop ] || exit 83\n\
         [ \"$3\" = -- ] || exit 84\n\
         printf '%s' \"$4\" > {}\n",
        marker.display()
    );
    crate::test_support::install_shell_program(&control, script);
    let running = LocalUnitFact {
        unit: unit.clone(),
        loaded: true,
        state: LocalUnitState::Running,
        invocation_id: Some("running-invocation".to_owned()),
        attempt: Some(request.attempt),
        lease_epoch: Some(request.lease_epoch),
        exit_record: None,
    };
    let executor = base.with_unit_probe(FactProbe(running.clone()));
    executor.reclaim_identity(&request.identity).await.unwrap();
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), unit);
    std::fs::remove_file(&marker).unwrap();
    assert!(matches!(
        executor
            .reclaim_identity_exact(&request.identity, Some("prior-invocation"))
            .await,
        Err(ExecutorError::AdoptedInvocationMismatch { .. })
    ));
    assert!(!marker.exists(), "replacement invocation was stopped");

    crate::test_support::rewrite_shell_program(&control, "#!/bin/sh\nexit 23\n");
    assert!(matches!(
        executor.reclaim_identity(&request.identity).await,
        Err(ExecutorError::UnitControl { .. })
    ));

    let missing_control = temp.path().join("missing-systemctl");
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
        .with_systemctl(&missing_control)
        .with_unit_probe(FactProbe(running));
    assert!(matches!(
        executor.reclaim_identity(&request.identity).await,
        Err(ExecutorError::UnitControl {
            unit: failed_unit,
            ..
        }) if failed_unit == unit
    ));
}

#[tokio::test]
async fn hard_reclaim_kills_and_awaits_a_direct_process_group() {
    let temp = tempfile::tempdir().unwrap();
    let started = temp.path().join("direct-started");
    let descendant_started = temp.path().join("descendant-started");
    let leaked = temp.path().join("descendant-leaked");
    let mut request = fixture_request("fixture-reclaim");
    request.cwd = Some(temp.path().to_owned());
    let unit = format!("tally-job-{}.service", request.identity.unit_uuid());
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
        .with_systemd_run(temp.path().join("missing-systemd-run"))
        .with_direct_fallback()
        .with_unit_probe(FactProbe(LocalUnitFact::absent(&unit)));
    let running_executor = executor.clone();
    let running_request = request.clone();
    let running = tokio::spawn(async move { running_executor.execute(running_request).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !started.exists() || !descendant_started.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    executor.reclaim_identity(&request.identity).await.unwrap();
    let outcome = running.await.unwrap().unwrap();
    assert!(matches!(
        outcome.termination,
        ExecutionTermination::Signaled { .. }
    ));
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert!(!leaked.exists(), "direct descendant survived hard reclaim");
}

#[tokio::test]
async fn launcher_failure_reclaims_a_unit_before_returning() {
    #[derive(Clone)]
    struct SequenceProbe(Arc<std::sync::Mutex<std::collections::VecDeque<LocalUnitFact>>>);
    impl LocalUnitProbe for SequenceProbe {
        fn inspect(
            &self,
            _unit: &str,
            _paths: &ExecutionPaths,
        ) -> Result<LocalUnitFact, ExecutorError> {
            self.0
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| ExecutorError::UnitProbe {
                    unit: "sequence".to_owned(),
                    detail: "probe sequence exhausted".to_owned(),
                })
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let request = request();
    let base = executor(temp.path());
    let unit = base.unit_name(&request.identity);
    let running = LocalUnitFact {
        unit: unit.clone(),
        loaded: true,
        state: LocalUnitState::Running,
        invocation_id: Some("still-running".to_owned()),
        attempt: Some(request.attempt),
        lease_epoch: Some(request.lease_epoch),
        exit_record: None,
    };
    let probe = SequenceProbe(Arc::new(std::sync::Mutex::new(
        [LocalUnitFact::absent(&unit), running].into(),
    )));
    let systemd_run = temp.path().join("fake-systemd-run");
    crate::test_support::install_shell_program(&systemd_run, "#!/bin/sh\nexit 23\n");
    let systemctl = temp.path().join("fake-systemctl");
    let marker = temp.path().join("stopped");
    crate::test_support::install_shell_program(
        &systemctl,
        format!("#!/bin/sh\nprintf '%s' \"$4\" > {}\n", marker.display()),
    );
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
        .with_systemd_run(systemd_run)
        .with_systemctl(systemctl)
        .with_unit_probe(probe);
    let result = executor.execute(request).await;
    assert!(
        matches!(
            &result,
            Err(ExecutorError::LauncherFailed {
                status: Some(23),
                ..
            })
        ),
        "unexpected launcher result: {result:?}"
    );
    assert_eq!(std::fs::read_to_string(marker).unwrap(), unit);
}

#[tokio::test]
async fn launcher_failure_without_visible_unit_preserves_error_promptly() {
    let temp = tempfile::tempdir().unwrap();
    let systemd_run = temp.path().join("fake-systemd-run");
    crate::test_support::install_shell_program(&systemd_run, "#!/bin/sh\nexit 23\n");
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
        .with_systemd_run(systemd_run)
        .with_unit_probe(AbsentProbe);

    let result = tokio::time::timeout(Duration::from_millis(100), executor.execute(request()))
        .await
        .expect("launcher failure was masked by reservation reclaim");
    assert!(
        matches!(
            result,
            Err(ExecutorError::LauncherFailed {
                status: Some(23),
                ..
            })
        ),
        "unexpected launcher result: {result:?}"
    );
}

#[tokio::test]
async fn reclaim_waits_for_a_registered_launch_to_become_visible() {
    #[derive(Clone)]
    struct VisibilityProbe {
        unit: String,
        visible: PathBuf,
    }

    impl LocalUnitProbe for VisibilityProbe {
        fn inspect(
            &self,
            _unit: &str,
            _paths: &ExecutionPaths,
        ) -> Result<LocalUnitFact, ExecutorError> {
            if self.visible.exists() {
                Ok(LocalUnitFact {
                    unit: self.unit.clone(),
                    loaded: true,
                    state: LocalUnitState::Running,
                    invocation_id: Some("delayed-launch".to_owned()),
                    attempt: Some(1),
                    lease_epoch: Some(1),
                    exit_record: None,
                })
            } else {
                Ok(LocalUnitFact::absent(&self.unit))
            }
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let request = request();
    let base = Executor::new(temp.path(), "/nix/store/example/bin/tally");
    let unit = base.unit_name(&request.identity);
    let started = temp.path().join("launch-started");
    let visible = temp.path().join("unit-visible");
    let systemd_run = temp.path().join("slow-systemd-run");
    crate::test_support::install_shell_program(
        &systemd_run,
        format!(
            "#!/bin/sh\n: > '{}'\nsleep 3\n: > '{}'\nexit 23\n",
            started.display(),
            visible.display()
        ),
    );
    let stopped = temp.path().join("unit-stopped");
    let systemctl = temp.path().join("fake-systemctl");
    crate::test_support::install_shell_program(
        &systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$4\" >> '{}'\n",
            stopped.display()
        ),
    );
    let executor = base
        .with_systemd_run(systemd_run)
        .with_systemctl(systemctl)
        .with_unit_probe(VisibilityProbe {
            unit: unit.clone(),
            visible,
        });
    let running_executor = executor.clone();
    let running_request = request.clone();
    let running = tokio::spawn(async move { running_executor.execute(running_request).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("fake systemd-run did not start");

    tokio::time::timeout(
        Duration::from_secs(5),
        executor.reclaim_identity(&request.identity),
    )
    .await
    .expect("reclaim did not wait for launch visibility")
    .unwrap();
    let stopped_units = std::fs::read_to_string(stopped).unwrap();
    assert!(
        !stopped_units.is_empty() && stopped_units.lines().all(|stopped| stopped == unit),
        "unexpected stopped units: {stopped_units:?}"
    );
    let result = running.await.unwrap();
    assert!(
        matches!(
            result,
            Err(ExecutorError::LauncherFailed {
                status: Some(23),
                ..
            })
        ),
        "unexpected launcher result: {result:?}"
    );
}

#[test]
fn systemd_show_interpretation_is_strict_and_carries_attempt_epoch() {
    let temp = tempfile::tempdir().unwrap();
    let request = request();
    let executor = executor(temp.path());
    let paths = executor.paths(&request.identity);
    let unit = executor.unit_name(&request.identity);
    let running = interpret_systemd_unit_show(
        &unit,
        &paths,
        b"LoadState=loaded\nActiveState=active\nInvocationID=abc123\nEnvironment=\"TALLY_POOL=two words\" TALLY_ATTEMPT=2 TALLY_LEASE_EPOCH=9\n",
    )
    .unwrap();
    assert_eq!(running.state, LocalUnitState::Running);
    assert_eq!(running.attempt, Some(2));
    assert_eq!(running.lease_epoch, Some(9));
    assert!(interpret_systemd_unit_show(
        &unit,
        &paths,
        b"LoadState=loaded\nActiveState=active\nInvocationID=abc123\nEnvironment=TALLY_ATTEMPT=2\n",
    )
    .is_err());
    assert!(interpret_systemd_unit_show(
        &unit,
        &paths,
        b"LoadState=not-found\nActiveState=inactive\nInvocationID=\nEnvironment=\nUnexpected=value\n",
    )
    .is_err());
}

#[test]
fn malformed_mismatched_and_missing_exit_records_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("exit.json");
    std::fs::write(&path, b"{").unwrap();
    assert!(read_exit_record(&path, "unit.service").is_err());
    let record = UnitExitRecord {
        accounting: None,
        schema_version: UNIT_EXIT_SCHEMA_VERSION,
        unit: "other.service".to_owned(),
        invocation_id: "id".to_owned(),
        attempt: 1,
        lease_epoch: 1,
        service_result: "success".to_owned(),
        exit_code: Some("exited".to_owned()),
        exit_status: Some("0".to_owned()),
    };
    write_exit_record(&path, &record).unwrap();
    assert!(read_exit_record(&path, "unit.service").is_err());
    let incomplete = HashMap::from([
        ("INVOCATION_ID", "id".to_owned()),
        ("SERVICE_RESULT", "success".to_owned()),
        ("TALLY_ATTEMPT", "1".to_owned()),
        ("TALLY_LEASE_EPOCH", "1".to_owned()),
    ]);
    assert!(persist_exit_record(&path, "unit.service", &incomplete).is_err());

    for invalid in [
        UnitExitRecord {
            accounting: None,
            schema_version: UNIT_EXIT_SCHEMA_VERSION,
            unit: "unit.service".to_owned(),
            invocation_id: "id".to_owned(),
            attempt: 1,
            lease_epoch: 1,
            service_result: "invented".to_owned(),
            exit_code: Some("exited".to_owned()),
            exit_status: Some("0".to_owned()),
        },
        UnitExitRecord {
            accounting: None,
            schema_version: UNIT_EXIT_SCHEMA_VERSION,
            unit: "unit.service".to_owned(),
            invocation_id: "id".to_owned(),
            attempt: 1,
            lease_epoch: 1,
            service_result: "success".to_owned(),
            exit_code: Some("invented".to_owned()),
            exit_status: Some("0".to_owned()),
        },
        UnitExitRecord {
            accounting: None,
            schema_version: UNIT_EXIT_SCHEMA_VERSION,
            unit: "unit.service".to_owned(),
            invocation_id: "id".to_owned(),
            attempt: 1,
            lease_epoch: 1,
            service_result: "success".to_owned(),
            exit_code: Some("exited".to_owned()),
            exit_status: None,
        },
    ] {
        write_exit_record(&path, &invalid).unwrap();
        assert!(read_exit_record(&path, "unit.service").is_err());
    }

    let realtime_signal = UnitExitRecord {
        accounting: None,
        schema_version: UNIT_EXIT_SCHEMA_VERSION,
        unit: "unit.service".to_owned(),
        invocation_id: "id".to_owned(),
        attempt: 1,
        lease_epoch: 1,
        service_result: "signal".to_owned(),
        exit_code: Some("killed".to_owned()),
        exit_status: Some("RTMIN+1".to_owned()),
    };
    write_exit_record(&path, &realtime_signal).unwrap();
    assert_eq!(
        classify_termination(&read_exit_record(&path, "unit.service").unwrap()).unwrap(),
        ExecutionTermination::Signaled {
            code: "killed".to_owned(),
            status: "RTMIN+1".to_owned(),
        }
    );
}

#[test]
fn startup_failure_without_main_process_metadata_is_durable() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("exit.json");
    let environment = HashMap::from([
        ("INVOCATION_ID", "id".to_owned()),
        ("SERVICE_RESULT", "resources".to_owned()),
        ("TALLY_ATTEMPT", "1".to_owned()),
        ("TALLY_LEASE_EPOCH", "1".to_owned()),
    ]);
    let record = persist_exit_record(&path, "unit.service", &environment).unwrap();
    assert_eq!(record.exit_code, None);
    assert_eq!(record.exit_status, None);
    assert_eq!(
        classify_termination(&record).unwrap(),
        ExecutionTermination::ServiceFailed {
            service_result: "resources".to_owned(),
            exit_code: None,
            exit_status: None,
        }
    );
    let json = std::fs::read_to_string(path).unwrap();
    assert!(json.contains("\"exitCode\":null"));
    assert!(json.contains("\"exitStatus\":null"));

    let protocol = UnitExitRecord {
        accounting: None,
        schema_version: UNIT_EXIT_SCHEMA_VERSION,
        unit: "unit.service".to_owned(),
        invocation_id: "id".to_owned(),
        attempt: 1,
        lease_epoch: 1,
        service_result: "protocol".to_owned(),
        exit_code: Some("exited".to_owned()),
        exit_status: Some("0".to_owned()),
    };
    assert!(matches!(
        classify_termination(&protocol).unwrap(),
        ExecutionTermination::ServiceFailed { .. }
    ));
}

#[test]
fn timeout_records_map_to_runtime_exceeded() {
    let record = UnitExitRecord {
        accounting: None,
        schema_version: UNIT_EXIT_SCHEMA_VERSION,
        unit: "unit.service".to_owned(),
        invocation_id: "id".to_owned(),
        attempt: 1,
        lease_epoch: 1,
        service_result: "timeout".to_owned(),
        exit_code: Some("killed".to_owned()),
        exit_status: Some("TERM".to_owned()),
    };
    assert_eq!(
        classify_termination(&record).unwrap(),
        ExecutionTermination::RuntimeExceeded
    );
}

#[test]
fn direct_child_fixture() {
    let Ok(pool) = std::env::var("TALLY_POOL") else {
        return;
    };
    match pool.as_str() {
        "fixture-exit127" => {
            println!("fixture-stdout");
            eprintln!("fixture-stderr");
            std::process::exit(127);
        }
        "fixture-timeout" => {
            if std::env::var_os("TALLY_TEST_DESCENDANT").is_some() {
                std::thread::sleep(Duration::from_secs(3));
                std::fs::write("descendant-survived", b"escaped").unwrap();
                return;
            }
            let executable = std::env::current_exe().unwrap();
            let mut descendant = std::process::Command::new(executable)
                .args([
                    "executor::tests::direct_child_fixture",
                    "--exact",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("TALLY_TEST_DESCENDANT", "1")
                .spawn()
                .unwrap();
            println!("fixture-before-timeout");
            std::thread::sleep(Duration::from_secs(30));
            descendant.wait().unwrap();
        }
        "fixture-reclaim" => {
            if std::env::var_os("TALLY_TEST_DESCENDANT").is_some() {
                std::fs::write("descendant-started", b"started").unwrap();
                std::thread::sleep(Duration::from_secs(1));
                std::fs::write("descendant-leaked", b"escaped").unwrap();
                return;
            }
            let executable = std::env::current_exe().unwrap();
            let mut descendant = std::process::Command::new(executable)
                .args([
                    "executor::tests::direct_child_fixture",
                    "--exact",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("TALLY_TEST_DESCENDANT", "1")
                .spawn()
                .unwrap();
            std::fs::write("direct-started", b"started").unwrap();
            std::thread::sleep(Duration::from_secs(30));
            descendant.wait().unwrap();
        }
        _ => {}
    }
}

fn fixture_request(pool: &str) -> ExecutionRequest {
    let executable = std::env::current_exe().unwrap();
    ExecutionRequest {
        identity: ExecutionIdentity {
            job_id: Uuid::new_v4(),
            task_uuid: None,
            task_ref: None,
        },
        parent: None,
        pools: vec![pool.to_owned()],
        lease_epoch: 1,
        attempt: 1,
        priority: Priority::Low,
        no_enqueue: false,
        argv: vec![
            executable.to_string_lossy().into_owned(),
            "executor::tests::direct_child_fixture".to_owned(),
            "--exact".to_owned(),
            "--nocapture".to_owned(),
            "--test-threads=1".to_owned(),
        ],
        yield_hook: None,
        tally_socket: None,
        job_token: None,
        environment: BTreeMap::new(),
        gh_origin: None,
        brief_hash: None,
        brief_path: None,
        brief_document: None,
        cwd: None,
        workspace: None,
        gate_manifest: None,
        git_ai: None,
        exec_attestation: None,
        hardening: AdapterHardening::None,
        extra_writable_paths: Vec::new(),
        credentials: BTreeMap::new(),
        limits: UnitLimits {
            cpu_weight: 100,
            memory_max_bytes: 1024 * 1024,
        },
        runtime_max_sec: None,
    }
}

#[tokio::test]
async fn missing_systemd_run_falls_back_once_and_leaf_127_is_not_retried() {
    let temp = tempfile::tempdir().unwrap();
    let request = fixture_request("fixture-exit127");
    let outcome = Executor::new(temp.path(), "/nix/store/example/bin/tally")
        .with_unit_probe(AbsentProbe)
        .with_systemd_run(temp.path().join("missing-systemd-run"))
        .with_direct_fallback()
        .execute(request)
        .await
        .unwrap();
    assert_eq!(outcome.backend, ExecutionBackend::Direct);
    assert_eq!(outcome.termination, ExecutionTermination::Exited(127));
    let mut stdout = String::new();
    File::open(outcome.paths.stdout)
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    assert_eq!(stdout.matches("fixture-stdout").count(), 1);
    let mut stderr = String::new();
    File::open(outcome.paths.stderr)
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(stderr.matches("fixture-stderr").count(), 1);
}

#[tokio::test]
async fn executor_default_refuses_direct_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let request = fixture_request("fixture-exit127");
    let result = Executor::new(temp.path(), "/nix/store/example/bin/tally")
        .with_unit_probe(AbsentProbe)
        .with_systemd_run(temp.path().join("missing-systemd-run"))
        .execute(request)
        .await;
    assert!(matches!(result, Err(ExecutorError::Spawn { .. })));
    let capture = temp.path().join(CAPTURE_DIRECTORY);
    assert!(capture
        .read_dir()
        .unwrap()
        .all(|entry| entry.unwrap().metadata().unwrap().len() == 0));
}

#[tokio::test]
async fn require_systemd_revokes_an_earlier_direct_fallback_opt_in() {
    let temp = tempfile::tempdir().unwrap();
    let request = fixture_request("fixture-exit127");
    let result = Executor::new(temp.path(), "/nix/store/example/bin/tally")
        .with_unit_probe(AbsentProbe)
        .with_systemd_run(temp.path().join("missing-systemd-run"))
        .with_direct_fallback()
        .require_systemd()
        .execute(request)
        .await;
    assert!(matches!(result, Err(ExecutorError::Spawn { .. })));
}

#[tokio::test]
async fn direct_fallback_times_out_and_refuses_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
        .with_unit_probe(AbsentProbe)
        .with_systemd_run(temp.path().join("missing-systemd-run"))
        .with_direct_fallback();
    let mut timeout = fixture_request("fixture-timeout");
    timeout.runtime_max_sec = Some(1);
    timeout.cwd = Some(temp.path().to_owned());
    let outcome = executor.execute(timeout).await.unwrap();
    assert_eq!(outcome.termination, ExecutionTermination::RuntimeExceeded);
    assert_eq!(outcome.record.service_result, "timeout");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!temp.path().join("descendant-survived").exists());

    let mut credentialed = fixture_request("fixture-exit127");
    credentialed
        .credentials
        .insert("secret".to_owned(), PathBuf::from("/run/secret"));
    assert!(matches!(
        executor.execute(credentialed).await,
        Err(ExecutorError::CredentialedFallback)
    ));
}

#[test]
fn the_capture_lock_lives_outside_every_job_writable_directory() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
    let request = request();
    let lock = executor.capture_lock_path(&request.identity);
    assert_eq!(
        lock,
        temp.path()
            .join(CAPTURE_LOCK_DIRECTORY)
            .join(format!("{}.capture.lock", request.identity.unit_uuid()))
    );
    // The naming stays greppable, and the directory is neither the one the
    // ExecStopPost recorder writes nor one holding a granted capture file.
    assert!(lock.to_str().unwrap().ends_with(CAPTURE_LOCK_SUFFIX));
    assert_ne!(
        lock.parent().unwrap(),
        temp.path().join(UNIT_EXIT_DIRECTORY)
    );

    executor.prepare_paths(&request.identity).unwrap();
    assert!(lock.exists());
    assert!(!temp
        .path()
        .join(UNIT_EXIT_DIRECTORY)
        .join(format!("{}.capture.lock", request.identity.unit_uuid()))
        .exists());
}

#[test]
fn a_legacy_unit_exit_lock_is_never_taken_by_the_new_path() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
    let request = request();
    let paths = executor.prepare_paths(&request.identity).unwrap();
    write_capture_generation(
        &paths.capture_generation,
        CaptureGeneration {
            attempt: request.attempt,
            lease_epoch: request.lease_epoch,
        },
    )
    .unwrap();
    std::fs::write(&paths.stderr, b"legacy lock ignored\n").unwrap();

    // A lock left behind by a pre-relocation daemon, held exclusively. If the
    // new path still consulted it this call would hit the deadline and error.
    let legacy = temp
        .path()
        .join(UNIT_EXIT_DIRECTORY)
        .join(format!("{}.capture.lock", request.identity.unit_uuid()));
    std::fs::write(&legacy, b"").unwrap();
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&legacy)
        .unwrap();
    FileExt::lock_exclusive(&holder).unwrap();

    let excerpt = executor
        .persist_failure_stderr(&request.identity, request.attempt, request.lease_epoch)
        .unwrap()
        .unwrap();
    assert_eq!(excerpt.text, "legacy lock ignored\n");
    drop(holder);
}

#[test]
fn a_held_capture_lock_fails_the_projection_inside_the_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
    let request = request();
    let paths = executor.prepare_paths(&request.identity).unwrap();
    write_capture_generation(
        &paths.capture_generation,
        CaptureGeneration {
            attempt: request.attempt,
            lease_epoch: request.lease_epoch,
        },
    )
    .unwrap();
    std::fs::write(&paths.stderr, b"blocked failure\n").unwrap();

    let holder = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(executor.capture_lock_path(&request.identity))
        .unwrap();
    FileExt::lock_exclusive(&holder).unwrap();

    let started = std::time::Instant::now();
    let error = executor
        .persist_failure_stderr(&request.identity, request.attempt, request.lease_epoch)
        .unwrap_err();
    let waited = started.elapsed();
    assert!(
        matches!(error, ExecutorError::CaptureLockContended { .. }),
        "unexpected error {error:?}"
    );
    assert!(
        waited >= CAPTURE_LOCK_DEADLINE,
        "returned too early: {waited:?}"
    );
    assert!(
        waited < CAPTURE_LOCK_DEADLINE * 4,
        "waited past the bound: {waited:?}"
    );
    // The projection is refused, not silently stale.
    assert!(!paths.failure_stderr.exists());

    // Once the holder lets go the same call succeeds.
    drop(holder);
    let excerpt = executor
        .persist_failure_stderr(&request.identity, request.attempt, request.lease_epoch)
        .unwrap()
        .unwrap();
    assert_eq!(excerpt.text, "blocked failure\n");
}

#[test]
fn a_dead_generation_never_mints_a_capture_lock() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
    let request = request();
    // Exactly what the startup reconciler does for a historically failed task:
    // no capture generation survives, so there is nothing to project and no
    // reason to leave a lock file behind for the retention sweep to chase.
    assert!(executor
        .persist_failure_stderr(&request.identity, request.attempt, request.lease_epoch)
        .unwrap()
        .is_none());
    let lock = executor.capture_lock_path(&request.identity);
    assert!(!lock.exists());
    assert!(!temp.path().join(CAPTURE_LOCK_DIRECTORY).exists());
}

#[test]
fn a_capture_lock_detached_from_its_name_is_never_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
    let request = request();
    let path = executor.capture_lock_path(&request.identity);
    let held = executor.lock_capture(&request.identity).unwrap();
    // The postcondition every acquisition now carries: the fd under the lock is
    // still what the path resolves to.
    assert!(capture_lock_still_named(&held, &path).unwrap());

    // Model the state the sweep can leave behind: the name is gone while a
    // holder still owns the inode and its exclusive lock. That is the state in
    // which an unrevalidated acquisition would have believed it held the
    // capture lock while a second holder ran concurrently on a fresh inode.
    std::fs::remove_file(&path).unwrap();
    assert_eq!(held.metadata().unwrap().nlink(), 0);
    assert!(!capture_lock_still_named(&held, &path).unwrap());

    // The next acquisition creates and locks a live inode, and its own
    // postcondition holds — so mutual exclusion is restored on the name rather
    // than silently split across two inodes.
    let fresh = executor.lock_capture(&request.identity).unwrap();
    assert_eq!(fresh.metadata().unwrap().nlink(), 1);
    assert!(capture_lock_still_named(&fresh, &path).unwrap());
    assert_ne!(
        fresh.metadata().unwrap().ino(),
        held.metadata().unwrap().ino()
    );
    // And the live lock does exclude: a further attempt hits the deadline.
    let contended = std::thread::scope(|scope| {
        scope
            .spawn(|| executor.lock_capture(&request.identity).map(|_| ()))
            .join()
            .unwrap()
    });
    assert!(
        matches!(contended, Err(ExecutorError::CaptureLockContended { .. })),
        "unexpected result {contended:?}"
    );
}

#[test]
fn hardening_presets_grant_the_capture_lock_directory_only_where_documented() {
    let state_dir = Path::new("/state tree");
    let executor = executor(state_dir);
    let lock_dir = state_dir.join(CAPTURE_LOCK_DIRECTORY);
    let lock = executor.capture_lock_path(&request().identity);
    assert!(lock.starts_with(&lock_dir));

    // Every variant, so a widened preset cannot silently reopen the surface the
    // relocation closed. `workspace` and `none` are documented exceptions — they
    // grant the state directory whole, or constrain nothing at all — and this
    // test asserts they still behave exactly that way rather than skipping them.
    for hardening in [
        AdapterHardening::Strict,
        AdapterHardening::Production,
        AdapterHardening::Workspace,
        AdapterHardening::None,
    ] {
        let mut request = request();
        request.hardening = hardening;
        request.workspace = Some(WorkspaceMetadata {
            repo: "acme/widgets".to_owned(),
            base_rev: "origin/main".to_owned(),
            branch: "tally/work".to_owned(),
            worktree_path: PathBuf::from("/work tree"),
        });
        let mut args = Vec::new();
        executor
            .push_hardening_properties(&mut args, &request)
            .unwrap();
        let writable = strings(&args)
            .into_iter()
            .find(|value| value.starts_with("ReadWritePaths="));

        let Some(writable) = writable else {
            // `none` emits no writable-path property at all: the job's
            // filesystem access is already unconstrained, so there is nothing
            // for the relocation to narrow.
            assert_eq!(hardening, AdapterHardening::None);
            continue;
        };
        let granted = writable
            .trim_start_matches("ReadWritePaths=")
            .split("\" \"")
            .map(|value| PathBuf::from(value.trim_matches('"')))
            .collect::<Vec<_>>();
        let containing = granted.iter().find(|path| lock.starts_with(path)).cloned();

        if hardening == AdapterHardening::Workspace {
            // The compatibility-era grant is the whole state directory, which
            // contains the lock. Documented in hardening.md as trusted-programs
            // only; the relocation moves that surface, it does not remove it.
            assert_eq!(
                containing,
                Some(state_dir.to_owned()),
                "workspace is expected to grant the state directory whole: {writable}"
            );
            continue;
        }

        assert!(
            !writable.contains(lock_dir.to_str().unwrap()),
            "{hardening:?} grants the capture lock directory: {writable}"
        );
        // `unit-exit` is granted whole — that is exactly why the lock left it.
        assert!(writable.contains(state_dir.join(UNIT_EXIT_DIRECTORY).to_str().unwrap()));
        // No granted path is an ancestor of the lock either: a job that could
        // write `capture/` could create the directory itself.
        assert!(
            containing.is_none(),
            "{hardening:?} grants {:?} which contains {}",
            containing,
            lock.display()
        );
    }
}

#[test]
fn parse_unit_accounting_reads_all_three_properties() {
    let accounting = parse_unit_accounting(
        "unit.service",
        b"CPUUsageNSec=1500000000\n\
          ExecMainStartTimestampMonotonic=1000000\n\
          ExecMainExitTimestampMonotonic=3500000\n",
    )
    .unwrap();
    assert_eq!(accounting.cpu_usage_nsec, Some(1_500_000_000));
    assert_eq!(accounting.exec_main_start_monotonic_usec, Some(1_000_000));
    assert_eq!(accounting.exec_main_exit_monotonic_usec, Some(3_500_000));
    assert_eq!(accounting.cpu_seconds(), Some(1.5));
    assert_eq!(accounting.wall_seconds(), Some(2.5));
}

#[test]
fn parse_unit_accounting_treats_not_set_as_typed_absence() {
    let accounting = parse_unit_accounting(
        "unit.service",
        b"CPUUsageNSec=[not set]\n\
          ExecMainStartTimestampMonotonic=[not set]\n\
          ExecMainExitTimestampMonotonic=[not set]\n",
    )
    .unwrap();
    assert_eq!(accounting, UnitAccounting::default());
    assert_eq!(accounting.cpu_seconds(), None);
    assert_eq!(accounting.wall_seconds(), None);
}

#[test]
fn parse_unit_accounting_never_computes_negative_wall_seconds_from_a_backwards_clock() {
    // Timestamps out of order are not something a real systemd emits, but a
    // parser that trusted them anyway would produce a nonsensical charge
    // rather than a typed absence. `checked_sub` inside `wall_seconds` is
    // exactly the guard this pins.
    let accounting = parse_unit_accounting(
        "unit.service",
        b"CPUUsageNSec=[not set]\n\
          ExecMainStartTimestampMonotonic=5000\n\
          ExecMainExitTimestampMonotonic=1000\n",
    )
    .unwrap();
    assert_eq!(accounting.wall_seconds(), None);
}

#[test]
fn parse_unit_accounting_rejects_malformed_and_unexpected_output() {
    assert!(parse_unit_accounting("unit.service", b"CPUUsageNSec=not-a-number\n").is_err());
    assert!(parse_unit_accounting("unit.service", b"not a line at all\n").is_err());
    assert!(parse_unit_accounting("unit.service", b"SomeOtherProperty=1\n").is_err());
    assert!(parse_unit_accounting("unit.service", b"CPUUsageNSec=1\nCPUUsageNSec=2\n").is_err());
}

#[test]
fn probe_unit_accounting_reports_a_typed_error_on_nonzero_exit() {
    let temp = tempfile::tempdir().unwrap();
    let systemctl = temp.path().join("fake-systemctl-fail");
    write_fake_program(&systemctl, "exit 1\n");
    assert!(probe_unit_accounting(&systemctl, "unit.service").is_err());
}

#[test]
fn probe_unit_accounting_reports_a_typed_error_when_the_binary_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("does-not-exist");
    assert!(probe_unit_accounting(&missing, "unit.service").is_err());
}

#[test]
fn probe_unit_accounting_issues_exactly_one_systemctl_show_with_the_accounting_properties() {
    let temp = tempfile::tempdir().unwrap();
    let systemctl = temp.path().join("fake-systemctl-accounting");
    let calls = temp.path().join("calls.txt");
    write_fake_program(
        &systemctl,
        &format!(
            r#"printf '%s\n' "$*" >> {calls}
echo "CPUUsageNSec=2000000000"
echo "ExecMainStartTimestampMonotonic=100"
echo "ExecMainExitTimestampMonotonic=100100"
"#,
            calls = calls.display()
        ),
    );
    let accounting = probe_unit_accounting(&systemctl, "tally-job-example.service").unwrap();
    assert_eq!(accounting.cpu_seconds(), Some(2.0));
    assert_eq!(accounting.wall_seconds(), Some(0.1));
    let calls = std::fs::read_to_string(&calls).unwrap();
    assert_eq!(
        calls.lines().count(),
        1,
        "expected exactly one systemctl invocation, got: {calls:?}"
    );
    assert!(calls.contains("--user show"));
    assert!(calls.contains("--property=CPUUsageNSec"));
    assert!(calls.contains("--property=ExecMainStartTimestampMonotonic"));
    assert!(calls.contains("--property=ExecMainExitTimestampMonotonic"));
    assert!(calls.contains("tally-job-example.service"));
}

fn write_fake_program(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(path, permissions).unwrap();
}

/// An `N-1` fixture: the exact `UnitExitRecord` shape written before #382,
/// with no `accounting` field in the JSON at all. A binary that gained the
/// field must still read a record an older binary wrote.
#[test]
fn a_pre_382_exit_record_with_no_accounting_field_still_parses() {
    let json = format!(
        r#"{{"schemaVersion":{version},"unit":"unit.service","invocationId":"id","attempt":1,"leaseEpoch":1,"serviceResult":"success","exitCode":"exited","exitStatus":"0"}}"#,
        version = UNIT_EXIT_SCHEMA_VERSION
    );
    let record: UnitExitRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(record.accounting, None);
    record.validate("unit.service").unwrap();

    // And the round trip: a record this binary writes with no measured
    // accounting serializes exactly like the pre-#382 shape, so a fleet mid
    // rollout never disagrees about what "unmeasured" looks like on disk.
    assert!(!serde_json::to_string(&record)
        .unwrap()
        .contains("accounting"));
}

#[test]
fn a_record_with_a_measured_accounting_sample_round_trips() {
    let record = UnitExitRecord {
        schema_version: UNIT_EXIT_SCHEMA_VERSION,
        unit: "unit.service".to_owned(),
        invocation_id: "id".to_owned(),
        attempt: 1,
        lease_epoch: 1,
        service_result: "success".to_owned(),
        exit_code: Some("exited".to_owned()),
        exit_status: Some("0".to_owned()),
        accounting: Some(UnitAccounting {
            cpu_usage_nsec: Some(1_500_000_000),
            exec_main_start_monotonic_usec: Some(1_000_000),
            exec_main_exit_monotonic_usec: Some(3_500_000),
        }),
    };
    record.validate("unit.service").unwrap();
    let json = serde_json::to_string(&record).unwrap();
    assert!(json.contains("\"cpuUsageNsec\":1500000000"));
    let round_tripped: UnitExitRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(round_tripped, record);
}
