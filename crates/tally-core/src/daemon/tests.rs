#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    use tempfile::tempdir;
    use tokio::net::UnixStream;

    use super::*;
    use crate::adapters::{
        AdapterConfig, AdapterTrace, ScrapeCapture, ScrapeMode, ScrapeStream, TraceFraming,
    };
    use crate::config::{
        CoResidencyPredicate, ExecutionTargetConfig, JournaldConfig, MeterBudgetClass, PoolConfig,
        PoolPredicate, ResourceKind, SshExecutorConfig, UsageMeterConfig,
    };
    use crate::evidence::{hash_artifact_file, RetryPolicy};
    use crate::executor::{
        read_exit_record, write_exit_record, ExecutionPaths, LocalUnitFact, LocalUnitProbe,
        LocalUnitState, RemoteCapture, RemoteCompletion, RemoteExecutorReply,
        RemoteExecutorRequest, RemoteExecutorResult, RemoteTransport, RemoteTransportError,
        UnitExitRecord, REMOTE_EXECUTOR_PROTOCOL_VERSION, UNIT_EXIT_SCHEMA_VERSION,
    };
    use crate::producers::{
        EmitOutcome, GhCliIntake, GhObservation, ProducerConfig, ProducerEngine,
        ReachabilityTransition,
    };
    use crate::recovery::RecoveryPlan;
    use crate::taskdb::{
        GhContextSnapshot, GhItemState, GhItemType, GhOrigin, WorkspaceMetadata,
        GH_CONTEXT_SCHEMA_VERSION, GH_ORIGIN_SCHEMA_VERSION,
    };
    use crate::witness::{Authorship, AuthorshipSession, AuthorshipStatus};
    use tally_client::RpcClient;

    #[test]
    fn dispatcher_methods_match_wire_inventory() {
        use crate::wire::{method_class, INTERNAL_RPC_METHODS, RPC_METHODS};

        let dispatched = DISPATCHER_METHODS
            .iter()
            .map(|(method, _)| *method)
            .collect::<BTreeSet<_>>();
        let inventoried = RPC_METHODS
            .iter()
            .chain(INTERNAL_RPC_METHODS)
            .copied()
            .collect::<BTreeSet<_>>();

        assert_eq!(dispatched.len(), DISPATCHER_METHODS.len());
        assert_eq!(inventoried.len(), RPC_METHODS.len() + INTERNAL_RPC_METHODS.len());
        assert_eq!(dispatched, inventoried);
        assert!(inventoried.iter().all(|method| method_class(method).is_some()));
    }

    struct ExitFileProbe;

    impl LocalUnitProbe for ExitFileProbe {
        fn inspect(
            &self,
            unit: &str,
            paths: &ExecutionPaths,
        ) -> Result<LocalUnitFact, ExecutorError> {
            if !paths.exit_record.exists() {
                return Ok(LocalUnitFact::absent(unit));
            }
            let record = read_exit_record(&paths.exit_record, unit)?;
            Ok(LocalUnitFact {
                unit: unit.to_owned(),
                loaded: false,
                state: LocalUnitState::Exited,
                invocation_id: Some(record.invocation_id.clone()),
                attempt: Some(record.attempt),
                lease_epoch: Some(record.lease_epoch),
                exit_record: Some(record),
            })
        }
    }

    struct AlwaysAvailableDerivation;

    impl DerivationAvailability for AlwaysAvailableDerivation {
        fn outputs_available_or_substitutable(&self, _drv: &Derivation) -> Result<bool, String> {
            Ok(true)
        }
    }

    struct NeverAvailableDerivation;

    impl DerivationAvailability for NeverAvailableDerivation {
        fn outputs_available_or_substitutable(&self, _drv: &Derivation) -> Result<bool, String> {
            Ok(false)
        }
    }

    struct RunningProbe {
        attempt: u32,
        lease_epoch: u64,
    }

    impl LocalUnitProbe for RunningProbe {
        fn inspect(
            &self,
            unit: &str,
            _paths: &ExecutionPaths,
        ) -> Result<LocalUnitFact, ExecutorError> {
            Ok(LocalUnitFact {
                unit: unit.to_owned(),
                loaded: true,
                state: LocalUnitState::Running,
                invocation_id: Some("restart-invocation".to_owned()),
                attempt: Some(self.attempt),
                lease_epoch: Some(self.lease_epoch),
                exit_record: None,
            })
        }
    }

    struct IntentObservingProbe {
        path: PathBuf,
        task_uuid: Uuid,
        inspections: Arc<AtomicUsize>,
    }

    impl LocalUnitProbe for IntentObservingProbe {
        fn inspect(
            &self,
            unit: &str,
            _paths: &ExecutionPaths,
        ) -> Result<LocalUnitFact, ExecutorError> {
            let intent =
                read_pool_loss_intent(&self.path).map_err(|error| ExecutorError::UnitProbe {
                    unit: unit.to_owned(),
                    detail: format!("pool-loss intent was not durable before reclaim: {error}"),
                })?;
            if intent.row.uuid != self.task_uuid {
                return Err(ExecutorError::UnitProbe {
                    unit: unit.to_owned(),
                    detail: "pool-loss intent names the wrong task generation".to_owned(),
                });
            }
            self.inspections.fetch_add(1, Ordering::SeqCst);
            Ok(LocalUnitFact::absent(unit))
        }
    }

    struct StallingCommitter {
        started: Option<oneshot::Sender<()>>,
        release: Arc<AtomicBool>,
    }

    impl ReplicaCommitter for StallingCommitter {
        fn commit<'a>(
            &'a mut self,
            _command: CommitCommand,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + 'a>> {
            let started = self.started.take();
            let release = self.release.clone();
            Box::pin(async move {
                if let Some(started) = started {
                    let _ = started.send(());
                }
                while !release.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(())
            })
        }
    }

    fn daemon_test_defaults() -> Config {
        let mut config = Config::default();
        // Most daemon tests use the Rust test harness as the executor binary.
        // The production wrapper is exercised independently with a real tally
        // binary, so keep these legacy fixtures focused on daemon behavior.
        config.attestations.exec.enable = false;
        config
    }

    fn one_pool_config() -> Config {
        Config {
            pools: BTreeMap::from([(
                "slot".to_owned(),
                PoolConfig {
                    resource: ResourceKind::BuildSlot,
                    predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                    ..PoolConfig::default()
                },
            )]),
            enqueue: Default::default(),
            lease: Default::default(),
            adapters: BTreeMap::from([("shell".to_owned(), AdapterConfig::default())]),
            producers: BTreeMap::new(),
            executors: BTreeMap::new(),
            journald: JournaldConfig { native: false },
            ..daemon_test_defaults()
        }
    }

    fn two_pool_config() -> Config {
        let mut config = one_pool_config();
        config.pools.insert(
            "zeta".to_owned(),
            PoolConfig {
                resource: ResourceKind::BuildSlot,
                predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                ..PoolConfig::default()
            },
        );
        config
    }

    fn window_pool_config() -> Config {
        Config {
            pools: BTreeMap::from([(
                "api".to_owned(),
                PoolConfig {
                    resource: ResourceKind::Budget,
                    predicate: PoolPredicate::WindowedConsumption(
                        crate::config::WindowedConsumptionPredicate {
                            window_sec: 60,
                            consumption_cap: 100,
                        },
                    ),
                    ..PoolConfig::default()
                },
            )]),
            enqueue: Default::default(),
            lease: Default::default(),
            adapters: BTreeMap::from([("shell".to_owned(), AdapterConfig::default())]),
            producers: BTreeMap::new(),
            executors: BTreeMap::new(),
            journald: JournaldConfig { native: false },
            ..daemon_test_defaults()
        }
    }

    fn hard_preempt_config() -> Config {
        let mut config = one_pool_config();
        config.pools.get_mut("slot").unwrap().hard_preempt = true;
        config
    }

    fn remote_config() -> Config {
        let mut config = one_pool_config();
        config.executors.insert(
            "worker".to_owned(),
            ExecutionTargetConfig::Ssh(SshExecutorConfig {
                host: "worker.example".to_owned(),
                user: "tally-worker".to_owned(),
                port: 22,
                ssh_program: PathBuf::from("/run/current-system/sw/bin/ssh"),
                identity_file: PathBuf::from("/run/credentials/tally-worker-key"),
                known_hosts_file: PathBuf::from("/etc/tally/worker-known-hosts"),
                program: PathBuf::from("/run/current-system/sw/bin/tally"),
                state_dir: PathBuf::from("/var/lib/tally-remote"),
                connect_timeout_sec: 3,
                server_alive_interval_sec: 2,
                server_alive_count_max: 2,
                retry_interval_ms: 10,
            }),
        );
        config
    }

    #[derive(Clone)]
    struct RecoveringRemoteTransport {
        calls: Arc<AtomicUsize>,
        release: Arc<AtomicBool>,
    }

    impl RemoteTransport for RecoveringRemoteTransport {
        fn call<'a>(
            &'a self,
            _config: &'a SshExecutorConfig,
            request: RemoteExecutorRequest,
        ) -> Pin<
            Box<dyn Future<Output = Result<RemoteExecutorReply, RemoteTransportError>> + Send + 'a>,
        > {
            let calls = self.calls.clone();
            let release = self.release.clone();
            Box::pin(async move {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                if call == 0 {
                    return Err(RemoteTransportError {
                        detail: "simulated SSH interruption after launch".to_owned(),
                    });
                }
                while !release.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                let RemoteExecutorRequest::Ensure {
                    request, evidence, ..
                } = request
                else {
                    return Err(RemoteTransportError {
                        detail: "unexpected remote operation".to_owned(),
                    });
                };
                let unit = format!("tally-job-{}.service", request.identity.unit_uuid());
                let evidence =
                    parse_evidence_specs(&evidence).map_err(|error| RemoteTransportError {
                        detail: error.to_string(),
                    })?;
                Ok(RemoteExecutorReply::Ok {
                    protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                    result: Box::new(RemoteExecutorResult::Completion(Box::new(
                        RemoteCompletion {
                            unit: unit.clone(),
                            record: UnitExitRecord {
                                schema_version: UNIT_EXIT_SCHEMA_VERSION,
                                unit,
                                invocation_id: "remote-long-job".to_owned(),
                                attempt: request.attempt,
                                lease_epoch: request.lease_epoch,
                                service_result: "success".to_owned(),
                                exit_code: Some("exited".to_owned()),
                                exit_status: Some("0".to_owned()),
                            },
                            termination: ExecutionTermination::Exited(0),
                            capture: RemoteCapture {
                                attempt: request.attempt,
                                lease_epoch: request.lease_epoch,
                                stdout_base64: Some(String::new()),
                                stderr_base64: Some(String::new()),
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
                            host_id: Some("worker".to_owned()),
                        },
                    ))),
                })
            })
        }
    }

    #[derive(Clone)]
    struct RestartRemoteTransport {
        calls: Arc<std::sync::Mutex<Vec<RemoteExecutorRequest>>>,
        attempt: u32,
        lease_epoch: u64,
    }

    impl RemoteTransport for RestartRemoteTransport {
        fn call<'a>(
            &'a self,
            _config: &'a SshExecutorConfig,
            request: RemoteExecutorRequest,
        ) -> Pin<
            Box<dyn Future<Output = Result<RemoteExecutorReply, RemoteTransportError>> + Send + 'a>,
        > {
            let calls = self.calls.clone();
            let attempt = self.attempt;
            let lease_epoch = self.lease_epoch;
            Box::pin(async move {
                calls.lock().unwrap().push(request.clone());
                let result = match request {
                    RemoteExecutorRequest::Probe { identity, .. } => {
                        let unit = format!("tally-job-{}.service", identity.unit_uuid());
                        RemoteExecutorResult::Fact(LocalUnitFact {
                            unit,
                            loaded: true,
                            state: LocalUnitState::Running,
                            invocation_id: Some("restart-remote-invocation".to_owned()),
                            attempt: Some(attempt),
                            lease_epoch: Some(lease_epoch),
                            exit_record: None,
                        })
                    }
                    RemoteExecutorRequest::Adopt {
                        request,
                        expected_invocation_id,
                        evidence,
                        ..
                    } => {
                        if expected_invocation_id != "restart-remote-invocation" {
                            return Ok(RemoteExecutorReply::Error {
                                protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                                message: "unexpected adoption identity".to_owned(),
                            });
                        }
                        let evidence = parse_evidence_specs(&evidence).map_err(|error| {
                            RemoteTransportError {
                                detail: error.to_string(),
                            }
                        })?;
                        let unit = format!("tally-job-{}.service", request.identity.unit_uuid());
                        RemoteExecutorResult::Completion(Box::new(RemoteCompletion {
                            unit: unit.clone(),
                            record: UnitExitRecord {
                                schema_version: UNIT_EXIT_SCHEMA_VERSION,
                                unit,
                                invocation_id: expected_invocation_id,
                                attempt: request.attempt,
                                lease_epoch: request.lease_epoch,
                                service_result: "success".to_owned(),
                                exit_code: Some("exited".to_owned()),
                                exit_status: Some("0".to_owned()),
                            },
                            termination: ExecutionTermination::Exited(0),
                            capture: RemoteCapture {
                                attempt: request.attempt,
                                lease_epoch: request.lease_epoch,
                                stdout_base64: Some(String::new()),
                                stderr_base64: Some(String::new()),
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
                            host_id: Some("worker".to_owned()),
                        }))
                    }
                    RemoteExecutorRequest::Ensure { .. } => {
                        return Ok(RemoteExecutorReply::Error {
                            protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                            message: "restart attempted a duplicate launch".to_owned(),
                        });
                    }
                    RemoteExecutorRequest::Reclaim { .. } => {
                        return Ok(RemoteExecutorReply::Error {
                            protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                            message: "unexpected reclaim".to_owned(),
                        });
                    }
                };
                Ok(RemoteExecutorReply::Ok {
                    protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                    result: Box::new(result),
                })
            })
        }
    }

    fn structured_adapter(program: &Path) -> AdapterConfig {
        AdapterConfig {
            argv: vec![
                program.to_string_lossy().into_owned(),
                "--structured".to_owned(),
            ],
            resume: Some(vec![
                program.to_string_lossy().into_owned(),
                "--resume".to_owned(),
                "%<sessionRef>%".to_owned(),
                "--model".to_owned(),
                "%<model>%".to_owned(),
            ]),
            scrape: BTreeMap::from([
                (
                    "branch".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stderr,
                        mode: ScrapeMode::Regex,
                        pattern: "(?m)^branch=(.+)$".to_owned(),
                    },
                ),
                (
                    "model".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..model".to_owned(),
                    },
                ),
                (
                    "sessionRef".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..session_id".to_owned(),
                    },
                ),
                (
                    "usage".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..usage".to_owned(),
                    },
                ),
                (
                    "finalMessage".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..final_message".to_owned(),
                    },
                ),
            ]),
            trace: None,
            yield_hook: Some(vec![
                "tally".to_owned(),
                "lease".to_owned(),
                "status".to_owned(),
            ]),
            env: BTreeMap::from([("CUSTOM_AGENT_MODE".to_owned(), "batch".to_owned())]),
            launch: crate::adapters::AdapterLaunchConfig::default(),
            hardening: Default::default(),
            extra_writable_paths: Vec::new(),
            skill_bundle: None,
            skill_revision: None,
            extra_config: BTreeMap::from([(
                "modelFlag".to_owned(),
                Value::String("--model".to_owned()),
            )]),
        }
    }

    fn settings() -> DaemonSettings {
        DaemonSettings {
            unit_limits: UnitLimits {
                cpu_weight: 100,
                memory_max_bytes: 64 * 1024 * 1024,
            },
            yield_grace: Duration::from_secs(1),
            recovery_policy: RecoveryPolicy {
                retry: RetryPolicy {
                    auto_pool_return: false,
                    auto_resource_return: false,
                    auto_bounded_requeue: false,
                },
                max_attempts: 1,
            },
            max_connections: DEFAULT_MAX_CONNECTIONS,
        }
    }

    fn fs1_paths(root: &Path) -> DaemonPaths {
        DaemonPaths {
            socket: root.join("run/tally.sock"),
            state_dir: root.join("state"),
            data_dir: root.join("data"),
        }
    }

    fn initialize_final_witness_state(paths: &DaemonPaths) {
        prepare_paths(paths).unwrap();
        drop(WitnessLedger::open(paths.witness_path()).unwrap());
    }

    async fn fs1_daemon(paths: &DaemonPaths) -> Daemon {
        let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
            .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
            .with_unit_probe(ExitFileProbe);
        Daemon::open_with_executor(one_pool_config(), paths.clone(), settings(), executor)
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn local_execution_token_is_durable_resolvable_remote_safe_and_revocable() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "priority": "high",
                        "adapter": "shell",
                        "source": "orchestrator",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let job_id = Uuid::parse_str(admitted["job_id"].as_str().unwrap()).unwrap();
                let mut job = {
                    let mut context = daemon.handler.context.write().await;
                    let stored = context.jobs.get_mut(&job_id).unwrap();
                    assert_eq!(stored.state, JobState::Paused);
                    stored.state = JobState::Running;
                    stored.clone()
                };

                let token = daemon
                    .handler
                    .prepare_execution(&mut job)
                    .await
                    .unwrap()
                    .unwrap()
                    .job_token
                    .unwrap();
                assert_eq!(token.len(), 64);
                assert!(token
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
                let digest = hash_job_token(&token);
                assert_eq!(
                    daemon.handler.job_tokens.borrow().get(&digest),
                    Some(&job_id)
                );
                assert_eq!(job.row.job_token_hash.as_deref(), Some(digest.as_str()));
                assert_eq!(
                    daemon.handler.context.read().await.rows[&job_id]
                        .job_token_hash
                        .as_deref(),
                    Some(digest.as_str())
                );

                let events = read_acknowledged_events(&paths.events_dir()).unwrap();
                assert_eq!(events.len(), 1);
                assert_eq!(
                    events[0].row.job_token_hash.as_deref(),
                    Some(digest.as_str())
                );
                assert!(!serde_json::to_string(&events[0]).unwrap().contains(&token));

                let request = execution_request(
                    &daemon.handler.executor,
                    &job,
                    settings().unit_limits,
                    ("/run/tally/tally.sock", Some(&token)),
                    &paths.data_dir,
                    &GitAiConfig::default(),
                    false,
                )
                .unwrap();
                assert_eq!(
                    request.tally_socket.as_deref(),
                    Some("/run/tally/tally.sock")
                );
                assert_eq!(request.job_token.as_deref(), Some(token.as_str()));

                let mut remote = job.clone();
                remote.row.executor = Some("remote-worker".to_owned());
                let remote_request = execution_request(
                    &daemon.handler.executor,
                    &remote,
                    settings().unit_limits,
                    ("/run/tally/tally.sock", Some(&token)),
                    &paths.data_dir,
                    &GitAiConfig::default(),
                    false,
                )
                .unwrap();
                assert!(remote_request.tally_socket.is_none());
                assert!(remote_request.job_token.is_none());

                daemon.handler.revoke_job_token(&job);
                assert!(!daemon.handler.job_tokens.borrow().contains_key(&digest));
            })
            .await;
    }

    async fn wait_for_connection_count(
        counts: &mut mpsc::UnboundedReceiver<usize>,
        expected: usize,
    ) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let count = counts
                    .recv()
                    .await
                    .expect("connection count hook must remain open");
                if count == expected {
                    break;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("connection count did not reach {expected}"));
    }

    #[test]
    fn retryable_accept_errors_are_explicit() {
        for errno in [libc::EMFILE, libc::ENFILE, libc::ECONNABORTED, libc::EINTR] {
            assert!(retryable_accept_error(&io::Error::from_raw_os_error(errno)));
        }
        assert!(!retryable_accept_error(&io::Error::from_raw_os_error(
            libc::EINVAL
        )));
    }

    #[test]
    fn daemon_settings_reject_zero_max_connections() {
        let mut invalid = settings();
        invalid.max_connections = 0;
        assert!(matches!(
            invalid.validate(),
            Err(DaemonError::Invalid(message)) if message == "max connections must be positive"
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn completed_connections_are_reaped() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let mut daemon = fs1_daemon(&paths).await;
                let (count_tx, mut count_rx) = mpsc::unbounded_channel();
                daemon.connection_count_hook = Some(count_tx);
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));

                let mut clients = Vec::new();
                for _ in 0..10 {
                    clients.push(UnixStream::connect(&paths.socket).await.unwrap());
                }
                wait_for_connection_count(&mut count_rx, 10).await;

                drop(clients);
                wait_for_connection_count(&mut count_rx, 0).await;

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn max_connections_defers_serving_until_a_slot_opens() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let mut daemon = fs1_daemon(&paths).await;
                daemon.handler.settings.max_connections = 2;
                let (count_tx, mut count_rx) = mpsc::unbounded_channel();
                daemon.connection_count_hook = Some(count_tx);
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));

                let first = UnixStream::connect(&paths.socket).await.unwrap();
                let second = UnixStream::connect(&paths.socket).await.unwrap();
                wait_for_connection_count(&mut count_rx, 2).await;

                let third = RpcClient::connect(&paths.socket).await.unwrap();
                let mut third_call = tokio::task::spawn_local(async move {
                    third.call("query.status", Some(json!({}))).await
                });
                assert!(
                    tokio::time::timeout(Duration::from_millis(50), &mut third_call)
                        .await
                        .is_err(),
                    "a connection beyond the cap was served before a slot opened"
                );

                drop(first);
                tokio::time::timeout(Duration::from_secs(1), &mut third_call)
                    .await
                    .expect("the deferred connection must be served after a slot opens")
                    .unwrap()
                    .unwrap();

                drop(second);
                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn old_format_witness_refuses_daemon_boot_with_archive_instruction() {
        let temp = tempdir().unwrap();
        let paths = fs1_paths(temp.path());
        fs::create_dir_all(&paths.data_dir).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../test/fixtures/ledger/old-format.jsonl"),
            paths.witness_path(),
        )
        .unwrap();
        let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
            .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
            .with_unit_probe(ExitFileProbe);

        let error = match Daemon::open_with_executor(
            one_pool_config(),
            paths.clone(),
            settings(),
            executor,
        )
        .await
        {
            Ok(_) => panic!("daemon unexpectedly booted over an old-format witness ledger"),
            Err(error) => error,
        };

        assert!(error
            .to_string()
            .contains("archive it aside before first boot: mv --"));
        match error {
            DaemonError::Witness(WitnessError::OldFormat { path, archive }) => {
                assert_eq!(path, paths.witness_path());
                assert!(archive
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("witness.jsonl.pre-"));
            }
            other => panic!("expected typed old-format witness error, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn old_format_events_refuse_first_boot_without_reading_or_mutating_them() {
        let temp = tempdir().unwrap();
        let paths = fs1_paths(temp.path());
        fs::create_dir_all(paths.events_dir()).unwrap();
        let legacy_event = paths.events_dir().join("legacy.enqueue.json");
        let legacy_bytes = b"not even parseable legacy event bytes\n";
        fs::write(&legacy_event, legacy_bytes).unwrap();
        let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
            .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
            .with_unit_probe(ExitFileProbe);

        let error = match Daemon::open_with_executor(
            one_pool_config(),
            paths.clone(),
            settings(),
            executor,
        )
        .await
        {
            Ok(_) => panic!("daemon unexpectedly booted over old-format events"),
            Err(error) => error,
        };

        assert!(error
            .to_string()
            .contains("archive it aside before first boot: mv --"));
        match error {
            DaemonError::OldFormatEvents { path, archive } => {
                assert_eq!(path, paths.events_dir());
                assert!(archive
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("events.pre-"));
            }
            other => panic!("expected typed old-format events error, got {other:?}"),
        }
        assert_eq!(fs::read(legacy_event).unwrap(), legacy_bytes);
        assert!(!paths.witness_path().exists());
        drop(acquire_daemon_lock(&paths.state_dir).unwrap());
    }

    #[test]
    fn rejected_startup_explicitly_unlocks_before_an_inherited_duplicate_closes() {
        let temp = tempdir().unwrap();
        let paths = fs1_paths(temp.path());
        prepare_paths(&paths).unwrap();
        let startup_lock = DaemonLockGuard::acquire(&paths.state_dir).unwrap();
        let inherited = startup_lock.file().try_clone().unwrap();

        drop(startup_lock);
        drop(acquire_daemon_lock(&paths.state_dir).unwrap());
        drop(inherited);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opened_daemon_explicitly_unlocks_before_an_inherited_duplicate_closes() {
        let temp = tempdir().unwrap();
        let paths = fs1_paths(temp.path());
        let daemon = fs1_daemon(&paths).await;
        let inherited = daemon._state_lock.file().try_clone().unwrap();

        drop(daemon);
        drop(acquire_daemon_lock(&paths.state_dir).unwrap());
        drop(inherited);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn empty_events_initialize_the_ledger_and_later_events_survive_restart() {
        let temp = tempdir().unwrap();
        let paths = fs1_paths(temp.path());
        fs::create_dir_all(paths.events_dir()).unwrap();

        let daemon = fs1_daemon(&paths).await;
        assert!(paths.witness_path().is_file());
        assert_eq!(paths.witness_path().metadata().unwrap().len(), 0);
        drop(daemon);

        let row = durable_row(Uuid::new_v4(), "post-cutover-pending", 1);
        write_enqueue_event_atomic(
            &paths.events_dir(),
            &DurableEnqueueEvent::new(row.clone()).unwrap(),
        )
        .unwrap();

        let restarted = fs1_daemon(&paths).await;
        assert!(restarted
            .initial_jobs
            .iter()
            .any(|job| job.task_uuid == Some(row.uuid)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn authorship_projection_reconstructs_from_durable_row_and_witness_after_restart() {
        let temp = tempdir().unwrap();
        let paths = fs1_paths(temp.path());
        prepare_paths(&paths).unwrap();
        let task_uuid = Uuid::new_v4();
        let mut row = durable_row(task_uuid, "authorship-query-restart", 1);
        row.adapter_options.model = Some("tally-model".to_owned());
        row.session_ref = Some("tally-session".to_owned());
        row.workspace = Some(WorkspaceMetadata {
            repo: "mecattaf/tally.nix".to_owned(),
            base_rev: "a".repeat(40),
            branch: "authorship-query-restart".to_owned(),
            worktree_path: temp.path().join("worktree"),
        });
        write_enqueue_event_atomic(
            &paths.events_dir(),
            &DurableEnqueueEvent::new(row.clone()).unwrap(),
        )
        .unwrap();
        WitnessLedger::open(paths.witness_path())
            .unwrap()
            .append(WitnessBody {
                task_uuid: Some(task_uuid.to_string()),
                transition_timestamp: "2026-07-26T20:00:00.000Z".to_owned(),
                verdict: Verdict::Pass,
                exit_code: 0,
                artifact_content_hash: None,
                store_paths: None,
                drv: None,
                gpu_seconds: None,
                wall_clock: 1.0,
                attempt: 1,
                lease_epoch: 1,
                dedup_key: row.dedup_key.clone(),
                payload_hash: row.payload_hash.clone(),
                brief_hash: None,
                origin: AdmissionOrigin::direct(EnqueueSource::Manual),
                orchestration: None,
                labor_class: LaborClass::Fresh,
                trace_ref: None,
                pools: vec!["slot".to_owned()],
                executor: None,
                host_id: None,
                charge: None,
                model: Some("tally-model".to_owned()),
                evidence_class: None,
                manifest_hash: None,
                completion: None,
                result_revision: Some("b".repeat(40)),
                authorship: Some(Authorship {
                    provider: "git-ai".to_owned(),
                    provider_version: "1.6.17".to_owned(),
                    note_ref: "refs/notes/ai".to_owned(),
                    status: AuthorshipStatus::Mismatch,
                    notes_ref_target: Some("c".repeat(40)),
                    note_content_sha256: Some(format!("sha256:{}", "d".repeat(64))),
                    reason: Some(
                        "git-ai-mismatch: Tally session/model differs from Git AI's correlated attribution"
                            .to_owned(),
                    ),
                }),
                authorship_sessions: Some(vec![AuthorshipSession {
                    tool: "codex".to_owned(),
                    id: "git-ai-session".to_owned(),
                    model: "git-ai-model".to_owned(),
                }]),
            })
            .unwrap();

        let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
            .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
            .with_unit_probe(ExitFileProbe);
        let first = Daemon::open_with_executor(
            one_pool_config(),
            paths.clone(),
            settings(),
            executor.clone(),
        )
        .await
        .unwrap();
        let before = first
            .handler
            .query("query.job", Some(json!({"id": task_uuid})))
            .await
            .unwrap();
        drop(first);

        let restarted = Daemon::open_with_executor(one_pool_config(), paths, settings(), executor)
            .await
            .unwrap();
        let after = restarted
            .handler
            .query("query.job", Some(json!({"id": task_uuid})))
            .await
            .unwrap();
        assert_eq!(after["protocolVersion"], 4);
        assert_eq!(after["job"]["authorship"], before["job"]["authorship"]);
        assert_eq!(
            after["job"]["authorship"]["workspace"]["value"]["repo"],
            "mecattaf/tally.nix"
        );
        assert_eq!(
            after["job"]["authorship"]["tallySession"]["value"],
            "tally-session"
        );
        assert_eq!(
            after["job"]["authorship"]["tallySession"]["authority"],
            "advisory-provider-capture"
        );
        assert_eq!(
            after["job"]["authorship"]["gitAiSessions"][0]["value"]["id"],
            "git-ai-session"
        );
        assert_eq!(
            after["job"]["authorship"]["gitAiSessions"][0]["authority"],
            "canonical-witness-fact"
        );
        assert_eq!(
            after["job"]["authorship"]["identityMismatch"],
            Value::Bool(true)
        );
        let proof = restarted
            .handler
            .query(
                "query.proof",
                Some(json!({"task": task_uuid, "attempt": 1})),
            )
            .await
            .unwrap();
        assert_eq!(proof["authorship"], after["job"]["authorship"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn substituted_drv_witness_skips_every_admission_surface_and_meter() {
        const DRV: &str = "/nix/store/00000000000000000000000000000000-node.drv";
        const OUT: &str = "/nix/store/11111111111111111111111111111111-node";

        let temp = tempdir().unwrap();
        let paths = fs1_paths(temp.path());
        let mut config = one_pool_config();
        config.pools.insert(
            "build".to_owned(),
            PoolConfig {
                resource: ResourceKind::BuildSlot,
                predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                ..PoolConfig::default()
            },
        );
        let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
            .with_systemd_run(temp.path().join("absent-systemd-run"))
            .with_unit_probe(ExitFileProbe);
        let daemon = Daemon::open_with_executor(config, paths.clone(), settings(), executor)
            .await
            .unwrap();
        daemon.handler.context.write().await.derivation_store = Arc::new(AlwaysAvailableDerivation);

        let task_uuid = "00000000-0000-4000-8000-000000000084";
        let response = daemon
            .handler
            .enqueue(Some(json!({
                "argv": ["nix", "build", "--no-link", format!("{DRV}^*")],
                "pool": ["build"],
                "adapter": "shell",
                "source": "orchestrator",
                "dedupKey": format!("drv:{DRV}"),
                "submission": {"mode": "full"},
                "evidence": [format!("store:{OUT}")],
                "drv": {
                    "drvPath": DRV,
                    "outputs": [{"name": "out", "path": OUT}]
                },
                "taskUuid": task_uuid,
                "orchestration": {
                    "flowRunId": "00000000-0000-4000-8000-000000000071"
                }
            })))
            .await
            .unwrap();
        assert_eq!(response["disposition"], "substituted");
        assert_eq!(response["taskUuid"], task_uuid);

        let context = daemon.handler.context.read().await;
        assert!(context.rows.is_empty());
        assert!(context.jobs.is_empty());
        assert!(context.query_rows.is_empty());
        assert!(read_acknowledged_events(&paths.events_dir())
            .unwrap()
            .is_empty());
        drop(context);

        let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.verdict, Verdict::Substituted);
        assert_eq!(record.labor_class, LaborClass::Substituted);
        assert_eq!(record.task_uuid.as_deref(), Some(task_uuid));
        assert_eq!(record.pools, ["build"]);
        assert_eq!(
            record.dedup_key.as_deref(),
            Some(format!("drv:{DRV}").as_str())
        );
        assert_eq!(record.store_paths, Some(vec![OUT.to_owned()]));
        assert_eq!(record.drv.as_ref().unwrap().drv_path, DRV);
        assert_eq!(record.wall_clock, 0.0);
        assert_eq!(record.attempt, 1);
        assert_eq!(record.lease_epoch, 1);
        assert!(record.gpu_seconds.is_none());
        assert!(record.charge.is_none());
        assert!(!crate::witness::counts_toward_canonical_gpu_seconds(record));

        {
            let mut context = daemon.handler.context.write().await;
            context.derivation_store = Arc::new(NeverAvailableDerivation);
            context.paused_pools.insert("build".to_owned());
        }
        let fresh_uuid = "00000000-0000-4000-8000-000000000085";
        let fresh = tokio::task::LocalSet::new()
            .run_until(daemon.handler.enqueue(Some(json!({
                "argv": ["nix", "build", "--no-link", format!("{DRV}^*")],
                "pool": ["build"],
                "adapter": "shell",
                "source": "orchestrator",
                "dedupKey": format!("drv:{DRV}"),
                "submission": {"mode": "full"},
                "evidence": [format!("store:{OUT}")],
                "drv": {
                    "drvPath": DRV,
                    "outputs": [{"name": "out", "path": OUT}]
                },
                "taskUuid": fresh_uuid,
                "orchestration": {
                    "flowRunId": "00000000-0000-4000-8000-000000000071"
                }
            }))))
            .await
            .unwrap();
        assert_eq!(fresh["disposition"], "created");
        assert_eq!(fresh["reusedRejected"], "store-path-invalid");
        let context = daemon.handler.context.read().await;
        assert!(context
            .rows
            .contains_key(&Uuid::parse_str(fresh_uuid).unwrap()));
        assert_eq!(
            read_acknowledged_events(&paths.events_dir()).unwrap().len(),
            1
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn non_empty_current_state_migrates_before_recovery_and_is_restart_stable() {
        let temp = tempdir().unwrap();
        let paths = fs1_paths(temp.path());

        let initialized = fs1_daemon(&paths).await;
        assert!(paths.witness_path().is_file());
        drop(initialized);

        let legacy = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test/fixtures/ledger/events/legacy-no-origin.enqueue.json"
        ))
        .replace("\"pool\": \"worker-gpu\"", "\"pool\": \"slot\"")
        .replace("\"leaseEpoch\": 7", "\"leaseEpoch\": 1");
        assert!(!legacy.contains("\"rowVersion\""));
        assert!(!legacy.contains("\"origin\""));
        fs::create_dir_all(paths.events_dir()).unwrap();
        let event_path = paths
            .events_dir()
            .join("00000000-0000-4000-8000-000000000301.enqueue.json");
        fs::write(&event_path, legacy).unwrap();

        let restarted = fs1_daemon(&paths).await;
        let row_uuid = Uuid::parse_str("00000000-0000-4000-8000-000000000311").unwrap();
        assert!(restarted
            .initial_jobs
            .iter()
            .any(|job| job.task_uuid == Some(row_uuid)));
        let migrated = fs::read(&event_path).unwrap();
        let event = read_acknowledged_events(&paths.events_dir()).unwrap();
        assert_eq!(event.len(), 1);
        assert_eq!(event[0].row.row_version, crate::taskdb::CURRENT_ROW_VERSION);
        assert_eq!(
            event[0].row.origin,
            Some(AdmissionOrigin::direct(EnqueueSource::Calendar))
        );
        drop(restarted);

        let second_restart = fs1_daemon(&paths).await;
        assert!(second_restart
            .initial_jobs
            .iter()
            .any(|job| job.task_uuid == Some(row_uuid)));
        assert_eq!(fs::read(event_path).unwrap(), migrated);
    }

    fn fs1_full_payload(
        dedup_key: &str,
        argv: &[&str],
        evidence: impl IntoIterator<Item = String>,
    ) -> Value {
        json!({
            "argv": argv,
            "pool": "slot",
            "priority": "high",
            "adapter": "shell",
            "source": "manual",
            "dedupKey": dedup_key,
            "submission": {"mode": "full"},
            "evidence": evidence.into_iter().collect::<Vec<_>>(),
        })
    }

    async fn fs1_wait(client: &RpcClient, response: &Value) -> Value {
        client
            .call(
                "queue.await_job",
                Some(json!({"task_uuid": response["task_uuid"]})),
            )
            .await
            .unwrap()
    }

    fn fs1_conflict(error: WireIoError) -> Value {
        match error {
            WireIoError::Rpc(WireErrorCode::DedupKeyConflict, _, Some(data)) => data,
            other => panic!("expected dedup-key-conflict, got {other:?}"),
        }
    }

    fn durable_row(uuid: Uuid, dedup_key: &str, lease_epoch: u64) -> RowSeed {
        RowSeed {
            row_version: crate::taskdb::CURRENT_ROW_VERSION,
            uuid,
            description: "durable reuse fixture".to_owned(),
            priority: Priority::High,
            source: EnqueueSource::Manual,
            adapter: "shell".to_owned(),
            pools: vec!["slot".to_owned()],
            executor: None,
            model: None,
            cwd: None,
            workspace: None,
            adapter_options: Default::default(),
            gate_manifest: None,
            resumed_from: None,
            dedup_key: Some(dedup_key.to_owned()),
            payload_hash: None,
            brief_hash: None,
            orchestration: None,
            session_ref: None,
            final_message: None,
            job_token_hash: None,
            lease_epoch,
            attempt: 1,
            argv: vec!["true".to_owned()],
            evidence: vec!["exit:0".to_owned()],
            drv: None,
            parent_uuid: None,
            consumption_estimate: None,
            runtime_max_sec: None,
            no_enqueue: false,
            credentials: BTreeMap::new(),
            origin: Some(AdmissionOrigin::direct(EnqueueSource::Manual)),
            gh_origin: None,
            related_trigger: None,
            evidence_class: None,
            manifest_hash: None,
        }
    }

    fn append_history_event(
        store: &mut LifecycleStore,
        row: &RowSeed,
        event: TallyEvent,
        attempt: u32,
        lease_epoch: u64,
        realtime_us: u64,
    ) {
        let terminal = matches!(event, TallyEvent::Completed | TallyEvent::Failed);
        let fields = EmitEvent {
            event,
            task_uuid: row.uuid.to_string(),
            class: row.priority,
            source: row.source,
            message: Some(format!("fixture {event} attempt={attempt}")),
            agent: Some(row.adapter.clone()),
            session_ref: row.session_ref.clone(),
            unit: Some(format!("tally-job-{}.service", row.uuid)),
            exit_code: terminal.then_some(if event == TallyEvent::Completed { 0 } else { 1 }),
            gpu_seconds: terminal.then_some(0.0),
            artifact_hash: (event == TallyEvent::Completed)
                .then(|| format!("sha256:{}", "a".repeat(64))),
            evidence: event.is_evidence().then(|| "exit:0".to_owned()),
            attempt: Some(attempt),
            lease_epoch: Some(lease_epoch),
            labor_class: Some(if attempt == 1 {
                LaborClass::Fresh
            } else {
                LaborClass::Recovered
            }),
            job_id: Some(row.uuid.to_string()),
            parent: row.parent_uuid.map(|uuid| uuid.to_string()),
            pools: Some(row.pools.clone()),
            executor: row.executor.clone(),
        }
        .into_fields()
        .unwrap();
        store.append_at(fields, realtime_us).unwrap();
    }

    fn append_fixture_witness(
        ledger: &mut WitnessLedger,
        row: &RowSeed,
        timestamp: &str,
        verdict: Verdict,
        exit_code: i32,
        attempt: u32,
        lease_epoch: u64,
    ) -> WitnessRecord {
        ledger
            .append(WitnessBody {
                task_uuid: Some(row.uuid.to_string()),
                transition_timestamp: timestamp.to_owned(),
                verdict,
                exit_code,
                artifact_content_hash: (verdict == Verdict::Pass)
                    .then(|| format!("sha256:{}", "a".repeat(64))),
                store_paths: None,
                drv: None,
                gpu_seconds: Some(f64::from(attempt)),
                wall_clock: 10.0 + f64::from(attempt),
                attempt,
                lease_epoch,
                dedup_key: row.dedup_key.clone(),
                payload_hash: row.payload_hash.clone(),
                brief_hash: row.brief_hash.clone(),
                origin: row
                    .origin
                    .clone()
                    .expect("fixture row carries admission origin"),
                orchestration: row.orchestration.clone(),
                labor_class: if attempt == 1 {
                    LaborClass::Fresh
                } else {
                    LaborClass::Recovered
                },
                trace_ref: None,
                pools: row.pools.clone(),
                executor: row.executor.clone(),
                host_id: None,
                charge: None,
                model: None,
                evidence_class: Some(json!({"fixture": "acceptance-24"})),
                manifest_hash: Some(json!("sha256:fixture-manifest")),
                completion: None,
                result_revision: None,
                authorship: None,
                authorship_sessions: None,
            })
            .unwrap()
    }

    fn seed_durable_query_fixture(
        root: &Path,
    ) -> (DaemonPaths, Uuid, Uuid, WitnessRecord, WitnessRecord) {
        let paths = DaemonPaths {
            socket: root.join("run/tally.sock"),
            state_dir: root.join("state"),
            data_dir: root.join("data"),
        };
        prepare_paths(&paths).unwrap();
        // Simulate the epochs owned by the two recorded attempts. The daemon
        // opened by the acceptance test is therefore a later restart.
        assert_eq!(bump_epoch(&paths.state_dir).unwrap(), 1);
        assert_eq!(bump_epoch(&paths.state_dir).unwrap(), 2);

        let parent_uuid = Uuid::new_v4();
        let child_uuid = Uuid::new_v4();
        let mut parent = durable_row(parent_uuid, "acceptance-parent", 1);
        parent.description = "acceptance parent".to_owned();
        let mut child = durable_row(child_uuid, "acceptance-child", 1);
        child.description = "acceptance child".to_owned();
        child.parent_uuid = Some(parent_uuid);
        write_enqueue_event_atomic(
            &paths.events_dir(),
            &DurableEnqueueEvent::new(parent.clone()).unwrap(),
        )
        .unwrap();
        write_enqueue_event_atomic(
            &paths.events_dir(),
            &DurableEnqueueEvent::new(child.clone()).unwrap(),
        )
        .unwrap();

        let mut history = LifecycleStore::open(&paths.data_dir).unwrap();
        let mut timestamp = 1_786_000_000_000_000_u64;
        for event in [
            TallyEvent::Enqueued,
            TallyEvent::Dispatched,
            TallyEvent::Started,
            TallyEvent::EvidenceFail,
            TallyEvent::Preempted,
        ] {
            append_history_event(&mut history, &parent, event, 1, 1, timestamp);
            timestamp += 1;
        }
        for event in [
            TallyEvent::Resumed,
            TallyEvent::Dispatched,
            TallyEvent::Started,
            TallyEvent::EvidencePass,
            TallyEvent::Completed,
        ] {
            append_history_event(&mut history, &parent, event, 2, 2, timestamp);
            timestamp += 1;
        }
        for event in [
            TallyEvent::Enqueued,
            TallyEvent::Dispatched,
            TallyEvent::Started,
            TallyEvent::EvidencePass,
            TallyEvent::Completed,
        ] {
            append_history_event(&mut history, &child, event, 1, 1, timestamp);
            timestamp += 1;
        }
        drop(history);

        let mut ledger = WitnessLedger::open(paths.witness_path()).unwrap();
        append_fixture_witness(
            &mut ledger,
            &parent,
            "2026-08-05T12:00:00.000Z",
            Verdict::Preempted,
            1,
            1,
            1,
        );
        let parent_pass = append_fixture_witness(
            &mut ledger,
            &parent,
            "2026-08-05T12:01:00.000Z",
            Verdict::Pass,
            0,
            2,
            2,
        );
        let chain_head = append_fixture_witness(
            &mut ledger,
            &child,
            "2026-08-05T12:02:00.000Z",
            Verdict::Pass,
            0,
            1,
            1,
        );
        drop(ledger);
        append_attestation(
            &paths.attestations_path(),
            json!({
                "kind": "adapter-scrape",
                "taskUuid": parent_uuid.to_string(),
                "jobId": parent_uuid.to_string(),
                "adapter": "shell",
                "attempt": 2,
                "leaseEpoch": 2,
                "captures": {"sessionRef": "advisory-session"},
                "usageAuthority": "advisory-only",
            }),
        )
        .unwrap();
        (paths, parent_uuid, child_uuid, parent_pass, chain_head)
    }

    fn gh_test_observation(node_id: &str, item_type: GhItemType) -> GhObservation {
        GhObservation {
            source: "notifications".to_owned(),
            repo: "acme/widgets".to_owned(),
            number: 42,
            html_url: match item_type {
                GhItemType::Issue => "https://github.com/acme/widgets/issues/42",
                GhItemType::PullRequest => "https://github.com/acme/widgets/pull/42",
            }
            .to_owned(),
            item_type,
            head_sha: (item_type == GhItemType::PullRequest)
                .then(|| "4242424242424242424242424242424242424242".to_owned()),
            node_id: node_id.to_owned(),
            item_author: "issue-author".to_owned(),
            trigger_actor: "contributor".to_owned(),
            self_actor: "tally-bot".to_owned(),
            notification_reason: Some("mention".to_owned()),
            trigger_kind: "assignment".to_owned(),
            event_id: Some("event-42".to_owned()),
            comment_id: None,
            trigger_timestamp: "2026-07-20T12:30:00Z".to_owned(),
            trigger_value: Some("tally-bot".to_owned()),
            context: GhContextSnapshot {
                schema_version: GH_CONTEXT_SCHEMA_VERSION,
                title: "Origin fixture".to_owned(),
                body: "untrusted body".to_owned(),
                state: Some(GhItemState::Open),
                head_sha: (item_type == GhItemType::PullRequest)
                    .then(|| "4242424242424242424242424242424242424242".to_owned()),
                labels: vec!["build".to_owned()],
                assignees: Vec::new(),
                triggering_comment: None,
            },
        }
    }

    fn gh_test_origin(node_id: &str, item_type: GhItemType) -> GhOrigin {
        let observation = gh_test_observation(node_id, item_type);
        GhOrigin {
            schema_version: GH_ORIGIN_SCHEMA_VERSION,
            producer: "github".to_owned(),
            source: observation.source,
            repo: observation.repo,
            number: observation.number,
            html_url: observation.html_url,
            item_type: Some(observation.item_type),
            head_sha: observation.head_sha,
            node_id: observation.node_id,
            item_author: observation.item_author,
            trigger_actor: observation.trigger_actor,
            self_actor: observation.self_actor,
            notification_reason: observation.notification_reason,
            trigger_kind: observation.trigger_kind,
            event_id: observation.event_id,
            comment_id: observation.comment_id,
            trigger_timestamp: Some(observation.trigger_timestamp),
            trigger_value: observation.trigger_value,
            context: Some(observation.context),
            actor_exclude: "self".to_owned(),
            allow_self_triggered: false,
            allowed_actors: Vec::new(),
        }
    }

    fn empty_plan() -> RecoveryPlan {
        RecoveryPlan {
            witness_lsn: 0,
            rows: Vec::new(),
            actions: Vec::new(),
            lease_epoch_fences: Vec::new(),
            advisory_return_attestations: Vec::new(),
        }
    }

    #[test]
    fn fsync_barrier_is_closed_over_exactly_three_stages() {
        assert_eq!(
            FSYNC_BEFORE_ACK_STAGES,
            &[
                AckStage::Admission,
                AckStage::LeaseGrant,
                AckStage::VerdictWitness
            ]
        );
    }

    #[test]
    fn no_gate_manifest_leaves_every_evidence_verdict_unchanged() {
        for verdict in [
            Verdict::Pass,
            Verdict::CleanExitNoArtifact,
            Verdict::Failed,
            Verdict::Cancelled,
            Verdict::Reused,
            Verdict::PoolVanished,
            Verdict::Preempted,
            Verdict::RuntimeExceeded,
        ] {
            assert_eq!(canonical_verdict(verdict, None), verdict);
        }
    }

    #[test]
    fn a_bound_authorship_note_never_rescues_a_failed_gate() {
        let completion: SemanticCompletion = serde_json::from_value(json!({
            "schemaVersion": 1,
            "execution": {
                "status": "success",
                "exitCode": 0,
                "reason": "process exited with code 0"
            },
            "gates": {
                "status": "fail",
                "artifact": {"resultRevision": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
                "gates": [{"id": "tests", "status": "fail"}]
            },
            "acceptance": {
                "status": "rejected",
                "policy": "execution-and-gates",
                "reason": "a required gate failed"
            }
        }))
        .unwrap();
        assert_eq!(
            canonical_verdict(Verdict::Pass, Some(&completion)),
            Verdict::Failed
        );
    }

    #[test]
    fn required_authorship_failure_is_a_failed_canonical_verdict() {
        let reason = "git-ai-missing-note: refs/notes/ai has no note for result";
        let completion: SemanticCompletion = serde_json::from_value(json!({
            "schemaVersion": 1,
            "execution": {"status": "failure", "reason": reason},
            "gates": {"status": "pass", "artifact": {}, "gates": []},
            "acceptance": {
                "status": "rejected",
                "policy": "execution-and-gates",
                "reason": "execution failed"
            }
        }))
        .unwrap();
        assert_eq!(completion.execution.reason, reason);
        assert_eq!(
            canonical_verdict(Verdict::Pass, Some(&completion)),
            Verdict::Failed
        );
    }

    #[test]
    fn terminal_witness_beats_a_stale_live_query_snapshot() {
        let projection = |witness_seq: Option<u64>| JobProjection {
            anchor: "job-1".to_owned(),
            task_uuid: Some("job-1".to_owned()),
            description: None,
            argv: None,
            brief_hash: None,
            orchestration: None,
            pools: Some(vec!["slot".to_owned()]),
            executor: None,
            source: Some("manual".to_owned()),
            session_ref: None,
            final_message: None,
            cwd: None,
            workspace: None,
            resumed_from: None,
            model: None,
            gh_origin: None,
            state: "completed".to_owned(),
            verdict: witness_seq.map(|_| Verdict::Pass),
            gpu_seconds: None,
            canonical_gpu_seconds: None,
            last_event_at: None,
            witness_seq,
            completion: None,
        };
        let live = HashMap::from([("job-1".to_owned(), "running".to_owned())]);
        let mut terminal = vec![projection(Some(7))];
        overlay_live_states(&mut terminal, &live);
        assert_eq!(terminal[0].state, "completed");
        assert_eq!(terminal[0].verdict, Some(Verdict::Pass));

        let mut unwitnessed = vec![projection(None)];
        overlay_live_states(&mut unwitnessed, &live);
        assert_eq!(unwitnessed[0].state, "running");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn events_ingress_uses_the_identical_enqueue_narrower_and_repairs_archive_gap() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut config = one_pool_config();
                config.pools.get_mut("slot").unwrap().credentials.insert(
                    "pool-token".to_owned(),
                    PathBuf::from("/run/credentials/pool-token"),
                );
                let daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();

                let direct_payload = json!({
                    "argv": ["same-narrower", "literal arg"],
                    "pool": "slot",
                    "priority": "high",
                    "adapter": "shell",
                    "source": "events-dir",
                    "dedupKey": "direct",
                    "evidence": ["exit:0"],
                    "evidenceClass": {
                        "arbitrary": [true, 7, {"nested": null}]
                    },
                    "manifestHash": "deliberately-not-validated://events manifest",
                    "credentials": {"token": "/run/credentials/token"}
                });
                let missing = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["must-not-run"],
                        "pool": "slot",
                        "source": "orchestrator",
                        "dedupKey": "missing-full-mode-credential",
                        "submission": {"mode": "full"},
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap_err();
                assert_eq!(missing.code, WireErrorCode::InvalidParams);
                assert!(missing.message.contains("pool-token"));
                assert!(missing.message.contains("slot"));
                let conflicting = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["must-not-run"],
                        "pool": "slot",
                        "credentials": {"pool-token": "/run/credentials/wrong"}
                    })))
                    .await
                    .unwrap_err();
                assert_eq!(conflicting.code, WireErrorCode::InvalidParams);
                assert!(conflicting
                    .message
                    .contains("conflicting pool and enqueue sources"));
                let direct = daemon
                    .handler
                    .enqueue(Some(direct_payload.clone()))
                    .await
                    .unwrap();

                fs::create_dir_all(paths.events_dir()).unwrap();
                let mut file_payload = direct_payload.clone();
                file_payload["dedupKey"] = Value::String("from-file".to_owned());
                fs::write(
                    paths.events_dir().join("valid.json"),
                    serde_json::to_vec(&file_payload).unwrap(),
                )
                .unwrap();
                let malformed = json!({
                    "argv": ["one"],
                    "invocation": "two",
                    "pool": "slot"
                });
                fs::write(
                    paths.events_dir().join("malformed.json"),
                    serde_json::to_vec(&malformed).unwrap(),
                )
                .unwrap();
                std::os::unix::fs::symlink("/etc/passwd", paths.events_dir().join("hostile.json"))
                    .unwrap();
                let durable_oversize = json!({
                    "argv": ["x".repeat(600 * 1024)],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "events-dir"
                });
                let durable_oversize_bytes = serde_json::to_vec(&durable_oversize).unwrap();
                assert!(durable_oversize_bytes.len() < 1024 * 1024);
                fs::write(
                    paths.events_dir().join("durable-oversize.json"),
                    durable_oversize_bytes,
                )
                .unwrap();
                let direct_error = daemon.handler.enqueue(Some(malformed)).await.unwrap_err();

                let drained = daemon.handler.drain(None).await.unwrap();
                assert_eq!(drained["enqueued"], 1);
                assert_eq!(drained["rejected"], 3);
                assert_eq!(drained["repaired"], 0);
                assert!(drained["outcomes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|outcome| outcome["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.contains(&direct_error.message))));
                assert!(paths.events_dir().join("done/valid.json").is_file());
                assert!(paths.events_dir().join("rejected/malformed.json").is_file());
                assert!(paths
                    .events_dir()
                    .join("rejected/durable-oversize.json")
                    .is_file());
                assert!(drained["outcomes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|outcome| outcome["file"] == "durable-oversize.json"
                        && outcome["reason"]
                            .as_str()
                            .is_some_and(|reason| reason.contains("durable-event limit"))));
                assert!(
                    fs::symlink_metadata(paths.events_dir().join("rejected/hostile.json"))
                        .unwrap()
                        .file_type()
                        .is_symlink()
                );

                let direct_id = Uuid::parse_str(direct["job_id"].as_str().unwrap()).unwrap();
                let context = daemon.handler.context.read().await;
                let direct_row = &context.jobs[&direct_id].row;
                let file_row = context
                    .jobs
                    .values()
                    .find(|job| job.row.dedup_key.as_deref() == Some("from-file"))
                    .unwrap()
                    .row
                    .clone();
                assert_eq!(file_row.argv, direct_row.argv);
                assert_eq!(file_row.pools, direct_row.pools);
                assert_eq!(file_row.priority, direct_row.priority);
                assert_eq!(file_row.adapter, direct_row.adapter);
                assert_eq!(file_row.source, direct_row.source);
                assert_eq!(file_row.evidence, direct_row.evidence);
                assert_eq!(file_row.evidence_class, direct_row.evidence_class);
                assert_eq!(file_row.manifest_hash, direct_row.manifest_hash);
                assert_eq!(
                    file_row.evidence_class,
                    Some(json!({"arbitrary": [true, 7, {"nested": null}]}))
                );
                assert_eq!(
                    file_row.manifest_hash,
                    Some(Value::String(
                        "deliberately-not-validated://events manifest".to_owned()
                    ))
                );
                assert_eq!(file_row.credentials, direct_row.credentials);
                assert_eq!(
                    direct_row.credentials["pool-token"],
                    PathBuf::from("/run/credentials/pool-token")
                );
                assert_eq!(
                    direct_row.credentials["token"],
                    PathBuf::from("/run/credentials/token")
                );
                drop(context);

                let repair_payload = json!({
                    "argv": ["repair-gap"],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "events-dir",
                    "dedupKey": "repair-gap"
                });
                fs::write(
                    paths.events_dir().join("repair.json"),
                    serde_json::to_vec(&repair_payload).unwrap(),
                )
                .unwrap();
                let claims = claim_ingress_files(&paths.events_dir()).unwrap();
                assert_eq!(claims.len(), 1);
                let payload = read_ingress_payload(&claims[0]).unwrap();
                daemon
                    .handler
                    .enqueue_payload(payload, Some(claims[0].ingress_id.clone()))
                    .await
                    .unwrap();
                assert!(claims[0].path.exists());

                let repaired = daemon.handler.drain(None).await.unwrap();
                assert_eq!(repaired["enqueued"], 0);
                assert_eq!(repaired["rejected"], 0);
                assert_eq!(repaired["repaired"], 1);
                assert!(paths.events_dir().join("done/repair.json").is_file());
                let events = crate::taskdb::read_acknowledged_events(&paths.events_dir()).unwrap();
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| {
                            event.ingress_id.as_deref() == Some(&claims[0].ingress_id)
                        })
                        .count(),
                    1
                );

                let transient_payload = json!({
                    "argv": ["transient-read"],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "events-dir"
                });
                fs::write(
                    paths.events_dir().join("transient.json"),
                    serde_json::to_vec(&transient_payload).unwrap(),
                )
                .unwrap();
                let transient_claim = claim_ingress_files(&paths.events_dir()).unwrap().remove(0);
                fs::set_permissions(&transient_claim.path, fs::Permissions::from_mode(0o000))
                    .unwrap();
                let transient_error = daemon.handler.drain(None).await.unwrap_err();
                assert_eq!(transient_error.code, WireErrorCode::Internal);
                assert!(transient_claim.path.exists());
                assert!(!paths.events_dir().join("rejected/transient.json").exists());
                fs::set_permissions(&transient_claim.path, fs::Permissions::from_mode(0o600))
                    .unwrap();
                let retried = daemon.handler.drain(None).await.unwrap();
                assert_eq!(retried["enqueued"], 1);
                assert!(paths.events_dir().join("done/transient.json").is_file());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn enqueue_opaque_metadata_witnesses_and_queries_verbatim() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    two_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let daemon_history = daemon.handler.history.clone();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let evidence_class = json!({
                    "arbitrary": [true, 7, {"nested": null}],
                    "label": "opaque"
                });
                let manifest_hash = "deliberately-not-validated://manifest value";
                let orchestration = json!({
                    "flowRunId": "00000000-0000-4000-8000-000000000062",
                    "promptRevision": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "skillRevision": "review-agent-v3"
                });
                let admitted = client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["true"],
                            "pool": ["zeta", "slot"],
                            "priority": "high",
                            "adapter": "shell",
                            "source": "manual",
                            "evidence": ["exit:0"],
                            "evidenceClass": evidence_class,
                            "manifestHash": manifest_hash,
                            "orchestration": orchestration
                        })),
                    )
                    .await
                    .unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
                let terminal = client
                    .call("queue.await_job", Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");

                let (report, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(report.ok);
                let record = records
                    .iter()
                    .find(|record| record.task_uuid.as_deref() == Some(&task_uuid))
                    .unwrap();
                assert_eq!(record.schema_version, crate::witness::WITNESS_SCHEMA_VERSION);
                assert_eq!(record.record_type, crate::witness::RecordType::Verdict);
                assert_eq!(
                    record.origin,
                    AdmissionOrigin::direct(EnqueueSource::Manual)
                );
                let expected_host_id = current_host_id().unwrap();
                assert_eq!(record.host_id.as_deref(), Some(expected_host_id.as_str()));
                assert_eq!(
                    record.pools,
                    ["slot".to_owned(), "zeta".to_owned()]
                );
                assert_eq!(record.evidence_class.as_ref(), Some(&evidence_class));
                assert_eq!(
                    record.manifest_hash,
                    Some(Value::String(manifest_hash.to_owned()))
                );
                assert_eq!(
                    record.orchestration.as_ref().unwrap().as_value(),
                    &orchestration
                );

                let raw_witness = fs::read_to_string(paths.witness_path()).unwrap();
                let fielded_line = raw_witness
                    .lines()
                    .find(|line| line.contains(&task_uuid))
                    .unwrap();
                let raw_record: Value = serde_json::from_str(fielded_line).unwrap();
                assert_eq!(raw_record["schemaVersion"], 2);
                assert_eq!(raw_record["recordType"], "verdict");
                assert!(raw_record["pools"].is_array());
                assert!(raw_record.get("pool").is_none());
                assert!(raw_record["origin"].is_object());
                assert_eq!(raw_record["hostId"], expected_host_id);
                assert!(
                    fielded_line.find("\"evidenceClass\"").unwrap()
                        < fielded_line.find("\"manifestHash\"").unwrap()
                );
                assert!(
                    fielded_line.find("\"manifestHash\"").unwrap()
                        < fielded_line.find("\"seq\"").unwrap()
                );

                let log = client
                    .call("query.log", Some(json!({"task": task_uuid})))
                    .await
                    .unwrap();
                let queried = log
                    .get("items")
                    .and_then(Value::as_array)
                    .unwrap()
                    .iter()
                    .find(|entry| entry["origin"] == "witness")
                    .unwrap();
                assert_eq!(queried["evidenceClass"], evidence_class);
                assert_eq!(queried["manifestHash"], manifest_hash);
                assert_eq!(queried["pool"], json!(["slot", "zeta"]));

                let events = read_acknowledged_events(&paths.events_dir()).unwrap();
                let durable = events
                    .iter()
                    .find(|event| event.row.uuid.to_string() == task_uuid)
                    .unwrap();
                assert_eq!(durable.row.pools, ["slot", "zeta"]);
                assert_eq!(
                    durable.row.orchestration.as_ref().unwrap().as_value(),
                    &orchestration
                );
                let status = client.call("query.status", Some(json!({}))).await.unwrap();
                let projected = status["jobs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["taskUuid"] == task_uuid)
                    .unwrap();
                assert_eq!(projected["pool"], json!(["slot", "zeta"]));
                assert!(daemon_history
                    .borrow()
                    .snapshot()
                    .records
                    .iter()
                    .any(|record| {
                        record.fields.task_uuid == task_uuid
                            && record.fields.pools.as_deref()
                                == Some(["slot".to_owned(), "zeta".to_owned()].as_slice())
                    }));

                let absent = client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["true"],
                            "pool": ["slot", "zeta"],
                            "priority": "high",
                            "adapter": "shell",
                            "source": "manual",
                            "evidence": ["exit:0"]
                        })),
                    )
                    .await
                    .unwrap();
                let absent_uuid = absent["task_uuid"].as_str().unwrap().to_owned();
                let terminal = client
                    .call("queue.await_job", Some(json!({"task_uuid": absent_uuid})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");
                let raw_witness = fs::read_to_string(paths.witness_path()).unwrap();
                let absent_line = raw_witness
                    .lines()
                    .find(|line| line.contains(&absent_uuid))
                    .unwrap();
                assert!(!absent_line.contains("\"evidence_class\""));
                assert!(!absent_line.contains("\"manifest_hash\""));

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn confirmed_pool_loss_witnesses_and_return_re_presents_the_same_row() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let program = temp.path().join("resumable-agent");
                let started = temp.path().join("started");
                let resumed = temp.path().join("resumed");
                crate::test_support::install_shell_program(
                    &program,
                    format!(
                        concat!(
                            "#!/bin/sh\n",
                            "if test \"$1\" = --resume; then\n",
                            "  printf '%s' \"$2\" > '{}'\n",
                            "  exit 0\n",
                            "fi\n",
                            "printf '%s\\n' '{{\"session_id\":\"durable-session\"}}'\n",
                            "> '{}'\n",
                            "sleep 30\n"
                        ),
                        resumed.display(),
                        started.display(),
                    ),
                );

                let mut config = two_pool_config();
                config.pools.get_mut("slot").unwrap().auto_resume = Some(true);
                config.adapters.insert(
                    "resumable".to_owned(),
                    AdapterConfig {
                        argv: vec![program.to_string_lossy().into_owned()],
                        resume: Some(vec![
                            program.to_string_lossy().into_owned(),
                            "--resume".to_owned(),
                            "%<sessionRef>%".to_owned(),
                        ]),
                        scrape: BTreeMap::from([(
                            "sessionRef".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPath,
                                pattern: "$..session_id".to_owned(),
                            },
                        )]),
                        trace: None,
                        yield_hook: None,
                        env: BTreeMap::new(),
                        launch: crate::adapters::AdapterLaunchConfig::default(),
                        hardening: Default::default(),
                        extra_writable_paths: Vec::new(),
                        skill_bundle: None,
                        skill_revision: None,
                        extra_config: BTreeMap::new(),
                    },
                );
                config.producers = serde_json::from_value(json!({
                    "health": {
                        "kind": "pool-reachability",
                        "probePool": "slot",
                        "hysteresis": 1
                    }
                }))
                .unwrap();
                let registry = config.producers.clone();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let restart_config = config.clone();
                let restart_executor = executor.clone();
                let mut retry_settings = settings();
                retry_settings.recovery_policy.max_attempts = 2;
                let mut daemon =
                    Daemon::open_with_executor(config, paths.clone(), retry_settings, executor)
                        .await
                        .unwrap();
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["initial"],
                        "pool": "slot",
                        "adapter": "resumable",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !started.exists() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                let parent_task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
                let child = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["true"],
                        "pool": "zeta",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"],
                        "dedupKey": "acceptance-child",
                        "parent": parent_task_uuid,
                        "callerJobId": admitted["job_id"],
                    })))
                    .await
                    .unwrap();
                let child_task_uuid = child["task_uuid"].as_str().unwrap().to_owned();
                let child_finished =
                    tokio::time::timeout(Duration::from_secs(2), daemon.completion_rx.recv())
                        .await
                        .unwrap()
                        .unwrap();
                assert_eq!(
                    child_finished.job_id.to_string(),
                    child["job_id"].as_str().unwrap()
                );
                daemon.finish_job(child_finished).await.unwrap();
                daemon.handler.drain_post_ack_tasks().await;

                let engine = ProducerEngine::new(&registry, paths.events_dir(), &paths.state_dir);
                let lost = engine
                    .observe_reachability("health", false, Utc::now())
                    .unwrap();
                assert_eq!(lost.transition, Some(ReachabilityTransition::Lost));
                let applied = daemon
                    .handler
                    .pool_transition(Some(json!({
                        "producer": "health",
                        "transition": "lost",
                        "generation": lost.generation,
                    })))
                    .await
                    .unwrap();
                assert_eq!(applied["affected"], 1);
                engine
                    .acknowledge_reachability_transition("health", lost.generation)
                    .unwrap();
                daemon.handler.drain_post_ack_tasks().await;
                let task_uuid = Uuid::parse_str(admitted["task_uuid"].as_str().unwrap()).unwrap();
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.jobs[&task_uuid].state, JobState::Completed);
                    assert!(context.unreachable_pools.contains("slot"));
                }
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 2);
                let first_parent = records
                    .iter()
                    .find(|record| record.task_uuid.as_deref() == Some(parent_task_uuid.as_str()))
                    .unwrap();
                assert_eq!(first_parent.verdict, Verdict::PoolVanished);
                assert_eq!(first_parent.attempt, 1);

                let returned = engine
                    .observe_reachability("health", true, Utc::now())
                    .unwrap();
                assert_eq!(returned.transition, Some(ReachabilityTransition::Returned));
                let transition_params = json!({
                    "producer": "health",
                    "transition": "returned",
                    "generation": returned.generation,
                });
                let first_handler = daemon.handler.clone();
                let second_handler = daemon.handler.clone();
                let (first, second) = tokio::join!(
                    first_handler.pool_transition(Some(transition_params.clone())),
                    second_handler.pool_transition(Some(transition_params)),
                );
                let first = first.unwrap();
                let second = second.unwrap();
                assert_eq!(
                    [first["applied"].as_bool(), second["applied"].as_bool()],
                    [Some(true), Some(false)]
                );
                assert_eq!(first["affected"], 1);
                assert_eq!(second["alreadyApplied"], true);
                engine
                    .acknowledge_reachability_transition("health", returned.generation)
                    .unwrap();

                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let finished = daemon.completion_rx.recv().await.unwrap();
                        daemon.finish_job(finished).await.unwrap();
                        if daemon
                            .handler
                            .context
                            .read()
                            .await
                            .jobs
                            .get(&task_uuid)
                            .is_some_and(|job| {
                                job.state == JobState::Completed && job.row.attempt == 2
                            })
                        {
                            break;
                        }
                    }
                })
                .await
                .unwrap();
                assert_eq!(fs::read_to_string(&resumed).unwrap(), "durable-session");
                let terminal = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": task_uuid.to_string()})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");
                assert_eq!(terminal["attempt"], 2);
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 3);
                let parent_records = records
                    .iter()
                    .filter(|record| record.task_uuid.as_deref() == Some(parent_task_uuid.as_str()))
                    .collect::<Vec<_>>();
                assert_eq!(parent_records.len(), 2);
                assert_eq!(parent_records[1].verdict, Verdict::Pass);
                assert_eq!(parent_records[1].attempt, 2);
                assert_eq!(parent_records[1].labor_class, LaborClass::Recovered);
                daemon.handler.drain_post_ack_tasks().await;

                drop(daemon);
                let restarted = Daemon::open_with_executor(
                    restart_config,
                    paths.clone(),
                    retry_settings,
                    restart_executor,
                )
                .await
                .unwrap();
                let jobs = restarted
                    .handler
                    .query("query.jobs", Some(json!({})))
                    .await
                    .unwrap();
                let parent = jobs["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["taskUuid"] == parent_task_uuid)
                    .unwrap();
                assert_eq!(parent["currentAttempt"], 2);
                assert_eq!(parent["terminalVerdict"], "pass");
                assert_eq!(parent["childTaskUuids"], json!([child_task_uuid.clone()]));
                let detail = restarted
                    .handler
                    .query("query.job", Some(json!({"id": parent_task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(detail["attempts"].as_array().unwrap().len(), 2);
                assert_eq!(detail["attempts"][0]["attempt"], 1);
                assert_eq!(detail["attempts"][1]["attempt"], 2);
                let log = restarted
                    .handler
                    .query(
                        "query.log",
                        Some(json!({"task": detail["job"]["taskUuid"]})),
                    )
                    .await
                    .unwrap();
                assert!(log["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| event["attempt"] == 1 && event["event"] == "failed"));
                assert!(log["items"].as_array().unwrap().iter().any(|event| {
                    event["attempt"] == 2
                        && event["authority"] == "canonical-witness-fact"
                        && event["terminalVerdict"] == "pass"
                }));
                let proof = restarted
                    .handler
                    .query(
                        "query.proof",
                        Some(json!({
                            "task": detail["job"]["taskUuid"],
                            "attempt": 2,
                        })),
                    )
                    .await
                    .unwrap();
                assert_eq!(proof["status"], "verified");
                assert_eq!(proof["witnessRecord"]["verdict"], "pass");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pool_loss_intent_recovers_both_crash_windows_exactly_once() {
        let temp = tempdir().unwrap();
        let paths = DaemonPaths {
            socket: temp.path().join("run/tally.sock"),
            state_dir: temp.path().join("state"),
            data_dir: temp.path().join("data"),
        };
        prepare_paths(&paths).unwrap();
        let row = durable_row(Uuid::new_v4(), "pool-loss-crash-window", 7);
        write_enqueue_event_atomic(
            &paths.events_dir(),
            &DurableEnqueueEvent::new(row.clone()).unwrap(),
        )
        .unwrap();
        let job = Job {
            job_id: row.uuid,
            task_uuid: Some(row.uuid),
            row: row.clone(),
            invocation: AdapterInvocation {
                argv: vec!["true".to_owned()],
                env: BTreeMap::new(),
                hardening: Default::default(),
                extra_writable_paths: Vec::new(),
                yield_hook: None,
            },
            labor_class: LaborClass::Fresh,
            state: JobState::Running,
            lease_id: None,
            adopted: false,
            adopted_invocation_id: None,
            model_is_advisory: false,
        };

        // Simulate a crash after the durable intent and before physical reclaim.
        let intent_path = write_pool_loss_intent(&paths.state_dir, &job).unwrap();
        assert_eq!(read_pool_loss_intent(&intent_path).unwrap().row, row);
        let inspections = Arc::new(AtomicUsize::new(0));
        let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
            .with_unit_probe(IntentObservingProbe {
                path: intent_path.clone(),
                task_uuid: job.row.uuid,
                inspections: inspections.clone(),
            });
        let mut ledger = WitnessLedger::open(paths.witness_path()).unwrap();
        let host_id = current_host_id().unwrap();
        reconcile_pool_loss_intents(&paths, &executor, &mut ledger, &host_id)
            .await
            .unwrap();
        assert_eq!(inspections.load(Ordering::SeqCst), 1);
        assert!(!intent_path.exists());
        let (report, records) = read_verified_records(&paths.witness_path()).unwrap();
        assert!(report.ok);
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].task_uuid.as_deref(),
            Some(row.uuid.to_string().as_str())
        );
        assert_eq!(records[0].verdict, Verdict::PoolVanished);
        assert_eq!(records[0].attempt, row.attempt);
        assert_eq!(records[0].lease_epoch, row.lease_epoch);
        assert_eq!(records[0].host_id.as_deref(), Some(host_id.as_str()));

        // Simulate a second crash after witness fsync and before intent removal.
        assert_eq!(
            write_pool_loss_intent(&paths.state_dir, &job).unwrap(),
            intent_path
        );
        reconcile_pool_loss_intents(&paths, &executor, &mut ledger, &host_id)
            .await
            .unwrap();
        assert_eq!(inspections.load(Ordering::SeqCst), 1);
        assert!(!intent_path.exists());
        let (report, records) = read_verified_records(&paths.witness_path()).unwrap();
        assert!(report.ok);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].verdict, Verdict::PoolVanished);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn startup_pool_loss_preserves_an_already_recorded_real_exit() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                prepare_paths(&paths).unwrap();
                initialize_final_witness_state(&paths);
                let row = durable_row(Uuid::new_v4(), "startup-real-exit", 1);
                write_enqueue_event_atomic(
                    &paths.events_dir(),
                    &DurableEnqueueEvent::new(row.clone()).unwrap(),
                )
                .unwrap();
                let mut config = one_pool_config();
                config.producers = serde_json::from_value(json!({
                    "health": {
                        "kind": "pool-reachability",
                        "probePool": "slot",
                        "hysteresis": 1
                    }
                }))
                .unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let identity = ExecutionIdentity {
                    job_id: row.uuid,
                    task_uuid: Some(row.uuid),
                };
                write_exit_record(
                    &executor.paths(&identity).exit_record,
                    &UnitExitRecord {
                        schema_version: crate::executor::UNIT_EXIT_SCHEMA_VERSION,
                        unit: executor.unit_name(&identity),
                        invocation_id: "recorded-before-startup".to_owned(),
                        attempt: 1,
                        lease_epoch: 1,
                        service_result: "success".to_owned(),
                        exit_code: Some("exited".to_owned()),
                        exit_status: Some("0".to_owned()),
                    },
                )
                .unwrap();
                let engine =
                    ProducerEngine::new(&config.producers, paths.events_dir(), &paths.state_dir);
                let lost = engine
                    .observe_reachability("health", false, Utc::now())
                    .unwrap();
                assert_eq!(lost.transition, Some(ReachabilityTransition::Lost));
                assert!(!pool_loss_intent_directory(&paths.state_dir).exists());

                let daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                assert_eq!(daemon.initial_jobs.len(), 1);
                assert!(daemon.handler.context.read().await.jobs[&row.uuid]
                    .lease_id
                    .is_none());
                let (shutdown, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let result = client
                    .call(
                        "queue.await_job",
                        Some(json!({"task_uuid": row.uuid.to_string()})),
                    )
                    .await
                    .unwrap();
                assert_eq!(result["verdict"], "pass");
                shutdown.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].verdict, Verdict::Pass);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn confirmed_return_leaves_nonresumable_rows_terminal() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                prepare_paths(&paths).unwrap();
                let row = durable_row(Uuid::new_v4(), "nonresumable-return", 1);
                write_enqueue_event_atomic(
                    &paths.events_dir(),
                    &DurableEnqueueEvent::new(row.clone()).unwrap(),
                )
                .unwrap();
                WitnessLedger::open(paths.witness_path())
                    .unwrap()
                    .append(WitnessBody {
                        task_uuid: Some(row.uuid.to_string()),
                        transition_timestamp: Utc::now()
                            .to_rfc3339_opts(SecondsFormat::Millis, true),
                        verdict: Verdict::PoolVanished,
                        exit_code: 1,
                        artifact_content_hash: None,
                        store_paths: None,
                        drv: None,
                        gpu_seconds: None,
                        wall_clock: 0.0,
                        attempt: 1,
                        lease_epoch: 1,
                        dedup_key: row.dedup_key.clone(),
                        payload_hash: row.payload_hash.clone(),
                        brief_hash: row.brief_hash.clone(),
                        origin: AdmissionOrigin::direct(EnqueueSource::Manual),
                        orchestration: row.orchestration.clone(),
                        labor_class: LaborClass::Fresh,
                        trace_ref: None,
                        pools: vec!["slot".to_owned()],
                        executor: None,
                        host_id: None,
                        charge: None,
                        model: None,
                        evidence_class: None,
                        manifest_hash: None,
                        completion: None,
                        result_revision: None,
                        authorship: None,
                        authorship_sessions: None,
                    })
                    .unwrap();
                let mut config = one_pool_config();
                config.pools.get_mut("slot").unwrap().auto_resume = Some(true);
                config.producers = serde_json::from_value(json!({
                    "health": {
                        "kind": "pool-reachability",
                        "probePool": "slot",
                        "hysteresis": 1
                    }
                }))
                .unwrap();
                let engine =
                    ProducerEngine::new(&config.producers, paths.events_dir(), &paths.state_dir);
                let lost = engine
                    .observe_reachability("health", false, Utc::now())
                    .unwrap();
                engine
                    .acknowledge_reachability_transition("health", lost.generation)
                    .unwrap();
                let returned = engine
                    .observe_reachability("health", true, Utc::now())
                    .unwrap();
                assert_eq!(returned.transition, Some(ReachabilityTransition::Returned));

                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(config, paths, settings(), executor)
                    .await
                    .unwrap();
                assert!(daemon.initial_jobs.is_empty());
                let terminal = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": row.uuid.to_string()})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pool-vanished");
                assert_eq!(terminal["attempt"], 1);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn successful_durable_gh_row_runs_the_concrete_completed_mutation_once() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                initialize_final_witness_state(&paths);
                let mut config = one_pool_config();
                config.producers = serde_json::from_value(json!({
                    "github": {
                        "kind": "gh",
                        "enable": true,
                        "sources": [{"notifications": {"repo": "acme/widgets"}}],
                        "triggers": {"assignments": ["tally-bot"]},
                        "postEvidence": true,
                        "enqueue": {"argv": ["true"], "pool": "slot"}
                    }
                }))
                .unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon =
                    Daemon::open_with_executor(config, paths, settings(), executor)
                        .await
                        .unwrap();
                let gh = temp.path().join("fake-gh");
                let requests = temp.path().join("gh-requests.jsonl");
                let calls = temp.path().join("gh-calls");
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
                            "  *TallyCompletionState*) printf '{{\"data\":{{\"node\":{{\"__typename\":\"Issue\",\"state\":\"OPEN\",\"comments\":{{\"nodes\":[],\"pageInfo\":{{\"hasNextPage\":false,\"endCursor\":null}}}}}}}}}}' ;;\n",
                            "  *TallyCompletionComment*) printf '{{\"data\":{{\"addComment\":{{}}}}}}' ;;\n",
                            "  *TallyCompletionIssue*) printf '{{\"data\":{{\"closeIssue\":{{}}}}}}' ;;\n",
                            "  *) exit 92 ;;\n",
                            "esac\n"
                        ),
                        requests.display(),
                        calls.display(),
                    ),
                );
                daemon.handler.gh_program = gh;

                let mut row = durable_row(Uuid::new_v4(), "gh:github:item-1", 1);
                row.source = EnqueueSource::Gh;
                row.adapter = "codex".to_owned();
                row.gh_origin = Some(gh_test_origin("item-1", GhItemType::Issue));
                let result = JobResult {
                    task_uuid: Some(row.uuid.to_string()),
                    job_id: row.uuid.to_string(),
                    verdict: Verdict::Pass,
                    exit_code: 0,
                    artifact_content_hash: Some("sha256:artifact".to_owned()),
                    attempt: 1,
                    lease_epoch: 1,
                    witness_seq: 9,
                    model: Some("gpt-5.6-codex".to_owned()),
                    completion: None,
                };
                daemon
                    .handler
                    .complete_gh_post_ack(row.clone(), result.clone());
                daemon
                    .handler
                    .complete_gh_post_ack(row.clone(), result.clone());
                daemon.handler.drain_post_ack_tasks().await;

                assert_eq!(fs::read(&calls).unwrap(), b"xxxx");
                let requests = fs::read_to_string(&requests)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str::<Value>(line).unwrap())
                    .collect::<Vec<_>>();
                let comment = requests
                    .iter()
                    .find(|request| request["query"]
                        .as_str()
                        .unwrap()
                        .contains("TallyCompletionComment"))
                    .unwrap();
                assert_eq!(comment["variables"]["itemId"], "item-1");
                let body = comment["variables"]["body"].as_str().unwrap();
                let (_, remainder) = body.split_once('\n').unwrap();
                let (encoded, trailer) = remainder.split_once("\n\n").unwrap();
                let evidence: Value = serde_json::from_str(encoded).unwrap();
                assert_eq!(evidence["producer"], "github");
                assert_eq!(evidence["source"], "notifications");
                assert_eq!(evidence["itemId"], "item-1");
                assert_eq!(evidence["state"], "COMPLETED");
                assert_eq!(evidence["evidence"]["taskUuid"], row.uuid.to_string());
                assert_eq!(evidence["evidence"]["witnessSeq"], 9);
                assert_eq!(evidence["evidence"]["verdict"], "pass");
                assert_eq!(
                    trailer,
                    format!(
                        "Assisted-by: codex:gpt-5.6-codex (tally:{} witness:9)",
                        row.uuid
                    )
                );

                let mut failed = result;
                failed.witness_seq = 10;
                failed.verdict = Verdict::Failed;
                daemon.handler.complete_gh_post_ack(row, failed);
                daemon.handler.drain_post_ack_tasks().await;
                assert_eq!(fs::read(calls).unwrap(), b"xxxx");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_24_7_producer_origin_survives_restart_and_joins_inventory() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                initialize_final_witness_state(&paths);
                let mut config = one_pool_config();
                config.producers = serde_json::from_value(json!({
                    "daily": {
                        "kind": "calendar",
                        "onCalendar": "daily",
                        "enqueue": {"argv": ["calendar"], "pool": "slot"}
                    },
                    "drop": {"kind": "events-dir"},
                    "github": {
                        "kind": "gh",
                        "enable": true,
                        "sources": [{"notifications": {"repo": "acme/widgets"}}],
                        "triggers": {"assignments": ["tally-bot"]},
                        "postEvidence": true,
                        "enqueue": {"argv": ["github"], "pool": "slot"}
                    },
                    "effects": {
                        "kind": "build-effect",
                        "watch": "jsonl",
                        "path": "/var/empty/tally-effects.jsonl",
                        "onKey": {"argv": ["effect"], "pool": "slot"}
                    },
                    "health": {
                        "kind": "pool-reachability",
                        "probePool": "slot",
                        "hysteresis": 2,
                        "onLost": {"argv": ["lost"], "pool": "slot"},
                        "onReturn": {"argv": ["returned"], "pool": "slot"},
                        "onReturnAttest": {
                            "argv": ["attest"],
                            "pool": "slot",
                            "noEnqueue": true
                        }
                    }
                }))
                .unwrap();
                config.validate().unwrap();
                let now = Utc::now();
                let engine =
                    ProducerEngine::new(&config.producers, paths.events_dir(), &paths.state_dir);
                engine.emit_calendar("daily", now).unwrap();
                engine
                    .emit_gh(
                        "github",
                        &gh_test_observation("PR-live-producer", GhItemType::PullRequest),
                        now,
                    )
                    .unwrap();
                engine
                    .emit_build_effect(
                        "effects",
                        Path::new("/nix/store/00000000000000000000000000000000-live-producer"),
                        now,
                    )
                    .unwrap();
                assert!(engine
                    .observe_reachability("health", false, now)
                    .unwrap()
                    .transition
                    .is_none());
                let lost = engine.observe_reachability("health", false, now).unwrap();
                assert!(lost.transition.is_some());
                engine
                    .acknowledge_reachability_transition("health", lost.generation)
                    .unwrap();
                assert!(engine
                    .observe_reachability("health", true, now)
                    .unwrap()
                    .transition
                    .is_none());
                let returned = engine.observe_reachability("health", true, now).unwrap();
                assert!(returned.transition.is_some());
                engine
                    .acknowledge_reachability_transition("health", returned.generation)
                    .unwrap();
                assert!(matches!(
                    config.producers["drop"],
                    ProducerConfig::EventsDir(_)
                ));
                fs::write(
                    paths.events_dir().join("drop-fixture.producer.json"),
                    serde_json::to_vec(&json!({
                        "argv": ["event"],
                        "pool": "slot"
                    }))
                    .unwrap(),
                )
                .unwrap();

                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    config.clone(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();
                let drained = daemon
                    .handler
                    .drain(Some(json!({"producer": "drop"})))
                    .await
                    .unwrap();
                assert_eq!(drained["enqueued"], 7);
                assert_eq!(drained["rejected"], 0);

                let context = daemon.handler.context.read().await;
                for (source, expected) in [
                    (EnqueueSource::Calendar, 1),
                    (EnqueueSource::EventsDir, 1),
                    (EnqueueSource::Gh, 1),
                    (EnqueueSource::BuildEffect, 1),
                    (EnqueueSource::PoolReachability, 3),
                ] {
                    assert_eq!(
                        context
                            .jobs
                            .values()
                            .filter(|job| job.row.source == source)
                            .count(),
                        expected
                    );
                }
                assert_eq!(
                    context
                        .jobs
                        .values()
                        .filter(|job| job.row.no_enqueue)
                        .count(),
                    1
                );
                let expected_origins = context
                    .jobs
                    .values()
                    .map(|job| {
                        (
                            job.row.uuid.to_string(),
                            job.row
                                .origin
                                .as_ref()
                                .and_then(|origin| origin.producer.as_ref())
                                .map(|producer| producer.name.clone())
                                .unwrap(),
                        )
                    })
                    .collect::<BTreeMap<_, _>>();
                assert_eq!(expected_origins.len(), 7);
                assert_eq!(
                    expected_origins.values().fold(
                        BTreeMap::<String, usize>::new(),
                        |mut counts, name| {
                            *counts.entry(name.clone()).or_default() += 1;
                            counts
                        }
                    ),
                    BTreeMap::from([
                        ("daily".to_owned(), 1),
                        ("drop".to_owned(), 1),
                        ("effects".to_owned(), 1),
                        ("github".to_owned(), 1),
                        ("health".to_owned(), 3),
                    ])
                );
                drop(context);
                assert!(crate::taskdb::read_acknowledged_events(&paths.events_dir())
                    .unwrap()
                    .iter()
                    .all(|event| event.ingress_id.is_some()));

                drop(daemon);
                let restarted =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                let jobs = restarted
                    .handler
                    .query("query.jobs", Some(json!({"limit": 100})))
                    .await
                    .unwrap();
                let observed_origins = jobs["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|job| {
                        (
                            job["taskUuid"].as_str().unwrap().to_owned(),
                            job["origin"]["value"]["producer"]["name"]
                                .as_str()
                                .unwrap()
                                .to_owned(),
                        )
                    })
                    .collect::<BTreeMap<_, _>>();
                assert_eq!(observed_origins, expected_origins);

                let producers = restarted
                    .handler
                    .query("query.producers", Some(json!({"name": "daily"})))
                    .await
                    .unwrap();
                assert_eq!(producers["items"][0]["name"], "daily");
                assert_eq!(producers["items"][0]["kind"], "calendar");
                assert_eq!(
                    producers["items"][0]["schedule"]["calendarExpression"],
                    "daily"
                );
                let watch_tail = restarted
                    .handler
                    .query("query.watch", Some(json!({})))
                    .await
                    .unwrap()["nextCursor"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                restarted
                    .handler
                    .producer_runtime_observed(Some(json!({"producer": "daily"})))
                    .await
                    .unwrap();
                let changes = restarted
                    .handler
                    .query(
                        "query.watch",
                        Some(json!({"after": watch_tail, "limit": 100})),
                    )
                    .await
                    .unwrap();
                assert_eq!(changes["items"][0]["kind"], "producer");
                assert_eq!(changes["items"][0]["payload"]["name"], "daily");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hydrated_github_pr_origin_reaches_launch_status_and_survives_restart() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                initialize_final_witness_state(&paths);
                let mut config = one_pool_config();
                config.producers = serde_json::from_value(json!({
                    "github": {
                        "kind": "gh",
                        "enable": true,
                        "sources": [{"notifications": {"repo": "acme/widgets"}}],
                        "triggers": {"mentions": ["@tally-bot inspect this"]},
                        "allowedActors": ["maintainer"],
                        "postEvidence": false,
                        "closeOnPass": false,
                        "enqueue": {"argv": ["handle-origin"], "pool": "slot"}
                    }
                }))
                .unwrap();
                config.validate().unwrap();

                let gh = temp.path().join("fake-gh-origin");
                crate::test_support::install_shell_program(
                    &gh,
                    concat!(
                        "#!/bin/sh\n",
                        "case \"$*\" in\n",
                        "  'api user') printf '{\"login\":\"tally-bot\"}' ;;\n",
                        "  'api --method GET notifications -f all=false -f participating=false -f per_page=100')\n",
                        "    printf '[{\"id\":\"notification-42\",\"reason\":\"mention\",\"updated_at\":\"2026-07-24T08:00:00Z\",\"repository\":{\"full_name\":\"acme/widgets\"},\"subject\":{\"type\":\"PullRequest\",\"url\":\"https://api.github.com/repos/acme/widgets/pulls/42\",\"latest_comment_url\":\"https://api.github.com/repos/acme/widgets/issues/comments/4200\"}}]' ;;\n",
                        "  'api /repos/acme/widgets/pulls/42')\n",
                        "    printf '{\"node_id\":\"PR_origin_42\",\"number\":42,\"html_url\":\"https://github.com/acme/widgets/pull/42\",\"state\":\"open\",\"title\":\"Hydrated PR\",\"body\":\"untrusted $(never-executed)\",\"user\":{\"login\":\"item-author\"},\"head\":{\"sha\":\"4242424242424242424242424242424242424242\"},\"labels\":[{\"name\":\"build\"}],\"assignees\":[{\"login\":\"tally-bot\"}]}' ;;\n",
                        "  'api /repos/acme/widgets/issues/comments/4200')\n",
                        "    printf '{\"id\":4200,\"body\":\"@tally-bot inspect this\",\"created_at\":\"2026-07-24T08:00:00Z\",\"updated_at\":\"2026-07-24T08:00:00Z\",\"user\":{\"login\":\"maintainer\"}}' ;;\n",
                        "  *) exit 91 ;;\n",
                        "esac\n",
                    ),
                );
                let engine =
                    ProducerEngine::new(&config.producers, paths.events_dir(), &paths.state_dir);
                let outcomes = engine
                    .poll_gh("github", &GhCliIntake::with_program(&gh), Utc::now())
                    .unwrap();
                let emitted = match outcomes.as_slice() {
                    [EmitOutcome::Emitted(path)] => path,
                    other => panic!("expected one emitted hydrated PR origin, got {other:?}"),
                };
                let payload: EnqueuePayload =
                    serde_json::from_slice(&fs::read(emitted).unwrap()).unwrap();
                let captured_origin = payload.gh_origin.unwrap();
                assert_eq!(captured_origin.repo, "acme/widgets");
                assert_eq!(captured_origin.number, 42);
                assert_eq!(
                    captured_origin.html_url,
                    "https://github.com/acme/widgets/pull/42"
                );
                assert_eq!(captured_origin.item_type, Some(GhItemType::PullRequest));
                assert_eq!(
                    captured_origin.head_sha.as_deref(),
                    Some("4242424242424242424242424242424242424242")
                );
                assert_eq!(captured_origin.node_id, "PR_origin_42");
                assert_eq!(captured_origin.item_author, "item-author");
                assert_eq!(captured_origin.trigger_actor, "maintainer");
                assert_eq!(captured_origin.self_actor, "tally-bot");
                assert_eq!(
                    captured_origin.notification_reason.as_deref(),
                    Some("mention")
                );
                assert_eq!(captured_origin.trigger_kind, "mention");
                assert_eq!(
                    captured_origin.event_id.as_deref(),
                    Some("notification-42")
                );
                assert_eq!(captured_origin.comment_id.as_deref(), Some("4200"));
                assert_eq!(
                    captured_origin
                        .context
                        .as_ref()
                        .unwrap()
                        .triggering_comment
                        .as_ref()
                        .unwrap()
                        .author,
                    "maintainer"
                );

                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    config.clone(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                let handler = daemon.handler.clone();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();
                assert_eq!(handler.drain(None).await.unwrap()["enqueued"], 1);

                let job = handler
                    .context
                    .read()
                    .await
                    .jobs
                    .values()
                    .find(|job| job.row.source == EnqueueSource::Gh)
                    .cloned()
                    .unwrap();
                assert_eq!(job.row.gh_origin.as_ref(), Some(&captured_origin));
                let request = execution_request(
                    &executor,
                    &job,
                    settings().unit_limits,
                    ("/run/tally/tally.sock", None),
                    &paths.data_dir,
                    &GitAiConfig::default(),
                    false,
                )
                .unwrap();
                let args = executor
                    .build_systemd_argv(&request)
                    .unwrap()
                    .into_iter()
                    .map(|arg| arg.into_string().unwrap())
                    .collect::<Vec<_>>();
                let launched_environment = args
                    .windows(2)
                    .filter(|pair| pair[0] == "--setenv")
                    .filter_map(|pair| pair[1].split_once('='))
                    .map(|(name, value)| (name.to_owned(), value.to_owned()))
                    .collect::<BTreeMap<_, _>>();
                let github_environment = launched_environment
                    .into_iter()
                    .filter(|(name, _)| name.starts_with("TALLY_GH_"))
                    .collect::<BTreeMap<_, _>>();
                assert_eq!(
                    github_environment,
                    BTreeMap::from([
                        ("TALLY_GH_REPO".to_owned(), "acme/widgets".to_owned()),
                        ("TALLY_GH_NUMBER".to_owned(), "42".to_owned()),
                        (
                            "TALLY_GH_URL".to_owned(),
                            "https://github.com/acme/widgets/pull/42".to_owned()
                        ),
                        ("TALLY_GH_TYPE".to_owned(), "pull_request".to_owned()),
                        (
                            "TALLY_GH_HEAD_SHA".to_owned(),
                            "4242424242424242424242424242424242424242".to_owned()
                        ),
                        ("TALLY_GH_NODE_ID".to_owned(), "PR_origin_42".to_owned()),
                        (
                            "TALLY_GH_TRIGGER_KIND".to_owned(),
                            "mention".to_owned()
                        ),
                        (
                            "TALLY_GH_TRIGGER_ACTOR".to_owned(),
                            "maintainer".to_owned()
                        ),
                        (
                            "TALLY_GH_EVENT_ID".to_owned(),
                            "notification-42".to_owned()
                        ),
                        ("TALLY_GH_COMMENT_ID".to_owned(), "4200".to_owned()),
                        (
                            "TALLY_GH_CONTEXT".to_owned(),
                            executor
                                .gh_context_path(&request.identity)
                                .to_string_lossy()
                                .into_owned()
                        ),
                    ])
                );

                let row = query_row(&job.row, RowStatus::Pending);
                let expected_projection = crate::query::GhOriginProjection {
                    repo: "acme/widgets".to_owned(),
                    number: 42,
                    url: "https://github.com/acme/widgets/pull/42".to_owned(),
                };
                let task_uuid = job.row.uuid.to_string();
                assert_eq!(row.gh_origin, Some(expected_projection.clone()));
                let status = handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                let projected = status["jobs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|projected| projected["taskUuid"] == task_uuid)
                    .unwrap();
                assert_eq!(
                    projected["ghOrigin"],
                    serde_json::to_value(&expected_projection).unwrap()
                );
                let standup = query_standup(
                    &[row],
                    &[],
                    &[],
                    &StandupOptions {
                        since: None,
                        since_realtime_us: None,
                        until: "2026-07-24T00:00:00Z".to_owned(),
                        source: Some("gh".to_owned()),
                    },
                );
                assert_eq!(standup.in_flight.len(), 1);
                assert_eq!(
                    standup.in_flight[0].gh_origin,
                    Some(expected_projection.clone())
                );

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
                let restarted =
                    Daemon::open_with_executor(config, paths, settings(), executor)
                        .await
                        .unwrap();
                let restarted_status = restarted
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                let restarted_job = restarted_status["jobs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|projected| projected["taskUuid"] == task_uuid)
                    .unwrap();
                assert_eq!(
                    restarted_job["ghOrigin"],
                    serde_json::to_value(expected_projection).unwrap()
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn codex_job_options_cwd_and_workspace_reach_systemd_as_exact_direct_values() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let mut config = one_pool_config();
                config.adapters.insert(
                    "codex".to_owned(),
                    AdapterConfig {
                        argv: vec![
                            "codex".to_owned(),
                            "exec".to_owned(),
                            "--json".to_owned(),
                            "--".to_owned(),
                        ],
                        launch: crate::adapters::AdapterLaunchConfig {
                            allow_pre_prompt_argv: true,
                            cwd_argv: Some(vec!["-C".to_owned(), "%<cwd>%".to_owned()]),
                            approval_policies: BTreeMap::from([("never".to_owned(), Vec::new())]),
                            sandbox_policies: BTreeMap::from([(
                                "danger-full-access".to_owned(),
                                Vec::new(),
                            )]),
                            ..crate::adapters::AdapterLaunchConfig::default()
                        },
                        ..AdapterConfig::default()
                    },
                );
                config.validate().unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor.clone())
                        .await
                        .unwrap();
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["author wave 3"],
                        "pool": "slot",
                        "adapter": "codex",
                        "cwd": "/worktrees/issue-28",
                        "workspace": {
                            "repo": "mecattaf/tally.nix",
                            "baseRev": "origin/main",
                            "branch": "wave-3-ergonomics",
                            "worktreePath": "/worktrees/issue-28"
                        },
                        "adapterOptions": {
                            "prePromptArgv": ["--dangerously-bypass-approvals-and-sandbox"],
                            "environment": {"NO_COLOR": "1"},
                            "approvalPolicy": "never",
                            "sandboxPolicy": "danger-full-access"
                        }
                    })))
                    .await
                    .unwrap();
                let job_id = Uuid::parse_str(admitted["job_id"].as_str().unwrap()).unwrap();
                let job = daemon
                    .handler
                    .context
                    .read()
                    .await
                    .jobs
                    .get(&job_id)
                    .cloned()
                    .unwrap();
                assert_eq!(
                    job.invocation.argv,
                    [
                        "codex",
                        "exec",
                        "--json",
                        "--dangerously-bypass-approvals-and-sandbox",
                        "-C",
                        "/worktrees/issue-28",
                        "--",
                        "author wave 3",
                    ]
                );
                assert_eq!(job.invocation.env["NO_COLOR"], "1");
                assert_eq!(
                    job.row.workspace.as_ref().unwrap().repo,
                    "mecattaf/tally.nix"
                );

                let request = execution_request(
                    &executor,
                    &job,
                    settings().unit_limits,
                    ("/run/tally/tally.sock", None),
                    &paths.data_dir,
                    &GitAiConfig::default(),
                    false,
                )
                .unwrap();
                let args = executor
                    .build_systemd_argv(&request)
                    .unwrap()
                    .into_iter()
                    .map(|argument| argument.into_string().unwrap())
                    .collect::<Vec<_>>();
                assert!(args
                    .windows(2)
                    .any(|pair| { pair == ["--working-directory", "/worktrees/issue-28"] }));
                for expected in [
                    "NO_COLOR=1",
                    "TALLY_WORKSPACE_REPO=mecattaf/tally.nix",
                    "TALLY_WORKSPACE_BASE_REV=origin/main",
                    "TALLY_WORKSPACE_BRANCH=wave-3-ergonomics",
                    "TALLY_WORKSPACE_PATH=/worktrees/issue-28",
                ] {
                    assert!(args.windows(2).any(|pair| pair == ["--setenv", expected]));
                }
                assert!(args.ends_with(&[
                    "--".to_owned(),
                    "codex".to_owned(),
                    "exec".to_owned(),
                    "--json".to_owned(),
                    "--dangerously-bypass-approvals-and-sandbox".to_owned(),
                    "-C".to_owned(),
                    "/worktrees/issue-28".to_owned(),
                    "--".to_owned(),
                    "author wave 3".to_owned(),
                ]));
                assert_eq!(
                    query_row(&job.row, RowStatus::Pending)
                        .workspace
                        .unwrap()
                        .worktree_path,
                    PathBuf::from("/worktrees/issue-28")
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn public_continuation_uses_the_scraped_session_without_manual_captures() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let program = temp.path().join("resumable-agent");
                crate::test_support::install_shell_program(
                    &program,
                    "#!/bin/sh\nprintf '%s\\n' '{\"thread_id\":\"session-28\"}'\n",
                );
                let mut config = one_pool_config();
                config.adapters.insert(
                    "resumable".to_owned(),
                    AdapterConfig {
                        argv: vec![
                            program.to_string_lossy().into_owned(),
                            "fresh".to_owned(),
                            "--".to_owned(),
                        ],
                        resume: Some(vec![
                            program.to_string_lossy().into_owned(),
                            "resume".to_owned(),
                            "%<sessionRef>%".to_owned(),
                            "--".to_owned(),
                        ]),
                        scrape: BTreeMap::from([(
                            "sessionRef".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPath,
                                pattern: "$..thread_id".to_owned(),
                            },
                        )]),
                        ..AdapterConfig::default()
                    },
                );
                config.validate().unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon = Daemon::open_with_executor(config, paths, settings(), executor)
                    .await
                    .unwrap();
                let first = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["initial request"],
                        "pool": "slot",
                        "adapter": "resumable"
                    })))
                    .await
                    .unwrap();
                let finished =
                    tokio::time::timeout(Duration::from_secs(2), daemon.completion_rx.recv())
                        .await
                        .unwrap()
                        .unwrap();
                daemon.finish_job(finished).await.unwrap();
                daemon.handler.drain_post_ack_tasks().await;
                let first_id = first["job_id"].as_str().unwrap();
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .jobs
                        .get(&Uuid::parse_str(first_id).unwrap())
                        .unwrap()
                        .row
                        .session_ref
                        .as_deref(),
                    Some("session-28")
                );

                let continued = daemon
                    .handler
                    .continue_job(Some(json!({
                        "resumeFrom": first_id,
                        "argv": ["address review"]
                    })))
                    .await
                    .unwrap();
                let continued_id = Uuid::parse_str(continued["job_id"].as_str().unwrap()).unwrap();
                let continued_job = daemon
                    .handler
                    .context
                    .read()
                    .await
                    .jobs
                    .get(&continued_id)
                    .cloned()
                    .unwrap();
                assert_eq!(
                    continued_job.invocation.argv,
                    [
                        program.to_string_lossy().into_owned(),
                        "resume".to_owned(),
                        "session-28".to_owned(),
                        "--".to_owned(),
                        "address review".to_owned(),
                    ]
                );
                assert_eq!(continued_job.row.resumed_from.as_deref(), Some(first_id));
                assert_eq!(continued_job.row.session_ref.as_deref(), Some("session-28"));

                let finished =
                    tokio::time::timeout(Duration::from_secs(2), daemon.completion_rx.recv())
                        .await
                        .unwrap()
                        .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let terminal = daemon
                    .handler
                    .await_job(Some(json!({"job_id": continued_id.to_string()})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn zero_exit_with_failed_and_missing_declared_gates_is_semantically_rejected() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let manifest = temp.path().join("gates.json");
                fs::write(
                    &manifest,
                    r#"{"schemaVersion":1,"artifact":{"commit":"abc"},"gates":[{"id":"tests","status":"fail","command":"cargo test","reason":"one test failed"}]}"#,
                )
                .unwrap();
                let program = temp.path().join("successful-job");
                crate::test_support::install_shell_program(&program, "#!/bin/sh\nexit 0\n");
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": [program],
                        "pool": "slot",
                        "gateManifest": {
                            "path": manifest,
                            "requiredGateIds": ["tests", "live"],
                            "acceptancePolicy": "execution-and-gates"
                        }
                    })))
                    .await
                    .unwrap();
                let finished = tokio::time::timeout(
                    Duration::from_secs(2),
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap()
                .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let terminal = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": admitted["task_uuid"]})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "failed");
                assert_eq!(terminal["exit_code"], 0);
                assert_eq!(terminal["completion"]["execution"]["status"], "success");
                assert_eq!(terminal["completion"]["gates"]["status"], "fail");
                assert_eq!(
                    terminal["completion"]["gates"]["missingRequiredGateIds"],
                    json!(["live"])
                );
                assert_eq!(
                    terminal["completion"]["acceptance"]["status"],
                    "rejected"
                );
                let status = daemon
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap();
                let public_job = status["jobs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["taskUuid"] == task_uuid)
                    .unwrap();
                assert_eq!(public_job["verdict"], "failed");
                let standup = daemon
                    .handler
                    .query("query.standup", Some(json!({})))
                    .await
                    .unwrap();
                assert!(standup["completed"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|entry| entry["taskUuid"] != task_uuid));
                let gate_failure = standup["gateFails"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["taskUuid"] == task_uuid)
                    .unwrap();
                assert_eq!(gate_failure["verdict"], "failed");
                let (_, witness) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(witness[0].verdict, Verdict::Failed);
                assert_eq!(witness[0].exit_code, 0);
                let completion = witness[0].completion.as_ref().unwrap();
                assert_eq!(
                    completion.execution.status,
                    crate::completion::ExecutionStatus::Success
                );
                assert_eq!(
                    completion.gates.status,
                    crate::completion::GateSummaryStatus::Fail
                );
                assert_eq!(
                    completion.acceptance.status,
                    crate::completion::AcceptanceStatus::Rejected
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preset_gate_defaults_distinguish_absent_manifest_from_gates_passed() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let observed_path = temp.path().join("observed-manifest-path");
                let program = temp.path().join("gate-aware-agent");
                crate::test_support::install_shell_program(
                    &program,
                    format!(
                        concat!(
                            "#!/bin/sh\n",
                            "test -n \"$TALLY_GATE_MANIFEST\" || exit 51\n",
                            "printf '%s' \"$TALLY_GATE_MANIFEST\" > '{}'\n",
                            "if test \"$1\" = write; then\n",
                            "  printf '%s' '{{\"schemaVersion\":1,\"artifact\":null,\"gates\":[]}}' > \"$TALLY_GATE_MANIFEST\"\n",
                            "fi\n",
                        ),
                        observed_path.display(),
                    ),
                );
                let mut config = one_pool_config();
                config
                    .adapters
                    .insert("codex".to_owned(), AdapterConfig::default());
                let executor =
                    Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                        .with_systemd_run(temp.path().join("absent-systemd-run"))
                        .with_unit_probe(ExitFileProbe);
                let mut daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();

                let absent = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": [program, "absent"],
                        "pool": "slot",
                        "adapter": "codex",
                    })))
                    .await
                    .unwrap();
                let finished =
                    tokio::time::timeout(Duration::from_secs(2), daemon.completion_rx.recv())
                        .await
                        .unwrap()
                        .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let absent_result = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": absent["task_uuid"]})))
                    .await
                    .unwrap();
                assert_eq!(absent_result["verdict"], "pass");
                assert_eq!(absent_result["completion"]["gates"]["status"], "not-run");
                let absent_uuid =
                    Uuid::parse_str(absent["task_uuid"].as_str().unwrap()).unwrap();
                assert!(daemon
                    .handler
                    .context
                    .read()
                    .await
                    .jobs
                    .get(&absent_uuid)
                    .unwrap()
                    .row
                    .gate_manifest
                    .is_none());
                assert_eq!(
                    fs::read_to_string(&observed_path).unwrap(),
                    paths
                        .state_dir
                        .join("capture")
                        .join(format!("{absent_uuid}.attempt-1.gates.json"))
                        .to_string_lossy()
                );

                let passed = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": [program, "write"],
                        "pool": "slot",
                        "adapter": "codex",
                    })))
                    .await
                    .unwrap();
                let finished =
                    tokio::time::timeout(Duration::from_secs(2), daemon.completion_rx.recv())
                        .await
                        .unwrap()
                        .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let passed_result = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": passed["task_uuid"]})))
                    .await
                    .unwrap();
                assert_eq!(passed_result["verdict"], "pass");
                assert_eq!(passed_result["completion"]["gates"]["status"], "pass");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_24_5_trace_and_scraped_usage_are_advisory_only() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let program = temp.path().join("custom-agent");
                crate::test_support::install_shell_program(
                    &program,
                    concat!(
                        "#!/bin/sh\n",
                        "test \"$1\" = --structured || exit 41\n",
                        "test \"$2\" = 'literal;$(not-a-shell)' || exit 42\n",
                        "test \"$CUSTOM_AGENT_MODE\" = batch || exit 43\n",
                        "test \"$TALLY_YIELD_HOOK\" = '[\"tally\",\"lease\",\"status\"]' || exit 44\n",
                        "test -S \"$TALLY_SOCKET\" || exit 45\n",
                        "test \"$3\" = '' || exit 46\n",
                        "test \"$4\" = --option-looking || exit 47\n",
                        "printf '%s\\n' '{\"event\":{\"session_id\":\"session-opaque\",\"model\":\"Provider/Model.Exact-CASE\",\"usage\":{\"input_tokens\":999999},\"final_message\":\"{\\\"answer\\\":42}\",\"claimed_verdict\":\"fail\",\"claimed_evidence\":\"fail\",\"claimed_charge\":999999,\"claimed_gpu_seconds\":999999}}'\n",
                        "printf '%s\\n' 'branch=adapter-test' >&2\n",
                        "sleep 0.1\n"
                    ),
                );
                let mut config = one_pool_config();
                let mut adapter = structured_adapter(&program);
                adapter.trace = Some(AdapterTrace {
                    stream: ScrapeStream::Stdout,
                    framing: TraceFraming::JsonLines,
                });
                config.adapters.insert("from-nix".to_owned(), adapter);
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon =
                    Daemon::open_with_executor(
                        config.clone(),
                        paths.clone(),
                        settings(),
                        executor.clone(),
                    )
                        .await
                        .unwrap();
                let unknown = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["must-not-run"],
                        "pool": "slot",
                        "adapter": "not-declared"
                    })))
                    .await
                    .unwrap_err();
                assert_eq!(unknown.code, WireErrorCode::InvalidParams);
                assert!(unknown.message.contains("unknown adapter"));
                assert!(daemon.handler.context.read().await.jobs.is_empty());
                assert!(!paths.events_dir().exists());
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["literal;$(not-a-shell)", "", "--option-looking"],
                        "pool": "slot",
                        "priority": "high",
                        "adapter": "from-nix",
                        "source": "manual",
                        "evidence": ["exit:0"],
                        "consumptionEstimate": 7
                    })))
                    .await
                    .unwrap();
                let job_id = admitted["job_id"].as_str().unwrap();
                let hook_status = daemon
                    .handler
                    .lease_status(Some(json!({"jobId": job_id})))
                    .await
                    .unwrap();
                assert_eq!(hook_status["held"], true);

                let finished = tokio::time::timeout(
                    Duration::from_secs(2),
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap()
                .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let terminal = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": admitted["task_uuid"]})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");

                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let enriched = daemon
                            .handler
                            .context
                            .read()
                            .await
                            .jobs
                            .get(&Uuid::parse_str(job_id).unwrap())
                            .and_then(|job| job.row.session_ref.as_deref())
                            == Some("session-opaque");
                        if enriched && paths.attestations_path().exists() {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();

                let attestation_line = fs::read_to_string(paths.attestations_path()).unwrap();
                let attestation: crate::witness::AttestationRecord =
                    serde_json::from_str(attestation_line.lines().next().unwrap()).unwrap();
                assert_eq!(attestation.payload["kind"], "adapter-scrape");
                assert_eq!(
                    attestation.payload["captures"]["model"],
                    "Provider/Model.Exact-CASE"
                );
                assert_eq!(
                    attestation.payload["captures"]["usage"]["input_tokens"],
                    999999
                );
                assert_eq!(attestation.payload["usageAuthority"], "advisory-only");
                let (report, witness) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(report.ok);
                assert_eq!(witness.len(), 1);
                assert_eq!(witness[0].verdict, Verdict::Pass);
                assert_eq!(witness[0].gpu_seconds, None);
                assert_eq!(witness[0].charge, None);
                assert_eq!(witness[0].model, None);
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .jobs
                        .get(&Uuid::parse_str(job_id).unwrap())
                        .unwrap()
                        .row
                        .model
                        .as_deref(),
                    Some("Provider/Model.Exact-CASE")
                );
                let task_uuid = admitted["task_uuid"].as_str().unwrap();
                let before = daemon
                    .handler
                    .query("query.job", Some(json!({"id": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(
                    before["job"]["finalMessage"],
                    json!({
                        "value": "{\"answer\":42}",
                        "authority": "advisory-provider-capture",
                        "provenance": "adapter-scrape",
                    })
                );
                let canonical_before = json!({
                    "priority": before["job"]["priority"],
                    "pool": before["job"]["pool"],
                    "evidenceSpecs": before["job"]["evidenceSpecs"],
                    "evidenceResult": before["job"]["evidenceResult"],
                    "terminalVerdict": before["job"]["terminalVerdict"],
                    "charge": before["job"]["charge"],
                    "gpuSeconds": before["job"]["gpuSeconds"],
                    "canonicalGpuSeconds": before["job"]["canonicalGpuSeconds"],
                });
                assert_eq!(canonical_before["priority"], "high");
                assert_eq!(canonical_before["pool"], "slot");
                assert_eq!(canonical_before["evidenceSpecs"], json!(["exit:0"]));
                assert_eq!(canonical_before["evidenceResult"], "pass");
                assert_eq!(canonical_before["terminalVerdict"], "pass");
                assert_eq!(canonical_before["charge"], Value::Null);
                assert_eq!(canonical_before["gpuSeconds"], Value::Null);
                assert_eq!(canonical_before["canonicalGpuSeconds"], Value::Null);

                let trace = daemon
                    .handler
                    .query(
                        "query.trace",
                        Some(json!({"task": task_uuid, "limit": 100})),
                    )
                    .await
                    .unwrap();
                assert_eq!(trace["items"].as_array().unwrap().len(), 1);
                assert_eq!(
                    trace["items"][0]["authority"],
                    "advisory-provider-capture"
                );
                assert_eq!(trace["items"][0]["provenance"], "provider-capture");
                assert_eq!(
                    trace["items"][0]["payload"]["event"]["claimed_verdict"],
                    "fail"
                );
                assert_eq!(
                    trace["items"][0]["payload"]["event"]["claimed_charge"],
                    999999
                );
                let after = daemon
                    .handler
                    .query("query.job", Some(json!({"id": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(
                    canonical_before,
                    json!({
                        "priority": after["job"]["priority"],
                        "pool": after["job"]["pool"],
                        "evidenceSpecs": after["job"]["evidenceSpecs"],
                        "evidenceResult": after["job"]["evidenceResult"],
                        "terminalVerdict": after["job"]["terminalVerdict"],
                        "charge": after["job"]["charge"],
                        "gpuSeconds": after["job"]["gpuSeconds"],
                        "canonicalGpuSeconds": after["job"]["canonicalGpuSeconds"],
                    })
                );

                drop(daemon);
                tokio::task::yield_now().await;
                fs::remove_file(paths.attestations_path()).unwrap();
                let reopened = Daemon::open_with_executor(
                    config.clone(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                assert_eq!(
                    reopened
                        .handler
                        .context
                        .read()
                        .await
                        .query_rows
                        .values()
                        .next()
                        .unwrap()
                        .session_ref
                        .as_deref(),
                    Some("session-opaque")
                );
                assert_eq!(
                    reopened
                        .handler
                        .context
                        .read()
                        .await
                        .query_rows
                        .values()
                        .next()
                        .unwrap()
                        .model
                        .as_deref(),
                    Some("Provider/Model.Exact-CASE")
                );
                assert_eq!(
                    reopened
                        .handler
                        .query("query.job", Some(json!({"id": task_uuid})))
                        .await
                        .unwrap()["job"]["finalMessage"]["value"],
                    "{\"answer\":42}"
                );
                let repaired = fs::read_to_string(paths.attestations_path()).unwrap();
                assert_eq!(repaired.lines().count(), 1);
                let repaired: crate::witness::AttestationRecord =
                    serde_json::from_str(repaired.lines().next().unwrap()).unwrap();
                assert_eq!(repaired.payload["reconciledAfterRestart"], true);
                assert_eq!(repaired.payload["leaseEpoch"], 1);

                drop(reopened);
                let mut db = TaskDb::open(&paths.data_dir).await.unwrap();
                let projected = db
                    .get_row(Uuid::parse_str(admitted["task_uuid"].as_str().unwrap()).unwrap())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(projected.value("session_ref"), Some("session-opaque"));
                assert_eq!(projected.value("model"), Some("Provider/Model.Exact-CASE"));
                assert_eq!(projected.value("final_message"), Some("{\"answer\":42}"));
                drop(db);

                let deduplicated =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                assert_eq!(
                    fs::read_to_string(paths.attestations_path())
                        .unwrap()
                        .lines()
                        .count(),
                    1
                );
                drop(deduplicated);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_24_9_queries_and_trace_never_project_credential_values() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let credential = temp.path().join("provider-token");
                let credential_value = "wave-five-super-secret-value";
                fs::write(&credential, credential_value).unwrap();
                fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();

                let mut config = one_pool_config();
                config
                    .pools
                    .get_mut("slot")
                    .unwrap()
                    .credentials
                    .insert("provider-token".to_owned(), credential.clone());
                config.adapters.get_mut("shell").unwrap().trace = Some(AdapterTrace {
                    stream: ScrapeStream::Stdout,
                    framing: TraceFraming::JsonLines,
                });
                config.validate().unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                let watch_tail = daemon
                    .handler
                    .query("query.watch", Some(json!({})))
                    .await
                    .unwrap()["nextCursor"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();
                let pool_change = daemon
                    .handler
                    .query(
                        "query.watch",
                        Some(json!({"after": watch_tail, "limit": 100})),
                    )
                    .await
                    .unwrap();
                assert!(pool_change["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|change| {
                        change["kind"] == "pool" && change["payload"]["update"] == "paused"
                    }));
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["/bin/true"],
                        "pool": "slot",
                        "priority": "high",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap();
                let proxy_attempt = daemon
                    .handler
                    .query(
                        "query.watch",
                        Some(json!({"method": "queue.resume", "params": {"all": true}})),
                    )
                    .await
                    .unwrap_err();
                assert_eq!(proxy_attempt.code, WireErrorCode::InvalidParams);
                assert!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .paused_pools
                        .contains("slot"),
                    "read-only query RPC changed queue state"
                );

                let responses = vec![
                    daemon
                        .handler
                        .query("query.jobs", Some(json!({})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.job", Some(json!({"id": task_uuid})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.log", Some(json!({"task": task_uuid})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.proof", Some(json!({"task": task_uuid})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.trace", Some(json!({"task": task_uuid})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.producers", Some(json!({})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.status", Some(json!({})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.render", Some(json!({"format": "json"})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.standup", Some(json!({})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.pools", Some(json!({})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.watch", Some(json!({})))
                        .await
                        .unwrap(),
                ];
                assert_eq!(
                    responses[0]["items"][0]["credentialNames"],
                    json!(["provider-token"])
                );
                assert_eq!(
                    responses[4]["generations"][0]["reason"],
                    "capture-not-retained-for-generation"
                );
                let encoded = serde_json::to_string(&responses).unwrap();
                assert!(!encoded.contains(credential_value));
                assert!(!encoded.contains(credential.to_string_lossy().as_ref()));
                assert_eq!(
                    fs::metadata(&credential).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminal_ack_precedes_scrape_and_shutdown_joins_attestation_writer() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let program = temp.path().join("checkpoint-agent");
                crate::test_support::install_shell_program(
                    &program,
                    concat!(
                        "#!/bin/sh\n",
                        "printf '%s\\n' '{\"event\":{\"session_id\":\"blocked-attestation\",\"model\":\"Exact/Blocked\",\"usage\":{\"tokens\":3}}}'\n",
                        "printf '%s\\n' 'branch=shutdown-test' >&2\n"
                    ),
                );
                let mut config = one_pool_config();
                config
                    .adapters
                    .insert("blocked".to_owned(), structured_adapter(&program));
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                    .await
                    .unwrap();
                let handler = daemon.handler.clone();
                let attestation_path = paths.attestations_path();
                let lock = OpenOptions::new()
                    .create(true)
                    .read(true)
                    .append(true)
                    .open(&attestation_path)
                    .unwrap();
                lock.lock_exclusive().unwrap();

                let (shutdown, shutdown_rx) = watch::channel(false);
                let mut daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let admitted = handler
                    .enqueue(Some(json!({
                        "argv": ["work"],
                        "pool": "slot",
                        "adapter": "blocked",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let terminal = tokio::time::timeout(
                    Duration::from_secs(2),
                    handler.await_job(Some(json!({"task_uuid": admitted["task_uuid"]}))),
                )
                .await
                .expect("terminal witness acknowledgement waited on scrape")
                .unwrap();
                assert_eq!(terminal["verdict"], "pass");

                shutdown.send(true).unwrap();
                assert!(tokio::time::timeout(Duration::from_millis(50), &mut daemon_task)
                    .await
                    .is_err());
                fs2::FileExt::unlock(&lock).unwrap();
                tokio::time::timeout(Duration::from_secs(2), daemon_task)
                    .await
                    .expect("daemon did not join the post-ack writer")
                    .unwrap()
                    .unwrap();
                assert!(verify_attestations(&attestation_path).unwrap().ok);
            })
            .await;
    }

    #[test]
    fn recovery_resume_scrapes_before_executor_capture_truncation() {
        let temp = tempdir().unwrap();
        let paths = DaemonPaths {
            socket: temp.path().join("run/tally.sock"),
            state_dir: temp.path().join("state"),
            data_dir: temp.path().join("data"),
        };
        let program = temp.path().join("agent");
        let mut config = one_pool_config();
        config
            .adapters
            .insert("resumable".to_owned(), structured_adapter(&program));
        let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap());
        let mut row = durable_row(Uuid::new_v4(), "resume-key", 2);
        row.adapter = "resumable".to_owned();
        row.argv = vec!["continue-work".to_owned()];
        row.attempt = 2;
        let capture_paths = executor.paths(&ExecutionIdentity {
            job_id: row.uuid,
            task_uuid: Some(row.uuid),
        });
        fs::create_dir_all(capture_paths.stdout.parent().unwrap()).unwrap();
        fs::create_dir_all(capture_paths.capture_generation.parent().unwrap()).unwrap();
        fs::write(
            &capture_paths.capture_generation,
            r#"{"attempt":1,"leaseEpoch":1}"#,
        )
        .unwrap();
        let original = b"{\"event\":{\"session_id\":\"resume-me\",\"model\":\"Exact/Model\",\"usage\":{\"input_tokens\":5}}}\n";
        fs::write(&capture_paths.stdout, original).unwrap();
        fs::write(&capture_paths.stderr, b"branch=recovery\n").unwrap();
        let action = RecoveryAction::RePresent {
            row: Box::new(row.clone()),
            trigger: crate::evidence::RetryTrigger::PoolReturn,
            previous_witness_seq: 1,
            previous_attempt: 1,
            previous_lease_epoch: 1,
        };
        let attestation_path = temp.path().join("attestations.jsonl");
        fs::write(
            &capture_paths.capture_generation,
            r#"{"attempt":0,"leaseEpoch":1}"#,
        )
        .unwrap();
        assert!(
            recovery_adapter_invocation(&config, &action, &row, &executor, &attestation_path)
                .unwrap_err()
                .to_string()
                .contains("does not match prior attempt")
        );
        assert_eq!(fs::read(&capture_paths.stdout).unwrap(), original);
        fs::write(
            &capture_paths.capture_generation,
            r#"{"attempt":1,"leaseEpoch":1}"#,
        )
        .unwrap();
        let blocked_attestation = temp.path().join("blocked-attestation");
        fs::create_dir(&blocked_attestation).unwrap();
        assert!(recovery_adapter_invocation(
            &config,
            &action,
            &row,
            &executor,
            &blocked_attestation,
        )
        .is_err());
        assert_eq!(fs::read(&capture_paths.stdout).unwrap(), original);

        let (invocation, captures) =
            recovery_adapter_invocation(&config, &action, &row, &executor, &attestation_path)
                .unwrap();
        assert_eq!(
            invocation.argv,
            [
                program.to_string_lossy().into_owned(),
                "--resume".to_owned(),
                "resume-me".to_owned(),
                "--model".to_owned(),
                "Exact/Model".to_owned(),
                "continue-work".to_owned(),
            ]
        );
        assert_eq!(
            captures.unwrap().captures["branch"],
            Value::String("recovery".to_owned())
        );
        assert_eq!(fs::read(&capture_paths.stdout).unwrap(), original);
        assert!(verified_adapter_attestation_captures(
            &attestation_path,
            row.uuid,
            &row.adapter,
            1,
            1,
        )
        .unwrap()
        .is_some());

        fs::write(
            &capture_paths.stdout,
            b"{\"event\":{\"model\":\"Exact/Model\"}}\n",
        )
        .unwrap();
        let missing_attestation = temp.path().join("missing-attestation.jsonl");
        assert!(matches!(
            recovery_adapter_invocation(&config, &action, &row, &executor, &missing_attestation),
            Err(DaemonError::Adapter(AdapterError::MissingCapture { .. }))
        ));
        assert_eq!(
            fs::read(&capture_paths.stdout).unwrap(),
            b"{\"event\":{\"model\":\"Exact/Model\"}}\n"
        );
        fs::write(
            &capture_paths.capture_generation,
            r#"{"attempt":1,"leaseEpoch":1}"#,
        )
        .unwrap();
        fs::write(&capture_paths.stdout, b"").unwrap();
        let (fallback, captures) =
            recovery_adapter_invocation(&config, &action, &row, &executor, &attestation_path)
                .unwrap();
        assert_eq!(fallback.argv[2], "resume-me");
        assert_eq!(fallback.argv[4], "Exact/Model");
        assert_eq!(captures.unwrap().captures["usage"]["input_tokens"], 5);
        let mut advisory_row = row;
        advisory_row.model = Some("Exact/Model".to_owned());
        let mut plan = empty_plan();
        plan.rows.push(crate::recovery::RecoveryRow {
            row: advisory_row.clone(),
            state: RecoveryRowState::Pending,
            labor_class: LaborClass::Fresh,
            guardrail_depth: 0,
        });
        plan.actions.push(action);
        plan.rows[0].row.session_ref = None;
        plan.rows[0].row.model = None;
        hydrate_represent_adapter_metadata(&mut plan, &config, &executor, &attestation_path)
            .unwrap();
        assert_eq!(plan.rows[0].row.session_ref.as_deref(), Some("resume-me"));
        assert_eq!(plan.rows[0].row.model.as_deref(), Some("Exact/Model"));
        let mut deleted_plan = empty_plan();
        let mut deleted_row = advisory_row.clone();
        deleted_row.session_ref = None;
        deleted_row.model = None;
        deleted_plan.rows.push(crate::recovery::RecoveryRow {
            row: deleted_row,
            state: RecoveryRowState::Deleted,
            labor_class: LaborClass::Fresh,
            guardrail_depth: 0,
        });
        fs::write(
            &capture_paths.capture_generation,
            r#"{"attempt":2,"leaseEpoch":2}"#,
        )
        .unwrap();
        fs::write(&capture_paths.stdout, original).unwrap();
        hydrate_completed_adapter_metadata(
            &mut deleted_plan,
            &config,
            &executor,
            &attestation_path,
        );
        assert_eq!(
            deleted_plan.rows[0].row.session_ref.as_deref(),
            Some("resume-me")
        );
        assert_eq!(
            deleted_plan.rows[0].row.model.as_deref(),
            Some("Exact/Model")
        );
        let mut adopted_plan = empty_plan();
        let mut adopted_row = advisory_row.clone();
        adopted_row.session_ref = None;
        adopted_row.model = None;
        adopted_plan.rows.push(crate::recovery::RecoveryRow {
            row: adopted_row,
            state: RecoveryRowState::AdoptedRunning,
            labor_class: LaborClass::Fresh,
            guardrail_depth: 0,
        });
        adopted_plan.actions.push(RecoveryAction::AdoptRunning {
            identity: RecoveryIdentity::Task(advisory_row.uuid),
            unit: executor.unit_name(&ExecutionIdentity {
                job_id: advisory_row.uuid,
                task_uuid: Some(advisory_row.uuid),
            }),
            invocation_id: "attempt-2-invocation".to_owned(),
            attempt: 2,
            lease_epoch: 2,
            labor_class: Some(LaborClass::Fresh),
        });
        hydrate_adopted_adapter_metadata(&mut adopted_plan, &attestation_path).unwrap();
        assert_eq!(
            adopted_plan.rows[0].row.session_ref.as_deref(),
            Some("resume-me")
        );
        assert_eq!(
            adopted_plan.rows[0].row.model.as_deref(),
            Some("Exact/Model")
        );
        assert!(recovered_model_is_advisory(
            &adopted_plan.rows[0].row,
            None,
            true,
        ));
        let recovered_job = Job {
            job_id: advisory_row.uuid,
            task_uuid: Some(advisory_row.uuid),
            row: advisory_row,
            invocation: fallback,
            labor_class: LaborClass::Fresh,
            state: JobState::Running,
            lease_id: None,
            adopted: false,
            adopted_invocation_id: None,
            model_is_advisory: true,
        };
        assert_eq!(canonical_job_model(&recovered_job), None);
    }

    #[test]
    fn job_barriers_are_deterministic_and_empty_drain_barriers_are_immediate() {
        let mut tracker = BarrierTracker::with_namespace(41);
        let barrier = tracker.register_job("task-1", 1);
        tracker.complete_job("task-1", json!({"verdict": "pass", "attempt": 1}));
        assert_eq!(tracker.retained_entry_count(), 0);
        assert_eq!(parse_job_barrier(&barrier).unwrap(), ("task-1", 1));
        assert_eq!(tracker.snapshot(Vec::new()), "barrier:drain:41:1");
        assert!(matches!(
            tracker.wait_barrier("barrier:drain:41:1").unwrap(),
            WaitRegistration::Ready(_)
        ));
        assert_eq!(
            BarrierTracker::with_namespace(42).snapshot(Vec::new()),
            "barrier:drain:42:1"
        );
    }

    #[test]
    fn fs2_completed_bookkeeping_is_bounded_and_terminal_parents_retire() {
        let mut tracker = BarrierTracker::with_namespace(7);
        for sequence in 0..10_000 {
            let stable = format!("task-{sequence}");
            tracker.register_job(&stable, 1);
            tracker.complete_job(&stable, json!({"attempt": 1, "sequence": sequence}));
        }
        assert_eq!(tracker.retained_entry_count(), 0);
        for sequence in 0..10_000 {
            tracker.snapshot([format!("still-running-{sequence}")]);
        }
        assert_eq!(
            tracker.retained_entry_count(),
            UNCLAIMED_DRAIN_BARRIER_LIMIT
        );

        for _ in 0..10_000 {
            let WaitRegistration::Pending(receiver) = tracker.wait_job("stuck-job") else {
                panic!("an active job wait must register");
            };
            drop(receiver);
        }
        tracker.register_job("prune-trigger", 1);
        assert!(
            tracker.job_waiters.is_empty(),
            "closed waiter senders are evicted on the next tracker operation"
        );

        let pending_barrier = tracker.snapshot(["stuck-job".to_owned()]);
        for _ in 0..10_000 {
            let WaitRegistration::Pending(receiver) =
                tracker.wait_barrier(&pending_barrier).unwrap()
            else {
                panic!("an incomplete drain barrier must register");
            };
            drop(receiver);
        }
        tracker.register_job("second-prune-trigger", 1);
        assert!(
            tracker
                .barriers
                .get(&pending_barrier)
                .unwrap()
                .waiters
                .is_empty(),
            "closed barrier waiter senders are evicted on the next tracker operation"
        );

        let mut guardrails = GuardrailState::new(GuardrailConfig::default()).unwrap();
        for sequence in 0..10_000 {
            let stable = format!("parent-{sequence}");
            guardrails.register_parent(
                stable.clone(),
                ParentInfo {
                    parent_uuid: stable.clone(),
                    depth: 0,
                    outstanding: 0,
                    no_enqueue: false,
                    terminal: false,
                },
            );
            guardrails.retire_parent(&stable);
        }
        assert_eq!(guardrails.parent_count(), 0);

        guardrails.register_parent(
            "parent-with-child",
            ParentInfo {
                parent_uuid: "parent-with-child".to_owned(),
                depth: 0,
                outstanding: 1,
                no_enqueue: false,
                terminal: false,
            },
        );
        guardrails.retire_parent("parent-with-child");
        assert!(guardrails.parent("parent-with-child").unwrap().terminal);
        guardrails
            .rollback_child_charge("parent-with-child")
            .unwrap();
        assert!(guardrails.parent("parent-with-child").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn query_pools_exposes_the_active_window_reset() {
        let temp = tempdir().unwrap();
        let paths = DaemonPaths {
            socket: temp.path().join("run/tally.sock"),
            state_dir: temp.path().join("state"),
            data_dir: temp.path().join("data"),
        };
        let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
            .with_systemd_run(temp.path().join("absent-systemd-run"))
            .with_unit_probe(ExitFileProbe);
        let mut config = window_pool_config();
        config.pools.get_mut("api").unwrap().usage_meter = Some(UsageMeterConfig {
            argv: vec!["meter-feeder".to_owned()],
            poll_interval_sec: 120,
            budget_class: MeterBudgetClass::Programmatic,
        });
        let meter_path = usage_meter_event_path(&paths.state_dir, "api");
        let daemon = Daemon::open_with_executor(config, paths, settings(), executor)
            .await
            .unwrap();
        let id = Uuid::new_v4();
        daemon
            .handler
            .context
            .write()
            .await
            .lease
            .admit(
                LeaseRequest {
                    job_id: id.to_string(),
                    // Explicit acquire/release reservations are held by the
                    // daemon itself. They vanish with its epoch on restart;
                    // unlike execution leases, they must never name a
                    // fictional job unit or be physically preempted.
                    unit: "tally-daemon.service".to_owned(),
                    pools: vec!["api".to_owned()],
                    priority: Priority::Medium,
                    admission_key: Some(format!("{id}:1")),
                    consumption_estimate: Some(40),
                    scheduling_group: LeaseSchedulingGroup::Standalone,
                },
                Utc::now(),
            )
            .unwrap();
        let pools = daemon
            .handler
            .query("query.pools", Some(json!({})))
            .await
            .unwrap();
        assert_eq!(pools["pools"][0]["consumptionUsed"], 40);
        assert_eq!(pools["pools"][0]["remainingBudget"], 60);
        assert!(pools["pools"][0]["resetAt"].as_str().is_some());

        fs::create_dir_all(meter_path.parent().unwrap()).unwrap();
        let observed_at = Utc::now();
        fs::write(
            &meter_path,
            serde_json::to_vec(&json!({
                "pool": "api",
                "budget_class": "programmatic",
                "utilization_pct": 80.0,
                "weekly_utilization_pct": 81.0,
                "reset_at": (observed_at + chrono::Duration::hours(1)).to_rfc3339(),
                "observed_at": observed_at.to_rfc3339(),
            }))
            .unwrap(),
        )
        .unwrap();
        let clamped = daemon
            .handler
            .query("query.pools", Some(json!({})))
            .await
            .unwrap();
        assert_eq!(clamped["pools"][0]["selfUtilizationPct"], 40.0);
        assert_eq!(clamped["pools"][0]["effectiveUtilizationPct"], 80.0);
        assert_eq!(clamped["pools"][0]["remainingBudget"], 20);
        assert_eq!(clamped["pools"][0]["signal"], "STOP");

        fs::write(
            &meter_path,
            serde_json::to_vec(&json!({
                "pool": "api",
                "budget_class": "programmatic",
                "utilization_pct": 10.0,
                "reset_at": (observed_at + chrono::Duration::hours(1)).to_rfc3339(),
                "observed_at": observed_at.to_rfc3339(),
            }))
            .unwrap(),
        )
        .unwrap();
        let cannot_grant = daemon
            .handler
            .query("query.pools", Some(json!({})))
            .await
            .unwrap();
        assert_eq!(cannot_grant["pools"][0]["effectiveUtilizationPct"], 40.0);
        assert_eq!(cannot_grant["pools"][0]["remainingBudget"], 60);

        fs::write(
            &meter_path,
            br#"{"pool":"wrong","budget_class":"programmatic","utilization_pct":99,"reset_at":"2999-01-01T00:00:00Z","observed_at":"2999-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let ignored = daemon
            .handler
            .query("query.pools", Some(json!({})))
            .await
            .unwrap();
        assert_eq!(ignored["pools"][0]["effectiveUtilizationPct"], 40.0);
        assert_eq!(ignored["pools"][0]["remainingBudget"], 60);
    }

    #[test]
    fn built_in_usage_feeder_routes_tokens_and_can_only_clamp_headroom_downward() {
        let temp = tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let mut config = window_pool_config();
        let captures = ScrapeResult {
            captures: BTreeMap::from([(
                "usage".to_owned(),
                json!({"input_tokens": 30, "output_tokens": 50}),
            )]),
        };
        assert!(
            feed_scraped_usage(&state_dir, &config.pools, &["api".to_owned()], &captures,)
                .is_empty()
        );
        let event = read_usage_meter(&state_dir, "api", 3600, Utc::now()).unwrap();
        assert_eq!(event.utilization_pct, 80.0);

        let projection = query_pools(&[PoolHeadroomFact {
            pool: "api".to_owned(),
            capacity: 1,
            held: 0,
            queued: 0,
            consumption: Some(WindowConsumptionFact {
                used: 40,
                cap: 100,
                reset_at: None,
            }),
            meter_utilization_pct: Some(event.utilization_pct),
            weekly_utilization_pct: None,
        }])
        .unwrap();
        assert_eq!(projection.pools[0].self_utilization_pct, 40.0);
        assert_eq!(projection.pools[0].effective_utilization_pct, 80.0);

        let path = usage_meter_event_path(&state_dir, "api");
        let low = ScrapeResult {
            captures: BTreeMap::from([("usage".to_owned(), json!({"total_tokens": 10}))]),
        };
        assert!(
            feed_scraped_usage(&state_dir, &config.pools, &["api".to_owned()], &low,).is_empty()
        );
        let low = read_usage_meter(&state_dir, "api", 3600, Utc::now()).unwrap();
        let projection = query_pools(&[PoolHeadroomFact {
            pool: "api".to_owned(),
            capacity: 1,
            held: 0,
            queued: 0,
            consumption: Some(WindowConsumptionFact {
                used: 40,
                cap: 100,
                reset_at: None,
            }),
            meter_utilization_pct: Some(low.utilization_pct),
            weekly_utilization_pct: None,
        }])
        .unwrap();
        assert_eq!(projection.pools[0].effective_utilization_pct, 40.0);

        let valid_bytes = fs::read(&path).unwrap();
        for malformed in [
            json!({"usage": {"total_tokens": "80"}}),
            json!({"usage": {"input_tokens": -1, "output_tokens": 4}}),
            json!({"usage": {"input_tokens": 0, "output_tokens": 0}}),
        ] {
            let captures = ScrapeResult {
                captures: malformed.as_object().unwrap().clone().into_iter().collect(),
            };
            assert!(
                feed_scraped_usage(&state_dir, &config.pools, &["api".to_owned()], &captures,)
                    .is_empty()
            );
            assert_eq!(fs::read(&path).unwrap(), valid_bytes);
        }

        config.pools.get_mut("api").unwrap().usage_meter = Some(UsageMeterConfig {
            argv: vec!["external-meter".to_owned()],
            poll_interval_sec: 120,
            budget_class: MeterBudgetClass::Programmatic,
        });
        fs::remove_file(&path).unwrap();
        assert!(
            feed_scraped_usage(&state_dir, &config.pools, &["api".to_owned()], &captures,)
                .is_empty()
        );
        assert!(
            !path.exists(),
            "an external meter must remain the sole authority"
        );

        let now = Utc::now();
        write_usage_meter(
            &state_dir,
            &UsageMeterObservation {
                pool: "api".to_owned(),
                budget_class: MeterBudgetClass::Programmatic,
                utilization_pct: 99.0,
                weekly_utilization_pct: None,
                observed_at: (now - chrono::Duration::seconds(121)).to_rfc3339(),
                reset_at: (now + chrono::Duration::hours(1)).to_rfc3339(),
            },
        )
        .unwrap();
        assert!(read_usage_meter(&state_dir, "api", 120, now).is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_conformance_cooperative_yield_obeys_grace_then_witnesses_preemption() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut fast_settings = settings();
                let yield_grace = Duration::from_millis(50);
                fast_settings.yield_grace = yield_grace;
                let mut daemon = Daemon::open_with_executor(
                    hard_preempt_config(),
                    paths.clone(),
                    fast_settings,
                    executor,
                )
                .await
                .unwrap();
                let low = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["sleep", "30"],
                        "pool": "slot",
                        "priority": "low",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let urgent = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "priority": "interrupt",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                assert_eq!(low["state"], "running");
                assert_eq!(urgent["state"], "queued");
                let lease_status = daemon
                    .handler
                    .lease_status(Some(json!({"jobId": low["task_uuid"]})))
                    .await
                    .unwrap();
                assert_eq!(lease_status["held"], true);
                assert_eq!(lease_status["yieldRequested"], true);
                let yield_deadline = chrono::DateTime::parse_from_rfc3339(
                    lease_status["yieldDeadline"].as_str().unwrap(),
                )
                .unwrap()
                .with_timezone(&Utc);

                Daemon::tick_leases_at(
                    daemon.handler.clone(),
                    yield_deadline - chrono::Duration::milliseconds(1),
                )
                .await
                .unwrap();
                assert!(
                    read_verified_records(&paths.witness_path())
                        .unwrap()
                        .1
                        .is_empty(),
                    "the holder was preempted before yieldGraceSec elapsed"
                );
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .jobs
                        .get(&Uuid::parse_str(low["job_id"].as_str().unwrap()).unwrap())
                        .unwrap()
                        .state,
                    JobState::Running
                );

                Daemon::tick_leases_at(
                    daemon.handler.clone(),
                    yield_deadline + chrono::Duration::milliseconds(1),
                )
                .await
                .unwrap();

                let low_result = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": low["task_uuid"]})))
                    .await
                    .unwrap();
                assert_eq!(low_result["verdict"], "preempted");
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        if daemon
                            .handler
                            .context
                            .read()
                            .await
                            .jobs
                            .get(&Uuid::parse_str(urgent["job_id"].as_str().unwrap()).unwrap())
                            .is_some_and(|job| job.state == JobState::Completed)
                        {
                            break;
                        }
                        let finished = daemon.completion_rx.recv().await.unwrap();
                        daemon.finish_job(finished).await.unwrap();
                    }
                })
                .await
                .unwrap();
                let urgent_result = tokio::time::timeout(
                    Duration::from_secs(2),
                    daemon
                        .handler
                        .await_job(Some(json!({"task_uuid": urgent["task_uuid"]}))),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(urgent_result["verdict"], "pass");
                let (report, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(report.ok);
                assert_eq!(records[0].verdict, Verdict::Preempted);
                assert_eq!(records[1].verdict, Verdict::Pass);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn interrupt_cooldown_waits_for_active_work_then_holds_the_pool() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let active = client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["sleep", "0.12"],
                            "pool": "slot",
                            "priority": "low",
                            "adapter": "shell",
                            "source": "manual",
                            "evidence": ["exit:0"]
                        })),
                    )
                    .await
                    .unwrap();
                let cooldown = client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["sleep", "0.05"],
                            "pool": "slot",
                            "priority": "interrupt",
                            "adapter": "shell",
                            "source": "manual",
                            "evidence": ["exit:0"],
                            "noEnqueue": true
                        })),
                    )
                    .await
                    .unwrap();
                assert_eq!(active["state"], "running");
                assert_eq!(cooldown["state"], "queued");

                let active_result = client
                    .call(
                        "queue.await_job",
                        Some(json!({"task_uuid": active["task_uuid"]})),
                    )
                    .await
                    .unwrap();
                assert_eq!(active_result["verdict"], "pass");
                let cooldown_result = client
                    .call(
                        "queue.await_job",
                        Some(json!({"task_uuid": cooldown["task_uuid"]})),
                    )
                    .await
                    .unwrap();
                assert_eq!(cooldown_result["verdict"], "pass");

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 2);
                assert!(records.iter().all(|record| record.verdict == Verdict::Pass));
                assert_eq!(
                    records[0].task_uuid.as_deref(),
                    active["task_uuid"].as_str()
                );
                assert_eq!(
                    records[1].task_uuid.as_deref(),
                    cooldown["task_uuid"].as_str()
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_transport_loss_retains_the_lease_until_authoritative_completion() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let calls = Arc::new(AtomicUsize::new(0));
                let release = Arc::new(AtomicBool::new(false));
                let transport = RecoveringRemoteTransport {
                    calls: calls.clone(),
                    release: release.clone(),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_remote_transport(transport);
                let mut daemon = Daemon::open_with_executor(
                    remote_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["opaque-worker-command", "two words", "$HOME"],
                        "pool": "slot",
                        "executor": "worker",
                        "priority": "high",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                assert_eq!(admitted["state"], "running");

                tokio::time::timeout(Duration::from_secs(1), async {
                    while calls.load(Ordering::SeqCst) < 2 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.lease.engine().held_in_pool("slot").unwrap(), 1);
                    let job_id = Uuid::parse_str(admitted["job_id"].as_str().unwrap()).unwrap();
                    assert_eq!(context.jobs[&job_id].state, JobState::Running);
                }
                assert!(daemon.completion_rx.try_recv().is_err());
                let (_, witness_before) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(witness_before.is_empty());

                release.store(true, Ordering::Release);
                let finished =
                    tokio::time::timeout(Duration::from_secs(1), daemon.completion_rx.recv())
                        .await
                        .unwrap()
                        .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let result = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": admitted["task_uuid"]})))
                    .await
                    .unwrap();
                assert_eq!(result["verdict"], "pass");
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .lease
                        .engine()
                        .held_in_pool("slot")
                        .unwrap(),
                    0
                );
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].executor.as_deref(), Some("worker"));
                assert_eq!(calls.load(Ordering::SeqCst), 2);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_conformance_coordinator_switch_bumps_epoch_and_re_adopts_remote_work() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                initialize_final_witness_state(&paths);
                assert_eq!(
                    bump_epoch(&paths.state_dir).unwrap(),
                    1,
                    "the first coordinator generation must exist before the switch"
                );
                let task_uuid = Uuid::new_v4();
                let mut row = durable_row(task_uuid, "restart-remote", 1);
                row.executor = Some("worker".to_owned());
                write_enqueue_event_atomic(
                    &paths.events_dir(),
                    &DurableEnqueueEvent::new(row).unwrap(),
                )
                .unwrap();

                let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
                let transport = RestartRemoteTransport {
                    calls: calls.clone(),
                    attempt: 1,
                    lease_epoch: 1,
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_remote_transport(transport);
                let daemon = Daemon::open_with_executor(
                    remote_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                assert_eq!(daemon.initial_jobs.len(), 1);
                assert!(daemon.initial_jobs[0].adopted);
                assert_eq!(daemon.handler.context.read().await.epoch, 2);
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .lease
                        .engine()
                        .held_in_pool("slot")
                        .unwrap(),
                    1
                );

                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let result = tokio::time::timeout(
                    Duration::from_secs(1),
                    client.call(
                        "queue.await_job",
                        Some(json!({"task_uuid": task_uuid.to_string()})),
                    ),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(result["verdict"], "pass");
                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();

                let calls = calls.lock().unwrap();
                assert_eq!(calls.len(), 2);
                assert!(matches!(calls[0], RemoteExecutorRequest::Probe { .. }));
                match &calls[1] {
                    RemoteExecutorRequest::Adopt { request, .. } => {
                        assert_eq!(request.attempt, 1);
                        assert_eq!(
                            request.lease_epoch, 1,
                            "re-adoption must target the exact pre-switch execution generation"
                        );
                    }
                    other => panic!("expected remote adoption, got {other:?}"),
                }
                assert!(!calls
                    .iter()
                    .any(|request| matches!(request, RemoteExecutorRequest::Ensure { .. })));
                let lease_events = LeaseEventLog::in_state_dir(&paths.state_dir)
                    .read()
                    .unwrap();
                assert!(lease_events.iter().any(|event| {
                    event.epoch == 2
                        && matches!(
                            &event.event,
                            crate::lease::LeaseEventKind::Granted { grant, .. }
                                if grant.epoch == 2 && grant.job_id == task_uuid.to_string()
                        )
                }));
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].executor.as_deref(), Some("worker"));
                assert_eq!(records[0].lease_epoch, 1);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_joins_an_in_flight_lease_tick_before_releasing_the_lock() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let notify_path = temp.path().join("notify.sock");
                let notify_socket = UnixDatagram::bind(&notify_path).unwrap();
                notify_socket
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                daemon.notifier = SystemdNotifier::with_socket(notify_path, None);
                let (tick_started_tx, mut tick_started_rx) = mpsc::unbounded_channel();
                let (release_tick_tx, release_tick_rx) = watch::channel(false);
                daemon.lease_tick_hook = Some(LeaseTickHook {
                    started: tick_started_tx,
                    release: release_tick_rx,
                });
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let mut daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));

                tokio::time::timeout(Duration::from_secs(1), tick_started_rx.recv())
                    .await
                    .expect("lease tick must start")
                    .expect("lease tick hook must remain open");
                shutdown_tx.send(true).unwrap();
                assert!(
                    tokio::time::timeout(Duration::from_millis(50), &mut daemon_task)
                        .await
                        .is_err(),
                    "shutdown must join, not detach or abort, the in-flight lease tick"
                );
                assert!(
                    acquire_daemon_lock(&paths.state_dir).is_err(),
                    "the daemon lock must fence a replacement until the tick finishes"
                );
                let mut notifications = Vec::new();
                let mut buffer = [0_u8; 64];
                for _ in 0..2 {
                    let received = notify_socket.recv(&mut buffer).unwrap();
                    notifications
                        .push(std::str::from_utf8(&buffer[..received]).unwrap().to_owned());
                }
                assert_eq!(
                    notifications,
                    ["READY=1\nSTATUS=tally daemon ready", "STOPPING=1"]
                );

                release_tick_tx.send(true).unwrap();
                tokio::time::timeout(Duration::from_secs(2), &mut daemon_task)
                    .await
                    .expect("shutdown must finish after the tick")
                    .expect("daemon task must not panic")
                    .expect("daemon shutdown must succeed");
                drop(acquire_daemon_lock(&paths.state_dir).unwrap());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stalled_replica_commit_does_not_stall_rpc_or_late_wait() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                prepare_paths(&paths).unwrap();
                drop(WitnessLedger::open(paths.witness_path()).unwrap());
                let epoch = bump_epoch(&paths.state_dir).unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let (commit_started_tx, commit_started_rx) = oneshot::channel();
                let release_commit = Arc::new(AtomicBool::new(false));
                let daemon = Daemon::build(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                    epoch,
                    empty_plan(),
                    Box::new(StallingCommitter {
                        started: Some(commit_started_tx),
                        release: release_commit.clone(),
                    }),
                )
                .unwrap();
                let context = daemon.handler.context.clone();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));

                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let admitted = client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["true"],
                            "pool": "slot",
                            "priority": "high",
                            "adapter": "shell",
                            "source": "orchestrator",
                            "evidence": ["exit:0"]
                        })),
                    )
                    .await
                    .unwrap();
                assert_eq!(admitted["task_uuid"], admitted["job_id"]);
                let durable_job_id = admitted["job_id"].as_str().unwrap();
                assert_eq!(
                    context
                        .read()
                        .await
                        .guardrails
                        .parent(durable_job_id)
                        .unwrap()
                        .parent_uuid,
                    durable_job_id
                );
                commit_started_rx.await.unwrap();

                tokio::time::timeout(
                    Duration::from_millis(250),
                    client.call("query.status", Some(json!({}))),
                )
                .await
                .expect("the socket must stay responsive while commit is stalled")
                .unwrap();

                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let complete = context
                            .read()
                            .await
                            .jobs
                            .values()
                            .all(|job| job.state == JobState::Completed);
                        if complete {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();

                let task_uuid = admitted["task_uuid"].as_str().unwrap();
                let waited = tokio::time::timeout(
                    Duration::from_millis(100),
                    client.call("queue.await_job", Some(json!({"task_uuid": task_uuid}))),
                )
                .await
                .expect("a late wait must resolve immediately")
                .unwrap();
                assert_eq!(waited["verdict"], "pass");

                let barrier = admitted["barrier"].as_str().unwrap();
                let barrier_result = client
                    .call("queue.await_barrier", Some(json!({"barrier": barrier})))
                    .await
                    .unwrap();
                assert_eq!(barrier_result["complete"], true);
                assert!(paths.events_dir().read_dir().unwrap().next().is_some());
                assert!(paths.witness_path().metadata().unwrap().len() > 0);

                release_commit.store(true, Ordering::Release);
                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
                assert!(!paths.socket.exists());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_never_detaches_a_stalled_replica_writer() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                prepare_paths(&paths).unwrap();
                drop(WitnessLedger::open(paths.witness_path()).unwrap());
                let epoch = bump_epoch(&paths.state_dir).unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let (commit_started_tx, commit_started_rx) = oneshot::channel();
                let release_commit = Arc::new(AtomicBool::new(false));
                let daemon = Daemon::build(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                    epoch,
                    empty_plan(),
                    Box::new(StallingCommitter {
                        started: Some(commit_started_tx),
                        release: release_commit.clone(),
                    }),
                )
                .unwrap();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["true"],
                            "pool": "slot",
                            "priority": "high",
                            "adapter": "shell",
                            "source": "manual",
                            "evidence": ["exit:0"]
                        })),
                    )
                    .await
                    .unwrap();
                commit_started_rx.await.unwrap();
                shutdown_tx.send(true).unwrap();
                tokio::time::sleep(Duration::from_millis(1_100)).await;
                assert!(!daemon_task.is_finished());
                assert!(acquire_daemon_lock(&paths.state_dir).is_err());

                release_commit.store(true, Ordering::Release);
                tokio::time::timeout(Duration::from_secs(2), daemon_task)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                drop(acquire_daemon_lock(&paths.state_dir).unwrap());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn restart_re_adopts_an_in_flight_multi_pool_job_with_the_complete_set() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                prepare_paths(&paths).unwrap();
                initialize_final_witness_state(&paths);
                assert_eq!(bump_epoch(&paths.state_dir).unwrap(), 1);
                let mut row = durable_row(Uuid::new_v4(), "restart-multi-pool", 1);
                row.pools = vec!["slot".to_owned(), "zeta".to_owned()];
                let token_hash = hash_job_token(&"ab".repeat(32));
                row.job_token_hash = Some(token_hash.clone());
                write_enqueue_event_atomic(
                    &paths.events_dir(),
                    &DurableEnqueueEvent::new(row.clone()).unwrap(),
                )
                .unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(RunningProbe {
                        attempt: 1,
                        lease_epoch: 1,
                    });

                let daemon = Daemon::open_with_executor(
                    two_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                assert_eq!(daemon.handler.context.read().await.epoch, 2);
                assert_eq!(daemon.initial_jobs.len(), 1);
                assert!(daemon.initial_jobs[0].adopted);
                assert_eq!(daemon.initial_jobs[0].row.pools, ["slot", "zeta"]);
                let context = daemon.handler.context.read().await;
                let recovered = &context.jobs[&row.uuid];
                assert!(recovered.adopted);
                assert_eq!(recovered.row.pools, ["slot", "zeta"]);
                assert!(recovered.lease_id.is_some());
                assert_eq!(context.lease.engine().held_in_pool("slot").unwrap(), 1);
                assert_eq!(context.lease.engine().held_in_pool("zeta").unwrap(), 1);
                assert_eq!(context.lease.engine().queue_len(), 0);
                drop(context);
                assert_eq!(
                    daemon.handler.job_tokens.borrow().get(&token_hash),
                    Some(&row.uuid)
                );
                let mut adopted = daemon.initial_jobs[0].clone();
                assert!(daemon
                    .handler
                    .prepare_execution(&mut adopted)
                    .await
                    .unwrap()
                    .unwrap()
                    .job_token
                    .is_none());
                assert_eq!(adopted.row.job_token_hash, Some(token_hash.clone()));
                assert_eq!(
                    daemon.handler.job_tokens.borrow().get(&token_hash),
                    Some(&row.uuid)
                );

                let lease_log =
                    fs::read_to_string(paths.state_dir.join(crate::lease::LEASE_EVENTS_FILE))
                        .unwrap();
                assert!(lease_log.contains(r#""pools":["slot","zeta"]"#));
                assert!(!lease_log.contains(r#""kind":"released""#));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn restart_reconstructs_terminal_wait_without_reexecution() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let first = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                let first_context = first.handler.context.clone();
                let (first_shutdown, first_shutdown_rx) = watch::channel(false);
                let first_task = tokio::task::spawn_local(first.run_until(first_shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let admitted = client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["true"],
                            "pool": "slot",
                            "priority": "high",
                            "adapter": "shell",
                            "source": "orchestrator",
                            "evidence": ["exit:0"]
                        })),
                    )
                    .await
                    .unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
                assert_eq!(admitted["job_id"], task_uuid);
                let barrier = admitted["barrier"].as_str().unwrap().to_owned();
                let first_result = client
                    .call("queue.await_job", Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(first_result["verdict"], "pass");
                let first_epoch = first_context.read().await.epoch;
                first_shutdown.send(true).unwrap();
                first_task.await.unwrap().unwrap();
                drop(client);
                drop(first_context);
                tokio::task::yield_now().await;

                let witness_before = fs::read(paths.witness_path()).unwrap();
                let exit_record = paths
                    .state_dir
                    .join(crate::executor::UNIT_EXIT_DIRECTORY)
                    .join(format!("{task_uuid}.json"));
                let exit_before = fs::read(&exit_record).unwrap();

                let second = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                assert_eq!(second.handler.context.read().await.epoch, first_epoch + 1);
                assert!(second.initial_jobs.is_empty());
                let (second_shutdown, second_shutdown_rx) = watch::channel(false);
                let second_task = tokio::task::spawn_local(second.run_until(second_shutdown_rx));
                let restarted_client = RpcClient::connect(&paths.socket).await.unwrap();
                let late = tokio::time::timeout(
                    Duration::from_millis(100),
                    restarted_client.call("queue.await_job", Some(json!({"task_uuid": task_uuid}))),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(late["verdict"], "pass");
                let late_barrier = restarted_client
                    .call("queue.await_barrier", Some(json!({"barrier": barrier})))
                    .await
                    .unwrap();
                assert_eq!(late_barrier["complete"], true);
                second_shutdown.send(true).unwrap();
                second_task.await.unwrap().unwrap();

                assert_eq!(fs::read(paths.witness_path()).unwrap(), witness_before);
                assert_eq!(fs::read(exit_record).unwrap(), exit_before);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn restart_finishes_reuse_event_without_reexecution() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                prepare_paths(&paths).unwrap();
                let artifact_hash = format!("sha256:{}", "a".repeat(64));
                let original = durable_row(Uuid::new_v4(), "dedup:crash", 1);
                write_enqueue_event_atomic(
                    &paths.events_dir(),
                    &DurableEnqueueEvent::new(original.clone()).unwrap(),
                )
                .unwrap();
                let mut ledger = WitnessLedger::open(paths.witness_path()).unwrap();
                let pass = ledger
                    .append(WitnessBody {
                        task_uuid: Some(original.uuid.to_string()),
                        transition_timestamp: Utc::now()
                            .to_rfc3339_opts(SecondsFormat::Millis, true),
                        verdict: Verdict::Pass,
                        exit_code: 0,
                        artifact_content_hash: Some(artifact_hash.clone()),
                        store_paths: None,
                        drv: None,
                        gpu_seconds: Some(0.0),
                        wall_clock: 0.0,
                        attempt: 1,
                        lease_epoch: 1,
                        dedup_key: original.dedup_key.clone(),
                        payload_hash: original.payload_hash.clone(),
                        brief_hash: original.brief_hash.clone(),
                        origin: AdmissionOrigin::direct(EnqueueSource::Manual),
                        orchestration: original.orchestration.clone(),
                        labor_class: LaborClass::Fresh,
                        trace_ref: None,
                        pools: vec!["slot".to_owned()],
                        executor: None,
                        host_id: None,
                        charge: None,
                        model: None,
                        evidence_class: None,
                        manifest_hash: None,
                        completion: None,
                        result_revision: None,
                        authorship: None,
                        authorship_sessions: None,
                    })
                    .unwrap();
                drop(ledger);

                let reused = durable_row(Uuid::new_v4(), "dedup:crash", 1);
                let reuse_event = DurableEnqueueEvent::new_reuse_with_depth(
                    reused.clone(),
                    0,
                    pass.seq,
                    Some(artifact_hash.clone()),
                    None,
                )
                .unwrap();
                write_enqueue_event_atomic(&paths.events_dir(), &reuse_event).unwrap();

                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                assert!(daemon.initial_jobs.is_empty());
                let waited = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": reused.uuid})))
                    .await
                    .unwrap();
                assert_eq!(waited["verdict"], "reused");
                let witness_after_repair = fs::read(paths.witness_path()).unwrap();
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 2);
                assert_eq!(records[1].verdict, Verdict::Reused);
                assert_eq!(records[1].labor_class, LaborClass::Reused);
                drop(daemon);

                let reopened = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                assert!(reopened.initial_jobs.is_empty());
                assert_eq!(
                    fs::read(paths.witness_path()).unwrap(),
                    witness_after_repair
                );
            })
            .await;
    }

    #[test]
    fn reuse_reconciliation_rejects_a_missing_prior_pass() {
        let temp = tempdir().unwrap();
        let events = temp.path().join("state/events");
        let witness = temp.path().join("data/witness.jsonl");
        fs::create_dir_all(events.parent().unwrap()).unwrap();
        let row = durable_row(Uuid::new_v4(), "dedup:corrupt", 1);
        let event = DurableEnqueueEvent::new_reuse_with_depth(
            row,
            0,
            99,
            Some(format!("sha256:{}", "b".repeat(64))),
            None,
        )
        .unwrap();
        write_enqueue_event_atomic(&events, &event).unwrap();
        let durable = collect_durable_recovery_facts(&events, &witness).unwrap();
        let mut ledger = WitnessLedger::open(&witness).unwrap();
        let paths = DaemonPaths {
            socket: temp.path().join("run/tally.sock"),
            state_dir: temp.path().join("state"),
            data_dir: temp.path().join("data"),
        };
        assert!(reconcile_reuse_witnesses(&paths, &durable, &mut ledger)
            .unwrap_err()
            .to_string()
            .contains("references missing witness 99"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn singleton_query_and_dedup_are_live_through_the_daemon() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let artifact = temp.path().join("already-built.txt");
                fs::write(&artifact, b"stable artifact\n").unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                let epoch = daemon.handler.context.read().await.epoch;
                let duplicate = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await;
                assert!(matches!(
                    duplicate,
                    Err(DaemonError::Invalid(message)) if message.contains("already owns")
                ));
                assert_eq!(
                    fs::read_to_string(paths.state_dir.join(crate::lease::LEASE_EPOCH_FILE))
                        .unwrap()
                        .trim(),
                    epoch.to_string()
                );

                let context = daemon.handler.context.clone();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let payload = json!({
                    "argv": ["true"],
                    "pool": "slot",
                    "priority": "high",
                    "adapter": "shell",
                    "source": "manual",
                    "dedupKey": "stable-artifact",
                    "evidence": [
                        format!("artifact:{}", artifact.display()),
                        "exit:0"
                    ]
                });
                let first = client
                    .call("queue.enqueue", Some(payload.clone()))
                    .await
                    .unwrap();
                let task_uuid = first["task_uuid"].as_str().unwrap();
                let terminal = client
                    .call("queue.await_job", Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");
                let (_, witness_before) = read_verified_records(&paths.witness_path()).unwrap();

                let status = client
                    .call("query.status", Some(json!({"pool": "slot"})))
                    .await
                    .unwrap();
                assert_eq!(status["protocolVersion"], 4);
                assert_eq!(status["pools"][0]["pool"], "slot");
                assert!(status["jobs"].as_array().unwrap().iter().any(|job| {
                    job["taskUuid"].as_str() == Some(task_uuid) && job["verdict"] == "pass"
                }));
                let pools = client.call("query.pools", Some(json!({}))).await.unwrap();
                assert_eq!(pools["pools"][0]["pool"], "slot");
                let render_text = client
                    .call("query.render", Some(json!({"format": "text"})))
                    .await
                    .unwrap();
                assert!(render_text
                    .as_str()
                    .is_some_and(|text| text.contains("\"protocolVersion\": 4")));
                let render_json = client
                    .call("query.render", Some(json!({"format": "json"})))
                    .await
                    .unwrap();
                assert_eq!(render_json["protocolVersion"], 4);

                let reused = client.call("queue.enqueue", Some(payload)).await.unwrap();
                assert_eq!(reused["state"], "reused");
                assert_eq!(reused["verdict"], "reused");
                let reused_uuid = reused["task_uuid"].as_str().unwrap().to_owned();
                assert_eq!(reused["job_id"], reused_uuid);
                let reused_wait = client
                    .call("queue.await_job", Some(json!({"task_uuid": reused_uuid})))
                    .await
                    .unwrap();
                assert_eq!(reused_wait["verdict"], "reused");
                let reused_barrier = client
                    .call(
                        "queue.await_barrier",
                        Some(json!({"barrier": reused["barrier"]})),
                    )
                    .await
                    .unwrap();
                assert_eq!(reused_barrier["complete"], true);
                let (report, witness_after) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(report.ok);
                assert_eq!(witness_after.len(), witness_before.len() + 1);
                assert_eq!(witness_after.last().unwrap().verdict, Verdict::Reused);
                assert_eq!(
                    witness_after.last().unwrap().labor_class,
                    LaborClass::Reused
                );
                assert_eq!(context.read().await.jobs.len(), 2);
                let standup = client.call("query.standup", Some(json!({}))).await.unwrap();
                assert_eq!(standup["reused"], 1);
                let missing = client
                    .call(
                        "queue.await_job",
                        Some(json!({"task_uuid": Uuid::new_v4().to_string()})),
                    )
                    .await
                    .unwrap_err();
                assert!(matches!(
                    missing,
                    crate::wire::WireIoError::Rpc(WireErrorCode::NotFound, _, _)
                ));

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
                drop(client);
                drop(context);
                let reopened =
                    Daemon::open_with_executor(one_pool_config(), paths, settings(), executor)
                        .await
                        .unwrap();
                assert_eq!(reopened.handler.context.read().await.epoch, epoch + 1);
                assert!(reopened.initial_jobs.is_empty());
                let (reopened_shutdown, reopened_shutdown_rx) = watch::channel(false);
                let reopened_socket = reopened.handler.context.read().await.paths.socket.clone();
                let reopened_task =
                    tokio::task::spawn_local(reopened.run_until(reopened_shutdown_rx));
                let reopened_client = RpcClient::connect(&reopened_socket).await.unwrap();
                let late_reused = reopened_client
                    .call("queue.await_job", Some(json!({"task_uuid": reused_uuid})))
                    .await
                    .unwrap();
                assert_eq!(late_reused["verdict"], "reused");
                reopened_shutdown.send(true).unwrap();
                reopened_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn multi_pool_admission_is_atomic_and_any_conflict_or_gate_blocks() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    two_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();

                let held = daemon
                    .handler
                    .acquire(Some(json!({"pool": "slot"})))
                    .await
                    .unwrap();
                let held_lease = held["outcome"]["granted"]["leaseId"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["true"],
                        "pool": ["zeta", "slot"],
                        "priority": "low",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                assert_eq!(admitted["state"], "queued");
                let task_uuid = Uuid::parse_str(admitted["task_uuid"].as_str().unwrap()).unwrap();

                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.jobs[&task_uuid].row.pools, ["slot", "zeta"]);
                    assert_eq!(context.lease.engine().held_in_pool("slot").unwrap(), 1);
                    assert_eq!(context.lease.engine().held_in_pool("zeta").unwrap(), 0);
                    assert_eq!(context.lease.engine().queued_in_pool("slot").unwrap(), 1);
                    assert_eq!(context.lease.engine().queued_in_pool("zeta").unwrap(), 1);
                }
                let events = read_acknowledged_events(&paths.events_dir()).unwrap();
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].row.pools, ["slot", "zeta"]);
                let status = daemon
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                let projected = status["jobs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["taskUuid"] == task_uuid.to_string())
                    .unwrap();
                assert_eq!(projected["pool"], json!(["slot", "zeta"]));

                let paused = daemon
                    .handler
                    .pause(Some(json!({"pool": "zeta"})))
                    .await
                    .unwrap();
                assert_eq!(paused["affected"], 1);
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.jobs[&task_uuid].state, JobState::Paused);
                    assert_eq!(context.lease.engine().queue_len(), 0);
                }

                daemon
                    .handler
                    .resume(Some(json!({"pool": "zeta"})))
                    .await
                    .unwrap();
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.jobs[&task_uuid].state, JobState::Queued);
                    assert_eq!(context.lease.engine().queued_in_pool("slot").unwrap(), 1);
                    assert_eq!(context.lease.engine().queued_in_pool("zeta").unwrap(), 1);
                }

                assert_eq!(daemon.handler.apply_pool_loss("zeta").await.unwrap(), 0);
                assert_eq!(daemon.handler.apply_pool_loss("slot").await.unwrap(), 0);
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.jobs[&task_uuid].state, JobState::Paused);
                    assert!(context.unreachable_paused_jobs.contains(&task_uuid));
                    assert_eq!(context.lease.engine().queue_len(), 0);
                }
                daemon.handler.apply_pool_return("zeta").await.unwrap();
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.jobs[&task_uuid].state, JobState::Paused);
                    assert!(context.unreachable_paused_jobs.contains(&task_uuid));
                }
                daemon.handler.apply_pool_return("slot").await.unwrap();
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.jobs[&task_uuid].state, JobState::Queued);
                    assert!(!context.unreachable_paused_jobs.contains(&task_uuid));
                    assert_eq!(context.lease.engine().queued_in_pool("slot").unwrap(), 1);
                    assert_eq!(context.lease.engine().queued_in_pool("zeta").unwrap(), 1);
                }
                let released = daemon
                    .handler
                    .release(Some(json!({"lease": held_lease})))
                    .await
                    .unwrap();
                assert_eq!(released["promoted"].as_array().unwrap().len(), 1);
                assert_eq!(released["promoted"][0]["pools"], json!(["slot", "zeta"]));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pause_withdraws_pending_lease_and_queued_cancel_is_durable() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let held = daemon
                    .handler
                    .acquire(Some(json!({"pool": "slot"})))
                    .await
                    .unwrap();
                let held_lease = held["outcome"]["granted"]["leaseId"]
                    .as_str()
                    .expect("granted lease id")
                    .to_owned();
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "priority": "low",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                assert_eq!(admitted["state"], "queued");
                let task_uuid = admitted["task_uuid"].as_str().unwrap();
                let queued_status = daemon
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                assert!(queued_status["jobs"].as_array().unwrap().iter().any(|job| {
                    job["taskUuid"].as_str() == Some(task_uuid) && job["state"] == "queued"
                }));
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .lease
                        .engine()
                        .queue_len(),
                    1
                );
                let paused = daemon
                    .handler
                    .pause(Some(json!({"pool": "slot"})))
                    .await
                    .unwrap();
                assert_eq!(paused["affected"], 1);
                let paused_status = daemon
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                assert!(paused_status["jobs"].as_array().unwrap().iter().any(|job| {
                    job["taskUuid"].as_str() == Some(task_uuid) && job["state"] == "paused"
                }));
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .lease
                        .engine()
                        .queue_len(),
                    0
                );

                let cancelled = daemon
                    .handler
                    .cancel(Some(json!({"task_uuid": task_uuid, "force": false})))
                    .await
                    .unwrap();
                assert_eq!(cancelled["affected"], 1);
                assert_eq!(cancelled["was"], "paused");
                let waited = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(waited["verdict"], "cancelled");
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.last().unwrap().verdict, Verdict::Cancelled);

                let released = daemon
                    .handler
                    .release(Some(json!({"lease": held_lease})))
                    .await
                    .unwrap();
                assert!(released["promoted"].as_array().unwrap().is_empty());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn forced_cancel_response_implies_durable_cancelled_witness() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                // A concurrently forked child temporarily inherits this same open-file
                // description until exec applies CLOEXEC. Keep the equivalent duplicate
                // alive across shutdown so reopen cannot depend on last-close timing.
                let inherited_lock = daemon._state_lock.file().try_clone().unwrap();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let admitted = client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["sleep", "30"],
                            "pool": "slot",
                            "priority": "low",
                            "adapter": "shell",
                            "source": "manual",
                            "evidence": ["exit:0"]
                        })),
                    )
                    .await
                    .unwrap();
                assert_eq!(admitted["state"], "running");
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
                let untouched = client
                    .call(
                        "queue.cancel",
                        Some(json!({"task_uuid": task_uuid, "force": false})),
                    )
                    .await
                    .unwrap();
                assert_eq!(untouched["affected"], 0);
                assert_eq!(untouched["was"], "running");
                assert!(read_verified_records(&paths.witness_path())
                    .unwrap()
                    .1
                    .is_empty());
                let cancelled = client
                    .call(
                        "queue.cancel",
                        Some(json!({"task_uuid": task_uuid, "force": true})),
                    )
                    .await
                    .unwrap();
                assert_eq!(cancelled["affected"], 1);
                let (report, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(report.ok);
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].verdict, Verdict::Cancelled);
                assert_eq!(records[0].task_uuid.as_deref(), Some(task_uuid.as_str()));

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
                drop(client);

                let reopened = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                drop(inherited_lock);
                assert!(reopened.initial_jobs.is_empty());
                let (second_shutdown, second_shutdown_rx) = watch::channel(false);
                let second_task = tokio::task::spawn_local(reopened.run_until(second_shutdown_rx));
                let restarted = RpcClient::connect(&paths.socket).await.unwrap();
                let late = restarted
                    .call("queue.await_job", Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(late["verdict"], "cancelled");
                second_shutdown.send(true).unwrap();
                second_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn panicking_producer_restarts_without_stopping_its_peer() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let (events_tx, mut events_rx) = mpsc::unbounded_channel();
                let panics = Rc::new(Cell::new(0_u32));
                let panics_for_factory = panics.clone();
                let panic_factory: SupervisedFactory = Rc::new(move || {
                    let panics = panics_for_factory.clone();
                    Box::pin(async move {
                        panics.set(panics.get() + 1);
                        panic!("producer fault injection");
                    })
                });
                let peer_runs = Rc::new(Cell::new(0_u32));
                let peer_for_factory = peer_runs.clone();
                let peer_factory: SupervisedFactory = Rc::new(move || {
                    let peer_runs = peer_for_factory.clone();
                    Box::pin(async move {
                        peer_runs.set(peer_runs.get() + 1);
                        std::future::pending::<()>().await;
                        Ok(())
                    })
                });
                let first = spawn_supervised(
                    SupervisedTask {
                        name: "faulty".to_owned(),
                        restart_delay: Duration::from_millis(1),
                        factory: panic_factory,
                    },
                    shutdown_rx.clone(),
                    events_tx.clone(),
                );
                let second = spawn_supervised(
                    SupervisedTask {
                        name: "peer".to_owned(),
                        restart_delay: Duration::from_millis(1),
                        factory: peer_factory,
                    },
                    shutdown_rx,
                    events_tx,
                );
                tokio::time::timeout(Duration::from_secs(1), async {
                    loop {
                        let _ = events_rx.recv().await;
                        if panics.get() >= 2 && peer_runs.get() == 1 {
                            break;
                        }
                    }
                })
                .await
                .unwrap();
                shutdown_tx.send(true).unwrap();
                first.await.unwrap();
                second.await.unwrap();
                assert!(panics.get() >= 2);
                assert_eq!(peer_runs.get(), 1);
            })
            .await;
    }

    #[test]
    fn sd_notify_ready_watchdog_and_stopping_are_datagrams() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("notify.sock");
        let socket = UnixDatagram::bind(&path).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let notifier = SystemdNotifier::with_socket(path, Some(Duration::from_secs(2)));
        for (send, expected) in [
            (
                SystemdNotifier::ready as fn(&SystemdNotifier) -> _,
                "READY=1\nSTATUS=tally daemon ready",
            ),
            (SystemdNotifier::watchdog, "WATCHDOG=1"),
            (SystemdNotifier::stopping, "STOPPING=1"),
        ] {
            send(&notifier).unwrap();
            let mut buffer = [0_u8; 128];
            let read = socket.recv(&mut buffer).unwrap();
            assert_eq!(&buffer[..read], expected.as_bytes());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_24_1_restart_reconstructs_lineage_two_attempts_log_and_proof() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let (paths, parent_uuid, child_uuid, parent_pass, _) =
                    seed_durable_query_fixture(temp.path());
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);

                // Open and drop one daemon before inspecting through the next
                // generation. This exercises lifecycle reload independently of
                // both the TaskChampion cache and the witness ledger.
                let first = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                drop(first);
                let restarted = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();

                let jobs = restarted
                    .handler
                    .query("query.jobs", Some(json!({})))
                    .await
                    .unwrap();
                assert_eq!(jobs["protocolVersion"], 4);
                assert_eq!(jobs["nextCursor"], Value::Null);
                assert_eq!(jobs["snapshot"]["history"]["complete"], true);
                let parent = jobs["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["taskUuid"] == parent_uuid.to_string())
                    .unwrap();
                assert_eq!(parent["terminalVerdict"], "pass");
                assert_eq!(parent["terminalAttempt"], 2);
                assert_eq!(parent["currentAttempt"], 2);
                assert_eq!(parent["leaseEpoch"], 2);
                assert_eq!(parent["evidenceResult"], "pass");
                assert_eq!(parent["lifecycleEvent"], "completed");
                assert_eq!(parent["childTaskUuids"], json!([child_uuid.to_string()]));
                assert_ne!(parent["liveState"], parent["terminalVerdict"]);
                assert_ne!(parent["rowStatus"], parent["evidenceResult"]);

                let job = restarted
                    .handler
                    .query("query.job", Some(json!({"id": parent_uuid.to_string()})))
                    .await
                    .unwrap();
                let attempts = job["attempts"].as_array().unwrap();
                assert_eq!(attempts.len(), 2);
                assert_eq!(attempts[0]["attempt"], 1);
                assert_eq!(attempts[0]["leaseEpoch"], 1);
                assert_eq!(attempts[0]["witnessRecords"][0]["verdict"], "preempted");
                assert_eq!(attempts[1]["attempt"], 2);
                assert_eq!(attempts[1]["leaseEpoch"], 2);
                assert_eq!(attempts[1]["evidenceResult"], "pass");

                let log = restarted
                    .handler
                    .query("query.log", Some(json!({"task": parent_uuid.to_string()})))
                    .await
                    .unwrap();
                assert!(log["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| event["attempt"] == 1 && event["event"] == "preempted"));
                assert!(log["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| event["attempt"] == 2 && event["event"] == "completed"));

                let proof = restarted
                    .handler
                    .query(
                        "query.proof",
                        Some(json!({"task": parent_uuid.to_string(), "attempt": 2})),
                    )
                    .await
                    .unwrap();
                assert_eq!(proof["status"], "verified");
                assert_eq!(
                    proof["witnessRecord"],
                    serde_json::to_value(parent_pass).unwrap()
                );
                assert_eq!(proof["evidence"]["observations"][0]["passed"], true);
                assert_eq!(proof["advisoryAttestations"].as_array().unwrap().len(), 1);

                let status = restarted
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                assert!(status["jobs"].as_array().unwrap().iter().any(|job| {
                    job["taskUuid"] == parent_uuid.to_string() && job["verdict"] == "pass"
                }));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_24_6_proof_matches_verified_record_and_reports_chain_head() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let (paths, parent_uuid, _, expected, expected_head) =
                    seed_durable_query_fixture(temp.path());
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let proof = daemon
                    .handler
                    .query(
                        "query.proof",
                        Some(json!({"task": parent_uuid.to_string(), "attempt": 2})),
                    )
                    .await
                    .unwrap();
                let (_, disk_records) = read_verified_records(&paths.witness_path()).unwrap();
                let disk = disk_records
                    .iter()
                    .find(|record| {
                        record.task_uuid.as_deref() == Some(parent_uuid.to_string().as_str())
                            && record.attempt == 2
                    })
                    .unwrap();
                assert_eq!(disk, &expected);
                assert_eq!(
                    proof["witnessRecord"],
                    serde_json::to_value(disk).unwrap(),
                    "proof must preserve every verified WitnessRecord field"
                );
                assert_eq!(proof["ledger"]["verified"], true);
                assert_eq!(
                    proof["ledger"]["chainHead"],
                    json!({"seq": expected_head.seq, "hash": expected_head.hash})
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_conformance_submission_created_and_attached_materialize_once() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                daemon
                    .handler
                    .pause(Some(json!({"pool": "slot", "all": false})))
                    .await
                    .unwrap();
                let context = daemon.handler.context.clone();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let first_client = RpcClient::connect(&paths.socket).await.unwrap();
                let second_client = RpcClient::connect(&paths.socket).await.unwrap();
                let payload = fs1_full_payload("fs1-concurrent", &["true"], ["exit:0".to_owned()]);
                let mut metadata_variant = payload.clone();
                metadata_variant["priority"] = json!("low");
                metadata_variant["consumptionEstimate"] = json!(99);
                metadata_variant["parent"] = json!("00000000-0000-4000-8000-000000000044");
                metadata_variant["wait"] = json!(true);
                let (first, second) = tokio::join!(
                    first_client.call("queue.enqueue", Some(payload.clone())),
                    second_client.call("queue.enqueue", Some(metadata_variant))
                );
                let first = first.unwrap();
                let second = second.unwrap();
                let dispositions = [
                    first["disposition"].as_str(),
                    second["disposition"].as_str(),
                ];
                assert!(dispositions.contains(&Some("created")));
                assert!(dispositions.contains(&Some("attached")));
                assert_eq!(first["task_uuid"], second["task_uuid"]);
                assert_eq!(first["payloadHash"], second["payloadHash"]);
                assert_eq!(first["schemaVersion"], 1);
                assert_eq!(second["schemaVersion"], 1);
                assert_eq!(first["attempt"], 1);
                assert_eq!(second["attempt"], 1);
                assert_eq!(context.read().await.jobs.len(), 1);
                let events = read_acknowledged_events(&paths.events_dir()).unwrap();
                assert_eq!(events.len(), 1);
                assert_eq!(
                    events[0].row.payload_hash.as_deref(),
                    first["payloadHash"].as_str()
                );

                first_client
                    .call("queue.resume", Some(json!({"pool": "slot", "all": false})))
                    .await
                    .unwrap();
                let wait_params = json!({"task_uuid": first["task_uuid"]});
                let (first_wait, second_wait) = tokio::join!(
                    first_client.call("queue.await_job", Some(wait_params.clone())),
                    second_client.call("queue.await_job", Some(wait_params))
                );
                let first_wait = first_wait.unwrap();
                let second_wait = second_wait.unwrap();
                assert_eq!(first_wait["verdict"], "pass");
                assert_eq!(first_wait, second_wait);
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 1);
                assert_eq!(
                    records[0].payload_hash.as_deref(),
                    first["payloadHash"].as_str()
                );

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_conformance_submission_conflicts_fail_closed_for_every_live_shape() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let running = client
                    .call(
                        "queue.enqueue",
                        Some(fs1_full_payload(
                            "fs1-running-conflict",
                            &["sleep", "0.2"],
                            ["exit:0".to_owned()],
                        )),
                    )
                    .await
                    .unwrap();
                assert_eq!(running["state"], "running");
                let running_error = client
                    .call(
                        "queue.enqueue",
                        Some(fs1_full_payload(
                            "fs1-running-conflict",
                            &["true"],
                            ["exit:0".to_owned()],
                        )),
                    )
                    .await
                    .unwrap_err();
                let running_data = fs1_conflict(running_error);
                assert_eq!(running_data["existingTaskUuid"], running["task_uuid"]);
                assert_ne!(
                    running_data["payloadHash"],
                    running_data["existingPayloadHash"]
                );

                let queued = client
                    .call(
                        "queue.enqueue",
                        Some(fs1_full_payload(
                            "fs1-queued-conflict",
                            &["true"],
                            ["exit:0".to_owned()],
                        )),
                    )
                    .await
                    .unwrap();
                assert_eq!(queued["state"], "queued");
                let queued_error = client
                    .call(
                        "queue.enqueue",
                        Some(fs1_full_payload(
                            "fs1-queued-conflict",
                            &["false"],
                            ["exit:0".to_owned()],
                        )),
                    )
                    .await
                    .unwrap_err();
                let queued_data = fs1_conflict(queued_error);
                assert_eq!(queued_data["existingTaskUuid"], queued["task_uuid"]);

                client
                    .call("queue.pause", Some(json!({"pool": "slot", "all": false})))
                    .await
                    .unwrap();
                let legacy = json!({
                    "argv": ["true"],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "manual",
                    "dedupKey": "fs1-legacy-live-residue",
                    "evidence": ["exit:0"],
                });
                let legacy_one = client
                    .call("queue.enqueue", Some(legacy.clone()))
                    .await
                    .unwrap();
                let legacy_two = client
                    .call("queue.enqueue", Some(legacy.clone()))
                    .await
                    .unwrap();
                assert_eq!(legacy_one["disposition"], "created");
                assert_eq!(legacy_two["disposition"], "created");
                assert_ne!(legacy_one["task_uuid"], legacy_two["task_uuid"]);
                let residue_error = client
                    .call(
                        "queue.enqueue",
                        Some(fs1_full_payload(
                            "fs1-legacy-live-residue",
                            &["true"],
                            ["exit:0".to_owned()],
                        )),
                    )
                    .await
                    .unwrap_err();
                let residue_data = fs1_conflict(residue_error);
                assert_eq!(residue_data["existing"].as_array().unwrap().len(), 2);
                assert_eq!(
                    read_acknowledged_events(&paths.events_dir()).unwrap().len(),
                    4
                );

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_conformance_submission_terminal_pass_is_reused_without_side_effects() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                let context = daemon.handler.context.clone();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let mut payload =
                    fs1_full_payload("fs1-vacuous-reuse", &["true"], ["exit:0".to_owned()]);
                let manifest = temp.path().join("gates.json");
                fs::write(
                    &manifest,
                    r#"{"schemaVersion":1,"artifact":null,"gates":[{"id":"tests","status":"pass"}]}"#,
                )
                .unwrap();
                payload["gateManifest"] = json!({
                    "path": manifest,
                    "requiredGateIds": ["tests"],
                    "acceptancePolicy": "manual",
                });
                let created = client
                    .call("queue.enqueue", Some(payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(created["disposition"], "created");
                let terminal = fs1_wait(&client, &created).await;
                assert_eq!(terminal["verdict"], "pass");
                let (_, before) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(before.len(), 1);
                assert!(before[0].artifact_content_hash.is_none());

                let reused = client.call("queue.enqueue", Some(payload)).await.unwrap();
                assert_eq!(reused["disposition"], "reused");
                assert_eq!(reused["task_uuid"], created["task_uuid"]);
                assert_eq!(reused["verdict"], "pass");
                assert_eq!(reused["witnessSeq"], terminal["witness_seq"]);
                assert_eq!(reused["payloadHash"], created["payloadHash"]);
                assert_eq!(context.read().await.jobs.len(), 1);
                assert_eq!(
                    read_acknowledged_events(&paths.events_dir()).unwrap().len(),
                    1
                );
                let (_, after) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(after, before);

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_conformance_submission_artifact_drift_creates_with_disclosure() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let clean_path = temp.path().join("clean.txt");
                let drift_path = temp.path().join("drift.txt");
                let declared_path = temp.path().join("declared.txt");
                let unavailable_path = temp.path().join("unavailable.txt");
                for path in [&clean_path, &drift_path, &declared_path, &unavailable_path] {
                    fs::write(path, b"original\n").unwrap();
                }
                let declared_hash = hash_artifact_file(&declared_path).unwrap();

                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let clean_payload = fs1_full_payload(
                    "fs1-clean-artifact",
                    &["true"],
                    [
                        format!("artifact:{}", clean_path.display()),
                        "exit:0".to_owned(),
                    ],
                );
                let clean = client
                    .call("queue.enqueue", Some(clean_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(fs1_wait(&client, &clean).await["verdict"], "pass");
                let clean_reused = client
                    .call("queue.enqueue", Some(clean_payload))
                    .await
                    .unwrap();
                assert_eq!(clean_reused["disposition"], "reused");

                let drift_payload = fs1_full_payload(
                    "fs1-artifact-drift",
                    &["true"],
                    [
                        format!("artifact:{}", drift_path.display()),
                        "exit:0".to_owned(),
                    ],
                );
                let drift = client
                    .call("queue.enqueue", Some(drift_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(fs1_wait(&client, &drift).await["verdict"], "pass");
                fs::write(&drift_path, b"changed\n").unwrap();
                let drift_rerun = client
                    .call("queue.enqueue", Some(drift_payload))
                    .await
                    .unwrap();
                assert_eq!(drift_rerun["disposition"], "created");
                assert_eq!(drift_rerun["reusedRejected"], "artifact-drift");
                assert_eq!(fs1_wait(&client, &drift_rerun).await["verdict"], "pass");

                let declared_payload = fs1_full_payload(
                    "fs1-declared-mismatch",
                    &["true"],
                    [
                        format!("artifact:{}", declared_path.display()),
                        format!(
                            "hash:sha256:{}",
                            declared_hash.trim_start_matches("sha256:")
                        ),
                        "exit:0".to_owned(),
                    ],
                );
                let declared = client
                    .call("queue.enqueue", Some(declared_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(fs1_wait(&client, &declared).await["verdict"], "pass");
                fs::write(&declared_path, b"changed\n").unwrap();
                let declared_rerun = client
                    .call("queue.enqueue", Some(declared_payload))
                    .await
                    .unwrap();
                assert_eq!(declared_rerun["disposition"], "created");
                assert_eq!(declared_rerun["reusedRejected"], "declared-hash-mismatch");
                assert_eq!(
                    fs1_wait(&client, &declared_rerun).await["verdict"],
                    "clean-exit-no-artifact"
                );

                let unavailable_payload = fs1_full_payload(
                    "fs1-artifact-unavailable",
                    &["true"],
                    [
                        format!("artifact:{}", unavailable_path.display()),
                        "exit:0".to_owned(),
                    ],
                );
                let unavailable = client
                    .call("queue.enqueue", Some(unavailable_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(fs1_wait(&client, &unavailable).await["verdict"], "pass");
                fs::remove_file(&unavailable_path).unwrap();
                let unavailable_rerun = client
                    .call("queue.enqueue", Some(unavailable_payload))
                    .await
                    .unwrap();
                assert_eq!(unavailable_rerun["disposition"], "created");
                assert_eq!(unavailable_rerun["reusedRejected"], "artifact-unavailable");
                assert_eq!(
                    unavailable_rerun["errorDetail"],
                    unavailable_path.to_string_lossy().as_ref()
                );
                assert_eq!(
                    fs1_wait(&client, &unavailable_rerun).await["verdict"],
                    "clean-exit-no-artifact"
                );

                assert_eq!(
                    read_acknowledged_events(&paths.events_dir()).unwrap().len(),
                    7
                );
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 7);

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_conformance_submission_terminal_failure_is_memoized_and_conflicts() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let attached_client = RpcClient::connect(&paths.socket).await.unwrap();
                let stale_client = RpcClient::connect(&paths.socket).await.unwrap();
                let failed_payload =
                    fs1_full_payload("fs1-memoized-failure", &["false"], ["exit:0".to_owned()]);
                let created = client
                    .call("queue.enqueue", Some(failed_payload.clone()))
                    .await
                    .unwrap();
                let failed = fs1_wait(&client, &created).await;
                assert_eq!(failed["verdict"], "failed");
                let terminal = client
                    .call("queue.enqueue", Some(failed_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(terminal["disposition"], "terminal");
                assert_eq!(terminal["task_uuid"], created["task_uuid"]);
                assert_eq!(terminal["verdict"], "failed");
                assert_eq!(terminal["witnessSeq"], failed["witness_seq"]);
                assert_eq!(
                    read_acknowledged_events(&paths.events_dir()).unwrap().len(),
                    1
                );

                let terminal_conflict = client
                    .call(
                        "queue.enqueue",
                        Some(fs1_full_payload(
                            "fs1-memoized-failure",
                            &["true"],
                            ["exit:0".to_owned()],
                        )),
                    )
                    .await
                    .unwrap_err();
                let conflict_data = fs1_conflict(terminal_conflict);
                assert_eq!(conflict_data["existingTaskUuid"], created["task_uuid"]);

                client
                    .call("queue.pause", Some(json!({"pool": "slot", "all": false})))
                    .await
                    .unwrap();
                let retry = client
                    .call(
                        "queue.retry",
                        Some(json!({"task_uuid": created["task_uuid"]})),
                    )
                    .await
                    .unwrap();
                assert_eq!(retry["schemaVersion"], 1);
                assert_eq!(retry["retried"], true);
                assert_eq!(retry["task_uuid"], created["task_uuid"]);
                assert_eq!(retry["attempt"], 2);
                assert_eq!(retry["payloadHash"], created["payloadHash"]);
                assert!(retry.get("disposition").is_none());
                let (_, before_retry_terminal) =
                    read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(before_retry_terminal.len(), 1);
                let events = read_acknowledged_events(&paths.events_dir()).unwrap();
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].retries.len(), 1);
                assert_eq!(events[0].retries[0].attempt, 2);
                assert_eq!(
                    events[0].retries[0].previous_witness_seq,
                    failed["witness_seq"].as_u64().unwrap()
                );
                let attached = attached_client
                    .call("queue.enqueue", Some(failed_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(attached["disposition"], "attached");
                assert_eq!(attached["task_uuid"], created["task_uuid"]);
                assert_eq!(attached["attempt"], 2);
                let stale_wait = stale_client.call(
                    "queue.await_job",
                    Some(json!({
                        "task_uuid": created["task_uuid"],
                        "attempt": 1
                    })),
                );
                let resume =
                    client.call("queue.resume", Some(json!({"pool": "slot", "all": false})));
                let (stale_wait, resume) = tokio::join!(stale_wait, resume);
                resume.unwrap();
                let stale_wait = stale_wait.unwrap();
                assert_eq!(stale_wait["attempt"], 2);
                assert_ne!(stale_wait["witness_seq"], failed["witness_seq"]);
                let wait_params = json!({"task_uuid": created["task_uuid"]});
                let (retried_wait, attached_wait) = tokio::join!(
                    client.call("queue.await_job", Some(wait_params.clone())),
                    attached_client.call("queue.await_job", Some(wait_params))
                );
                let retried_wait = retried_wait.unwrap();
                assert_eq!(retried_wait, attached_wait.unwrap());
                assert_eq!(retried_wait["attempt"], 2);
                assert_eq!(retried_wait["verdict"], "failed");
                let latest_terminal = client
                    .call("queue.enqueue", Some(failed_payload))
                    .await
                    .unwrap();
                assert_eq!(latest_terminal["disposition"], "terminal");
                assert_eq!(latest_terminal["witnessSeq"], retried_wait["witness_seq"]);

                let passing_payload =
                    fs1_full_payload("fs1-pass-no-retry", &["true"], ["exit:0".to_owned()]);
                let passing = client
                    .call("queue.enqueue", Some(passing_payload))
                    .await
                    .unwrap();
                assert_eq!(fs1_wait(&client, &passing).await["verdict"], "pass");
                let pass_retry = client
                    .call(
                        "queue.retry",
                        Some(json!({"task_uuid": passing["task_uuid"]})),
                    )
                    .await
                    .unwrap_err();
                assert!(matches!(
                    pass_retry,
                    WireIoError::Rpc(WireErrorCode::InvalidParams, _, _)
                ));
                let missing_retry = client
                    .call(
                        "queue.retry",
                        Some(json!({"task_uuid": Uuid::new_v4().to_string()})),
                    )
                    .await
                    .unwrap_err();
                assert!(matches!(
                    missing_retry,
                    WireIoError::Rpc(WireErrorCode::InvalidParams, _, _)
                ));

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs1_explicit_retry_survives_restart_on_the_same_row_and_next_attempt() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let payload =
                    fs1_full_payload("fs1-retry-restart", &["false"], ["exit:0".to_owned()]);
                let created = client
                    .call("queue.enqueue", Some(payload.clone()))
                    .await
                    .unwrap();
                let task_uuid = created["task_uuid"].as_str().unwrap().to_owned();
                assert_eq!(fs1_wait(&client, &created).await["attempt"], 1);
                client
                    .call("queue.pause", Some(json!({"pool": "slot", "all": false})))
                    .await
                    .unwrap();
                let retry = client
                    .call("queue.retry", Some(json!({"task_uuid": task_uuid.clone()})))
                    .await
                    .unwrap();
                assert_eq!(retry["attempt"], 2);
                assert_eq!(retry["state"], "paused");
                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();

                let restarted = fs1_daemon(&paths).await;
                let (restart_shutdown_tx, restart_shutdown_rx) = watch::channel(false);
                let restarted_task =
                    tokio::task::spawn_local(restarted.run_until(restart_shutdown_rx));
                let restarted_client = RpcClient::connect(&paths.socket).await.unwrap();
                let terminal = restarted_client
                    .call(
                        "queue.await_job",
                        Some(json!({"task_uuid": task_uuid.clone()})),
                    )
                    .await
                    .unwrap();
                assert_eq!(terminal["task_uuid"], task_uuid);
                assert_eq!(terminal["attempt"], 2);
                assert_eq!(terminal["verdict"], "failed");
                let latest = restarted_client
                    .call("queue.enqueue", Some(payload))
                    .await
                    .unwrap();
                assert_eq!(latest["disposition"], "terminal");
                assert_eq!(latest["attempt"], 2);
                assert_eq!(latest["witnessSeq"], terminal["witness_seq"]);

                let events = read_acknowledged_events(&paths.events_dir()).unwrap();
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].retries.len(), 1);
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(
                    records
                        .iter()
                        .map(|record| record.attempt)
                        .collect::<Vec<_>>(),
                    [1, 2]
                );
                assert_eq!(records[0].task_uuid, records[1].task_uuid);
                assert_eq!(records[0].payload_hash, records[1].payload_hash);

                restart_shutdown_tx.send(true).unwrap();
                restarted_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_conformance_submission_legacy_behavior_remains_byte_and_behavior_compatible() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let artifact = temp.path().join("legacy-artifact.txt");
                fs::write(&artifact, b"legacy\n").unwrap();
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let legacy = json!({
                    "argv": ["true"],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "manual",
                    "dedupKey": "fs1-legacy-reuse",
                    "evidence": [
                        format!("artifact:{}", artifact.display()),
                        "exit:0"
                    ],
                });
                let first = client
                    .call("queue.enqueue", Some(legacy.clone()))
                    .await
                    .unwrap();
                assert_eq!(first["schemaVersion"], 1);
                assert_eq!(first["disposition"], "created");
                assert!(first.get("payloadHash").is_none());
                assert_eq!(fs1_wait(&client, &first).await["verdict"], "pass");
                let reused = client
                    .call("queue.enqueue", Some(legacy.clone()))
                    .await
                    .unwrap();
                assert_eq!(reused["disposition"], "reused");
                assert_eq!(reused["verdict"], "reused");
                assert_ne!(reused["task_uuid"], first["task_uuid"]);

                let manifest = temp.path().join("legacy-gates.json");
                fs::write(
                    &manifest,
                    r#"{"schemaVersion":1,"artifact":null,"gates":[{"id":"tests","status":"pass"}]}"#,
                )
                .unwrap();
                let manifest_payload = json!({
                    "argv": ["true"],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "manual",
                    "dedupKey": "fs1-legacy-manifest",
                    "evidence": [
                        format!("artifact:{}", artifact.display()),
                        "exit:0"
                    ],
                    "gateManifest": {
                        "path": manifest,
                        "requiredGateIds": ["tests"],
                        "acceptancePolicy": "manual"
                    }
                });
                let manifest_first = client
                    .call("queue.enqueue", Some(manifest_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(
                    fs1_wait(&client, &manifest_first).await["verdict"],
                    "pass"
                );
                let manifest_second = client
                    .call("queue.enqueue", Some(manifest_payload))
                    .await
                    .unwrap();
                assert_eq!(manifest_second["disposition"], "created");
                assert_ne!(manifest_second["task_uuid"], manifest_first["task_uuid"]);
                assert_eq!(
                    fs1_wait(&client, &manifest_second).await["verdict"],
                    "pass"
                );

                let failed_legacy = json!({
                    "argv": ["false"],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "manual",
                    "dedupKey": "fs1-legacy-failure",
                    "evidence": ["exit:0"],
                });
                let failed_first = client
                    .call("queue.enqueue", Some(failed_legacy.clone()))
                    .await
                    .unwrap();
                assert_eq!(
                    fs1_wait(&client, &failed_first).await["verdict"],
                    "failed"
                );
                let failed_second = client
                    .call("queue.enqueue", Some(failed_legacy))
                    .await
                    .unwrap();
                assert_eq!(failed_second["disposition"], "created");
                assert_ne!(failed_second["task_uuid"], failed_first["task_uuid"]);
                assert_eq!(
                    fs1_wait(&client, &failed_second).await["verdict"],
                    "failed"
                );

                let mut unrecorded_legacy = json!({
                    "argv": ["true"],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "manual",
                    "dedupKey": "fs1-unrecorded-terminal",
                    "evidence": ["exit:0"],
                });
                let unrecorded = client
                    .call("queue.enqueue", Some(unrecorded_legacy.clone()))
                    .await
                    .unwrap();
                assert_eq!(fs1_wait(&client, &unrecorded).await["verdict"], "pass");
                unrecorded_legacy["submission"] = json!({"mode": "full"});
                let full_after_legacy = client
                    .call("queue.enqueue", Some(unrecorded_legacy))
                    .await
                    .unwrap();
                assert_eq!(full_after_legacy["disposition"], "created");
                assert_eq!(
                    full_after_legacy["reusedRejected"],
                    "payload-hash-unrecorded"
                );
                assert_ne!(full_after_legacy["task_uuid"], unrecorded["task_uuid"]);
                assert_eq!(
                    fs1_wait(&client, &full_after_legacy).await["verdict"],
                    "pass"
                );

                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records[0].payload_hash, None);
                assert_eq!(records[1].verdict, Verdict::Reused);
                assert_eq!(records[1].payload_hash, None);

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs2_large_brief_and_provenance_round_trip_group_and_enforce_max_nodes() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let brief_source = temp.path().join("brief.json");
                let brief_document = json!({
                    "mission": "exercise the structured brief path",
                    "acceptance": ["brief is durable", "provenance is witnessed"],
                    "payload": "x".repeat(70 * 1024),
                });
                fs::write(
                    &brief_source,
                    serde_json::to_vec_pretty(&brief_document).unwrap(),
                )
                .unwrap();
                assert!(fs::metadata(&brief_source).unwrap().len() > 64 * 1024);

                let flow_run_id = Uuid::new_v4().to_string();
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let mut payload = fs1_full_payload(
                    "flow:brief-round-trip:0",
                    &[
                        "sh",
                        "-c",
                        "test -n \"$TALLY_BRIEF\" && test -f \"$TALLY_BRIEF\"",
                    ],
                    ["exit:0".to_owned()],
                );
                payload["source"] = json!("orchestrator");
                payload["briefPath"] = json!(brief_source);
                payload["orchestration"] = json!({
                    "flowName": "brief-round-trip",
                    "flowRunId": flow_run_id,
                    "scriptHash": "sha256-script-generation",
                    "nodeOrdinal": 0,
                    "nodeLabel": "verify-brief",
                    "maxNodes": 1,
                    "selection": {
                        "selector": "pooled-fast",
                        "members": ["worker-a", "worker-b"]
                    }
                });
                let created = client
                    .call("queue.enqueue", Some(payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(created["disposition"], "created");
                let task_uuid = Uuid::parse_str(created["task_uuid"].as_str().unwrap()).unwrap();
                assert_eq!(task_uuid.get_version_num(), 7);
                let brief_hash = created["payloadHash"]
                    .as_str()
                    .expect("full submission returns payloadHash")
                    .to_owned();
                let terminal = fs1_wait(&client, &created).await;
                assert_eq!(terminal["verdict"], "pass");

                let events = read_acknowledged_events(&paths.events_dir()).unwrap();
                assert_eq!(events.len(), 1);
                let durable_brief_hash = events[0].row.brief_hash.clone().unwrap();
                assert_ne!(durable_brief_hash, brief_hash);
                assert_eq!(
                    events[0].row.orchestration.as_ref().unwrap().as_value()["selection"]
                        ["members"],
                    json!(["worker-a", "worker-b"])
                );
                let stored_path =
                    brief::content_path(&paths.data_dir, &durable_brief_hash).unwrap();
                let stored = fs::read(&stored_path).unwrap();
                assert!(stored.len() > 64 * 1024);
                assert_eq!(
                    stored,
                    serde_json::to_vec(&brief_document).unwrap(),
                    "the daemon stores parsed canonical JSON, not source formatting"
                );
                assert_eq!(
                    fs::metadata(&stored_path).unwrap().permissions().mode() & 0o777,
                    0o600
                );

                let (_, witness) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(witness.len(), 1);
                assert_eq!(
                    witness[0].brief_hash.as_deref(),
                    Some(durable_brief_hash.as_str())
                );
                assert_eq!(
                    witness[0].orchestration.as_ref().unwrap().as_value()["nodeLabel"],
                    "verify-brief"
                );

                let grouped = client
                    .call("query.jobs", Some(json!({"flowRun": flow_run_id.clone()})))
                    .await
                    .unwrap();
                assert_eq!(grouped["items"].as_array().unwrap().len(), 1);
                assert_eq!(
                    grouped["items"][0]["briefHash"],
                    Value::String(durable_brief_hash.clone())
                );
                assert_eq!(
                    grouped["items"][0]["orchestration"]["flowRunId"],
                    flow_run_id
                );
                assert_eq!(
                    grouped["items"][0]["argv"],
                    json!([
                        "sh",
                        "-c",
                        "test -n \"$TALLY_BRIEF\" && test -f \"$TALLY_BRIEF\""
                    ])
                );
                let unrelated = client
                    .call(
                        "query.jobs",
                        Some(json!({"flowRun": Uuid::new_v4().to_string()})),
                    )
                    .await
                    .unwrap();
                assert!(unrelated["items"].as_array().unwrap().is_empty());

                let mut replay = payload.clone();
                replay["orchestration"]["maxNodes"] = json!(999);
                replay["orchestration"]["selection"]["members"] = json!(["worker-z"]);
                let reused = client.call("queue.enqueue", Some(replay)).await.unwrap();
                assert_eq!(reused["disposition"], "reused");
                assert_eq!(reused["task_uuid"], created["task_uuid"]);
                assert_eq!(reused["payloadHash"], created["payloadHash"]);

                let mut changed_key = payload.clone();
                changed_key["dedupKey"] = json!("flow:brief-round-trip:changed-key");
                changed_key["argv"] = json!(["false"]);
                let changed_key_error = client
                    .call("queue.enqueue", Some(changed_key))
                    .await
                    .unwrap_err();
                match changed_key_error {
                    WireIoError::Rpc(WireErrorCode::DedupKeyConflict, _, Some(data)) => {
                        assert_eq!(data["existingTaskUuid"], created["task_uuid"]);
                        assert_eq!(
                            data["existingOrchestration"]["nodeOrdinal"],
                            payload["orchestration"]["nodeOrdinal"]
                        );
                    }
                    other => panic!("expected same-ordinal dedup conflict, got {other:?}"),
                }

                let mut overflow = payload;
                overflow["dedupKey"] = json!("flow:brief-round-trip:1");
                overflow["orchestration"]["nodeOrdinal"] = json!(1);
                let overflow_error = client
                    .call("queue.enqueue", Some(overflow))
                    .await
                    .unwrap_err();
                match overflow_error {
                    WireIoError::Rpc(WireErrorCode::FlowNodeCap, _, Some(data)) => {
                        assert_eq!(data["flowRunId"], flow_run_id);
                        assert_eq!(data["maxNodes"], 1);
                        assert_eq!(data["existingNodes"], 1);
                    }
                    other => panic!("expected flow-node-cap, got {other:?}"),
                }

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();

                let restarted = fs1_daemon(&paths).await;
                let status = restarted
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                let projection = status["jobs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["taskUuid"] == created["task_uuid"])
                    .unwrap();
                assert_eq!(projection["briefHash"], durable_brief_hash);
                assert_eq!(
                    projection["orchestration"]["scriptHash"],
                    "sha256-script-generation"
                );
                drop(restarted);
                tokio::task::yield_now().await;

                fs::write(&stored_path, b"{}").unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let error = match Daemon::open_with_executor(
                    one_pool_config(),
                    paths,
                    settings(),
                    executor,
                )
                .await
                {
                    Ok(_) => panic!("tampered durable brief unexpectedly survived restart"),
                    Err(error) => error,
                };
                assert!(error.to_string().contains("durable brief"));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs2_outstanding_converges_across_terminal_rollback_and_restart() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let mut config = one_pool_config();
                config.enqueue.fanout_cap = 1;
                config.pools.get_mut("slot").unwrap().credentials.insert(
                    "token".to_owned(),
                    PathBuf::from("/run/credentials/slot-token"),
                );
                config.pools.insert(
                    "flow".to_owned(),
                    PoolConfig {
                        resource: ResourceKind::BuildSlot,
                        predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                        ..PoolConfig::default()
                    },
                );
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon =
                    Daemon::open_with_executor(config.clone(), paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();

                let parent = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["true"],
                        "pool": "flow",
                        "adapter": "shell",
                        "source": "manual",
                        "dedupKey": "fs2-parent",
                        "submission": {"mode": "full"},
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let parent_uuid = parent["task_uuid"].as_str().unwrap().to_owned();
                let child_payload = |key: &str| {
                    json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "orchestrator",
                        "dedupKey": key,
                        "submission": {"mode": "full"},
                        "callerJobId": parent_uuid,
                        "credentials": {"token": "/run/credentials/slot-token"},
                        "evidence": ["exit:0"]
                    })
                };
                let first = daemon
                    .handler
                    .enqueue(Some(child_payload("fs2-child-1")))
                    .await
                    .unwrap();
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(
                        context.guardrails.parent(&parent_uuid).unwrap().outstanding,
                        1
                    );
                }
                let capped = daemon
                    .handler
                    .enqueue(Some(child_payload("fs2-child-at-cap")))
                    .await
                    .unwrap_err();
                assert_eq!(capped.code, WireErrorCode::InvalidParams);
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .guardrails
                        .parent(&parent_uuid)
                        .unwrap()
                        .outstanding,
                    1
                );

                let first_uuid = Uuid::parse_str(first["task_uuid"].as_str().unwrap()).unwrap();
                {
                    let mut context = daemon.handler.context.write().await;
                    finalize_forced_locked(
                        &mut context,
                        first_uuid,
                        Verdict::Cancelled,
                        false,
                        false,
                    )
                    .unwrap();
                    assert_eq!(
                        context.guardrails.parent(&parent_uuid).unwrap().outstanding,
                        0
                    );
                }

                let rollback = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "orchestrator",
                        "dedupKey": "fs2-rollback",
                        "callerJobId": parent_uuid,
                        "credentials": {"token": "/run/credentials/different-token"},
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap_err();
                assert_eq!(rollback.code, WireErrorCode::InvalidParams);
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .guardrails
                        .parent(&parent_uuid)
                        .unwrap()
                        .outstanding,
                    0
                );

                let second = daemon
                    .handler
                    .enqueue(Some(child_payload("fs2-child-2")))
                    .await
                    .unwrap();
                assert_eq!(second["disposition"], "created");
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .guardrails
                        .parent(&parent_uuid)
                        .unwrap()
                        .outstanding,
                    1
                );
                drop(daemon);
                tokio::task::yield_now().await;

                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let restarted = Daemon::open_with_executor(config, paths, settings(), executor)
                    .await
                    .unwrap();
                let second_uuid = Uuid::parse_str(second["task_uuid"].as_str().unwrap()).unwrap();
                let mut context = restarted.handler.context.write().await;
                assert_eq!(
                    context.guardrails.parent(&parent_uuid).unwrap().outstanding,
                    1
                );
                finalize_forced_locked(&mut context, second_uuid, Verdict::Cancelled, false, false)
                    .unwrap();
                assert_eq!(
                    context.guardrails.parent(&parent_uuid).unwrap().outstanding,
                    0
                );
            })
            .await;
    }

    #[test]
    fn daemon_paths_create_no_docs_or_deferred_scope() {
        let temp = tempdir().unwrap();
        let paths = DaemonPaths {
            socket: temp.path().join("run/tally.sock"),
            state_dir: temp.path().join("state"),
            data_dir: temp.path().join("data"),
        };
        prepare_paths(&paths).unwrap();
        assert!(paths.state_dir.is_dir());
        assert!(paths.data_dir.is_dir());
        assert!(paths.socket.parent().unwrap().is_dir());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 3);
    }
}
