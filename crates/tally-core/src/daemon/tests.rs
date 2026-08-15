#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use tempfile::tempdir;
    use tokio::net::UnixStream;

    use super::*;
    use crate::adapters::{
        AdapterConfig, AdapterLaunchConfig, AdapterTrace, AdapterValueOverride, ScrapeCapture,
        ScrapeMode, ScrapeStream, TraceFraming,
    };
    use crate::config::{
        CoResidencyPredicate, ExecutionTargetConfig, JournaldConfig, MeterBudgetClass, PoolConfig,
        PoolPredicate, ResourceKind, SshExecutorConfig, UsageMeterConfig,
    };
    use crate::evidence::{hash_artifact_file, RetryPolicy};
    use crate::wire::RequestId;
    use crate::executor::{
        read_exit_record, ExecutionBackend, ExecutionPaths, LocalUnitFact,
        LocalUnitProbe, LocalUnitState, RemoteCapture, RemoteCompletion, RemoteExecutorReply,
        RemoteExecutorRequest, RemoteExecutorResult, RemoteTransport, RemoteTransportError,
        UnitExitRecord, REMOTE_EXECUTOR_PROTOCOL_VERSION, UNIT_EXIT_SCHEMA_VERSION,
    };
    use crate::recovery::{RecoveryPlan, RecoveryRow};
    use crate::witness::{
        append_attestation, canonical_gpu_seconds, counts_toward_canonical_gpu_seconds,
        verify_attestations,
    };
    use tally_client::RpcClient;

    /// Positive test progress is never a latency assertion (#419). The suite
    /// can be sharing a loaded host with other full suites, so every event,
    /// count, and state-publication wait gets the same generous ceiling. The
    /// deadline only distinguishes a wedged test from a slow one.
    const POSITIVE_PROGRESS_BACKSTOP: Duration = Duration::from_secs(60);

    async fn await_positive_progress<T>(
        description: &str,
        progress: impl std::future::Future<Output = T>,
    ) -> T {
        tokio::time::timeout(POSITIVE_PROGRESS_BACKSTOP, progress)
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "{description} made no progress within the test liveness backstop of {:?}",
                    POSITIVE_PROGRESS_BACKSTOP
                )
            })
    }

    /// Short waits are retained only where *absence* during the window is the
    /// semantic fact under test. Callers continue using the future afterwards,
    /// so a late positive result is not discarded or retried.
    async fn assert_no_progress_within<T>(
        description: &str,
        window: Duration,
        progress: impl std::future::Future<Output = T>,
    ) {
        assert!(
            tokio::time::timeout(window, progress).await.is_err(),
            "{description} unexpectedly made progress within {window:?}"
        );
    }

    fn direct_executor(state_dir: &Path) -> Executor {
        Executor::new(state_dir, std::env::current_exe().unwrap()).with_direct_fallback()
    }

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
                    resource: Some(ResourceKind::BuildSlot),
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
                resource: Some(ResourceKind::BuildSlot),
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
                    resource: Some(ResourceKind::Budget),
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
        published: mpsc::UnboundedSender<usize>,
        release: watch::Receiver<bool>,
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
            let published = self.published.clone();
            let mut release = self.release.clone();
            Box::pin(async move {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                published
                    .send(call + 1)
                    .expect("remote invocation observer must remain open");
                if call == 0 {
                    return Err(RemoteTransportError {
                        detail: "simulated SSH interruption after launch".to_owned(),
                    });
                }
                while !*release.borrow_and_update() {
                    release
                        .changed()
                        .await
                        .expect("remote completion release must remain open");
                }
                let RemoteExecutorRequest::Ensure {
                    request, evidence, ..
                } = request
                else {
                    return Err(RemoteTransportError {
                        detail: "unexpected remote operation".to_owned(),
                    });
                };
                let unit = request.identity.unit_name();
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
                                accounting: None,
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
                        let unit = identity.unit_name();
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
                        let unit = request.identity.unit_name();
                        RemoteExecutorResult::Completion(Box::new(RemoteCompletion {
                            unit: unit.clone(),
                            record: UnitExitRecord {
                                accounting: None,
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
            resume_requires_launch_cwd: false,
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
                        counter_scope: None,
                        fields: Default::default(),
                    },
                ),
                (
                    "model".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..model".to_owned(),
                        counter_scope: None,
                        fields: Default::default(),
                    },
                ),
                (
                    "sessionRef".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..session_id".to_owned(),
                        counter_scope: None,
                        fields: Default::default(),
                    },
                ),
                (
                    "usage".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..usage".to_owned(),
                        counter_scope: None,
                        fields: Default::default(),
                    },
                ),
                (
                    "finalMessage".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..final_message".to_owned(),
                        counter_scope: None,
                        fields: Default::default(),
                    },
                ),
            ]),
            usage_counter_scope: crate::adapters::UsageCounterScope::Attempt,
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
                cpu_weight: Some(100),
                memory_max_bytes: Some(64 * 1024 * 1024),
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
        let executor = direct_executor(&paths.state_dir)
            .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
            .with_unit_probe(ExitFileProbe);
        Daemon::open_with_executor(one_pool_config(), paths.clone(), settings(), executor)
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn option_like_workload_head_is_a_typed_pre_launch_refusal_without_a_job() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let mut config = one_pool_config();
                config.adapters.insert(
                    "probe".to_owned(),
                    AdapterConfig {
                        argv: vec!["provider".to_owned()],
                        launch: AdapterLaunchConfig {
                            reject_option_like_workload_head: true,
                            ..AdapterLaunchConfig::default()
                        },
                        ..AdapterConfig::default()
                    },
                );
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon =
                    Daemon::open_with_executor(config, paths, settings(), executor)
                        .await
                        .unwrap();

                for argument in ["--version", "-p"] {
                    let refused = daemon
                        .handler
                        .enqueue_as_client(Some(json!({
                            "argv": [argument],
                            "pool": "slot",
                            "adapter": "probe",
                            "source": "manual",
                            "evidence": ["exit:0"],
                        })))
                        .await
                        .unwrap_err();
                    assert_eq!(refused.code, WireErrorCode::PreLaunchRefusal);
                    assert_eq!(
                        refused.message,
                        format!(
                            "adapter \"probe\" pre-launch refusal option-like-workload-head at index 0: {argument:?}"
                        )
                    );
                    assert_eq!(
                        refused.data,
                        Some(json!({
                            "schemaVersion": 1,
                            "phase": "pre-launch",
                            "reason": "option-like-workload-head",
                            "adapter": "probe",
                            "index": 0,
                            "argument": argument,
                        }))
                    );

                    let context = daemon.handler.context.read().await;
                    assert!(context.rows.is_empty(), "a refused render wrote a durable row");
                    assert!(context.jobs.is_empty(), "a refused render created an executable job");
                    assert!(
                        context.query_rows.is_empty(),
                        "a refused render created a query projection"
                    );
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hard_storage_budget_refuses_new_intake_but_keeps_existing_work_and_queries_legible() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let mut config = one_pool_config();
                let budget = crate::config::StorageBudgetConfig {
                    warning_bytes: 1024 * 1024,
                    hard_bytes: 2 * 1024 * 1024,
                    warning_free_bytes: 2,
                    minimum_free_bytes: 1,
                };
                config.storage.data_dir = budget.clone();
                config.storage.state_dir = budget;
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();
                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let admitted_id = Uuid::parse_str(admitted["job_id"].as_str().unwrap()).unwrap();

                fs::write(paths.data_dir.join("controlled-pressure"), vec![0_u8; 3 * 1024 * 1024])
                    .unwrap();
                let cached = daemon
                    .handler
                    .query("query.storage", Some(json!({})))
                    .await
                    .unwrap();
                assert_eq!(cached["dataDir"]["level"], "ok");
                assert_eq!(cached["intake"]["accepting"], true);
                daemon.handler.refresh_storage_now().await;
                let storage = daemon
                    .handler
                    .query("query.storage", Some(json!({})))
                    .await
                    .unwrap();
                assert_eq!(storage["dataDir"]["level"], "hard");
                assert_eq!(storage["intake"]["accepting"], false);
                assert_eq!(storage["schemaVersion"], 3);
                assert!(storage["taskchampion"].is_null());

                let rows_before = daemon.handler.context.read().await.rows.len();
                let refused = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap_err();
                assert_eq!(refused.code, WireErrorCode::StorageBudgetExceeded);
                assert!(refused.message.contains("already-admitted work continues"));
                assert_eq!(daemon.handler.context.read().await.rows.len(), rows_before);
                assert_eq!(
                    daemon.handler.context.read().await.jobs[&admitted_id].state,
                    JobState::Paused
                );
                let status = daemon
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                assert_eq!(status["storage"]["intake"]["accepting"], false);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn storage_sampling_uses_the_blocking_pool_without_stalling_the_local_runtime() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                daemon
                    .handler
                    .storage
                    .borrow_mut()
                    .monitor
                    .set_sample_delay(Duration::from_millis(200));

                let handler = daemon.handler.clone();
                let started = Instant::now();
                let refresh = tokio::task::spawn_local(async move {
                    handler.refresh_storage_now().await;
                });
                tokio::task::yield_now().await;
                assert!(
                    started.elapsed() < Duration::from_millis(100),
                    "blocking storage walk stalled the current-thread runtime"
                );
                refresh.await.unwrap();
            })
            .await;
    }

    /// Wait for the storage timer's next sample, returning its stamp.
    ///
    /// The deadline is a liveness backstop, not a measurement bound: it
    /// separates "the tick was suppressed" from "the tick was late", and only
    /// the first is a defect. A suppressed tick never samples at all, so a
    /// ceiling two orders of magnitude above the interval still fails loudly on
    /// it while no amount of host load reaches it.
    async fn await_next_storage_sample(handler: &DaemonHandler, previous: &str) -> String {
        await_positive_progress("storage-timer sample publication", async {
            loop {
                let sampled_at = handler.cached_storage().sampled_at;
                if sampled_at != previous {
                    return sampled_at;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn storage_timer_samples_once_per_configured_interval() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let mut config = one_pool_config();
                config.storage.poll_interval_sec = 1;
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon =
                    Daemon::open_with_executor(config, paths, settings(), executor)
                        .await
                        .unwrap();
                let handler = daemon.handler.clone();
                let initial = handler.cached_storage().sampled_at;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));

                // The claim is "one sample per configured interval", and its two
                // halves have opposite relationships with load (#419). The
                // lower bound -- no sample before the interval elapses -- is
                // load-safe, because load can only delay a timer, never advance
                // it. The upper bound is not: this used to sleep 1,150 ms and
                // assert the sample had landed, which gives a tick plus a
                // blocking filesystem walk 150 ms of slack on a host that may
                // be running a hundred other test threads. So the lower bound
                // is asserted and the upper bound is waited for.
                tokio::time::sleep(Duration::from_millis(500)).await;
                assert_eq!(
                    handler.cached_storage().sampled_at,
                    initial,
                    "a sample landed before the configured interval elapsed"
                );
                let first = await_next_storage_sample(&handler, &initial).await;
                let second = await_next_storage_sample(&handler, &first).await;
                assert_ne!(
                    second, first,
                    "second configured tick was incorrectly suppressed"
                );

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_sampler_panic_does_not_lose_the_monitor() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                daemon
                    .handler
                    .storage
                    .borrow_mut()
                    .monitor
                    .set_sample_panic_once();

                let failed = daemon.handler.refresh_storage_now().await;
                assert!(failed.monitor_error.is_some());
                assert!(!failed.intake.accepting);

                let recovered = daemon.handler.refresh_storage_now().await;
                assert!(recovered.monitor_error.is_none());
                assert!(recovered.intake.accepting);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn enqueue_keeps_tree_metrics_cached_but_checks_free_space_live() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let mut config = one_pool_config();
                let budget = crate::config::StorageBudgetConfig {
                    warning_bytes: u64::MAX - 1,
                    hard_bytes: u64::MAX,
                    warning_free_bytes: 100,
                    minimum_free_bytes: 50,
                };
                config.storage.data_dir = budget.clone();
                config.storage.state_dir = budget;
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();
                let before = daemon.handler.cached_storage();
                daemon
                    .handler
                    .storage
                    .borrow_mut()
                    .monitor
                    .set_free_space_override(49, 1_000);

                let rows_before = daemon.handler.context.read().await.rows.len();
                let refused = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap_err();

                assert_eq!(refused.code, WireErrorCode::StorageBudgetExceeded);
                assert_eq!(daemon.handler.context.read().await.rows.len(), rows_before);
                let after = daemon.handler.cached_storage();
                assert_eq!(after.sampled_at, before.sampled_at);
                assert_eq!(after.data_dir.filesystem_available_bytes, Some(49));
                assert_eq!(after.data_dir.level, crate::storage::BudgetLevel::Hard);
                assert!(!after.intake.accepting);
                assert!(after.active_warnings.iter().any(|warning| {
                    warning.store == "dataDir"
                        && warning.pressures.iter().any(|pressure| {
                            pressure.resource
                                == crate::storage::StoragePressureResource::FilesystemAvailableBytes
                        })
                }));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unavailable_storage_monitor_has_a_distinct_intake_error() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                {
                    let mut storage = daemon.handler.storage.borrow_mut();
                    storage
                        .monitor
                        .record_sample_worker_failure("test monitor outage");
                    storage.snapshot = storage.monitor.query_snapshot();
                }
                let rows_before = daemon.handler.context.read().await.rows.len();
                let refused = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap_err();
                assert_eq!(refused.code, WireErrorCode::StorageMonitorUnavailable);
                assert!(refused.message.contains("storage monitor is unavailable"));
                assert_eq!(daemon.handler.context.read().await.rows.len(), rows_before);
            })
            .await;
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
                    .enqueue_as_client(Some(json!({
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

    /// Drive a request through the real dispatcher so that caller-class
    /// resolution runs. Calling `DaemonHandler::enqueue` directly would skip it.
    async fn rpc(
        handler: &DaemonHandler,
        method: &str,
        params: Value,
    ) -> Result<Value, WireError> {
        handler
            .handle(RequestFrame {
                id: RequestId::Number(1),
                method: method.to_owned(),
                params: Some(params),
            })
            .await
    }

    /// Register a capability token for an already-admitted job, standing in for
    /// the mint that `prepare_execution` performs when the job starts running.
    fn issue_job_token(handler: &DaemonHandler, job_id: Uuid, seed: &str) -> String {
        let token = seed.repeat(64 / seed.len());
        handler
            .job_tokens
            .borrow_mut()
            .insert(hash_job_token(&token), job_id);
        token
    }

    async fn admitted_parent(handler: &DaemonHandler, admitted: &Value) -> Option<String> {
        let job_id = Uuid::parse_str(admitted["job_id"].as_str().unwrap()).unwrap();
        handler.context.read().await.jobs[&job_id]
            .row
            .parent_uuid
            .map(|uuid| uuid.to_string())
    }

    fn child_request(token: &str, extra: Value) -> Value {
        let mut params = json!({
            "argv": ["true"],
            "pool": "slot",
            "adapter": "shell",
            "source": "orchestrator",
            "evidence": ["exit:0"],
            "callerJobToken": token,
        });
        let object = params.as_object_mut().unwrap();
        for (key, value) in extra.as_object().unwrap() {
            object.insert(key.clone(), value.clone());
        }
        params
    }

    /// Close the loop between the mint and the enforcement.
    ///
    /// The other capability tests register a synthetic token so they can focus
    /// on what admission does with it. This one takes the token that
    /// `prepare_execution` actually minted, hands it back exactly as the job's
    /// `TALLY_JOB_TOKEN` would carry it, and checks the daemon resolves the same
    /// job — the daemon → job → CLI → enqueue round trip, minus the two links
    /// that need a real unit (`executor_live` proves the unit receives the
    /// variable; `cli_rpc` proves the CLI forwards it as `callerJobToken`).
    #[tokio::test(flavor = "current_thread")]
    async fn a_minted_token_round_trips_back_to_the_job_that_owns_it() {
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
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let job_id = Uuid::parse_str(admitted["job_id"].as_str().unwrap()).unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();

                let mut job = {
                    let mut context = daemon.handler.context.write().await;
                    let stored = context.jobs.get_mut(&job_id).unwrap();
                    stored.state = JobState::Running;
                    stored.clone()
                };
                let minted = daemon
                    .handler
                    .prepare_execution(&mut job)
                    .await
                    .unwrap()
                    .unwrap()
                    .job_token
                    .unwrap();

                // The token as the job would read it out of its own environment.
                let request = execution_request(
                    &daemon.handler.executor,
                    &job,
                    settings().unit_limits,
                    ("/run/tally/tally.sock", Some(&minted)),
                    &paths.data_dir,
                    false,
                )
                .unwrap();
                let carried = request.job_token.clone().unwrap();
                assert_eq!(carried, minted);

                let child = rpc(
                    &daemon.handler,
                    "queue.enqueue",
                    child_request(&carried, json!({"dedupKey": "round-trip"})),
                )
                .await
                .unwrap();
                assert_eq!(
                    admitted_parent(&daemon.handler, &child).await.as_deref(),
                    Some(task_uuid.as_str())
                );

                // Revoking at terminal ends the round trip for that generation.
                daemon.handler.revoke_job_token(&job);
                let after_revoke = rpc(
                    &daemon.handler,
                    "queue.enqueue",
                    child_request(&carried, json!({"dedupKey": "round-trip-revoked"})),
                )
                .await
                .unwrap_err();
                assert_eq!(
                    after_revoke.message,
                    "callerJobToken is not a live job capability; it was never minted or has been revoked"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_token_fixes_caller_identity_and_rejects_sibling_impersonation() {
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

                let mine = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let sibling = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let my_job_id = Uuid::parse_str(mine["job_id"].as_str().unwrap()).unwrap();
                let my_task_uuid = mine["task_uuid"].as_str().unwrap().to_owned();
                let sibling_task_uuid = sibling["task_uuid"].as_str().unwrap().to_owned();
                let token = issue_job_token(&daemon.handler, my_job_id, "ab");

                // A token with no callerJobId is parented to the token's job, so
                // dropping the field cannot turn a child into a root submission.
                let implicit = rpc(
                    &daemon.handler,
                    "queue.enqueue",
                    child_request(&token, json!({"dedupKey": "token-implicit"})),
                )
                .await
                .unwrap();
                assert_eq!(
                    admitted_parent(&daemon.handler, &implicit).await.as_deref(),
                    Some(my_task_uuid.as_str())
                );

                // Naming your own identity alongside the token is still accepted.
                let explicit = rpc(
                    &daemon.handler,
                    "queue.enqueue",
                    child_request(
                        &token,
                        json!({"dedupKey": "token-explicit", "callerJobId": my_task_uuid}),
                    ),
                )
                .await
                .unwrap();
                assert_eq!(
                    admitted_parent(&daemon.handler, &explicit).await.as_deref(),
                    Some(my_task_uuid.as_str())
                );

                // Naming a sibling is the forgery the token exists to stop.
                let forged = rpc(
                    &daemon.handler,
                    "queue.enqueue",
                    child_request(
                        &token,
                        json!({"dedupKey": "token-forged", "callerJobId": sibling_task_uuid}),
                    ),
                )
                .await
                .unwrap_err();
                assert_eq!(forged.code, WireErrorCode::InvalidParams);
                assert_eq!(
                    forged.message,
                    "callerJobId is not accepted as authorization; identity derives from TALLY_JOB_TOKEN"
                );
                // The rejected attempt left no fan-out charge on the sibling.
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .guardrails
                        .parent(&sibling_task_uuid)
                        .map_or(0, |info| info.outstanding),
                    0
                );

                // A token that was never minted is a hard error, not a demotion
                // to operator class.
                let unknown = rpc(
                    &daemon.handler,
                    "queue.enqueue",
                    child_request(&"cd".repeat(32), json!({"dedupKey": "token-unknown"})),
                )
                .await
                .unwrap_err();
                assert_eq!(unknown.code, WireErrorCode::InvalidParams);
                assert_eq!(
                    unknown.message,
                    "callerJobToken is not a live job capability; it was never minted or has been revoked"
                );

                // So is a token revoked when its job reached a terminal state.
                daemon
                    .handler
                    .job_tokens
                    .borrow_mut()
                    .remove(&hash_job_token(&token));
                let revoked = rpc(
                    &daemon.handler,
                    "queue.enqueue",
                    child_request(&token, json!({"dedupKey": "token-revoked"})),
                )
                .await
                .unwrap_err();
                assert_eq!(
                    revoked.message,
                    "callerJobToken is not a live job capability; it was never minted or has been revoked"
                );

                // An operator presenting no token keeps the pre-token behaviour.
                let root = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"],
                        "dedupKey": "operator-root"
                    })))
                    .await
                    .unwrap();
                assert!(admitted_parent(&daemon.handler, &root).await.is_none());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_token_cannot_shed_no_enqueue_depth_or_fanout_by_dropping_caller_job_id() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let mut config = one_pool_config();
                config.enqueue.fanout_cap = 1;
                config.enqueue.depth_cap = 1;
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(config, paths, settings(), executor)
                    .await
                    .unwrap();
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();

                let sealed = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"],
                        "noEnqueue": true
                    })))
                    .await
                    .unwrap();
                let sealed_token = issue_job_token(
                    &daemon.handler,
                    Uuid::parse_str(sealed["job_id"].as_str().unwrap()).unwrap(),
                    "ab",
                );
                let denied = rpc(
                    &daemon.handler,
                    "queue.enqueue",
                    child_request(&sealed_token, json!({"dedupKey": "sealed-child"})),
                )
                .await
                .unwrap_err();
                assert!(
                    denied.message.contains("carries the noEnqueue capability"),
                    "unexpected message {}",
                    denied.message
                );

                let open = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let open_token = issue_job_token(
                    &daemon.handler,
                    Uuid::parse_str(open["job_id"].as_str().unwrap()).unwrap(),
                    "cd",
                );
                let child = rpc(
                    &daemon.handler,
                    "queue.enqueue",
                    child_request(&open_token, json!({"dedupKey": "fanout-first"})),
                )
                .await
                .unwrap();
                let capped = rpc(
                    &daemon.handler,
                    "queue.enqueue",
                    child_request(&open_token, json!({"dedupKey": "fanout-second"})),
                )
                .await
                .unwrap_err();
                assert!(
                    capped.message.contains("fanoutCap"),
                    "unexpected message {}",
                    capped.message
                );

                // depthCap is 1, so the admitted child may not enqueue at all.
                let grandchild_token = issue_job_token(
                    &daemon.handler,
                    Uuid::parse_str(child["job_id"].as_str().unwrap()).unwrap(),
                    "ef",
                );
                let too_deep = rpc(
                    &daemon.handler,
                    "queue.enqueue",
                    child_request(&grandchild_token, json!({"dedupKey": "too-deep"})),
                )
                .await
                .unwrap_err();
                assert!(
                    too_deep.message.contains("depthCap"),
                    "unexpected message {}",
                    too_deep.message
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_capability_is_denied_admin_and_producer_methods() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let token = issue_job_token(
                    &daemon.handler,
                    Uuid::parse_str(admitted["job_id"].as_str().unwrap()).unwrap(),
                    "ab",
                );

                for (method, params) in [
                    ("queue.pause", json!({"all": true, "callerJobToken": token})),
                    ("queue.resume", json!({"all": true, "callerJobToken": token})),
                    ("queue.drain", json!({"callerJobToken": token})),
                    (
                        "queue.cancel",
                        json!({"task_uuid": admitted["task_uuid"], "callerJobToken": token}),
                    ),
                    (
                        "queue.retry",
                        json!({"task_uuid": admitted["task_uuid"], "callerJobToken": token}),
                    ),
                    (
                        "__producer.runtime-observed",
                        json!({"callerJobToken": token}),
                    ),
                ] {
                    let error = rpc(&daemon.handler, method, params).await.unwrap_err();
                    assert_eq!(error.code, WireErrorCode::InvalidParams);
                    assert_eq!(
                        error.message,
                        format!("method {method} is not available to a job capability")
                    );
                }

                // A malformed token is a request error, not a silent demotion.
                let malformed = rpc(
                    &daemon.handler,
                    "query.status",
                    json!({"callerJobToken": 17}),
                )
                .await
                .unwrap_err();
                assert_eq!(malformed.message, "callerJobToken must be a string");
            })
            .await;
    }

    async fn wait_for_connection_count(
        counts: &mut mpsc::UnboundedReceiver<usize>,
        expected: usize,
    ) {
        await_positive_progress(
            "connection-count publication",
            async {
                loop {
                    let count = counts
                        .recv()
                        .await
                        .expect("connection count hook must remain open");
                    if count == expected {
                        break;
                    }
                }
            },
        )
        .await;
    }

    #[test]
    fn accounting_witness_fields_charges_cpu_seconds_regardless_of_pool() {
        let accounting = UnitAccounting {
            cpu_usage_nsec: Some(2_500_000_000),
            exec_main_start_monotonic_usec: Some(1_000_000),
            exec_main_exit_monotonic_usec: Some(4_500_000),
            ..UnitAccounting::default()
        };
        let (charge, gpu_seconds) = accounting_witness_fields(Some(accounting), false);
        assert_eq!(
            charge,
            Some(Charge {
                unit: "cpu-second".to_owned(),
                amount: 2.5,
                class_name: "measured".to_owned(),
            })
        );
        assert_eq!(gpu_seconds, None, "not a GPU-pool job");
    }

    #[test]
    fn accounting_witness_fields_fills_gpu_seconds_only_for_a_gpu_pool_job() {
        let accounting = UnitAccounting {
            cpu_usage_nsec: Some(2_500_000_000),
            exec_main_start_monotonic_usec: Some(1_000_000),
            exec_main_exit_monotonic_usec: Some(4_500_000),
            ..UnitAccounting::default()
        };
        let (charge, gpu_seconds) = accounting_witness_fields(Some(accounting), true);
        assert_eq!(charge.map(|charge| charge.amount), Some(2.5));
        assert_eq!(
            gpu_seconds,
            Some(3.5),
            "main-process wall-clock runtime, not CPU time"
        );
    }

    /// The durable failure fact for an OOM termination names the kill and the
    /// effective cap instead of whatever noise came last (vestige-sweep V-3).
    /// This is also the surface where the child-kill shape becomes a failure
    /// at all: the classification of an `exited` record with a nonzero
    /// `oom_kill` counter lands here as a named failure, not as the agent's
    /// own innocent exit status.
    #[test]
    fn oom_termination_failure_fact_names_the_kill_and_the_effective_cap() {
        let fact = execution_fact_for_termination(&ExecutionTermination::OomKilled {
            oom_kill_count: Some(1),
            memory_max_bytes: Some(8_589_934_592),
        });
        assert_eq!(fact.status, ExecutionStatus::Failure);
        assert_eq!(fact.exit_code, None);
        assert_eq!(
            fact.reason,
            "process ended by kernel OOM kill (cgroup memory.events oom_kill=1; effective MemoryMax=8589934592 bytes)"
        );
    }

    /// An OOM under no declared unit cap names the absence honestly — the
    /// kill came from somewhere else (host pressure, an outer cgroup), and
    /// the fact says so instead of inventing a cap.
    #[test]
    fn oom_termination_failure_fact_names_an_absent_cap_as_undeclared() {
        let fact = execution_fact_for_termination(&ExecutionTermination::OomKilled {
            oom_kill_count: None,
            memory_max_bytes: None,
        });
        assert_eq!(fact.status, ExecutionStatus::Failure);
        assert_eq!(
            fact.reason,
            "process ended by kernel OOM kill (cgroup memory.events oom_kill unmeasured; no unit MemoryMax declared)"
        );
    }

    #[test]
    fn accounting_witness_fields_never_fabricates_a_zero_when_unmeasured() {
        let (charge, gpu_seconds) = accounting_witness_fields(None, true);
        assert_eq!(charge, None);
        assert_eq!(gpu_seconds, None);
        let (charge, gpu_seconds) = accounting_witness_fields(None, false);
        assert_eq!(charge, None);
        assert_eq!(gpu_seconds, None);
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
                assert_no_progress_within(
                    "connection beyond the cap",
                    Duration::from_millis(50),
                    &mut third_call,
                )
                .await;

                drop(first);
                await_positive_progress("deferred connection", &mut third_call)
                    .await
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
        let executor = direct_executor(&paths.state_dir)
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
        let executor = direct_executor(&paths.state_dir)
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

    #[tokio::test(flavor = "current_thread")]
    async fn foreign_change_log_is_discarded_without_blocking_daemon_boot() {
        let temp = tempdir().unwrap();
        let paths = fs1_paths(temp.path());
        fs::create_dir_all(&paths.data_dir).unwrap();
        let change_path = paths.data_dir.join(crate::watch::CHANGE_FILE);
        fs::write(&change_path, b"{\"legacy\":true}\n").unwrap();
        let executor = direct_executor(&paths.state_dir)
            .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
            .with_unit_probe(ExitFileProbe);

        let daemon = Daemon::open_with_executor(
            one_pool_config(),
            paths.clone(),
            settings(),
            executor,
        )
        .await
        .expect("disposable foreign watch state must not block daemon boot");

        let bytes = fs::read(&change_path).unwrap();
        let records = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<crate::watch::ChangeRecord>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(!records.is_empty(), "startup did not seed the fresh feed");
        assert_eq!(records[0].sequence, 1);
        assert!(records
            .iter()
            .all(|record| record.schema_version == crate::watch::CHANGE_SCHEMA_VERSION));

        drop(daemon);
        drop(acquire_daemon_lock(&paths.state_dir).unwrap());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn symlinked_state_directory_refuses_daemon_boot_with_relocation_instruction() {
        let temp = tempdir().unwrap();
        let paths = fs1_paths(temp.path());
        let legacy_state = temp.path().join("legacy-state");
        fs::create_dir(&legacy_state).unwrap();
        let legacy_epoch = legacy_state.join("lease_epoch");
        fs::write(&legacy_epoch, b"41\n").unwrap();
        symlink(&legacy_state, &paths.state_dir).unwrap();
        let executor = direct_executor(&paths.state_dir)
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
            Ok(_) => panic!("daemon unexpectedly booted over a symlinked state directory"),
            Err(error) => error,
        };

        let message = error.to_string();
        assert!(message.contains(&paths.state_dir.display().to_string()));
        assert!(message.contains("replace it with a real directory"));
        assert!(message.contains("move the state files into it before starting tally"));
        match error {
            DaemonError::InvalidStateDirectory { path } => assert_eq!(path, paths.state_dir),
            other => panic!("expected typed state-directory error, got {other:?}"),
        }
        assert_eq!(fs::read(legacy_epoch).unwrap(), b"41\n");
        assert_eq!(fs::read_dir(&legacy_state).unwrap().count(), 1);
        assert!(!paths.data_dir.exists());
        assert!(!paths.socket.parent().unwrap().exists());
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
    async fn substituted_drv_witness_skips_every_admission_surface_and_meter() {
        const DRV: &str = "/nix/store/00000000000000000000000000000000-node.drv";
        const OUT: &str = "/nix/store/11111111111111111111111111111111-node";

        let temp = tempdir().unwrap();
        let paths = fs1_paths(temp.path());
        let mut config = one_pool_config();
        config.pools.insert(
            "build".to_owned(),
            PoolConfig {
                resource: Some(ResourceKind::BuildSlot),
                predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                ..PoolConfig::default()
            },
        );
        let executor = direct_executor(&paths.state_dir)
            .with_systemd_run(temp.path().join("absent-systemd-run"))
            .with_unit_probe(ExitFileProbe);
        let daemon = Daemon::open_with_executor(config, paths.clone(), settings(), executor)
            .await
            .unwrap();
        daemon.handler.context.write().await.derivation_store = Arc::new(AlwaysAvailableDerivation);

        let task_uuid = "00000000-0000-4000-8000-000000000084";
        let response = daemon
            .handler
            .enqueue_as_client(Some(json!({
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
            .run_until(daemon.handler.enqueue_as_client(Some(json!({
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
            usage_predecessor: None,
            session_cwd: None,
            final_message: None,
            usage_accounting: None,
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
            related_trigger: None,
            evidence_class: None,
            manifest_hash: None,
            usage: None,
            context_tokens: None,
            context_window: None,
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
            task_ref: None,
            journal_scope: None,
            projection_scope: None,
            class: row.priority,
            source: row.source,
            message: Some(format!("fixture {event} attempt={attempt}")),
            agent: Some(row.adapter.clone()),
            session_ref: row.session_ref.clone(),
            unit: Some(format!("tally-job-{}.service", row.uuid)),
            exit_code: terminal.then_some(if event == TallyEvent::Completed { 0 } else { 1 }),
            stderr_tail: None,
            stderr_truncated: None,
            gpu_seconds: terminal.then_some(0.0),
            context_tokens: None,
            context_window: None,
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
                error: None,
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
    fn substituted_terminal_results_are_successful_lifecycle_events() {
        assert_eq!(
            terminal_lifecycle_event(Verdict::Substituted, false),
            TallyEvent::WitnessEmitted
        );
        assert_eq!(
            terminal_lifecycle_event(Verdict::Substituted, true),
            TallyEvent::Completed
        );
    }

    /// #407: a terminal failure whose adapter stderr never existed was probed
    /// again at every startup, forever, one warning per probe, all of it
    /// charged to `TimeoutStartSec`.
    ///
    /// The mechanism is upstream of this pass. `write_capture_generation` is
    /// fsynced before `systemd-run` creates the unit, and
    /// `archive_current_capture` returns early when any of the capture set is
    /// missing — so an attempt that failed before its stderr stream existed
    /// leaves a generation marker nothing ever retires, and this pass read
    /// that marker as "recoverable" at every start.
    ///
    /// What is asserted here is the disposition: a definitive answer is
    /// recorded once, a transient one is not, and the pass stops before the
    /// first transient record so a later start still retries it.
    #[test]
    fn failure_stderr_recovery_is_one_shot_and_stops_at_the_first_transient_record() {
        let temp = tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let executor = direct_executor(&state_dir);
        let mut ledger = WitnessLedger::open(temp.path().join("witness.jsonl")).unwrap();

        // Three terminal failures, each with a capture generation naming this
        // attempt. The first and third have no adapter stderr at all: the
        // permanent shape. The second has one that cannot be opened without
        // following a link, which is a refusal rather than an absence and
        // therefore must not be written off.
        let mut seeded = Vec::new();
        for index in 0..3_usize {
            let uuid = Uuid::new_v4();
            let row = durable_row(uuid, &format!("failure-{index}"), 1);
            let identity = ExecutionIdentity {
                job_id: uuid,
                task_uuid: Some(uuid),
                task_ref: None,
            };
            let paths = executor.paths(&identity);
            fs::create_dir_all(paths.capture_generation.parent().unwrap()).unwrap();
            fs::create_dir_all(paths.stderr.parent().unwrap()).unwrap();
            fs::write(&paths.capture_generation, br#"{"attempt":1,"leaseEpoch":1}"#).unwrap();
            if index == 1 {
                std::os::unix::fs::symlink("absent-target", &paths.stderr).unwrap();
            }
            assert!(!paths.stderr.is_file());
            let record = append_fixture_witness(
                &mut ledger,
                &row,
                "2026-08-05T18:34:00.000Z",
                Verdict::Failed,
                1,
                1,
                1,
            );
            seeded.push((record, executor.capture_lock_path(&identity)));
        }
        let records = seeded
            .iter()
            .map(|(record, _)| record.clone())
            .collect::<Vec<_>>();

        // First pass: every record is probed. `persist_failure_stderr` takes
        // the capture lock, so the lock file existing is the witness that the
        // probe happened at all.
        startup::reconcile_failure_stderr(&records, &executor, &state_dir).unwrap();
        for (_, lock) in &seeded {
            assert!(lock.exists(), "every record is probed on the first pass");
        }
        let cursor = state_dir.join(startup::FAILURE_STDERR_CURSOR_FILE);
        let recorded: serde_json::Value =
            serde_json::from_slice(&fs::read(&cursor).unwrap()).unwrap();
        // Stops at the first record it could not settle, not at the last one
        // it could: the third record is definitive but sits behind the second.
        assert_eq!(recorded["reconciledThroughSeq"], json!(records[0].seq));
        assert_eq!(recorded["schemaVersion"], json!(1));

        // Second pass: the settled record is not probed again, and the two
        // behind the cursor still are.
        for (_, lock) in &seeded {
            fs::remove_file(lock).unwrap();
        }
        startup::reconcile_failure_stderr(&records, &executor, &state_dir).unwrap();
        assert!(
            !seeded[0].1.exists(),
            "a settled record must never be probed again"
        );
        assert!(seeded[1].1.exists(), "a deferred record is retried");
        assert!(
            seeded[2].1.exists(),
            "a record behind a deferred one is retried"
        );

        // And once the refusal is gone the cursor clears the whole ledger, so
        // the steady-state cost of this pass is zero probes.
        let uuid_one = Uuid::parse_str(records[1].task_uuid.as_deref().unwrap()).unwrap();
        let identity_one = ExecutionIdentity {
            job_id: uuid_one,
            task_uuid: Some(uuid_one),
            task_ref: None,
        };
        fs::remove_file(executor.paths(&identity_one).stderr).unwrap();
        startup::reconcile_failure_stderr(&records, &executor, &state_dir).unwrap();
        for (_, lock) in &seeded {
            let _ = fs::remove_file(lock);
        }
        startup::reconcile_failure_stderr(&records, &executor, &state_dir).unwrap();
        for (_, lock) in &seeded {
            assert!(!lock.exists(), "a fully reconciled ledger probes nothing");
        }
        let recorded: serde_json::Value =
            serde_json::from_slice(&fs::read(&cursor).unwrap()).unwrap();
        assert_eq!(recorded["reconciledThroughSeq"], json!(records[2].seq));
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
    fn terminal_witness_beats_a_stale_live_query_snapshot() {
        let projection = |witness_seq: Option<u64>| JobProjection {
            anchor: "job-1".to_owned(),
            task_uuid: Some("job-1".to_owned()),
            task_ref: None,
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
                let executor = direct_executor(&paths.state_dir)
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
                    .enqueue_as_client(Some(json!({
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
                    .enqueue_as_client(Some(json!({
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
                    .enqueue_as_client(Some(direct_payload.clone()))
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
                let direct_error = daemon.handler.enqueue_as_client(Some(malformed)).await.unwrap_err();

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
                    .enqueue_payload(
                        payload,
                        Some(claims[0].ingress_id.clone()),
                        CallerIdentity::Client,
                    )
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
                let executor = direct_executor(&paths.state_dir)
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
                let executor = direct_executor(&paths.state_dir)
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
                    .enqueue_as_client(Some(json!({
                        "argv": ["author wave 3"],
                        "pool": "slot",
                        "adapter": "codex",
                        "cwd": "/worktrees/explicit-issue-28",
                        "workspace": {
                            "repo": "mecattaf/tally.nix",
                            "baseRev": "origin/main",
                            "branch": "wave-3-ergonomics",
                            "worktreePath": "/worktrees/workspace-issue-28"
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
                        "/worktrees/explicit-issue-28",
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
                    false,
                )
                .unwrap();
                assert_eq!(
                    request.cwd.as_deref(),
                    Some(Path::new("/worktrees/explicit-issue-28")),
                    "an explicit payload cwd must win over workspace.worktreePath"
                );
                let args = executor
                    .build_systemd_argv(&request)
                    .unwrap()
                    .into_iter()
                    .map(|argument| argument.into_string().unwrap())
                    .collect::<Vec<_>>();
                assert!(args
                    .windows(2)
                    .any(|pair| {
                        pair == ["--working-directory", "/worktrees/explicit-issue-28"]
                    }));
                for expected in [
                    "NO_COLOR=1",
                    "TALLY_WORKSPACE_REPO=mecattaf/tally.nix",
                    "TALLY_WORKSPACE_BASE_REV=origin/main",
                    "TALLY_WORKSPACE_BRANCH=wave-3-ergonomics",
                    "TALLY_WORKSPACE_PATH=/worktrees/workspace-issue-28",
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
                    "/worktrees/explicit-issue-28".to_owned(),
                    "--".to_owned(),
                    "author wave 3".to_owned(),
                ]));
                assert_eq!(
                    query_row(&job.row, RowStatus::Pending)
                        .workspace
                        .unwrap()
                        .worktree_path,
                    PathBuf::from("/worktrees/workspace-issue-28")
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn flow_workspace_without_cwd_reaches_systemd_as_working_directory() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let worktree = temp.path().join("flow-worktree");
                fs::create_dir(&worktree).unwrap();
                let worktree_string = worktree.to_str().unwrap();
                let executor = direct_executor(&paths.state_dir)
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
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();
                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": [
                            "/bin/sh",
                            "-c",
                            "test \"$(pwd -P)\" = \"$1\"",
                            "cwd-check",
                            worktree_string
                        ],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "orchestrator",
                        "dedupKey": "flow:cwd-regression:0",
                        "submission": {"mode": "full"},
                        "orchestration": {
                            "flowName": "cwd-regression",
                            "flowRunId": "00000000-0000-4000-8000-000000000232",
                            "scriptHash": "sha256:cwd-regression",
                            "nodeOrdinal": 0,
                            "maxNodes": 1
                        },
                        "workspace": {
                            "repo": "mecattaf/tally.nix",
                            "baseRev": "origin/main",
                            "branch": "issue-232",
                            "worktreePath": worktree_string
                        },
                        "evidence": [],
                        "noEnqueue": true,
                        "credentials": {},
                        "wait": false
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
                assert!(job.row.cwd.is_none(), "flows do not submit raw cwd");

                let request = execution_request(
                    &executor,
                    &job,
                    settings().unit_limits,
                    ("/run/tally/tally.sock", None),
                    &paths.data_dir,
                    false,
                )
                .unwrap();
                assert_eq!(
                    request.cwd.as_deref(),
                    Some(worktree.as_path())
                );
                let args = executor
                    .build_systemd_argv(&request)
                    .unwrap()
                    .into_iter()
                    .map(|argument| argument.into_string().unwrap())
                    .collect::<Vec<_>>();
                assert!(args.windows(2).any(|pair| {
                    pair[0] == "--working-directory" && pair[1] == worktree_string
                }));

                let mut invalid_job = job.clone();
                invalid_job
                    .row
                    .workspace
                    .as_mut()
                    .unwrap()
                    .worktree_path = PathBuf::from("/worktrees/issue-%n");
                let invalid_request = execution_request(
                    &executor,
                    &invalid_job,
                    settings().unit_limits,
                    ("/run/tally/tally.sock", None),
                    &paths.data_dir,
                    false,
                )
                .unwrap();
                assert!(matches!(
                    executor.build_systemd_argv(&invalid_request),
                    Err(ExecutorError::InvalidRequest(detail))
                        if detail == "working directory must not contain systemd specifier character %"
                ));

                let outcome = executor.execute(request).await.unwrap();
                assert_eq!(outcome.backend, ExecutionBackend::Direct);
                assert_eq!(outcome.termination, ExecutionTermination::Exited(0));
            })
            .await;
    }

    /// A codex-shaped adapter whose base prefix ends in `--` so the cwd argv
    /// template has somewhere to render, and whose program simply fails: the
    /// retry path needs a terminal witness, not a real agent.
    fn cwd_argv_config(exit_code: u8) -> Config {
        let mut config = one_pool_config();
        config.adapters.insert(
            "codex".to_owned(),
            AdapterConfig {
                argv: vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    format!("exit {exit_code}"),
                    "--".to_owned(),
                ],
                launch: crate::adapters::AdapterLaunchConfig {
                    cwd_argv: Some(vec!["-C".to_owned(), "%<cwd>%".to_owned()]),
                    ..crate::adapters::AdapterLaunchConfig::default()
                },
                ..AdapterConfig::default()
            },
        );
        config.validate().unwrap();
        config
    }

    fn flow_cwd_payload(worktree: &Path, cwd: Option<&Path>) -> Value {
        let mut payload = json!({
            "argv": ["author wave 3"],
            "pool": "slot",
            "adapter": "codex",
            "source": "orchestrator",
            "dedupKey": "flow:cwd-argv:0",
            "submission": {"mode": "full"},
            "orchestration": {
                "flowName": "cwd-argv",
                "flowRunId": "00000000-0000-4000-8000-000000000318",
                "scriptHash": "sha256:cwd-argv",
                "nodeOrdinal": 0,
                "maxNodes": 1
            },
            "workspace": {
                "repo": "mecattaf/tally.nix",
                "baseRev": "origin/main",
                "branch": "issue-318",
                "worktreePath": worktree.to_str().unwrap()
            },
            "evidence": [],
            "noEnqueue": true,
            "credentials": {},
            "wait": false
        });
        if let Some(cwd) = cwd {
            payload["cwd"] = json!(cwd.to_str().unwrap());
        }
        payload
    }

    fn rendered_cwd_argument(argv: &[String]) -> Option<&str> {
        argv.windows(2)
            .find(|pair| pair[0] == "-C")
            .map(|pair| pair[1].as_str())
    }

    /// #232 gave a flow node the right *process* cwd by deriving it from the
    /// workspace, but the adapter argv was rendered from the raw row cwd,
    /// which a flow never submits. The witnessed argv therefore omitted
    /// `-C <worktree>` while the process ran in it. Admission, recovery, and
    /// the execution request now resolve one effective cwd, so they cannot
    /// disagree; an explicit payload cwd still outranks the workspace.
    #[tokio::test(flavor = "current_thread")]
    async fn flow_workspace_without_cwd_renders_the_adapter_cwd_argv() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let worktree = temp.path().join("flow-worktree");
                fs::create_dir(&worktree).unwrap();
                let explicit = temp.path().join("explicit-cwd");
                fs::create_dir(&explicit).unwrap();
                let config = cwd_argv_config(0);
                let executor = direct_executor(&paths.state_dir)
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

                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(flow_cwd_payload(&worktree, None)))
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
                assert!(job.row.cwd.is_none(), "flows do not submit raw cwd");
                assert_eq!(
                    rendered_cwd_argument(&job.invocation.argv),
                    worktree.to_str(),
                    "the admission render must carry -C <worktreePath>"
                );
                let request = execution_request(
                    &executor,
                    &job,
                    settings().unit_limits,
                    ("/run/tally/tally.sock", None),
                    &paths.data_dir,
                    false,
                )
                .unwrap();
                assert_eq!(request.cwd.as_deref(), Some(worktree.as_path()));
                assert_eq!(
                    rendered_cwd_argument(&request.argv),
                    request.cwd.as_deref().and_then(Path::to_str),
                    "the witnessed argv and the process cwd must be the same directory"
                );

                // An explicit payload cwd is still the submission's decision.
                let with_cwd = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["author wave 3"],
                        "pool": "slot",
                        "adapter": "codex",
                        "cwd": explicit.to_str().unwrap(),
                        "workspace": {
                            "repo": "mecattaf/tally.nix",
                            "baseRev": "origin/main",
                            "branch": "issue-318",
                            "worktreePath": worktree.to_str().unwrap()
                        }
                    })))
                    .await
                    .unwrap();
                let explicit_job_id =
                    Uuid::parse_str(with_cwd["job_id"].as_str().unwrap()).unwrap();
                let explicit_job = daemon
                    .handler
                    .context
                    .read()
                    .await
                    .jobs
                    .get(&explicit_job_id)
                    .cloned()
                    .unwrap();
                assert_eq!(
                    rendered_cwd_argument(&explicit_job.invocation.argv),
                    explicit.to_str()
                );

                // Recovery re-renders the invocation from the durable row
                // alone. Before the shared helper it lost the -C argument the
                // admitted job had.
                drop(daemon);
                let recovered = Daemon::open_with_executor(
                    config,
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                let recovered_job = recovered
                    .handler
                    .context
                    .read()
                    .await
                    .jobs
                    .get(&job_id)
                    .cloned()
                    .unwrap();
                assert!(recovered_job.row.cwd.is_none());
                assert_eq!(
                    rendered_cwd_argument(&recovered_job.invocation.argv),
                    worktree.to_str(),
                    "the recovery render must carry -C <worktreePath>"
                );
            })
            .await;
    }

    /// The retry path re-renders from the durable row too, so it is the third
    /// site that used to drop the flow node's `-C <worktree>`.
    #[tokio::test(flavor = "current_thread")]
    async fn retried_flow_node_keeps_its_workspace_cwd_argv() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let worktree = temp.path().join("flow-worktree");
                fs::create_dir(&worktree).unwrap();
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    cwd_argv_config(3),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let handler = daemon.handler.clone();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let admitted = client
                    .call("queue.enqueue", Some(flow_cwd_payload(&worktree, None)))
                    .await
                    .unwrap();
                assert_eq!(fs1_wait(&client, &admitted).await["verdict"], "failed");
                client
                    .call("queue.pause", Some(json!({"pool": "slot", "all": false})))
                    .await
                    .unwrap();
                let retried = client
                    .call(
                        "queue.retry",
                        Some(json!({"task_uuid": admitted["task_uuid"]})),
                    )
                    .await
                    .unwrap();
                assert_eq!(retried["retried"], true);
                assert_eq!(retried["attempt"], 2);

                let task_uuid =
                    Uuid::parse_str(admitted["task_uuid"].as_str().unwrap()).unwrap();
                let retried_job = handler
                    .context
                    .read()
                    .await
                    .jobs
                    .get(&task_uuid)
                    .cloned()
                    .unwrap();
                assert_eq!(retried_job.row.attempt, 2);
                assert!(retried_job.row.cwd.is_none());
                assert_eq!(
                    rendered_cwd_argument(&retried_job.invocation.argv),
                    worktree.to_str(),
                    "the retry render must carry -C <worktreePath>"
                );

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
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
                                counter_scope: None,
                                fields: Default::default(),
                            },
                        )]),
                        ..AdapterConfig::default()
                    },
                );
                config.validate().unwrap();
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon = Daemon::open_with_executor(config, paths, settings(), executor)
                    .await
                    .unwrap();
                let first = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["initial request"],
                        "pool": "slot",
                        "adapter": "resumable"
                    })))
                    .await
                    .unwrap();
                let finished =
                    await_positive_progress("initial continuation source", daemon.completion_rx.recv())
                        .await
                        .unwrap();
                daemon.finish_job(finished).await.unwrap();
                daemon.handler.drain_post_ack_tasks().await;
                let first_id = first["job_id"].as_str().unwrap();
                // The scraped session pointer outlives the job: the job is
                // terminal and retired from the live map (#395), and the
                // continuation below reads the pointer back from exactly this
                // fact.
                let first_uuid = Uuid::parse_str(first_id).unwrap();
                {
                    let context = daemon.handler.context.read().await;
                    assert!(!context.jobs.contains_key(&first_uuid));
                    assert_eq!(
                        context.query_rows[&first_uuid].session_ref.as_deref(),
                        Some("session-28")
                    );
                }

                let continued = daemon
                    .handler
                    .continue_job_as_client(Some(json!({
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
                let predecessor = UsagePredecessor::new(first_uuid, 1, 1);
                assert_eq!(
                    continued_job.row.usage_predecessor,
                    Some(predecessor.clone()),
                    "a cross-task continuation durably names the exact source attempt"
                );
                assert_eq!(
                    continued_job.invocation.usage_accounting,
                    UsageAccountingMode::Resume {
                        predecessor: Some(predecessor),
                    },
                    "the accounting lineage must travel with the rendered resume"
                );

                let finished =
                    await_positive_progress("continued job completion", daemon.completion_rx.recv())
                        .await
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

    /// #443. The stock Codex stream does not identify the model when Codex
    /// selected its default. Recovery must retain that absence: a required
    /// `%<model>%` would make this real, complete capture impossible to
    /// continue, while inventing a model would silently pin a choice Codex
    /// never reported. An explicit job option is a different fact and remains
    /// a typed, durable request on both sides of the restart.
    #[tokio::test(flavor = "current_thread")]
    async fn default_model_codex_recovery_is_model_less_while_explicit_model_is_preserved() {
        const CODEX_CAPTURE: &str =
            include_str!("../../../../test/fixtures/usage/codex.jsonl");

        fn codex_config(program: &Path) -> Config {
            let mut config = one_pool_config();
            config.adapters.insert(
                "codex".to_owned(),
                AdapterConfig {
                    argv: vec![
                        program.to_string_lossy().into_owned(),
                        "exec".to_owned(),
                        "--json".to_owned(),
                        "--".to_owned(),
                    ],
                    resume: Some(vec![
                        program.to_string_lossy().into_owned(),
                        "-C".to_owned(),
                        "%<cwd>%".to_owned(),
                        "exec".to_owned(),
                        "resume".to_owned(),
                        "--json".to_owned(),
                        "%<sessionRef>%".to_owned(),
                        "--".to_owned(),
                    ]),
                    scrape: BTreeMap::from([
                        (
                            "sessionRef".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPath,
                                pattern: "$..thread_id".to_owned(),
                                counter_scope: None,
                                fields: BTreeMap::new(),
                            },
                        ),
                        (
                            "model".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPath,
                                pattern: "$..model".to_owned(),
                                counter_scope: None,
                                fields: BTreeMap::new(),
                            },
                        ),
                        (
                            "usage".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPath,
                                pattern: "$..usage".to_owned(),
                                counter_scope: Some(
                                    crate::adapters::UsageCounterScope::SessionCumulative,
                                ),
                                fields: BTreeMap::from([
                                    (
                                        "inputTokensWithCacheRead".to_owned(),
                                        vec!["input_tokens".to_owned()],
                                    ),
                                    (
                                        "cacheReadTokens".to_owned(),
                                        vec!["cached_input_tokens".to_owned()],
                                    ),
                                    (
                                        "cacheWriteTokens".to_owned(),
                                        vec!["cache_write_input_tokens".to_owned()],
                                    ),
                                    (
                                        "outputTokens".to_owned(),
                                        vec!["output_tokens".to_owned()],
                                    ),
                                    (
                                        "reasoningTokens".to_owned(),
                                        vec!["reasoning_output_tokens".to_owned()],
                                    ),
                                ]),
                            },
                        ),
                        (
                            "finalMessage".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPathLast,
                                pattern: "$[?@.type == 'item.completed' && @.item.type == 'agent_message'].item.text".to_owned(),
                                counter_scope: None,
                                fields: BTreeMap::new(),
                            },
                        ),
                    ]),
                    launch: AdapterLaunchConfig {
                        cwd_argv: Some(vec!["-C".to_owned(), "%<cwd>%".to_owned()]),
                        resume_options_before_capture: Some("sessionRef".to_owned()),
                        model: Some(AdapterValueOverride {
                            argv: vec!["--model".to_owned(), "%<value>%".to_owned()],
                            allowed_values: vec!["gpt-5.6-codex".to_owned()],
                        }),
                        ..AdapterLaunchConfig::default()
                    },
                    usage_counter_scope:
                        crate::adapters::UsageCounterScope::SessionCumulative,
                    ..AdapterConfig::default()
                },
            );
            config
        }

        async fn finish_next(daemon: &mut Daemon) {
            let finished =
                tokio::time::timeout(Duration::from_secs(5), daemon.completion_rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
            daemon.finish_job(finished).await.unwrap();
            daemon.handler.drain_post_ack_tasks().await;
        }

        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let cwd = temp.path().join("worktree");
                fs::create_dir(&cwd).unwrap();
                let program = temp.path().join("codex");
                assert!(CODEX_CAPTURE.ends_with('\n'));
                crate::test_support::install_shell_program(
                    &program,
                    format!(
                        "#!/bin/sh\ncat <<'TALLY_REAL_CODEX_CAPTURE'\n{CODEX_CAPTURE}TALLY_REAL_CODEX_CAPTURE\n"
                    ),
                );
                let config = codex_config(&program);
                config.validate().unwrap();
                let state_dir = paths.state_dir.clone();
                let absent_systemd_run = temp.path().join("absent-systemd-run");
                let executor = || {
                    direct_executor(&state_dir)
                        .with_systemd_run(absent_systemd_run.clone())
                        .with_unit_probe(ExitFileProbe)
                };
                let mut daemon = Daemon::open_with_executor(
                    config.clone(),
                    paths.clone(),
                    settings(),
                    executor(),
                )
                .await
                .unwrap();

                let default = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["initial default-model request"],
                        "pool": "slot",
                        "adapter": "codex",
                        "cwd": cwd,
                    })))
                    .await
                    .unwrap();
                finish_next(&mut daemon).await;
                let default_id = default["job_id"].as_str().unwrap().to_owned();
                let default_uuid = Uuid::parse_str(&default_id).unwrap();

                let explicit = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["initial explicit-model request"],
                        "pool": "slot",
                        "adapter": "codex",
                        "cwd": cwd,
                        "adapterOptions": {"model": "gpt-5.6-codex"},
                    })))
                    .await
                    .unwrap();
                finish_next(&mut daemon).await;
                let explicit_id = explicit["job_id"].as_str().unwrap().to_owned();
                let explicit_uuid = Uuid::parse_str(&explicit_id).unwrap();
                drop(daemon);

                // Recovery replays the retained real capture. The default
                // row has a session, final answer, and normalized usage, but
                // still no model; the explicit row retains its typed request.
                let mut restarted =
                    Daemon::open_with_executor(config, paths, settings(), executor())
                        .await
                        .unwrap();
                {
                    let context = restarted.handler.context.read().await;
                    let default_row = &context.rows[&default_uuid];
                    assert_eq!(
                        default_row.session_ref.as_deref(),
                        Some("codex-usage-thread")
                    );
                    assert!(default_row.model.is_none());
                    assert!(default_row.adapter_options.model.is_none());
                    let usage = default_row
                        .usage
                        .as_ref()
                        .and_then(crate::usage::UsageObservation::breakdown)
                        .expect("the real Codex capture reports usage");
                    assert_eq!(usage.input_tokens_as_reported, Some(7_060_166));
                    assert_eq!(usage.input_tokens, Some(262_086));
                    assert_eq!(usage.cache_read_tokens, Some(6_798_080));
                    assert_eq!(usage.cache_write_tokens, Some(0));
                    assert_eq!(usage.output_tokens, Some(32_842));
                    assert_eq!(usage.reasoning_tokens, Some(15_163));
                    assert_eq!(
                        context.query_rows[&default_uuid].final_message.as_deref(),
                        Some("<redacted text>")
                    );
                    assert!(context.query_rows[&default_uuid].model.is_none());
                    assert_eq!(
                        context.rows[&explicit_uuid]
                            .adapter_options
                            .model
                            .as_deref(),
                        Some("gpt-5.6-codex")
                    );
                }

                let continued_default = restarted
                    .handler
                    .continue_job_as_client(Some(json!({
                        "resumeFrom": default_id,
                        "argv": ["continue default"],
                    })))
                    .await
                    .unwrap();
                let continued_default_uuid =
                    Uuid::parse_str(continued_default["job_id"].as_str().unwrap()).unwrap();
                let default_argv = restarted.handler.context.read().await.jobs
                    [&continued_default_uuid]
                    .invocation
                    .argv
                    .clone();
                assert_eq!(
                    default_argv,
                    vec![
                        program.to_string_lossy().into_owned(),
                        "-C".to_owned(),
                        cwd.to_string_lossy().into_owned(),
                        "exec".to_owned(),
                        "resume".to_owned(),
                        "--json".to_owned(),
                        "codex-usage-thread".to_owned(),
                        "--".to_owned(),
                        "continue default".to_owned(),
                    ]
                );
                assert!(!default_argv.iter().any(|argument| argument == "--model"));
                finish_next(&mut restarted).await;

                let continued_explicit = restarted
                    .handler
                    .continue_job_as_client(Some(json!({
                        "resumeFrom": explicit_id,
                        "argv": ["continue explicit"],
                    })))
                    .await
                    .unwrap();
                let continued_explicit_uuid =
                    Uuid::parse_str(continued_explicit["job_id"].as_str().unwrap()).unwrap();
                let explicit_argv = restarted.handler.context.read().await.jobs
                    [&continued_explicit_uuid]
                    .invocation
                    .argv
                    .clone();
                assert_eq!(
                    explicit_argv,
                    vec![
                        program.to_string_lossy().into_owned(),
                        "-C".to_owned(),
                        cwd.to_string_lossy().into_owned(),
                        "exec".to_owned(),
                        "resume".to_owned(),
                        "--json".to_owned(),
                        "--model".to_owned(),
                        "gpt-5.6-codex".to_owned(),
                        "codex-usage-thread".to_owned(),
                        "--".to_owned(),
                        "continue explicit".to_owned(),
                    ]
                );
                assert_eq!(
                    explicit_argv
                        .iter()
                        .filter(|argument| argument.as_str() == "--model")
                        .count(),
                    1
                );
                finish_next(&mut restarted).await;
            })
            .await;
    }

    /// #425. A harness that resolves a session by the directory it was
    /// launched in cannot reach that session from anywhere else, and pi's
    /// version of "cannot reach" is exit 0 with an interactive prompt on
    /// stderr — a successful attempt that did no work, which is the worst
    /// shape a headless pipeline can be handed. `queue.continue` is the one
    /// seam where the directory can move, so it is the one that refuses.
    #[tokio::test(flavor = "current_thread")]
    async fn a_continuation_off_the_launch_directory_is_refused_naming_both_directories() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let launched_in = temp.path().join("session-home");
                let elsewhere = temp.path().join("another-checkout");
                std::fs::create_dir(&launched_in).unwrap();
                std::fs::create_dir(&elsewhere).unwrap();
                let program = temp.path().join("cwd-keyed-agent");
                crate::test_support::install_shell_program(
                    &program,
                    "#!/bin/sh\nprintf '%s\\n' '{\"thread_id\":\"session-31\"}'\n",
                );
                let mut config = one_pool_config();
                config.adapters.insert(
                    "cwd-keyed".to_owned(),
                    AdapterConfig {
                        argv: vec![
                            program.to_string_lossy().into_owned(),
                            "fresh".to_owned(),
                            "--".to_owned(),
                        ],
                        resume_requires_launch_cwd: true,
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
                                counter_scope: None,
                                fields: Default::default(),
                            },
                        )]),
                        ..AdapterConfig::default()
                    },
                );
                config.validate().unwrap();
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon = Daemon::open_with_executor(config, paths, settings(), executor)
                    .await
                    .unwrap();
                let first = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["initial request"],
                        "pool": "slot",
                        "adapter": "cwd-keyed",
                        "cwd": launched_in.to_string_lossy(),
                    })))
                    .await
                    .unwrap();
                let finished =
                    await_positive_progress("cwd-keyed source completion", daemon.completion_rx.recv())
                        .await
                        .unwrap();
                daemon.finish_job(finished).await.unwrap();
                daemon.handler.drain_post_ack_tasks().await;
                let first_id = first["job_id"].as_str().unwrap().to_owned();

                let refused = daemon
                    .handler
                    .continue_job_as_client(Some(json!({
                        "resumeFrom": first_id,
                        "argv": ["address review"],
                        "cwd": elsewhere.to_string_lossy(),
                    })))
                    .await
                    .unwrap_err();
                let message = format!("{refused:?}");
                assert!(
                    message.contains(launched_in.to_str().unwrap())
                        && message.contains(elsewhere.to_str().unwrap()),
                    "the refusal must name the recorded and the requested directory: {message}"
                );

                // Fail-closed means nothing was admitted: no second row, no
                // unit, nothing for an operator to mistake for progress.
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.rows.len(), 1);
                }

                // The control. The same continuation in the directory the
                // session was launched in is admitted and renders the resume
                // argv, so the refusal above is about the directory and not
                // about continuation being broken.
                let continued = daemon
                    .handler
                    .continue_job_as_client(Some(json!({
                        "resumeFrom": first_id,
                        "argv": ["address review"],
                        "cwd": launched_in.to_string_lossy(),
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
                        "session-31".to_owned(),
                        "--".to_owned(),
                        "address review".to_owned(),
                    ]
                );
                // The record travels with the pointer: the continuation row
                // carries both, so continuing the continuation is guarded too.
                assert_eq!(
                    continued_job.row.session_cwd,
                    Some(RecordedLaunchCwd::In(launched_in.clone()))
                );
            })
            .await;
    }

    /// The adapter both #425 tests below share: cwd-keyed, resumable, and it
    /// prints one session pointer.
    fn cwd_keyed_config(program: &Path) -> Config {
        let mut config = one_pool_config();
        config.adapters.insert(
            "cwd-keyed".to_owned(),
            AdapterConfig {
                argv: vec![
                    program.to_string_lossy().into_owned(),
                    "fresh".to_owned(),
                    "--".to_owned(),
                ],
                resume_requires_launch_cwd: true,
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
                        counter_scope: None,
                        fields: Default::default(),
                    },
                )]),
                ..AdapterConfig::default()
            },
        );
        config.validate().unwrap();
        config
    }

    fn launch_cwd_recovery_row(uuid: Uuid, launched_in: &Path) -> RowSeed {
        let mut row = durable_row(uuid, "launch-cwd-recovery", 2);
        row.adapter = "cwd-keyed".to_owned();
        row.cwd = Some(launched_in.to_path_buf());
        row.argv = vec!["continue recovered work".to_owned()];
        row.attempt = 2;
        row
    }

    fn append_session_attestation(
        path: &Path,
        row: &RowSeed,
        attempt: u32,
        lease_epoch: u64,
        session_ref: &str,
    ) {
        append_attestation(
            path,
            json!({
                "kind": "adapter-scrape",
                "taskUuid": row.uuid.to_string(),
                "jobId": row.uuid.to_string(),
                "adapter": row.adapter,
                "attempt": attempt,
                "leaseEpoch": lease_epoch,
                "captures": {"sessionRef": session_ref},
                "usageAuthority": "advisory-only",
            }),
        )
        .unwrap();
    }

    fn assert_session_launch_cwd_reaches_resume_guard(
        config: &Config,
        row: &RowSeed,
        session_ref: &str,
        launched_in: &Path,
    ) {
        assert_eq!(row.session_ref.as_deref(), Some(session_ref));
        assert_eq!(
            row.session_cwd,
            Some(RecordedLaunchCwd::In(launched_in.to_path_buf()))
        );
        AdapterEngine::new(&config.adapters)
            .guard_resume_launch_cwd(
                &row.adapter,
                row.session_cwd.as_ref(),
                Some(launched_in),
            )
            .unwrap();
    }

    /// #440. A pool-return representation recovers its session pointer from
    /// the previous attempt's attestation before it is installed. The launch
    /// directory record must be hydrated at that same represent-only seam.
    #[test]
    fn represent_hydration_records_launch_cwd_beside_the_pointer() {
        let temp = tempdir().unwrap();
        let launched_in = temp.path().join("represent-session-home");
        let program = temp.path().join("cwd-keyed-agent");
        let config = cwd_keyed_config(&program);
        let executor = Executor::new(temp.path().join("state"), std::env::current_exe().unwrap());
        let row = launch_cwd_recovery_row(Uuid::new_v4(), &launched_in);
        let attestation_path = temp.path().join("attestations.jsonl");
        append_session_attestation(&attestation_path, &row, 1, 1, "represent-session");
        let mut attestations = SharedAttestations::new(attestation_path);
        let mut plan = empty_plan();
        plan.rows.push(RecoveryRow {
            row: row.clone(),
            state: RecoveryRowState::Pending,
            labor_class: LaborClass::Recovered,
            guardrail_depth: 0,
        });
        plan.actions.push(RecoveryAction::RePresent {
            row: Box::new(row),
            trigger: crate::evidence::RetryTrigger::PoolReturn,
            previous_witness_seq: 1,
            previous_attempt: 1,
            previous_lease_epoch: 1,
            previous_usage_predecessor: None,
        });

        hydrate_represent_adapter_metadata(&mut plan, &config, &executor, &mut attestations)
            .unwrap();

        assert_session_launch_cwd_reaches_resume_guard(
            &config,
            &plan.rows[0].row,
            "represent-session",
            &launched_in,
        );
    }

    /// #440. An adopted attempt has no current capture to scrape, so its
    /// continuation identity comes from the latest prior attestation. Bind
    /// the launch record to that adopted-metadata path specifically.
    #[test]
    fn adopted_metadata_records_launch_cwd_beside_the_pointer() {
        let temp = tempdir().unwrap();
        let launched_in = temp.path().join("adopted-session-home");
        let program = temp.path().join("cwd-keyed-agent");
        let config = cwd_keyed_config(&program);
        let row = launch_cwd_recovery_row(Uuid::new_v4(), &launched_in);
        let attestation_path = temp.path().join("attestations.jsonl");
        append_session_attestation(&attestation_path, &row, 1, 1, "adopted-session");
        let mut attestations = SharedAttestations::new(attestation_path);
        let mut plan = empty_plan();
        plan.rows.push(RecoveryRow {
            row: row.clone(),
            state: RecoveryRowState::AdoptedRunning,
            labor_class: LaborClass::Recovered,
            guardrail_depth: 0,
        });
        plan.actions.push(RecoveryAction::AdoptRunning {
            identity: RecoveryIdentity::Task(row.uuid),
            unit: format!("tally-job-{}.service", row.uuid),
            invocation_id: "adopted-invocation".to_owned(),
            attempt: row.attempt,
            lease_epoch: row.lease_epoch,
            labor_class: Some(LaborClass::Recovered),
        });

        hydrate_adopted_adapter_metadata(&mut plan, &mut attestations).unwrap();

        assert_session_launch_cwd_reaches_resume_guard(
            &config,
            &plan.rows[0].row,
            "adopted-session",
            &launched_in,
        );
    }

    /// #440. A durable continued row keeps `session_ref`, while serde drops
    /// `session_cwd`. `QueueExisting` therefore has to reinstall the record
    /// when it turns that recovered row into a live job.
    #[tokio::test(flavor = "current_thread")]
    async fn recovered_job_install_records_launch_cwd_beside_the_pointer() {
        let temp = tempdir().unwrap();
        let paths = DaemonPaths {
            socket: temp.path().join("run/tally.sock"),
            state_dir: temp.path().join("state"),
            data_dir: temp.path().join("data"),
        };
        let launched_in = temp.path().join("installed-session-home");
        fs::create_dir(&launched_in).unwrap();
        let program = temp.path().join("cwd-keyed-agent");
        let config = cwd_keyed_config(&program);
        let mut row = launch_cwd_recovery_row(Uuid::new_v4(), &launched_in);
        row.attempt = 1;
        row.lease_epoch = 1;
        row.resumed_from = Some(Uuid::new_v4().to_string());
        row.session_ref = Some("installed-session".to_owned());
        initialize_final_witness_state(&paths);
        write_enqueue_event_atomic(
            &paths.events_dir(),
            &DurableEnqueueEvent::new(row.clone()).unwrap(),
        )
        .unwrap();
        let persisted = read_acknowledged_events(&paths.events_dir()).unwrap();
        assert_eq!(persisted[0].row.session_ref.as_deref(), Some("installed-session"));
        assert_eq!(persisted[0].row.session_cwd, None);

        let executor = direct_executor(&paths.state_dir)
            .with_systemd_run(temp.path().join("absent-systemd-run"))
            .with_unit_probe(ExitFileProbe);
        let daemon =
            Daemon::open_with_executor(config.clone(), paths, settings(), executor)
                .await
                .unwrap();
        let installed = daemon
            .handler
            .context
            .read()
            .await
            .jobs
            .get(&row.uuid)
            .cloned()
            .unwrap();

        assert_session_launch_cwd_reaches_resume_guard(
            &config,
            &installed.row,
            "installed-session",
            &launched_in,
        );
    }

    /// #440. Completion scrapes after the terminal acknowledgement and the
    /// live job has already been retired. Observe that exact enriched job so
    /// this test cannot pass through `FoundJob::observed_row`'s later recovery.
    #[tokio::test(flavor = "current_thread")]
    async fn completion_scrape_records_launch_cwd_beside_the_pointer() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let launched_in = temp.path().join("completed-session-home");
                fs::create_dir(&launched_in).unwrap();
                let program = temp.path().join("cwd-keyed-agent");
                crate::test_support::install_shell_program(
                    &program,
                    "#!/bin/sh\nprintf '%s\\n' '{\"thread_id\":\"completed-session\"}'\n",
                );
                let config = cwd_keyed_config(&program);
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon = Daemon::open_with_executor(
                    config.clone(),
                    paths,
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["initial work"],
                        "pool": "slot",
                        "adapter": "cwd-keyed",
                        "cwd": launched_in.to_string_lossy(),
                    })))
                    .await
                    .unwrap();
                let job_id = Uuid::parse_str(admitted["job_id"].as_str().unwrap()).unwrap();
                let mut enriched_rx = completion::observe_completion_enrichment(job_id);
                let finished = await_positive_progress(
                    "launch-cwd completion",
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let enriched = await_positive_progress(
                    "post-completion launch-cwd enrichment",
                    enriched_rx.recv(),
                )
                .await
                .unwrap();

                assert_session_launch_cwd_reaches_resume_guard(
                    &config,
                    &enriched.row,
                    "completed-session",
                    &launched_in,
                );
            })
            .await;
    }

    /// #425 (F2). "This row declared no working directory" and "nothing
    /// recorded where this session was launched" are different facts, and only
    /// the second is a reason to refuse.
    ///
    /// A row that declares no cwd runs wherever the service manager put the
    /// daemon; a continuation that also declares none runs in that same
    /// directory, so the session is reachable and the continuation must be
    /// admitted. `tally enqueue --adapter pi -- ...` with no `--cwd` produces
    /// exactly this row -- `cli/enqueue.rs` defaults the cwd to nothing -- so
    /// collapsing the two facts would permanently block continuations for the
    /// one preset the invariant was written for.
    #[tokio::test(flavor = "current_thread")]
    async fn a_continuation_declaring_no_directory_reaches_a_session_launched_with_none() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let program = temp.path().join("cwd-keyed-agent");
                crate::test_support::install_shell_program(
                    &program,
                    "#!/bin/sh\nprintf '%s\\n' '{\"thread_id\":\"session-42\"}'\n",
                );
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon = Daemon::open_with_executor(
                    cwd_keyed_config(&program),
                    paths,
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                // No `cwd` on either call. Both attempts inherit the daemon's.
                let first = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["initial request"],
                        "pool": "slot",
                        "adapter": "cwd-keyed",
                    })))
                    .await
                    .unwrap();
                let finished =
                    await_positive_progress("default-cwd source completion", daemon.completion_rx.recv())
                        .await
                        .unwrap();
                daemon.finish_job(finished).await.unwrap();
                daemon.handler.drain_post_ack_tasks().await;
                let first_id = first["job_id"].as_str().unwrap().to_owned();

                let continued = daemon
                    .handler
                    .continue_job_as_client(Some(json!({
                        "resumeFrom": first_id,
                        "argv": ["address review"],
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
                        "session-42".to_owned(),
                        "--".to_owned(),
                        "address review".to_owned(),
                    ]
                );
                // The record says "declared none", which is a record, not the
                // absence of one.
                assert_eq!(
                    continued_job.row.session_cwd,
                    Some(RecordedLaunchCwd::ServiceManagerDefault)
                );
            })
            .await;
    }

    /// #425. The record has to survive a daemon restart, because the pointer
    /// does: startup re-derives `session_ref` from the retained adapter
    /// attestation, and a record that is not re-derived beside it leaves a
    /// recovered row refusing its own continuation with `UnrecordedLaunchCwd`.
    ///
    /// This binds the `apply_adapter_metadata` seam in `daemon/startup.rs`
    /// directly -- the row in `context.rows` after recovery is asserted, not
    /// only the end-to-end continuation, because the continuation path also
    /// re-derives at `FoundJob::observed_row` and would pass without it.
    #[tokio::test(flavor = "current_thread")]
    async fn a_restarted_daemon_re_derives_the_launch_record_beside_the_pointer() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let launched_in = temp.path().join("session-home");
                std::fs::create_dir(&launched_in).unwrap();
                let program = temp.path().join("cwd-keyed-agent");
                crate::test_support::install_shell_program(
                    &program,
                    "#!/bin/sh\nprintf '%s\\n' '{\"thread_id\":\"session-55\"}'\n",
                );
                let config = cwd_keyed_config(&program);
                let state_dir = paths.state_dir.clone();
                let absent_systemd_run = temp.path().join("absent-systemd-run");
                let executor = || {
                    direct_executor(&state_dir)
                        .with_systemd_run(absent_systemd_run.clone())
                        .with_unit_probe(ExitFileProbe)
                };
                let mut daemon = Daemon::open_with_executor(
                    config.clone(),
                    paths.clone(),
                    settings(),
                    executor(),
                )
                .await
                .unwrap();
                let first = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["initial request"],
                        "pool": "slot",
                        "adapter": "cwd-keyed",
                        "cwd": launched_in.to_string_lossy(),
                    })))
                    .await
                    .unwrap();
                let finished = await_positive_progress(
                    "restart source completion",
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap();
                daemon.finish_job(finished).await.unwrap();
                daemon.handler.drain_post_ack_tasks().await;
                let first_id = first["job_id"].as_str().unwrap().to_owned();
                let first_uuid = Uuid::parse_str(&first_id).unwrap();
                drop(daemon);

                // The restart.
                let restarted =
                    Daemon::open_with_executor(config, paths, settings(), executor())
                        .await
                        .unwrap();
                {
                    let context = restarted.handler.context.read().await;
                    let recovered = &context.rows[&first_uuid];
                    assert_eq!(
                        recovered.session_ref.as_deref(),
                        Some("session-55"),
                        "startup re-derives the pointer from the retained attestation"
                    );
                    assert_eq!(
                        recovered.session_cwd,
                        Some(RecordedLaunchCwd::In(launched_in.clone())),
                        "and must re-derive the launch record beside it"
                    );
                }

                // End to end: the recovered row continues in its launch
                // directory, and is refused anywhere else.
                let continued = restarted
                    .handler
                    .continue_job_as_client(Some(json!({
                        "resumeFrom": first_id,
                        "argv": ["address review"],
                        "cwd": launched_in.to_string_lossy(),
                    })))
                    .await
                    .unwrap();
                assert!(continued["job_id"].is_string());
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
                let executor = direct_executor(&paths.state_dir)
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
                    .enqueue_as_client(Some(json!({
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
                let finished =
                    await_positive_progress("gate verdict completion", daemon.completion_rx.recv())
                        .await
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

    /// Issue #382: a GPU-pool job's witness carries real, measured
    /// `gpuSeconds` and `charge` — never the always-`None` and always-
    /// fabricated-`Some(0.0)` these fields used to carry through this exact
    /// completion path (`run.rs`'s `finish_job`).
    ///
    /// The exit recorder's own accounting probe is exercised end to end
    /// elsewhere (`crates/tally/tests/record_unit_exit_accounting.rs`,
    /// against the real `tally` binary and a fake `systemctl`). This test
    /// instead proves the daemon-side wiring: a measured `UnitAccounting`
    /// sample on the `ExecutionOutcome` reaches the witness record, the
    /// completion lifecycle event, and `canonical_gpu_seconds`, gated
    /// correctly on the job's pool actually being `vram`-resource. The
    /// direct-fallback backend this suite otherwise uses never sets
    /// `accounting` (it has no `ExecStopPost`), so the sample is placed on
    /// `finished.outcome` the one place a test can reach it before
    /// `finish_job` consumes it — the same shape a real systemd completion
    /// would have handed the daemon.
    #[tokio::test(flavor = "current_thread")]
    async fn a_gpu_pool_jobs_witness_carries_measured_gpu_seconds_and_charge() {
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
                config.pools.insert(
                    "gpu".to_owned(),
                    PoolConfig {
                        resource: Some(ResourceKind::Vram),
                        predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                        ..PoolConfig::default()
                    },
                );
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                let history = daemon.handler.history.clone();
                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "gpu",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"],
                    })))
                    .await
                    .unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
                let mut finished =
                    await_positive_progress("GPU job completion", daemon.completion_rx.recv())
                        .await
                        .unwrap();
                // The direct-fallback backend never probes systemd, so this
                // stands in for what a real `ExecStopPost` accounting probe
                // would have embedded in the exit record: 2.5 measured
                // CPU-seconds, and 3.5 seconds of measured main-process
                // wall-clock runtime for the GPU-pool job.
                if let Some(Ok(outcome)) = finished.outcome.as_mut() {
                    outcome.record.accounting = Some(Box::new(UnitAccounting {
                        cpu_usage_nsec: Some(2_500_000_000),
                        exec_main_start_monotonic_usec: Some(1_000_000),
                        exec_main_exit_monotonic_usec: Some(4_500_000),
                        ..UnitAccounting::default()
                    }));
                } else {
                    panic!("expected a successful direct-fallback completion");
                }
                daemon.finish_job(finished).await.unwrap();
                let terminal = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");

                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                let record = records
                    .iter()
                    .find(|record| record.task_uuid.as_deref() == Some(task_uuid.as_str()))
                    .unwrap();
                assert_eq!(record.gpu_seconds, Some(3.5));
                assert_eq!(
                    record.charge,
                    Some(Charge {
                        unit: "cpu-second".to_owned(),
                        amount: 2.5,
                        class_name: "measured".to_owned(),
                    })
                );
                assert!(counts_toward_canonical_gpu_seconds(record));
                assert_eq!(canonical_gpu_seconds(records.iter().cloned()), 3.5);

                // The completion lifecycle event carries the same measured
                // value, not the old fabricated `Some(0.0)`.
                //
                // `completed_event` is emitted from the post-ack `spawn_local`
                // task, so `await_job` returning terminal says nothing about
                // whether that task has run (#419). Awaiting it is the only
                // thing that puts the event before this assertion; without the
                // drain the assertion wins the race almost always and loses it
                // under a loaded host, which is a flake, not a bug.
                daemon.handler.drain_post_ack_tasks().await;
                assert!(history.borrow().snapshot().records.iter().any(|entry| {
                    entry.fields.task_uuid == task_uuid && entry.fields.gpu_seconds == Some(3.5)
                }));
            })
            .await;
    }

    /// #382 HIGH-1 (post-merge repair): `vram` is `ResourceKind`'s default,
    /// so a pool whose config omits `resource` entirely must NOT read as a
    /// GPU pool. Before this repair, `resource_kind(pool) ==
    /// Some(ResourceKind::Vram)` compared against the *effective*
    /// (defaulted) resource, so a job in a pool that declared nothing at
    /// all — `{"capacity": 2}`, no `resource` key — got a real,
    /// non-fabricated-looking `gpuSeconds` on a host with no GPU. This pins
    /// the fix: `declared_resource_kind` reads the raw `Option` the pool
    /// config carries, so silence never reads as a declaration.
    #[tokio::test(flavor = "current_thread")]
    async fn a_pool_that_declares_no_resource_never_gets_gpu_seconds() {
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
                // No `resource` key at all — exactly the shape an operator
                // writes when they never intended a GPU pool, e.g.
                // `services.tally.pools.worker = { capacity = 2; };`.
                config.pools.insert(
                    "silent".to_owned(),
                    PoolConfig {
                        capacity: 2,
                        predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                        ..PoolConfig::default()
                    },
                );
                assert_eq!(config.pools["silent"].resource, None);
                assert_eq!(
                    config.pools["silent"].resource(),
                    ResourceKind::Vram,
                    "the effective default is unchanged by this repair"
                );
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "silent",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"],
                    })))
                    .await
                    .unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
                let mut finished = await_positive_progress(
                    "unclassified-pool job completion",
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap();
                if let Some(Ok(outcome)) = finished.outcome.as_mut() {
                    outcome.record.accounting = Some(Box::new(UnitAccounting {
                        cpu_usage_nsec: Some(1_000_000_000),
                        exec_main_start_monotonic_usec: Some(1_000_000),
                        exec_main_exit_monotonic_usec: Some(3_500_000),
                        ..UnitAccounting::default()
                    }));
                } else {
                    panic!("expected a successful direct-fallback completion");
                }
                daemon.finish_job(finished).await.unwrap();
                daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();

                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                let record = records
                    .iter()
                    .find(|record| record.task_uuid.as_deref() == Some(task_uuid.as_str()))
                    .unwrap();
                assert_eq!(
                    record.gpu_seconds, None,
                    "a pool that declared no resource must never carry gpuSeconds"
                );
                // The generic charge is unaffected: CPU accounting is not
                // gated on the GPU-pool question at all.
                assert_eq!(
                    record.charge,
                    Some(Charge {
                        unit: "cpu-second".to_owned(),
                        amount: 1.0,
                        class_name: "measured".to_owned(),
                    })
                );
            })
            .await;
    }

    /// A non-GPU-pool job's measured accounting still charges CPU-seconds,
    /// but never fills `gpuSeconds` — the field means "held a `vram` pool",
    /// not "any job that happened to be measured".
    #[tokio::test(flavor = "current_thread")]
    async fn a_non_gpu_pool_jobs_witness_carries_charge_but_no_gpu_seconds() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = direct_executor(&paths.state_dir)
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
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"],
                    })))
                    .await
                    .unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
                let mut finished = await_positive_progress(
                    "non-GPU job completion",
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap();
                if let Some(Ok(outcome)) = finished.outcome.as_mut() {
                    outcome.record.accounting = Some(Box::new(UnitAccounting {
                        cpu_usage_nsec: Some(1_000_000_000),
                        exec_main_start_monotonic_usec: Some(1_000_000),
                        exec_main_exit_monotonic_usec: Some(2_000_000),
                        ..UnitAccounting::default()
                    }));
                } else {
                    panic!("expected a successful direct-fallback completion");
                }
                daemon.finish_job(finished).await.unwrap();
                daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();

                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                let record = records
                    .iter()
                    .find(|record| record.task_uuid.as_deref() == Some(task_uuid.as_str()))
                    .unwrap();
                assert_eq!(record.gpu_seconds, None);
                assert_eq!(
                    record.charge,
                    Some(Charge {
                        unit: "cpu-second".to_owned(),
                        amount: 1.0,
                        class_name: "measured".to_owned(),
                    })
                );
            })
            .await;
    }

    /// A probe that never measured anything (the exit recorder's
    /// `systemctl` call failed, or this is a pre-#382 record) must never
    /// surface as a fabricated `Some(0.0)` anywhere downstream — not on the
    /// witness, and not on the completion lifecycle event.
    #[tokio::test(flavor = "current_thread")]
    async fn unmeasured_accounting_never_fabricates_a_zero_on_witness_or_event() {
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
                config.pools.insert(
                    "gpu".to_owned(),
                    PoolConfig {
                        resource: Some(ResourceKind::Vram),
                        predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                        ..PoolConfig::default()
                    },
                );
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                let history = daemon.handler.history.clone();
                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "gpu",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"],
                    })))
                    .await
                    .unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
                let finished = await_positive_progress(
                    "unmeasured-accounting completion",
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap();
                // Left as the direct-fallback backend produced it:
                // `accounting: None`, exactly like a failed probe.
                daemon.finish_job(finished).await.unwrap();
                daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();

                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                let record = records
                    .iter()
                    .find(|record| record.task_uuid.as_deref() == Some(task_uuid.as_str()))
                    .unwrap();
                assert_eq!(record.gpu_seconds, None);
                assert_eq!(record.charge, None);
                assert!(history.borrow().snapshot().records.iter().any(|entry| {
                    entry.fields.task_uuid == task_uuid && entry.fields.gpu_seconds.is_none()
                }));
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
                    direct_executor(&paths.state_dir)
                        .with_systemd_run(temp.path().join("absent-systemd-run"))
                        .with_unit_probe(ExitFileProbe);
                let mut daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();

                let absent = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": [program, "absent"],
                        "pool": "slot",
                        "adapter": "codex",
                    })))
                    .await
                    .unwrap();
                let finished = await_positive_progress(
                    "absent-manifest completion",
                    daemon.completion_rx.recv(),
                )
                .await
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
                // From `context.rows`, which keeps the admitted seed for the
                // daemon's lifetime; the job itself is terminal and retired
                // out of the live map (#395).
                assert!(daemon
                    .handler
                    .context
                    .read()
                    .await
                    .rows
                    .get(&absent_uuid)
                    .unwrap()
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
                    .enqueue_as_client(Some(json!({
                        "argv": [program, "write"],
                        "pool": "slot",
                        "adapter": "codex",
                    })))
                    .await
                    .unwrap();
                let finished = await_positive_progress(
                    "passed-gates completion",
                    daemon.completion_rx.recv(),
                )
                .await
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

    /// A run's cost, end to end: a real attempt is scraped, normalized, and
    /// attested, and `query.run` and `query.standup` both sum it per attempt
    /// over the run's **durable membership** — including for a run that owns
    /// no row for the task at all, which is the W-316 shape.
    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_384_run_and_standup_roll_per_attempt_usage_up_to_the_run() {
        const RUN_A: &str = "00000000-0000-4000-8000-0000000003a0";
        const RUN_B: &str = "00000000-0000-4000-8000-0000000003b0";
        const RUN_C: &str = "00000000-0000-4000-8000-0000000003c0";
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let program = temp.path().join("usage-agent");
                // The claude-code shape: `input_tokens` excludes both cache
                // halves, so the fresh-prompt volume is only visible once
                // `cache_creation_input_tokens` is added to it.
                crate::test_support::install_shell_program(
                    &program,
                    concat!(
                        "#!/bin/sh\n",
                        "printf '%s\\n' '{\"event\":{\"session_id\":\"usage-session\",",
                        "\"usage\":{\"input_tokens\":83,\"cache_creation_input_tokens\":265127,",
                        "\"cache_read_input_tokens\":11093140,\"output_tokens\":22298}}}'\n",
                        "printf '%s\\n' 'branch=usage' >&2\n"
                    ),
                );
                let mut config = one_pool_config();
                let mut adapter = structured_adapter(&program);
                adapter.scrape.insert(
                    "usage".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..usage".to_owned(),
                        counter_scope: None,
                        fields: serde_json::from_str(
                            r#"{"inputTokens":["input_tokens"],"cacheReadTokens":["cache_read_input_tokens"],"cacheWriteTokens":["cache_creation_input_tokens"],"outputTokens":["output_tokens"]}"#,
                        )
                        .unwrap(),
                    },
                );
                config
                    .adapters
                    .insert("usage-agent".to_owned(), adapter.clone());
                // The same harness, read through a mapping that has drifted in
                // exactly one key: `cacheReadTokens` names a path this stream
                // does not carry. Every other declared path still resolves, so
                // the attempt reports usage and contributes -- and the
                // 11,093,140 cache-read tokens leave the total in silence
                // unless the rollup checks per-component coverage.
                let mut drifted = adapter;
                drifted.scrape.insert(
                    "usage".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..usage".to_owned(),
                        counter_scope: None,
                        fields: serde_json::from_str(
                            r#"{"inputTokens":["input_tokens"],"cacheReadTokens":["cache_read_input_tokens_v2"],"cacheWriteTokens":["cache_creation_input_tokens"],"outputTokens":["output_tokens"]}"#,
                        )
                        .unwrap(),
                    },
                );
                config.adapters.insert("drift-agent".to_owned(), drifted);
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon = Daemon::open_with_executor(
                    config.clone(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["work"],
                        "pool": "slot",
                        "adapter": "usage-agent",
                        "source": "manual",
                        "evidence": ["exit:0"],
                        "orchestration": {
                            "flowName": "spec-build",
                            "flowRunId": RUN_A,
                            "nodeOrdinal": 1,
                            "nodeLabel": "agent-t01"
                        }
                    })))
                    .await
                    .unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
                let finished =
                    await_positive_progress("usage-bearing completion", daemon.completion_rx.recv())
                        .await
                        .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let terminal = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");
                // The scrape, the normalization, and the attestation append all
                // happen post-ack, so wait for the ledger to actually hold this
                // attempt rather than for the file to merely exist -- the
                // daemon creates it empty at startup.
                await_positive_progress("usage attestation publication", async {
                    loop {
                        let attested = fs::read_to_string(paths.attestations_path())
                            .unwrap_or_default()
                            .lines()
                            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                            .any(|record| record["payload"]["taskUuid"] == task_uuid.as_str());
                        if attested {
                            break;
                        }
                        // Sleep rather than spin: this suite runs in parallel
                        // with tests that measure cgroup CPU seconds, and a
                        // busy poll burning a core is a plausible amplifier of
                        // their timing flakes.
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await;

                let view = daemon
                    .handler
                    .query("query.run", Some(json!({"id": RUN_A})))
                    .await
                    .unwrap();
                let usage = &view["usage"];
                assert_eq!(usage["authority"], "advisory-provider-capture");
                assert_eq!(usage["coverage"]["ledgerVerified"], true);
                assert_eq!(usage["coverage"]["tasks"], 1);
                assert_eq!(usage["coverage"]["attemptsObserved"], 1);
                assert_eq!(usage["coverage"]["attemptsReported"], 1);
                assert_eq!(usage["coverage"]["attemptsReportedWithComponents"], 1);
                assert_eq!(usage["tokens"]["inputTokens"]["value"], 83);
                assert_eq!(usage["tokens"]["cacheWriteTokens"]["value"], 265_127);
                assert_eq!(usage["tokens"]["cacheReadTokens"]["value"], 11_093_140);
                assert_eq!(usage["tokens"]["outputTokens"]["value"], 22_298);
                // The whole point: `inputTokens` alone would report 83 fresh
                // input tokens for an attempt that sent 265,210 of them.
                assert_eq!(usage["tokens"]["freshInputTokens"]["value"], 265_210);
                assert_eq!(usage["tokens"]["totalTokens"]["value"], 11_380_648);
                assert_eq!(
                    usage["tokens"]["totalTokens"]["source"],
                    "derived-from-components"
                );
                assert_eq!(usage["caveats"], json!([]));

                // A second run was handed the same node and owns no row for
                // it. Membership is the only place that fact is written down,
                // and the rollup must charge run B for the attempt anyway.
                let record = crate::flow_membership::FlowMembershipRecord::new(
                    RUN_B.to_owned(),
                    task_uuid.clone(),
                    crate::flow_membership::MembershipDisposition::Attached,
                    Some(7),
                    Some("b-node-7".to_owned()),
                );
                crate::flow_membership::record_membership(
                    &paths.flow_membership_path(),
                    &record,
                    crate::flow_membership::FlowMembership::default(),
                    &BTreeSet::new(),
                )
                .unwrap();
                let attached = daemon
                    .handler
                    .query("query.run", Some(json!({"id": RUN_B})))
                    .await
                    .unwrap();
                assert_eq!(attached["usage"]["coverage"]["attemptsReported"], 1);
                assert_eq!(attached["usage"]["tokens"]["freshInputTokens"]["value"], 265_210);

                // And the stand-up window carries the same rollup for every
                // run it touched, both the creating run and the attached one.
                let digest = daemon
                    .handler
                    .query("query.standup", Some(json!({})))
                    .await
                    .unwrap();
                let runs = digest["runs"].as_array().unwrap();
                assert_eq!(
                    runs.iter()
                        .map(|run| run["flowRunId"].as_str().unwrap())
                        .collect::<Vec<_>>(),
                    [RUN_A, RUN_B]
                );
                for run in runs {
                    assert_eq!(run["usage"]["tokens"]["totalTokens"]["value"], 11_380_648);
                    assert_eq!(run["usage"]["coverage"]["attemptsReported"], 1);
                }

                // Same harness, one drifted key. The attempt reports usage and
                // contributes, so it is not `reportedWithoutFigures` -- and a
                // rollup that only checked that bucket would grade this run
                // complete while reporting 2.5% of what it cost.
                let drifted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["work"],
                        "pool": "slot",
                        "adapter": "drift-agent",
                        "source": "manual",
                        "evidence": ["exit:0"],
                        "orchestration": {
                            "flowName": "spec-build",
                            "flowRunId": RUN_C,
                            "nodeOrdinal": 1,
                            "nodeLabel": "agent-t02"
                        }
                    })))
                    .await
                    .unwrap();
                let drifted_task = drifted["task_uuid"].as_str().unwrap().to_owned();
                let finished = await_positive_progress(
                    "drifted-usage completion",
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap();
                daemon.finish_job(finished).await.unwrap();
                daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": drifted_task})))
                    .await
                    .unwrap();
                await_positive_progress("drifted-usage attestation publication", async {
                    loop {
                        let attested = fs::read_to_string(paths.attestations_path())
                            .unwrap_or_default()
                            .lines()
                            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                            .any(|record| {
                                record["payload"]["taskUuid"] == drifted_task.as_str()
                            });
                        if attested {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await;

                let view = daemon
                    .handler
                    .query("query.run", Some(json!({"id": RUN_C})))
                    .await
                    .unwrap();
                let usage = &view["usage"];
                assert_eq!(usage["coverage"]["attemptsReported"], 1);
                assert_eq!(
                    usage["coverage"]["attemptsReportedWithoutFigures"], 0,
                    "one key drifted, not all of them"
                );
                assert_eq!(
                    usage["coverage"]["attemptsReportedWithComponents"], 1,
                    "the attempt reported components, so the threshold judges it"
                );
                assert_eq!(usage["tokens"]["inputTokens"]["attempts"], 1);
                assert_eq!(usage["tokens"]["cacheReadTokens"]["attempts"], 0);
                assert_eq!(
                    usage["tokens"]["totalTokens"]["value"],
                    83 + 265_127 + 22_298,
                    "the drifted component silently left the total"
                );
                assert_eq!(
                    usage["caveats"],
                    json!(["partial-components"]),
                    "and the run must say so rather than grading itself complete"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_completed_empty_scrape_is_a_durable_typed_absence() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let config = one_pool_config();
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "evidence": ["exit:0"],
                    })))
                    .await
                    .unwrap();
                let finished = await_positive_progress(
                    "empty-scrape completion",
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap();
                daemon.finish_job(finished).await.unwrap();
                daemon.handler.drain_post_ack_tasks().await;

                let line = fs::read_to_string(paths.attestations_path()).unwrap();
                let record: crate::witness::AttestationRecord =
                    serde_json::from_str(line.lines().next().unwrap()).unwrap();
                assert_eq!(record.payload["taskUuid"], admitted["task_uuid"]);
                assert_eq!(record.payload["captures"], json!({}));
                assert_eq!(record.payload["usage"], json!({"state": "not-declared"}));
                assert_eq!(
                    record.payload["usageEvidence"],
                    json!({
                        "schemaVersion": 1,
                        "declaredFields": [],
                        "counterScope": "attempt",
                        "observed": {"state": "not-declared"},
                        "accounting": {
                            "state": "exact",
                            "basis": "fresh",
                            "usage": {"state": "not-declared"},
                        },
                    })
                );
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
                // No `contextWindow` scrape is declared, so the config
                // ceiling is what this attempt's `query.job` should render —
                // proof the config provenance path is real end to end, not
                // only unit-tested against a synthetic `ScrapeResult`.
                adapter
                    .extra_config
                    .insert("contextWindow".to_owned(), json!(200_000));
                config.adapters.insert("from-nix".to_owned(), adapter);
                let executor = direct_executor(&paths.state_dir)
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
                    .enqueue_as_client(Some(json!({
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
                    .enqueue_as_client(Some(json!({
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

                let finished = await_positive_progress(
                    "trace-bearing completion",
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let terminal = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": admitted["task_uuid"]})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");

                await_positive_progress("adapter metadata publication", async {
                    loop {
                        // Read from the query fact, not `context.jobs`: the
                        // job is terminal by now and terminal jobs are retired
                        // out of the live map (#395). The query fact is where
                        // the post-ack scrape has always landed too.
                        let enriched = daemon
                            .handler
                            .context
                            .read()
                            .await
                            .query_rows
                            .get(&Uuid::parse_str(job_id).unwrap())
                            .and_then(|fact| fact.session_ref.as_deref())
                            == Some("session-opaque");
                        if enriched && paths.attestations_path().exists() {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await;

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
                // The normalized record is persisted beside the raw capture,
                // keyed by the same task/attempt/lease-epoch triple, and it
                // says which of the three states this attempt is in.
                assert_eq!(
                    attestation.payload["usage"],
                    json!({
                        "state": "reported",
                        "breakdown": {
                            "shape": "components",
                            "inputTokens": 999999,
                            "inputTokensAsReported": 999999,
                            "totalTokens": {
                                "value": 999999,
                                "source": "derived-from-components",
                            },
                        },
                    })
                );
                assert_eq!(attestation.payload["attempt"], 1);
                assert_eq!(attestation.payload["leaseEpoch"], 1);
                assert_eq!(
                    attestation.payload["usageEvidence"],
                    json!({
                        "schemaVersion": 1,
                        "declaredFields": ["inputTokens", "outputTokens", "totalTokens"],
                        "counterScope": "attempt",
                        "observed": attestation.payload["usage"],
                        "accounting": {
                            "state": "exact",
                            "basis": "fresh",
                            "usage": attestation.payload["usage"],
                        },
                    }),
                    "raw observation and per-attempt accounting are distinct durable fields"
                );
                let (report, witness) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(report.ok);
                assert_eq!(witness.len(), 1);
                assert_eq!(witness[0].verdict, Verdict::Pass);
                assert_eq!(witness[0].gpu_seconds, None);
                assert_eq!(witness[0].charge, None);
                assert_eq!(witness[0].model, None);
                // Same reason as the poll above: the observed model survives
                // the job on the query fact, not in the live map (#395).
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .query_rows
                        .get(&Uuid::parse_str(job_id).unwrap())
                        .unwrap()
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
                assert_eq!(
                    before["job"]["usage"],
                    json!({
                        "value": {
                            "state": "reported",
                            "breakdown": {
                                "shape": "components",
                                "inputTokens": 999999,
                                "inputTokensAsReported": 999999,
                                "totalTokens": {
                                    "value": 999999,
                                    "source": "derived-from-components",
                                },
                            },
                        },
                        "authority": "advisory-provider-capture",
                        "provenance": "adapter-scrape",
                    }),
                    "the breakdown renders as an advisory provider capture, never collapsed \
                     into a canonical authority"
                );
                assert_eq!(
                    before["job"]["usageAccounting"],
                    json!({
                        "value": {
                            "state": "exact",
                            "basis": "fresh",
                            "usage": attestation.payload["usage"],
                        },
                        "authority": "advisory-attestation",
                        "provenance": "adapter-scrape usageEvidence.accounting",
                    }),
                    "query detail must not present the raw observation as the charge"
                );
                // Occupancy rides beside `usage`, but is read through its own
                // narrower capture, never derived from `usage`'s
                // session-lifetime total: this synthetic adapter declares no
                // `occupancy` capture (it is not claude-code-shaped), so
                // `contextTokens` is absent rather than reusing the same
                // 999999 `usage` reports -- proof the two are no longer the
                // same number under two names. `contextWindow` is
                // config-declared here and renders with the advisory,
                // non-durable authority true of a live-config value that
                // vanishes on restart.
                assert_eq!(
                    before["job"].get("contextTokens"),
                    None,
                    "no occupancy capture is declared for this adapter"
                );
                assert_eq!(
                    before["job"]["contextWindow"],
                    json!({
                        "value": 200000,
                        "authority": "advisory-config",
                        "provenance": "adapter-config",
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
                // `query.trace` exposes both fields too, flattened beside
                // `sessionRef` on the same lane -- `contextTokens` absent for
                // the same reason it is on `query.job`.
                assert_eq!(trace["items"][0].get("contextTokens"), None);
                assert_eq!(trace["items"][0]["contextWindow"], 200000);
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
                assert_eq!(repaired.payload["usageEvidence"]["accounting"]["basis"], "fresh");

                drop(reopened);

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
                let executor = direct_executor(&paths.state_dir)
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
                    .enqueue_as_client(Some(json!({
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
                let executor = direct_executor(&paths.state_dir)
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
                    .enqueue_as_client(Some(json!({
                        "argv": ["work"],
                        "pool": "slot",
                        "adapter": "blocked",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let terminal = await_positive_progress(
                    "terminal witness acknowledgement",
                    handler.await_job(Some(json!({"task_uuid": admitted["task_uuid"]}))),
                )
                .await
                .unwrap();
                assert_eq!(terminal["verdict"], "pass");

                shutdown.send(true).unwrap();
                assert_no_progress_within(
                    "shutdown while the attestation writer is locked",
                    Duration::from_millis(50),
                    &mut daemon_task,
                )
                .await;
                fs2::FileExt::unlock(&lock).unwrap();
                await_positive_progress("post-ack writer join", daemon_task)
                    .await
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
        let mut adapter = structured_adapter(&program);
        adapter.usage_counter_scope = crate::adapters::UsageCounterScope::SessionCumulative;
        config.adapters.insert("resumable".to_owned(), adapter);
        let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap());
        let mut row = durable_row(Uuid::new_v4(), "resume-key", 2);
        row.adapter = "resumable".to_owned();
        row.argv = vec!["continue-work".to_owned()];
        row.attempt = 2;
        row.usage_predecessor = Some(UsagePredecessor::new(row.uuid, 1, 1));
        let capture_paths = executor.paths(&ExecutionIdentity {
            job_id: row.uuid,
            task_uuid: Some(row.uuid),
            task_ref: None,
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
            previous_usage_predecessor: None,
        };
        let attestation_path = temp.path().join("attestations.jsonl");
        let mut attestations = SharedAttestations::new(attestation_path.clone());
        fs::write(
            &capture_paths.capture_generation,
            r#"{"attempt":0,"leaseEpoch":1}"#,
        )
        .unwrap();
        assert!(
            recovery_adapter_invocation(&config, &action, &row, &executor, &mut attestations)
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
            &mut SharedAttestations::new(blocked_attestation),
        )
        .is_err());
        assert_eq!(fs::read(&capture_paths.stdout).unwrap(), original);

        let (invocation, captures) =
            recovery_adapter_invocation(&config, &action, &row, &executor, &mut attestations)
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
            invocation.usage_accounting,
            UsageAccountingMode::Resume {
                predecessor: row.usage_predecessor.clone(),
            },
            "restart re-presentation must preserve the bound predecessor"
        );
        assert_eq!(
            captures.unwrap().captures["branch"],
            Value::String("recovery".to_owned())
        );
        assert_eq!(fs::read(&capture_paths.stdout).unwrap(), original);
        assert!(verified_adapter_attestation_captures(
            &mut attestations,
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
            recovery_adapter_invocation(
                &config,
                &action,
                &row,
                &executor,
                &mut SharedAttestations::new(missing_attestation),
            ),
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
            recovery_adapter_invocation(&config, &action, &row, &executor, &mut attestations)
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
        hydrate_represent_adapter_metadata(&mut plan, &config, &executor, &mut attestations)
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
        hydrate_completed_adapter_metadata(&mut deleted_plan, &config, &executor, &mut attestations);
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
                task_ref: None,
            }),
            invocation_id: "attempt-2-invocation".to_owned(),
            attempt: 2,
            lease_epoch: 2,
            labor_class: Some(LaborClass::Fresh),
        });
        hydrate_adopted_adapter_metadata(&mut adopted_plan, &mut attestations).unwrap();
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
        let executor = direct_executor(&paths.state_dir)
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

    /// An adapter shaped exactly like every adapter that existed before the
    /// usage mapping did: a `usage` capture and no declared field paths. What
    /// it normalizes to is what the meter feeder must keep charging.
    fn legacy_shaped_usage(captures: &ScrapeResult) -> crate::usage::UsageEvidence {
        let adapter = AdapterConfig {
            argv: vec!["agent".to_owned()],
            scrape: BTreeMap::from([(
                "usage".to_owned(),
                ScrapeCapture {
                    stream: ScrapeStream::Stdout,
                    mode: ScrapeMode::JsonPath,
                    pattern: "$..usage".to_owned(),
                    counter_scope: None,
                    fields: Default::default(),
                },
            )]),
            ..Default::default()
        };
        crate::usage::account_usage(
            "legacy",
            &adapter,
            captures,
            &UsageAccountingMode::Fresh,
            None,
        )
    }

    #[test]
    fn usage_predecessor_lookup_pins_the_exact_lease_and_last_duplicate() {
        let temp = tempdir().unwrap();
        let mut attestations =
            SharedAttestations::new(temp.path().join("usage-predecessor.jsonl"));
        let task = "00000000-0000-4000-8000-000000000403";
        for (lease_epoch, marker) in [(7, "lease-7-old"), (8, "lease-8"), (7, "lease-7-new")]
        {
            attestations
                .ledger()
                .unwrap()
                .append(json!({
                    "kind": "adapter-scrape",
                    "taskUuid": task,
                    "adapter": "codex",
                    "attempt": 2,
                    "leaseEpoch": lease_epoch,
                    "marker": marker,
                }))
                .unwrap();
        }

        let selected = attestations
            .usage_predecessor_payload(&UsageAccountingMode::Resume {
                predecessor: Some(UsagePredecessor::new(task, 2, 7)),
            })
            .unwrap()
            .unwrap();
        assert_eq!(selected["marker"], "lease-7-new");
        assert_eq!(selected["leaseEpoch"], 7);

        let other_lease = attestations
            .usage_predecessor_payload(&UsageAccountingMode::Resume {
                predecessor: Some(UsagePredecessor::new(task, 2, 8)),
            })
            .unwrap()
            .unwrap();
        assert_eq!(other_lease["marker"], "lease-8");
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
            feed_scraped_usage(
                &state_dir,
                &config.pools,
                &["api".to_owned()],
                &legacy_shaped_usage(&captures),
            )
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
            feed_scraped_usage(
                &state_dir,
                &config.pools,
                &["api".to_owned()],
                &legacy_shaped_usage(&low),
            ).is_empty()
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
        for (malformed, diagnostic_expected) in [
            (json!({"usage": {"total_tokens": "80"}}), true),
            (
                json!({"usage": {"input_tokens": -1, "output_tokens": 4}}),
                true,
            ),
            (
                json!({"usage": {"input_tokens": 0, "output_tokens": 0}}),
                false,
            ),
        ] {
            let captures = ScrapeResult {
                captures: malformed.as_object().unwrap().clone().into_iter().collect(),
            };
            let diagnostics = feed_scraped_usage(
                &state_dir,
                &config.pools,
                &["api".to_owned()],
                &legacy_shaped_usage(&captures),
            );
            assert_eq!(
                !diagnostics.is_empty(),
                diagnostic_expected,
                "typed accounting failures are surfaced while a genuine zero is a no-op"
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
            feed_scraped_usage(
                &state_dir,
                &config.pools,
                &["api".to_owned()],
                &legacy_shaped_usage(&captures),
            )
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
                let executor = direct_executor(&paths.state_dir)
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
                    .enqueue_as_client(Some(json!({
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
                    .enqueue_as_client(Some(json!({
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
                await_positive_progress("urgent job retirement", async {
                    loop {
                        // Terminal now means retired from the live map
                        // (#395), not present-and-`Completed`.
                        if !daemon
                            .handler
                            .context
                            .read()
                            .await
                            .jobs
                            .contains_key(
                                &Uuid::parse_str(urgent["job_id"].as_str().unwrap()).unwrap(),
                            )
                        {
                            break;
                        }
                        let finished = daemon.completion_rx.recv().await.unwrap();
                        daemon.finish_job(finished).await.unwrap();
                    }
                })
                .await;
                let urgent_result = await_positive_progress(
                    "urgent job terminal publication",
                    daemon
                        .handler
                        .await_job(Some(json!({"task_uuid": urgent["task_uuid"]}))),
                )
                .await
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
                let executor = direct_executor(&paths.state_dir)
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
                let (published_tx, mut published_rx) = mpsc::unbounded_channel();
                let (release_tx, release_rx) = watch::channel(false);
                let transport = RecoveringRemoteTransport {
                    calls: calls.clone(),
                    published: published_tx,
                    release: release_rx,
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
                    .enqueue_as_client(Some(json!({
                        "argv": ["opaque-worker-command", "two words", "$HOME"],
                        "pool": "slot",
                        "executor": "worker",
                        "priority": "high",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"],
                        "orchestration": {
                            "flowRunId": "00000000-0000-4000-8000-000000000260",
                            "taskRef": "crm/t07"
                        }
                    })))
                    .await
                    .unwrap();
                assert_eq!(admitted["state"], "running");
                assert_eq!(admitted["taskRef"], "crm/t07");

                await_positive_progress("second remote invocation publication", async {
                    loop {
                        let count = published_rx
                            .recv()
                            .await
                            .expect("remote invocation observer must remain open");
                        if count == 2 {
                            break;
                        }
                    }
                })
                .await;
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.lease.engine().held_in_pool("slot").unwrap(), 1);
                    let job_id = Uuid::parse_str(admitted["job_id"].as_str().unwrap()).unwrap();
                    assert_eq!(context.jobs[&job_id].state, JobState::Running);
                }
                assert!(daemon.completion_rx.try_recv().is_err());
                let (_, witness_before) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(witness_before.is_empty());

                release_tx.send(true).unwrap();
                let finished =
                    await_positive_progress("remote completion", daemon.completion_rx.recv())
                        .await
                        .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let result = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": admitted["task_uuid"]})))
                    .await
                    .unwrap();
                assert_eq!(result["verdict"], "pass");
                assert_eq!(result["taskRef"], "crm/t07");
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
                row.orchestration = Some(
                    Orchestration::new(json!({
                        "flowRunId": "00000000-0000-4000-8000-000000000260",
                        "taskRef": "crm/t07"
                    }))
                    .unwrap(),
                );
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
                let result = await_positive_progress(
                    "re-adopted remote completion",
                    client.call(
                        "queue.await_job",
                        Some(json!({"task_uuid": task_uuid.to_string()})),
                    ),
                )
                .await
                .unwrap();
                assert_eq!(result["verdict"], "pass");
                assert_eq!(result["taskRef"], "crm/t07");
                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();

                let calls = calls.lock().unwrap();
                assert_eq!(calls.len(), 2);
                assert!(matches!(calls[0], RemoteExecutorRequest::Probe { .. }));
                match &calls[1] {
                    RemoteExecutorRequest::Adopt { request, .. } => {
                        assert_eq!(request.attempt, 1);
                        assert_eq!(
                            request.identity.task_ref.as_ref().map(TaskRef::as_str),
                            Some("crm/t07")
                        );
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
                let executor = direct_executor(&paths.state_dir)
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
                    .set_read_timeout(Some(POSITIVE_PROGRESS_BACKSTOP))
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

                await_positive_progress("lease tick start", tick_started_rx.recv())
                    .await
                    .expect("lease tick hook must remain open");
                shutdown_tx.send(true).unwrap();
                assert_no_progress_within(
                    "shutdown while an in-flight lease tick is held",
                    Duration::from_millis(50),
                    &mut daemon_task,
                )
                .await;
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
                await_positive_progress("daemon shutdown after lease tick", &mut daemon_task)
                    .await
                    .expect("daemon task must not panic")
                    .expect("daemon shutdown must succeed");
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
                let executor = direct_executor(&paths.state_dir)
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
                let executor = direct_executor(&paths.state_dir)
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
                let late = await_positive_progress(
                    "restarted terminal waiter",
                    restarted_client.call("queue.await_job", Some(json!({"task_uuid": task_uuid}))),
                )
                .await
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
                        error: None,
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
                let executor = direct_executor(&paths.state_dir)
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
                assert_eq!(status["protocolVersion"], 5);
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
                    .is_some_and(|text| text.contains("\"protocolVersion\": 5")));
                let render_json = client
                    .call("query.render", Some(json!({"format": "json"})))
                    .await
                    .unwrap();
                assert_eq!(render_json["protocolVersion"], 5);

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
                // Every job in this fixture is terminal, so the live map is
                // empty (#395).
                assert!(context.read().await.jobs.is_empty());
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
                let executor = direct_executor(&paths.state_dir)
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
                    .enqueue_as_client(Some(json!({
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
    async fn campaign_pool_namespace_crosses_enqueue_and_control_front_doors() {
        let local = LocalSet::new();
        local
            .run_until(async {
                const CAMPAIGN_POOL: &str = "campaign/mecattaf/tally.nix";

                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths,
                    settings(),
                    executor,
                )
                .await
                .unwrap();

                assert!(!daemon
                    .handler
                    .context
                    .read()
                    .await
                    .config
                    .pools
                    .contains_key(CAMPAIGN_POOL));
                let paused = daemon
                    .handler
                    .pause(Some(json!({"pool": CAMPAIGN_POOL})))
                    .await
                    .unwrap();
                assert_eq!(paused["paused"], json!([CAMPAIGN_POOL]));

                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": CAMPAIGN_POOL,
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                assert_eq!(admitted["state"], "paused");
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();

                for rejected_pool in ["campaign/mecattaf", "unconfigured"] {
                    let control_error = daemon
                        .handler
                        .pause(Some(json!({"pool": rejected_pool})))
                        .await
                        .unwrap_err();
                    assert_eq!(control_error.code, WireErrorCode::InvalidParams);
                    assert_eq!(
                        control_error.message,
                        format!("unknown pool {rejected_pool:?}")
                    );

                    let enqueue_error = daemon
                        .handler
                        .enqueue_as_client(Some(json!({
                            "argv": ["true"],
                            "pool": rejected_pool,
                            "adapter": "shell",
                            "source": "manual",
                            "evidence": ["exit:0"]
                        })))
                        .await
                        .unwrap_err();
                    assert_eq!(enqueue_error.code, WireErrorCode::InvalidParams);
                    assert_eq!(
                        enqueue_error.message,
                        format!("unknown pool {rejected_pool:?}")
                    );
                }

                let resumed = daemon
                    .handler
                    .resume(Some(json!({"pool": CAMPAIGN_POOL})))
                    .await
                    .unwrap();
                assert_eq!(resumed["resumed"], json!([CAMPAIGN_POOL]));
                let finished = await_positive_progress(
                    "campaign namespace job completion",
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let terminal = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .lease
                        .engine()
                        .declared_resource_kind(CAMPAIGN_POOL),
                    Some(ResourceKind::Mutex)
                );
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
                let executor = direct_executor(&paths.state_dir)
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
                    .enqueue_as_client(Some(json!({
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
                let executor = direct_executor(&paths.state_dir)
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
                await_positive_progress("supervisor restart events", async {
                    loop {
                        let _ = events_rx.recv().await;
                        if panics.get() >= 2 && peer_runs.get() == 1 {
                            break;
                        }
                    }
                })
                .await;
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
            .set_read_timeout(Some(POSITIVE_PROGRESS_BACKSTOP))
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

    /// #379: startup is charged to `TimeoutStartSec`, so every phase boundary
    /// has to buy more of it. `EXTEND_TIMEOUT_USEC=` restarts the start
    /// timeout from receipt, which turns one budget for the whole of
    /// `Daemon::open` into one budget per phase; `STATUS=` makes a slow start
    /// legible in `systemctl status` instead of silent.
    #[test]
    fn every_startup_phase_extends_the_start_timeout_and_names_itself() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("notify.sock");
        let socket = UnixDatagram::bind(&path).unwrap();
        // This short socket timeout serves the final no-more-datagrams
        // assertion below. The positive datagrams are synchronously queued
        // before any receive, so they do not use elapsed time as progress.
        socket
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let notifier = SystemdNotifier::with_socket(path, None);

        let mut timeline = startup::StartupTimeline::begin(notifier, "prepare");
        timeline.phase("row-migration");
        timeline.phase("unit-facts");
        let report = timeline.finish();

        for phase in ["prepare", "row-migration", "unit-facts"] {
            let mut buffer = [0_u8; 256];
            let read = socket.recv(&mut buffer).unwrap();
            assert_eq!(
                std::str::from_utf8(&buffer[..read]).unwrap(),
                format!("EXTEND_TIMEOUT_USEC=90000000\nSTATUS=starting: {phase}"),
            );
        }
        // Nothing else is sent by the timeline: its extension is per phase.
        // (The unit-facts loop additionally renews on its own time-throttled
        // cadence while it is visiting rows — #428 — but that is driven by
        // row progress, not by these phase boundaries.)
        let mut buffer = [0_u8; 256];
        assert!(socket.recv(&mut buffer).is_err());

        // The report is the durable half. Every phase is named with its own
        // wall-clock, because a total alone is what #379 already had and could
        // not attribute.
        assert!(report.starts_with("startup complete in "), "{report}");
        assert!(report.contains("of a 90s per-phase budget"), "{report}");
        for phase in ["prepare", "row-migration", "unit-facts"] {
            assert!(
                report.contains(&format!(" {phase}=")),
                "{report} omits {phase}"
            );
        }
    }

    /// #428: the unit-facts phase renews the start timeout from inside its
    /// per-row loop, so the 90 s budget bounds progress stalls rather than
    /// the phase's total O(corpus) cost. The throttle is time-based; driving
    /// it with a zero interval makes every visited row renew, which is the
    /// seam that proves the notification actually flows from inside
    /// `collect_local_unit_facts` — remove the in-loop extension and this
    /// observes no datagrams.
    #[tokio::test]
    async fn unit_facts_progress_renews_the_start_timeout_from_inside_the_loop() {
        let temp = tempdir().unwrap();
        let (paths, _) = seed_scale_corpus(temp.path(), 0, 3);
        let durable = crate::recovery::collect_durable_recovery_facts(
            &paths.events_dir(),
            &paths.witness_path(),
        )
        .unwrap();
        let executor = direct_executor(&paths.state_dir)
            .with_systemd_run(temp.path().join("absent-systemd-run"))
            .with_unit_probe(ExitFileProbe);

        let notify_path = temp.path().join("notify.sock");
        let socket = UnixDatagram::bind(&notify_path).unwrap();
        // This short socket timeout serves the final no-more-datagrams
        // assertion below. Collection has finished before these queued
        // positive datagrams are read.
        socket
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let notifier = SystemdNotifier::with_socket(notify_path, None);
        let mut extension =
            startup::UnitFactsExtension::new(notifier, Duration::ZERO, durable.events().len());
        collect_local_unit_facts(&executor, &durable, || extension.row_visited())
            .await
            .unwrap();

        for visited in 1..=3 {
            let mut buffer = [0_u8; 256];
            let read = socket.recv(&mut buffer).unwrap();
            assert_eq!(
                std::str::from_utf8(&buffer[..read]).unwrap(),
                format!("EXTEND_TIMEOUT_USEC=90000000\nSTATUS=starting: unit-facts ({visited}/3 rows)"),
            );
        }
        let mut buffer = [0_u8; 256];
        assert!(
            socket.recv(&mut buffer).is_err(),
            "one renewal per visited row, nothing more"
        );
    }

    /// The shipped in-loop renewal interval, pinned: a small fraction of the
    /// phase budget, so a slow-but-moving unit-facts loop renews several
    /// times per budget and never rides the 90 s deadline.
    #[test]
    fn unit_facts_extend_interval_is_a_small_fraction_of_the_phase_budget() {
        assert_eq!(startup::UNIT_FACTS_EXTEND_INTERVAL, Duration::from_secs(10));
        assert!(startup::UNIT_FACTS_EXTEND_INTERVAL * 3 < startup::STARTUP_PHASE_BUDGET);
    }

    /// The full phase list, pinned through the line `run_loop` actually emits.
    ///
    /// `daemon_open_records_every_startup_phase_in_order` below can only see
    /// what `Daemon::open` returns, so it stops one phase short:
    /// `initial-recovery` is opened inside `run_loop`, which is precisely where
    /// the initial job recovery loop lives — the pre-`READY` work a later lane
    /// is most likely to extend. Deleting that phase left the whole suite green, which made
    /// #379's claim that the list is pinned one level stronger than the code.
    ///
    /// This asserts the artefact instead: the rendered report line, in order,
    /// with the total and the budget it names. `doc/src/operating/recovery.md`
    /// advertises exactly this string to operators.
    #[tokio::test(flavor = "current_thread")]
    async fn the_startup_report_line_names_every_phase_including_the_one_run_loop_opens() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let mut daemon = fs1_daemon(&paths).await;
                let (report_tx, mut report_rx) = mpsc::unbounded_channel();
                daemon.startup_report_hook = Some(report_tx);
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                shutdown_tx.send(true).unwrap();
                daemon.run_until(shutdown_rx).await.unwrap();

                let report = report_rx.try_recv().expect("run_loop reports its startup");
                assert!(
                    report.starts_with("startup complete in "),
                    "unexpected report: {report}"
                );
                assert!(
                    report.contains("of a 90s per-phase budget"),
                    "unexpected report: {report}"
                );
                // Every `name=` token, in the order the line carries them.
                let phases = report
                    .split_whitespace()
                    .filter_map(|token| token.split_once('='))
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>();
                assert_eq!(
                    phases,
                    vec![
                        "prepare",
                        "row-migration",
                        "durable-facts",
                        "gcroots",
                        "unit-facts",
                        "recovery-plan",
                        "storage",
                        "lease-engine",
                        "failure-stderr",
                        "install-jobs",
                        "initial-recovery",
                    ],
                    "the report line is what operators read; a phase that vanishes from it \
                     under-attributes the startup budget silently: {report}"
                );
            })
            .await;
    }

    /// The phase list is a contract, not decoration: it is what a later lane
    /// adding startup work checks its own cost against (#379). Pinning it here
    /// means work added outside a named phase shows up as a failing test
    /// rather than as another silent minute in the journal.
    ///
    /// This covers the eleven phases `Daemon::open` owns and fails with the
    /// offending name visible in the diff; the twelfth is covered by
    /// `the_startup_report_line_names_every_phase_including_the_one_run_loop_opens`.
    #[tokio::test(flavor = "current_thread")]
    async fn daemon_open_records_every_startup_phase_in_order() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                assert_eq!(
                    daemon.startup.as_ref().unwrap().phase_names(),
                    vec![
                        "prepare",
                        "row-migration",
                        "durable-facts",
                        "gcroots",
                        "unit-facts",
                        "recovery-plan",
                        "storage",
                        "lease-engine",
                        "failure-stderr",
                        "install-jobs",
                    ],
                );
            })
            .await;
    }

    /// Read every datagram already queued on `socket`. The socket must carry a
    /// short read timeout.
    ///
    /// Deliberately untimed (#419). This used to stamp `Instant::now()` on each
    /// datagram as it was read, and the keepalive assertion then measured the
    /// gaps between those stamps — which are observation instants, not send
    /// instants. Datagrams queue in the socket buffer, so a collector thread
    /// descheduled for longer than one watchdog period reads a burst of pings
    /// with one large gap in front of it and reddens a daemon that pinged
    /// perfectly. Measured: injecting a single 500 ms sleep into the collector
    /// loop fails both keepalive tests with `a 551ms silence would have missed
    /// a 400ms service watchdog` while the payload list still holds all 19
    /// keepalives the daemon sent.
    fn drain_notifications(socket: &UnixDatagram) -> Vec<String> {
        let mut seen = Vec::new();
        loop {
            let mut buffer = [0_u8; 256];
            match socket.recv(&mut buffer) {
                Ok(read) => seen.push(String::from_utf8_lossy(&buffer[..read]).into_owned()),
                Err(_) => return seen,
            }
        }
    }

    /// The defect: the keepalive used to be a `select!` arm, so it was only
    /// polled when the dispatch loop came back around to poll it. One slow arm
    /// body held the ping past `WatchdogSec` and systemd killed a daemon that
    /// was working fine.
    ///
    /// The stall here is five watchdog periods long. The keepalive must ping
    /// right through it.
    #[tokio::test(flavor = "current_thread")]
    async fn watchdog_keepalive_pings_while_a_dispatch_arm_awaits() {
        let observed = dispatch_stall_notifications(StallShape::Awaiting).await;
        assert_keepalive_held(&observed, WATCHDOG_UNDER_TEST, WATCHDOG_UNDER_TEST * 5);
    }

    /// The stall that matters most is not an `await` at all. `WitnessLedger::
    /// append` and `LifecycleLog::compact_if_over_limit` spend their time in
    /// `flock`, `write_all` and `sync_all`, which hold the daemon's single
    /// runtime thread outright. A keepalive that only survives awaited stalls
    /// would leave that class exactly where it was.
    #[tokio::test(flavor = "current_thread")]
    async fn watchdog_keepalive_pings_while_a_dispatch_arm_blocks_the_runtime_thread() {
        let observed = dispatch_stall_notifications(StallShape::BlockingTheThread).await;
        assert_keepalive_held(&observed, WATCHDOG_UNDER_TEST, WATCHDOG_UNDER_TEST * 5);
    }

    const WATCHDOG_UNDER_TEST: Duration = Duration::from_millis(400);

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum StallShape {
        Awaiting,
        BlockingTheThread,
    }

    /// Run a daemon whose first dispatch-loop arm body is held for five
    /// watchdog periods, and return every notify datagram it sent, timed.
    async fn dispatch_stall_notifications(shape: StallShape) -> Vec<String> {
        let local = LocalSet::new();
        local
            .run_until(async move {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = direct_executor(&paths.state_dir)
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
                // This is an empty-queue polling interval. The collector's
                // positive-progress bound is the shared deadline below.
                notify_socket
                    .set_read_timeout(Some(Duration::from_millis(50)))
                    .unwrap();
                let watchdog = WATCHDOG_UNDER_TEST;
                daemon.notifier = SystemdNotifier::with_socket(notify_path, Some(watchdog));
                let stall = watchdog * 5;
                let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
                let (release_tx, release_rx) = watch::channel(false);
                daemon.dispatch_stall_hook = Some(DispatchStallHook {
                    entered: entered_tx,
                    release: release_rx,
                    blocking: (shape == StallShape::BlockingTheThread).then_some(stall),
                    blocked: Rc::new(Cell::new(false)),
                });

                // The notify socket is read on an OS thread of its own. A
                // blocking read on the runtime thread would be indistinguishable
                // from the stall under test.
                let collector = std::thread::spawn(move || {
                    // The deadline is a liveness backstop, not a measurement
                    // bound (#419). It used to be `stall + watchdog * 8`, which
                    // on a loaded host can expire before the daemon's own
                    // `STOPPING=1` arrives -- and then the assertion that
                    // STOPPING is last fails for no reason but this thread's
                    // scheduling. The loop still ends the instant STOPPING is
                    // seen, so a generous ceiling costs nothing and only bounds
                    // a daemon that never stops at all.
                    let deadline = Instant::now() + POSITIVE_PROGRESS_BACKSTOP;
                    let mut seen = Vec::new();
                    while Instant::now() < deadline {
                        seen.extend(drain_notifications(&notify_socket));
                        if seen.iter().any(|payload| payload == "STOPPING=1") {
                            break;
                        }
                    }
                    seen
                });

                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let entered_at = Instant::now();
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                if shape == StallShape::BlockingTheThread {
                    // The hook publishes immediately before blocking this
                    // single runtime thread. The receiver can only be polled
                    // once that block ends, so observing the publication both
                    // proves entry and measures the completed blocked interval
                    // without guessing that 10 ms was enough time to enter.
                    await_positive_progress("blocking dispatch stall publication", entered_rx.recv())
                        .await
                        .expect("dispatch stall hook must remain open");
                    assert!(
                        entered_at.elapsed() >= stall,
                        "the runtime thread was released after {:?}, so it never held for {stall:?}",
                        entered_at.elapsed()
                    );
                } else {
                    await_positive_progress("dispatch stall-hook entry", entered_rx.recv())
                        .await
                        .expect("the stall hook must remain open");
                    tokio::time::sleep(stall).await;
                }
                release_tx.send(true).unwrap();
                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
                collector.join().unwrap()
            })
            .await
    }

    /// systemd's rule is the only one that matters: the daemon must keep
    /// pinging right through a stalled dispatch loop, from `READY=1` to
    /// `STOPPING=1`.
    ///
    /// Counted, not timed (#419), for the reason [`drain_notifications`]
    /// records: the only clock available to an observer is its own, and its own
    /// is the one that load perturbs. The count is not perturbed — every
    /// datagram the daemon sent is still queued and still read — so this
    /// asserts what the daemon did rather than when this thread noticed.
    ///
    /// `stall` is how long the dispatch arm was held. The keepalive owes one
    /// ping per [`keepalive_cadence`], a quarter period, so a stall of N
    /// service periods is covered by roughly 4N pings; requiring N is a floor
    /// with a factor of four of headroom over correct behaviour and two orders
    /// of magnitude over the defect, which emitted none at all because the
    /// keepalive was a `select!` arm the stalled loop never came back to poll.
    /// What it no longer catches is a keepalive that pings in a burst and then
    /// falls silent inside the stall; production cadence is a fixed timer, so
    /// that shape is not a regression this suite can produce, and a false red
    /// on every loaded host is a certainty.
    fn assert_keepalive_held(observed: &[String], watchdog: Duration, stall: Duration) {
        let payloads = observed.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(
            payloads.first().copied(),
            Some("READY=1\nSTATUS=tally daemon ready"),
            "{payloads:?}"
        );
        assert_eq!(
            payloads.last().copied(),
            Some("STOPPING=1"),
            "no keepalive may follow the daemon's own STOPPING: {payloads:?}"
        );
        let pings = payloads
            .iter()
            .filter(|payload| payload.starts_with("WATCHDOG=1"))
            .count();
        let owed = (stall.as_millis() / watchdog.as_millis()) as usize;
        assert!(
            pings >= owed,
            "{pings} keepalive(s) across a {stall:?} stall cannot cover a {watchdog:?} service \
             watchdog; at least {owed} were owed: {payloads:?}"
        );
    }

    /// A `select!` arm body that never returns is a wedged daemon whichever way
    /// it is stuck. It gets bounded headroom, not immunity: pinging past the
    /// horizon would convert a loud restart into a silent hang.
    #[test]
    fn watchdog_keepalive_falls_silent_when_a_dispatch_arm_never_returns() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("notify.sock");
        let socket = UnixDatagram::bind(&path).unwrap();
        // `drain_notifications` uses this short interval to observe an empty
        // queue, which is the semantic fact asserted by this test.
        socket
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        // Horizon 2 s, notice 400 ms, cadence 50 ms.
        let watchdog = Duration::from_millis(200);
        let notifier = SystemdNotifier::with_socket(path, Some(watchdog));
        let (fatal_tx, _fatal_rx) = mpsc::unbounded_channel();
        let keepalive = notifier
            .keepalive(fatal_tx)
            .expect("a watched service starts a keepalive");
        let progress = keepalive.progress();

        std::thread::sleep(watchdog * 5);
        assert!(
            drain_notifications(&socket)
                .iter()
                .filter(|payload| payload.as_str() == "WATCHDOG=1")
                .count()
                >= 3,
            "a slow dispatch arm gets headroom, not a dead service"
        );
        std::thread::sleep(watchdog * 8);
        let _ = drain_notifications(&socket);
        std::thread::sleep(watchdog * 4);
        assert_eq!(
            drain_notifications(&socket),
            Vec::<String>::new(),
            "a dispatch loop that never comes back must still reach the service watchdog"
        );

        progress.stamp();
        std::thread::sleep(watchdog);
        assert!(
            !drain_notifications(&socket).is_empty(),
            "a loop that comes back must be stood for again"
        );
    }

    #[test]
    fn keepalive_pings_while_overdue_and_withholds_past_the_horizon() {
        let watchdog = Duration::from_secs(30);
        let notice = dispatch_stall_notice(watchdog);
        let horizon = dispatch_stall_horizon(watchdog);
        for (age, expected) in [
            (Duration::ZERO, KeepaliveVerdict::Ping),
            (notice, KeepaliveVerdict::Ping),
            (notice + Duration::from_millis(1), KeepaliveVerdict::PingOverdue),
            (horizon, KeepaliveVerdict::PingOverdue),
            (horizon + Duration::from_millis(1), KeepaliveVerdict::Withhold),
        ] {
            assert_eq!(
                keepalive_verdict(age, notice, horizon),
                expected,
                "age {age:?}"
            );
        }
    }

    /// The daemon's liveness bounds are derived from `WatchdogSec`, which lives
    /// in the nix modules. Pin what those divisors mean at the value both
    /// modules ship, so moving either surface has to move this too.
    #[test]
    fn watchdog_budgets_are_pinned_at_the_shipped_service_period() {
        let watchdog = Duration::from_secs(30);
        assert_eq!(keepalive_cadence(watchdog), Duration::from_millis(7_500));
        assert_eq!(dispatch_stall_notice(watchdog), Duration::from_secs(60));
        assert_eq!(dispatch_stall_horizon(watchdog), Duration::from_secs(300));
    }


    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_24_1_restart_reconstructs_lineage_two_attempts_log_and_proof() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let (paths, parent_uuid, child_uuid, parent_pass, _) =
                    seed_durable_query_fixture(temp.path());
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);

                // Open and drop one daemon before inspecting through the next
                // generation. This exercises lifecycle reload independently of
                // the witness ledger.
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
                assert_eq!(jobs["protocolVersion"], 5);
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
                let executor = direct_executor(&paths.state_dir)
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

    /// Issue #395, the guard the prune's neutrality actually rests on.
    ///
    /// `finish_job` reads the job, then awaits the scrape, the capture and the
    /// accounting **with the context lock dropped**, then re-checks under the
    /// write lock. Before the prune the job was still in the map when it
    /// reached a terminal disposition mid-flight, so `is_some_and` was the
    /// right polarity there; after it, a job retired underneath that window is
    /// *absent*, and reading absence as "still eligible" falls straight through
    /// to `append_context_witness` — a **second canonical witness for one
    /// execution**, on top of the cancelled witness the forced path already
    /// appended.
    ///
    /// That is durable, on the append-only ledger `query run`, `query proof`,
    /// the standup rollup and the attestation chain all sum over, and it is
    /// reachable only under a race — so no ordinary fixture reaches it. Every
    /// other forced-cancel test in this file retires the job *before*
    /// `finish_job` runs and is answered by its **first** lookup, which is why
    /// they cannot see this branch at all.
    ///
    /// This one steps into the window on purpose: hold `finish_job` between its
    /// two phases, force-cancel the job there, let it resume, and count the
    /// canonical witnesses for that exact `(task, attempt, leaseEpoch)`.
    #[tokio::test(flavor = "current_thread")]
    async fn a_job_retired_between_finish_jobs_two_phases_is_witnessed_exactly_once() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let mut daemon = fs1_daemon(&paths).await;
                let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
                let (release_tx, release_rx) = watch::channel(false);
                daemon.finish_job_hook = Some(FinishJobHook {
                    entered: entered_tx,
                    release: release_rx,
                });

                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
                let finished = await_positive_progress(
                    "finish-job race completion",
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap();
                let attempt = finished.attempt;
                let lease_epoch = finished.lease_epoch;

                // Phase one has read the job and dropped the lock. Everything
                // after this point in `finish_job` is the window.
                let finishing = daemon.finish_job(finished);
                tokio::pin!(finishing);
                await_positive_progress("finish-job race-window entry", async {
                    tokio::select! {
                        result = &mut finishing => {
                            panic!("finish_job must stall in its own window, returned {result:?}")
                        }
                        entered = entered_rx.recv() => {
                            entered.expect("finish_job must reach the window");
                        }
                    }
                })
                .await;

                // Retire it inside the window, through the real path that does
                // it: a forced cancel. This is the shape the guard exists for.
                let cancelled = daemon.handler.cancel_one(&task_uuid, true).await.unwrap();
                assert_eq!(cancelled["affected"], 1, "{cancelled}");
                {
                    let context = daemon.handler.context.read().await;
                    assert!(
                        !context
                            .jobs
                            .contains_key(&Uuid::parse_str(&task_uuid).unwrap()),
                        "the forced cancel must have retired the job before finish_job resumes"
                    );
                }

                // Let the second phase run against the map it now finds.
                release_tx.send(true).unwrap();
                // Held, not unwrapped yet: the ledger is the property under
                // test, and a `finish_job` that both double-witnesses *and*
                // fails afterwards must be reported as the double-witness it
                // is, not as whatever it tripped over next.
                let outcome = finishing.await;
                daemon.handler.drain_post_ack_tasks().await;

                let (report, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(report.ok);
                let for_execution = records
                    .iter()
                    .filter(|record| {
                        record.task_uuid.as_deref() == Some(task_uuid.as_str())
                            && record.attempt == attempt
                            && record.lease_epoch == lease_epoch
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    for_execution.len(),
                    1,
                    "one execution must leave exactly one canonical witness; got {:?}",
                    for_execution
                        .iter()
                        .map(|record| record.verdict)
                        .collect::<Vec<_>>()
                );
                // And it is the forced one, not a second verdict invented by
                // the execution that lost the race.
                assert_eq!(for_execution[0].verdict, Verdict::Cancelled);
                // The execution that lost the race is a quiet no-op, not an
                // error: nothing went wrong, it simply has nothing left to do.
                outcome.expect("a job retired mid-flight is a no-op for finish_job");
            })
            .await;
    }

    /// Issue #395: `context.jobs` is the daemon's *live* set, and stays one.
    ///
    /// It used to keep every job the daemon had admitted since it started, for
    /// the daemon's whole lifetime, with no `remove`, `retain` or `clear`
    /// anywhere in the tree. That is hot-path cost and not only resident
    /// memory: the compaction live set, the dedup sweep and the guardrail
    /// child count are all rebuilt over this map on the admission path, and
    /// every one of them discards `Completed` entries on the way past.
    ///
    /// Three things have to hold together, so all three are pinned here: a job
    /// that reaches a terminal disposition is gone from the map, the map does
    /// not grow across repeated terminal work, and the verb that can still be
    /// asked about a finished job still answers it. (The other such verb,
    /// `--resume-from`, is covered end to end by
    /// `public_continuation_uses_the_scraped_session_without_manual_captures`.)
    #[tokio::test(flavor = "current_thread")]
    async fn a_job_that_reaches_a_terminal_state_leaves_the_live_job_map() {
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

                let mut first = None;
                for index in 0..4 {
                    let created = client
                        .call(
                            "queue.enqueue",
                            Some(fs1_full_payload(
                                &format!("prune-395-{index}"),
                                &["true"],
                                ["exit:0".to_owned()],
                            )),
                        )
                        .await
                        .unwrap();
                    assert_eq!(created["disposition"], "created");
                    let terminal = fs1_wait(&client, &created).await;
                    assert_eq!(terminal["verdict"], "pass");

                    let task_uuid =
                        Uuid::parse_str(created["task_uuid"].as_str().unwrap()).unwrap();
                    let context = context.read().await;
                    assert!(
                        !context.jobs.contains_key(&task_uuid),
                        "job {task_uuid} reached a terminal state and must not remain in \
                         context.jobs"
                    );
                    // Not just "this one left": the map does not accumulate.
                    // A count that grew by one per round would be the same
                    // leak wearing a passing assertion.
                    assert!(
                        context.jobs.is_empty(),
                        "context.jobs grew to {} after {} terminal jobs",
                        context.jobs.len(),
                        index + 1
                    );
                    // The invariant every consumer of this map already relied
                    // on, now true by construction rather than by filtering:
                    // the live set the compaction builds (`jobs.values()`
                    // filtered to `state != Completed`) sees exactly the same
                    // entries it would have seen before.
                    assert!(context.jobs.values().all(|job| job.state
                        != JobState::Completed));
                    // And nothing the daemon can still be asked about was
                    // dropped with it.
                    assert!(context.rows.contains_key(&task_uuid));
                    assert_eq!(context.query_rows[&task_uuid].status, RowStatus::Completed);
                    drop(context);
                    first.get_or_insert(created["task_uuid"].clone());
                }

                // A retired job still answers `cancel`, from the row and query
                // fact that outlived it, rather than 404ing because the live
                // map forgot it.
                let already = client
                    .call(
                        "queue.cancel",
                        Some(json!({"task_uuid": first.unwrap(), "force": true})),
                    )
                    .await
                    .unwrap();
                assert_eq!(already["already_terminal"], true);
                assert_eq!(already["affected"], 0);
                assert_eq!(already["was"], "completed");

                // An id the daemon has never admitted is still not found: the
                // fallback answers for jobs it retired, not for anything.
                let unknown = client
                    .call(
                        "queue.cancel",
                        Some(json!({"task_uuid": Uuid::new_v4().to_string(), "force": true})),
                    )
                    .await
                    .unwrap_err();
                assert!(
                    matches!(unknown, WireIoError::Rpc(WireErrorCode::NotFound, _, _)),
                    "{unknown:?}"
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
                // Both dispositions are terminal, so neither is in the live map
                // (#395): the first was retired when it completed, the reused
                // one was terminal on arrival and never joined.
                assert!(context.read().await.jobs.is_empty());
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

    /// A retry must not carry the previous attempt's usage forward.
    ///
    /// Today the cloned row is usage-free by accident: completion writes the
    /// record into `context.jobs` and `context.query_details`, never into
    /// `context.rows`. This test forces the premise that accident depends on —
    /// it plants a record on the durable row, which is exactly what a
    /// natural-looking "make completion write rows back too" change would
    /// produce — and asserts the retry still renders no usage under the new
    /// attempt. Without `row.usage = None` in the retry path, attempt N-1's
    /// tokens would surface under attempt N with provider-capture authority.
    #[tokio::test(flavor = "current_thread")]
    async fn retrying_a_job_does_not_carry_the_previous_attempts_usage_forward() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let mut daemon = fs1_daemon(&paths).await;
                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["false"],
                        "pool": "slot",
                        "priority": "high",
                        "adapter": "shell",
                        "source": "manual",
                        "dedupKey": "retry-usage",
                        "evidence": ["exit:0"],
                    })))
                    .await
                    .unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
                let finished = await_positive_progress(
                    "retry source completion",
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let terminal = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": task_uuid.clone()})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "failed");
                assert_eq!(terminal["attempt"], 1);

                // Plant attempt 1's usage on the durable row and on its query
                // detail, the state a row-writing completion path would leave.
                let uuid = Uuid::parse_str(&task_uuid).unwrap();
                let planted = crate::usage::observe(
                    &AdapterConfig {
                        argv: vec!["agent".to_owned()],
                        scrape: BTreeMap::from([(
                            "usage".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPath,
                                pattern: "$..usage".to_owned(),
                                counter_scope: None,
                                fields: Default::default(),
                            },
                        )]),
                        ..Default::default()
                    },
                    &ScrapeResult {
                        captures: BTreeMap::from([(
                            "usage".to_owned(),
                            json!({"input_tokens": 4096, "output_tokens": 512}),
                        )]),
                    },
                );
                assert!(!planted.is_absent(), "the planted record is a measurement");
                {
                    let mut context = daemon.handler.context.write().await;
                    context.rows.get_mut(&uuid).unwrap().usage = Some(planted.clone());
                    context.query_details.get_mut(&uuid).unwrap().usage = Some(planted);
                }
                let before = daemon
                    .handler
                    .query("query.job", Some(json!({"id": task_uuid.clone()})))
                    .await
                    .unwrap();
                assert_eq!(before["job"]["currentAttempt"], 1);
                assert_eq!(
                    before["job"]["usage"]["value"]["state"], "reported",
                    "the planted record is visible for the attempt that produced it"
                );

                let retried = daemon
                    .handler
                    .retry_job(Some(json!({"task_uuid": task_uuid.clone()})))
                    .await
                    .unwrap();
                assert_eq!(retried["attempt"], 2);
                {
                    let context = daemon.handler.context.read().await;
                    let row = context.rows.get(&uuid).unwrap();
                    assert_eq!(
                        row.usage, None,
                        "the retried row must carry no usage of its own"
                    );
                    assert_eq!(row.usage_accounting, None);
                    assert_eq!(
                        row.usage_predecessor, None,
                        "a fresh retry is fresh even though its attempt is greater than one"
                    );
                    assert_eq!(
                        context.jobs.get(&uuid).unwrap().invocation.usage_accounting,
                        UsageAccountingMode::Fresh,
                        "freshness is bound to the rendered invocation, not inferred from attempt"
                    );
                }
                let after = daemon
                    .handler
                    .query("query.job", Some(json!({"id": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(after["job"]["currentAttempt"], 2);
                assert_eq!(
                    after["job"].get("usage"),
                    None,
                    "a retried attempt must not report the previous attempt's tokens"
                );
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
                        "test -n \"$TALLY_BRIEF\" && test -f \"$TALLY_BRIEF\" && test \"$TALLY_TASK_REF\" = crm/t07",
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
                    "taskRef": "crm/t07",
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
                assert_eq!(created["taskRef"], "crm/t07");
                let task_uuid = Uuid::parse_str(created["task_uuid"].as_str().unwrap()).unwrap();
                assert_eq!(task_uuid.get_version_num(), 7);
                let brief_hash = created["payloadHash"]
                    .as_str()
                    .expect("full submission returns payloadHash")
                    .to_owned();
                let terminal = fs1_wait(&client, &created).await;
                assert_eq!(terminal["verdict"], "pass");
                assert_eq!(terminal["taskRef"], "crm/t07");
                assert!(paths
                    .state_dir
                    .join("capture")
                    .join(format!("{task_uuid}.t07.out"))
                    .is_file());

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
                assert_eq!(
                    witness[0].orchestration.as_ref().unwrap().as_value()["taskRef"],
                    "crm/t07"
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
                assert_eq!(grouped["items"][0]["taskRef"], "crm/t07");
                assert_eq!(
                    grouped["items"][0]["unit"],
                    format!("tally-job-crm-t07-{task_uuid}.service")
                );
                assert_eq!(
                    grouped["items"][0]["argv"],
                    json!([
                        "sh",
                        "-c",
                        "test -n \"$TALLY_BRIEF\" && test -f \"$TALLY_BRIEF\" && test \"$TALLY_TASK_REF\" = crm/t07"
                    ])
                );
                let compact_lifecycle = await_positive_progress(
                    "journal+witness lifecycle publication",
                    async {
                        loop {
                            let lifecycle = client
                                .call(
                                    "query.log",
                                    Some(json!({
                                        "task": task_uuid.to_string(),
                                        "limit": 100,
                                        "provenance": false,
                                    })),
                                )
                                .await
                                .unwrap();
                            if lifecycle["items"].as_array().unwrap().iter().any(|event| {
                                event["origin"] == "journal+witness"
                                    && event["terminalVerdict"] == "pass"
                            }) {
                                break lifecycle;
                            }
                            tokio::task::yield_now().await;
                        }
                    },
                )
                .await;
                let lifecycle = client
                    .call(
                        "query.log",
                        Some(json!({"task": task_uuid.to_string(), "limit": 100})),
                    )
                    .await
                    .unwrap();
                assert!(lifecycle["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|event| event["taskRef"] == "crm/t07"));
                assert!(compact_lifecycle["items"].as_array().unwrap().len()
                    < lifecycle["items"].as_array().unwrap().len());
                assert!(!compact_lifecycle["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| matches!(
                        event["event"].as_str(),
                        Some("evidence_pass" | "evidence_fail")
                    )));
                assert!(compact_lifecycle["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| {
                        event["origin"] == "journal+witness"
                            && event["terminalVerdict"] == "pass"
                    }));
                let run = client
                    .call("query.run", Some(json!({"id": flow_run_id.clone()})))
                    .await
                    .unwrap();
                assert_eq!(run["flowRunId"], flow_run_id);
                assert_eq!(run["flowName"], "brief-round-trip");
                // A finished non-spec-build run has no reconciled task table,
                // so its state comes from the node verdicts rather than the
                // task counts.
                assert_eq!(run["state"], "complete");
                assert!(run.get("tasks").is_none());
                assert_eq!(run["items"], json!([{"taskUuid": task_uuid}]));
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
                assert_eq!(reused["taskRef"], "crm/t07");

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
                        assert_eq!(data["existingTaskRef"], "crm/t07");
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
                let executor = direct_executor(&paths.state_dir)
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
                        resource: Some(ResourceKind::BuildSlot),
                        predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                        ..PoolConfig::default()
                    },
                );
                let executor = direct_executor(&paths.state_dir)
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
                    .enqueue_as_client(Some(json!({
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
                    .enqueue_as_client(Some(child_payload("fs2-child-1")))
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
                    .enqueue_as_client(Some(child_payload("fs2-child-at-cap")))
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
                    .enqueue_as_client(Some(json!({
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
                    .enqueue_as_client(Some(child_payload("fs2-child-2")))
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

                let executor = direct_executor(&paths.state_dir)
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

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "capacity envelope soak; run explicitly with --ignored"]
    async fn capacity_envelope_soak_bounds_startup_queries_and_change_log() {
        const ROWS: usize = 20_000;
        const LIFECYCLE_EVENTS: usize = 50_000;
        const CHANGE_APPENDS: usize = 12_288;

        fn rss_kib() -> u64 {
            std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:"))
                .unwrap()
                .trim()
                .trim_end_matches(" kB")
                .trim()
                .parse()
                .unwrap()
        }

        fn thread_rchar() -> u64 {
            std::fs::read_to_string("/proc/thread-self/io")
                .unwrap()
                .lines()
                .find_map(|line| line.strip_prefix("rchar: "))
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        }

        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                prepare_paths(&paths).unwrap();
                for _ in 0..3 {
                    bump_epoch(&paths.state_dir).unwrap();
                }

                // 20k durable rows; 10k with three witness attempts and 10k
                // with two: 50k witness records total, all terminal.
                let mut ledger = WitnessLedger::open(paths.witness_path()).unwrap();
                let mut history = LifecycleStore::open(&paths.data_dir).unwrap();
                let mut timestamp_us = 1_786_000_000_000_000_u64;
                let mut lifecycle_events = 0_usize;
                for index in 0..ROWS {
                    let row = durable_row(Uuid::new_v4(), &format!("soak-{index}"), 1);
                    write_enqueue_event_atomic(
                        &paths.events_dir(),
                        &DurableEnqueueEvent::new(row.clone()).unwrap(),
                    )
                    .unwrap();
                    let attempts: &[(Verdict, i32)] = if index < ROWS / 2 {
                        &[
                            (Verdict::Preempted, 1),
                            (Verdict::Preempted, 1),
                            (Verdict::Pass, 0),
                        ]
                    } else {
                        &[(Verdict::Preempted, 1), (Verdict::Pass, 0)]
                    };
                    for (attempt_index, (verdict, exit_code)) in attempts.iter().enumerate() {
                        let attempt = attempt_index as u32 + 1;
                        append_fixture_witness(
                            &mut ledger,
                            &row,
                            "2026-07-28T10:00:00.000Z",
                            *verdict,
                            *exit_code,
                            attempt,
                            u64::from(attempt),
                        );
                    }
                    if lifecycle_events < LIFECYCLE_EVENTS {
                        for event in [TallyEvent::Enqueued, TallyEvent::Started] {
                            append_history_event(&mut history, &row, event, 1, 1, timestamp_us);
                            timestamp_us += 1;
                            lifecycle_events += 1;
                        }
                        if index % 2 == 0 {
                            append_history_event(
                                &mut history,
                                &row,
                                TallyEvent::Completed,
                                1,
                                1,
                                timestamp_us,
                            );
                            timestamp_us += 1;
                            lifecycle_events += 1;
                        }
                    }
                }
                drop(ledger);
                drop(history);

                // Startup over the populated state performs at most two full
                // witness verifications.
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let before_open =
                    crate::witness::FULL_VERIFICATION_PASSES.with(std::cell::Cell::get);
                let opened_at = Instant::now();
                let daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let open_elapsed = opened_at.elapsed();
                let full_passes = crate::witness::FULL_VERIFICATION_PASSES
                    .with(std::cell::Cell::get)
                    - before_open;
                assert!(
                    full_passes <= 2,
                    "startup performed {full_passes} full witness verifications"
                );

                // First page: no additional full verification, bounded item
                // count, and its latency is reported for the record.
                let before_query =
                    crate::witness::FULL_VERIFICATION_PASSES.with(std::cell::Cell::get);
                let first_page_at = Instant::now();
                let first = daemon
                    .handler
                    .query("query.jobs", Some(json!({"limit": 100})))
                    .await
                    .unwrap();
                let first_page_elapsed = first_page_at.elapsed();
                assert_eq!(
                    crate::witness::FULL_VERIFICATION_PASSES.with(std::cell::Cell::get)
                        - before_query,
                    0,
                    "first page re-verified the whole ledger"
                );
                assert!(first["items"].as_array().unwrap().len() <= 100);
                let cursor = first["nextCursor"].as_str().unwrap().to_owned();

                // Continuation: zero witness verification passes and near-zero
                // read IO on this thread.
                let before_continuation =
                    crate::witness::FULL_VERIFICATION_PASSES.with(std::cell::Cell::get);
                let rchar_before = thread_rchar();
                let second = daemon
                    .handler
                    .query("query.jobs", Some(json!({"limit": 100, "cursor": cursor})))
                    .await
                    .unwrap();
                let continuation_rchar = thread_rchar() - rchar_before;
                assert_eq!(
                    crate::witness::FULL_VERIFICATION_PASSES.with(std::cell::Cell::get)
                        - before_continuation,
                    0,
                    "continuation verified the ledger"
                );
                assert!(!second["items"].as_array().unwrap().is_empty());
                assert!(
                    continuation_rchar < 256 * 1024,
                    "continuation read {continuation_rchar} bytes"
                );

                // Steady-state queries hold RSS within a bounded envelope.
                let rss_before = rss_kib();
                for _ in 0..25 {
                    daemon
                        .handler
                        .query("query.jobs", Some(json!({"limit": 100})))
                        .await
                        .unwrap();
                }
                let rss_delta_kib = rss_kib().saturating_sub(rss_before);
                assert!(
                    rss_delta_kib < 512 * 1024,
                    "25 queries grew RSS by {rss_delta_kib} KiB"
                );

                eprintln!(
                    "soak: open={open_elapsed:?} first-page={first_page_elapsed:?} \
                     continuation-rchar={continuation_rchar} rss-delta={rss_delta_kib}KiB"
                );
                drop(daemon);

                // Change log: per-event durable cost is O(record), observable
                // as at most appends/capacity + 1 whole-file rewrites and a
                // durable file bounded by twice the retention window.
                use std::os::unix::fs::MetadataExt;
                let change_dir = temp.path().join("changes-soak");
                let mut store = ChangeStore::open(&change_dir).unwrap();
                let change_path = change_dir.join(crate::watch::CHANGE_FILE);
                let mut inode = std::fs::metadata(&change_path).unwrap().ino();
                let mut rewrites = 0_usize;
                let mut max_line = 0_u64;
                for index in 0..CHANGE_APPENDS {
                    store
                        .append_now(
                            ChangeKind::Lifecycle,
                            json!({"index": index, "payload": "x".repeat(160)}),
                        )
                        .unwrap();
                    let metadata = std::fs::metadata(&change_path).unwrap();
                    if metadata.ino() != inode {
                        rewrites += 1;
                        inode = metadata.ino();
                    }
                    max_line = max_line.max(metadata.len());
                }
                assert!(
                    rewrites <= CHANGE_APPENDS / crate::watch::CHANGE_RETENTION_RECORDS + 1,
                    "{rewrites} whole-file rewrites for {CHANGE_APPENDS} appends"
                );
                let final_len = std::fs::metadata(&change_path).unwrap().len();
                assert!(
                    final_len
                        <= 2 * (crate::watch::CHANGE_RETENTION_RECORDS as u64) * 512,
                    "durable change log holds {final_len} bytes"
                );
                eprintln!("soak: change rewrites={rewrites} final-bytes={final_len}");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn startup_performs_at_most_two_full_witness_verifications() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let (paths, _, _, _, _) = seed_durable_query_fixture(temp.path());
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let before = crate::witness::FULL_VERIFICATION_PASSES.with(std::cell::Cell::get);
                let daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let after = crate::witness::FULL_VERIFICATION_PASSES.with(std::cell::Cell::get);
                assert!(
                    after - before <= 2,
                    "startup performed {} full witness verifications",
                    after - before
                );

                // Queries reuse the startup-verified view: a fresh envelope
                // performs no additional full pass.
                let before_query =
                    crate::witness::FULL_VERIFICATION_PASSES.with(std::cell::Cell::get);
                daemon
                    .handler
                    .query("query.jobs", Some(json!({"limit": 10})))
                    .await
                    .unwrap();
                let after_query =
                    crate::witness::FULL_VERIFICATION_PASSES.with(std::cell::Cell::get);
                assert_eq!(
                    after_query - before_query,
                    0,
                    "query re-verified the whole ledger"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn continuation_pages_skip_witness_reads_and_stay_frozen() {
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
                for _ in 0..3 {
                    daemon
                        .handler
                        .enqueue_as_client(Some(json!({
                            "argv": ["true"],
                            "pool": "slot",
                            "priority": "high",
                            "adapter": "shell",
                            "source": "manual",
                            "evidence": ["exit:0"]
                        })))
                        .await
                        .unwrap();
                }
                let first = daemon
                    .handler
                    .query("query.jobs", Some(json!({"limit": 1})))
                    .await
                    .unwrap();
                assert_eq!(first["items"].as_array().unwrap().len(), 1);
                let cursor = first["nextCursor"].as_str().unwrap().to_owned();
                let reference = daemon
                    .handler
                    .query("query.jobs", Some(json!({"limit": 1, "cursor": cursor})))
                    .await
                    .unwrap();

                // Corrupt the witness ledger: a fresh envelope must refuse...
                fs::write(paths.witness_path(), b"garbage\n").unwrap();
                assert!(daemon
                    .handler
                    .query("query.jobs", Some(json!({"limit": 1})))
                    .await
                    .is_err());

                // ...while continuation pages serve the frozen snapshot with
                // zero witness reads, byte-identical to the pre-mutation page.
                let replayed = daemon
                    .handler
                    .query("query.jobs", Some(json!({"limit": 1, "cursor": cursor})))
                    .await
                    .unwrap();
                assert_eq!(replayed, reference);
                let final_cursor = replayed["nextCursor"].as_str().unwrap().to_owned();
                let last = daemon
                    .handler
                    .query(
                        "query.jobs",
                        Some(json!({"limit": 1, "cursor": final_cursor})),
                    )
                    .await
                    .unwrap();
                assert_eq!(last["items"].as_array().unwrap().len(), 1);
            })
            .await;
    }

    fn monitor_node_payload(flow_run_id: &str, ordinal: usize) -> Value {
        let mut payload = fs1_full_payload(
            &format!("flow:{flow_run_id}:{ordinal}"),
            &["true"],
            ["exit:0".to_owned()],
        );
        payload["source"] = json!("orchestrator");
        payload["orchestration"] = json!({
            "flowName": "monitor-contract",
            "flowRunId": flow_run_id,
            "scriptHash": "sha256-monitor-contract",
            "nodeOrdinal": ordinal,
            "nodeLabel": format!("node-{ordinal}"),
            "maxNodes": 8,
            "selection": {"selector": "pooled-fast", "members": ["worker-a"]},
        });
        payload
    }

    /// #247/#316: a monitor watching a live flow run must be able to tell "no
    /// new events" from "you are looking at a stale or capped page". The
    /// reported symptom was a `query log --flow-run` window that stayed frozen
    /// at the first node with `nextCursor: null` while the run advanced. Every
    /// poll here must surface the events the run just produced.
    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_316_repeated_polls_surface_new_lifecycle_events_on_a_live_run() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                // Paused: admission still writes lifecycle history, so the
                // stream advances deterministically without racing execution.
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();
                let flow_run_id = Uuid::new_v4().to_string();

                let mut seen = 0_usize;
                let mut positions = Vec::new();
                for ordinal in 0..6 {
                    daemon
                        .handler
                        .enqueue_as_client(Some(monitor_node_payload(&flow_run_id, ordinal)))
                        .await
                        .unwrap();
                    let window = daemon
                        .handler
                        .query(
                            "query.log",
                            Some(json!({"flowRun": flow_run_id, "limit": 1000})),
                        )
                        .await
                        .unwrap();
                    let items = window["items"].as_array().unwrap().len();
                    assert!(
                        items > seen,
                        "poll after node {ordinal} returned the same {items}-item window while \
                         the run advanced"
                    );
                    seen = items;
                    assert_eq!(window["truncated"], false, "the window was capped silently");
                    positions.push(window["position"].as_str().unwrap().to_owned());
                }
                // The durable position is monotone across the run: a monitor
                // holding it can tell motion from silence.
                let mut sorted = positions.clone();
                sorted.sort();
                sorted.dedup();
                assert_eq!(sorted, positions, "positions did not advance monotonically");

                // The #247 symptom itself, pinned. The lifecycle window is
                // ordered oldest-first, so page one is *permanently* stale by
                // construction: a reader who only ever sees page one watches
                // an advancing run without observing a single new event. The
                // one signal that this is not the whole window is `truncated`
                // / `nextCursor` -- which is why the human path now follows
                // the cursor to the end instead of stopping here.
                let first_page = |ordinal: usize| {
                    let handler = &daemon.handler;
                    let flow_run_id = flow_run_id.clone();
                    async move {
                        let page = handler
                            .query("query.log", Some(json!({"flowRun": flow_run_id, "limit": 2})))
                            .await
                            .unwrap();
                        assert_eq!(
                            page["truncated"], true,
                            "poll {ordinal} hid its truncation"
                        );
                        page["items"].clone()
                    }
                };
                let stale = first_page(0).await;
                daemon
                    .handler
                    .enqueue_as_client(Some(monitor_node_payload(&flow_run_id, 6)))
                    .await
                    .unwrap();
                assert_eq!(
                    first_page(1).await,
                    stale,
                    "page one is expected to be frozen; if it moved, the window is not \
                     oldest-first and the #247 diagnosis needs revisiting"
                );
                let whole = daemon
                    .handler
                    .query(
                        "query.log",
                        Some(json!({"flowRun": flow_run_id, "limit": 1000})),
                    )
                    .await
                    .unwrap();
                assert!(whole["items"].as_array().unwrap().len() > seen);
                assert_eq!(whole["truncated"], false);
            })
            .await;
    }

    /// The wave-3 W-316 reproduction, now run against the fix (#380).
    ///
    /// It used to assert the defect: a second run submitting an identical node
    /// while the first was still in flight is told `attached`, is handed the
    /// task UUID, and could never see that task in its own window — **same
    /// items, `nextCursor: null`, ground truth advancing**, with no truncation
    /// involved. The mechanism was that membership was recomputed per call by
    /// scanning durable row details and witness records for an orchestration
    /// capsule naming that `flowRunId` (`query_v2::flow_run_tasks`), a
    /// lifecycle event whose task was not in that set was dropped outright
    /// (`lifecycle_matches`), and an `attached` admission writes no row
    /// (`enqueue::full_live_disposition` returns before any
    /// `query_details.insert`) while the canonical payload hash excludes the
    /// orchestration capsule (`wire::canonical_payload`).
    ///
    /// Everything above the fix line still holds — the admission is still
    /// `attached`, the row still belongs to the first run, the capsule is still
    /// out of the payload hash. What changed is that the admission now also
    /// writes durable membership, so the run that was handed the task UUID
    /// resolves it back to itself. The assertions below are the old ones,
    /// inverted.
    #[tokio::test(flavor = "current_thread")]
    async fn repro_316_an_attached_node_is_visible_to_the_run_that_submitted_it() {
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
                let first_run = Uuid::new_v4().to_string();
                let second_run = Uuid::new_v4().to_string();

                // The first run admits the node: created, and visible to it.
                let created = daemon
                    .handler
                    .enqueue_as_client(Some(monitor_node_payload(&first_run, 0)))
                    .await
                    .unwrap();
                assert_eq!(created["disposition"], "created");
                let shared_task = created["task_uuid"].as_str().unwrap().to_owned();

                // A re-triggered run submits the identical node while the
                // first one is still in flight. Only the capsule differs, and
                // the capsule is not part of the payload hash.
                let mut resubmitted = monitor_node_payload(&first_run, 0);
                resubmitted["orchestration"]["flowRunId"] = json!(second_run);
                let attached = daemon
                    .handler
                    .enqueue_as_client(Some(resubmitted))
                    .await
                    .unwrap();
                assert_eq!(
                    attached["disposition"], "attached",
                    "the seam under test needs an attached admission: {attached}"
                );
                assert_eq!(
                    attached["task_uuid"].as_str(),
                    Some(shared_task.as_str()),
                    "the second run was handed the very task it cannot see"
                );

                let poll = |flow_run: String| {
                    let handler = &daemon.handler;
                    async move {
                        handler
                            .query(
                                "query.log",
                                Some(json!({"flowRun": flow_run, "limit": 1000})),
                            )
                            .await
                            .unwrap()
                    }
                };

                // The inverted symptom, asserted field by field. The second run
                // sees the node it was handed, in its own window, immediately.
                let before = poll(second_run.clone()).await;
                assert!(
                    !before["items"].as_array().unwrap().is_empty(),
                    "the attaching run must see the node it was handed: {before}"
                );
                assert!(before["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|item| item["taskUuid"] == json!(shared_task)));
                assert_eq!(
                    before["flowRunTasks"], 1,
                    "membership is durable at admission, not scanned off a row \
                     that belongs to another run: {before}"
                );
                assert!(before["nextCursor"].is_null());
                assert_eq!(before["truncated"], false);

                // Ground truth advances: the shared task keeps emitting, and
                // the first run's window grows to prove the events are real
                // and durable rather than absent.
                let first_before = poll(first_run.clone()).await["items"]
                    .as_array()
                    .unwrap()
                    .len();
                for event in [TallyEvent::Started, TallyEvent::Heartbeat] {
                    let mut emit = crate::journal::EmitEvent::enqueued(
                        shared_task.clone(),
                        Priority::High,
                        EnqueueSource::Orchestrator,
                    );
                    emit.event = event;
                    emit.agent = Some("shell".to_owned());
                    emit.attempt = Some(1);
                    emit.lease_epoch = Some(1);
                    emit.job_id = Some(shared_task.clone());
                    emit.unit = Some(format!("tally-job-{shared_task}.service"));
                    daemon
                        .handler
                        .history
                        .borrow_mut()
                        .append_now(emit.into_fields().unwrap())
                        .unwrap();
                }
                let first_after = poll(first_run.clone()).await;
                assert_eq!(
                    first_after["items"].as_array().unwrap().len(),
                    first_before + 2,
                    "the events must be durable and visible to the run that owns the row"
                );

                // ...and the second run's window advances by exactly the same
                // two events, because it is looking at the same real work. The
                // frozen window is gone: this is what #247 should have shown.
                let after = poll(second_run.clone()).await;
                assert_eq!(
                    after["items"].as_array().unwrap().len(),
                    before["items"].as_array().unwrap().len() + 2,
                    "the attaching run's window must advance with ground truth: {after}"
                );
                assert!(after["nextCursor"].is_null());
                assert_eq!(after["truncated"], false);

                // The position still advances, because it is the head of the
                // whole lifecycle stream rather than of this filter. That is
                // also LOW-3 reproduced: the docs used to state the proof of
                // quiet as "empty items AND the same position", a conjunction
                // that cannot hold on any daemon with concurrent work.
                assert_ne!(
                    after["position"], before["position"],
                    "the stream head must move even when the filtered window does not"
                );

                // Both runs resolve to the one task they both hold. The count
                // no longer distinguishes "attached-only run" from "quiet run",
                // because there is no longer such a thing as an attached-only
                // run: attaching joins.
                assert_eq!(after["flowRunTasks"], 1);
                assert_eq!(poll(first_run.clone()).await["flowRunTasks"], 1);

                // `query jobs --flow-run` resolves the same membership, so the
                // attaching run can see its node as a job and not only as a
                // stream of events.
                let jobs = daemon
                    .handler
                    .query(
                        "query.jobs",
                        Some(json!({"flowRun": second_run.clone(), "limit": 1000})),
                    )
                    .await
                    .unwrap();
                assert_eq!(jobs["flowRunTasks"], 1, "{jobs}");
                assert_eq!(
                    jobs["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|item| item["anchor"].as_str().unwrap())
                        .collect::<Vec<_>>(),
                    vec![shared_task.as_str()],
                    "{jobs}"
                );

                // `query run` inherits the same membership: the attaching run
                // is a real run with one node rather than an unknown job.
                let run_view = daemon
                    .handler
                    .query("query.run", Some(json!({"id": second_run.clone()})))
                    .await
                    .unwrap();
                assert_eq!(
                    run_view["currentNodes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|node| node["taskUuid"].as_str().unwrap())
                        .collect::<Vec<_>>(),
                    vec![shared_task.as_str()],
                    "{run_view}"
                );

                // The durable fact behind all of it, on disk, named.
                let ledger = std::fs::read_to_string(
                    paths.data_dir.join(crate::flow_membership::FLOW_MEMBERSHIP_FILE),
                )
                .unwrap();
                assert!(
                    ledger.contains(&format!(r#""flowRunId":"{second_run}""#))
                        && ledger.contains(r#""disposition":"attached""#),
                    "the attached admission must have written durable membership: {ledger}"
                );

                // An unfiltered query has no membership to report, so the
                // field is absent rather than a misleading zero.
                let unfiltered = daemon
                    .handler
                    .query("query.log", Some(json!({"limit": 1000})))
                    .await
                    .unwrap();
                assert!(unfiltered.get("flowRunTasks").is_none());
            })
            .await;
    }

    /// One flow node payload, submitted under a named run and ordinal.
    fn flow_node_payload(
        flow_run_id: &str,
        ordinal: u64,
        dedup_key: &str,
        argv: &[&str],
        evidence: impl IntoIterator<Item = String>,
    ) -> Value {
        let mut payload = fs1_full_payload(dedup_key, argv, evidence);
        payload["source"] = json!("orchestrator");
        payload["orchestration"] = json!({
            "flowName": "membership-contract",
            "flowRunId": flow_run_id,
            "scriptHash": "sha256-membership-contract",
            "nodeOrdinal": ordinal,
            "nodeLabel": format!("node-{ordinal}"),
            "maxNodes": 8,
        });
        payload
    }

    fn membership_ledger(paths: &DaemonPaths) -> Vec<Value> {
        let path = paths
            .data_dir
            .join(crate::flow_membership::FLOW_MEMBERSHIP_FILE);
        match std::fs::read_to_string(path) {
            Ok(text) => text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).unwrap())
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("membership ledger unreadable: {error}"),
        }
    }

    /// #380, the acceptance bullet in full: an admission under a `flowRunId`
    /// makes the run's membership in its outcome durable, for **every**
    /// disposition, including the three that write no row of their own.
    ///
    /// The corpus is built so its truth is known independently of anything the
    /// query surface computes: each phase-one node's task UUID comes back in
    /// its own admission response, and those UUIDs — not a count, and not a
    /// diff against the old behaviour — are what the second run's window is
    /// checked against.
    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_380_every_disposition_binds_the_run_to_the_task_it_was_handed() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let first_run = Uuid::new_v4().to_string();
                let second_run = Uuid::new_v4().to_string();

                // Phase one, run A. Two nodes run to a real terminal verdict --
                // one passing, one failing -- because `reused` and `terminal`
                // are decided by the governing witness, not by the row. A test
                // that admitted and completed a node in the same breath would
                // not reproduce the window under test at all.
                let pass_payload = flow_node_payload(
                    &first_run,
                    0,
                    "flow-380-pass",
                    &["true"],
                    ["exit:0".to_owned()],
                );
                let created_pass = client
                    .call("queue.enqueue", Some(pass_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(created_pass["disposition"], "created");
                assert_eq!(fs1_wait(&client, &created_pass).await["verdict"], "pass");

                let fail_payload = flow_node_payload(
                    &first_run,
                    1,
                    "flow-380-fail",
                    &["false"],
                    ["exit:0".to_owned()],
                );
                let created_fail = client
                    .call("queue.enqueue", Some(fail_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(created_fail["disposition"], "created");
                assert_ne!(fs1_wait(&client, &created_fail).await["verdict"], "pass");

                // The third node stays live, which is what makes an `attached`
                // disposition possible: pause the pool, then admit.
                client
                    .call("queue.pause", Some(json!({"all": true})))
                    .await
                    .unwrap();
                let live_payload = flow_node_payload(
                    &first_run,
                    2,
                    "flow-380-live",
                    &["true"],
                    ["exit:0".to_owned()],
                );
                let created_live = client
                    .call("queue.enqueue", Some(live_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(created_live["disposition"], "created");

                let task_of = |response: &Value| response["task_uuid"].as_str().unwrap().to_owned();
                let pass_task = task_of(&created_pass);
                let fail_task = task_of(&created_fail);
                let live_task = task_of(&created_live);
                let mut expected_members =
                    vec![pass_task.clone(), fail_task.clone(), live_task.clone()];
                expected_members.sort();

                // Phase two, run B: the same three payloads under a new run ID.
                // Only the capsule differs, and the capsule is not part of the
                // payload hash, so each resolves to a row-less disposition.
                let under_second_run = |payload: &Value, ordinal: u64| {
                    let mut payload = payload.clone();
                    payload["orchestration"]["flowRunId"] = json!(second_run);
                    payload["orchestration"]["nodeOrdinal"] = json!(ordinal);
                    payload["orchestration"]["nodeLabel"] = json!(format!("b-node-{ordinal}"));
                    payload
                };
                let reused = client
                    .call("queue.enqueue", Some(under_second_run(&pass_payload, 5)))
                    .await
                    .unwrap();
                assert_eq!(reused["disposition"], "reused", "{reused}");
                assert_eq!(task_of(&reused), pass_task);

                let terminal = client
                    .call("queue.enqueue", Some(under_second_run(&fail_payload, 6)))
                    .await
                    .unwrap();
                assert_eq!(terminal["disposition"], "terminal", "{terminal}");
                assert_eq!(task_of(&terminal), fail_task);

                let attached = client
                    .call("queue.enqueue", Some(under_second_run(&live_payload, 7)))
                    .await
                    .unwrap();
                assert_eq!(attached["disposition"], "attached", "{attached}");
                assert_eq!(task_of(&attached), live_task);

                // The fifth disposition. A conflict admits nothing, so it makes
                // the run a member of nothing -- asserted, not assumed, because
                // a conflict that silently joined a run would be a claim about
                // work this run does not have.
                let mut conflicting = under_second_run(&pass_payload, 8);
                conflicting["argv"] = json!(["true", "different"]);
                let error = client
                    .call("queue.enqueue", Some(conflicting))
                    .await
                    .unwrap_err();
                let conflict = fs1_conflict(error);
                assert_eq!(conflict["dedupKey"], "flow-380-pass");

                // The durable ledger, read off disk: exactly the bindings the
                // two runs were handed, and nothing else.
                let ledger = membership_ledger(&paths);
                let mut second_run_records = ledger
                    .iter()
                    .filter(|record| record["flowRunId"] == json!(second_run))
                    .map(|record| {
                        (
                            record["taskUuid"].as_str().unwrap().to_owned(),
                            record["disposition"].as_str().unwrap().to_owned(),
                            record["nodeOrdinal"].as_u64().unwrap(),
                        )
                    })
                    .collect::<Vec<_>>();
                second_run_records.sort();
                let mut expected_records = vec![
                    (pass_task.clone(), "reused".to_owned(), 5),
                    (fail_task.clone(), "terminal".to_owned(), 6),
                    (live_task.clone(), "attached".to_owned(), 7),
                ];
                expected_records.sort();
                assert_eq!(
                    second_run_records, expected_records,
                    "each row-less disposition must bind the submitting run to \
                     the task it was handed, under the ordinal *it* submitted"
                );
                assert_eq!(
                    ledger
                        .iter()
                        .filter(|record| record["flowRunId"] == json!(first_run))
                        .count(),
                    3,
                    "created admissions are recorded too, so membership is one fact \
                     for every disposition rather than a special case: {ledger:?}"
                );

                // And what the operator surfaces now say.
                let members = |flow_run: String| {
                    let client = &client;
                    async move {
                        let window = client
                            .call(
                                "query.log",
                                Some(json!({"flowRun": flow_run, "limit": 1000})),
                            )
                            .await
                            .unwrap();
                        let mut tasks = window["items"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|item| item["taskUuid"].as_str().unwrap().to_owned())
                            .collect::<Vec<_>>();
                        tasks.sort();
                        tasks.dedup();
                        (window["flowRunTasks"].as_u64().unwrap(), tasks)
                    }
                };
                let (second_count, second_tasks) = members(second_run.clone()).await;
                assert_eq!(second_count, 3);
                assert_eq!(
                    second_tasks, expected_members,
                    "the run's own window must show the tasks the run was handed"
                );
                let (first_count, first_tasks) = members(first_run.clone()).await;
                assert_eq!(first_count, 3);
                assert_eq!(
                    first_tasks, expected_members,
                    "and the run that created them keeps exactly what it had"
                );

                let jobs = client
                    .call(
                        "query.jobs",
                        Some(json!({"flowRun": second_run.clone(), "limit": 1000})),
                    )
                    .await
                    .unwrap();
                let mut job_anchors = jobs["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|item| item["anchor"].as_str().unwrap().to_owned())
                    .collect::<Vec<_>>();
                job_anchors.sort();
                assert_eq!(job_anchors, expected_members, "{jobs}");
                assert_eq!(jobs["flowRunTasks"], 3);

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    /// `queue.retry` is not one of the five dispositions, but it is an
    /// admission decision, and a node retried under a run that predates the
    /// membership ledger is how an older run's membership gets completed. It
    /// records before it mutates anything, so a refused retry leaves nothing
    /// behind but a fact its own row already implied.
    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_380_retrying_a_flow_node_backfills_the_runs_membership() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let flow_run = Uuid::new_v4().to_string();
                let created = client
                    .call(
                        "queue.enqueue",
                        Some(flow_node_payload(
                            &flow_run,
                            0,
                            "flow-380-retry",
                            &["false"],
                            ["exit:0".to_owned()],
                        )),
                    )
                    .await
                    .unwrap();
                let task_uuid = created["task_uuid"].as_str().unwrap().to_owned();
                assert_ne!(fs1_wait(&client, &created).await["verdict"], "pass");

                // Take the ledger away, so the retry is the only thing that can
                // put the binding back: this is the N-1 row an estate advancing
                // its pin actually has.
                std::fs::remove_file(
                    paths
                        .data_dir
                        .join(crate::flow_membership::FLOW_MEMBERSHIP_FILE),
                )
                .unwrap();
                client
                    .call("queue.pause", Some(json!({"pool": "slot", "all": false})))
                    .await
                    .unwrap();
                let retry = client
                    .call("queue.retry", Some(json!({"task_uuid": task_uuid.clone()})))
                    .await
                    .unwrap();
                assert_eq!(retry["attempt"], 2);

                let ledger = membership_ledger(&paths);
                assert_eq!(ledger.len(), 1, "{ledger:?}");
                assert_eq!(ledger[0]["flowRunId"], json!(flow_run));
                assert_eq!(ledger[0]["taskUuid"], json!(task_uuid));
                assert_eq!(ledger[0]["disposition"], "retried");

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    /// Seed a ledger with `runs x nodes` records ending just before `now`.
    fn seed_membership_ledger(paths: &DaemonPaths, runs: usize, nodes: usize) -> usize {
        seed_membership_ledger_with(paths, runs, nodes, "")
    }

    /// `prefix` is written verbatim ahead of the generated padding, so a test
    /// can plant a specific run at a specific age.
    fn seed_membership_ledger_with(
        paths: &DaemonPaths,
        runs: usize,
        nodes: usize,
        prefix: &str,
    ) -> usize {
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        let mut text = prefix.to_owned();
        for run in 0..runs {
            for node in 0..nodes {
                text.push_str(&format!(
                    "{{\"schemaVersion\":1,\"flowRunId\":\"seed-{run:06}\",\
                     \"taskUuid\":\"seed-{run:06}-{node:04}\",\"disposition\":\"created\",\
                     \"nodeOrdinal\":{node},\"recordedAt\":\"2020-01-01T00:00:00.000Z\"}}\n"
                ));
            }
        }
        std::fs::write(
            paths
                .data_dir
                .join(crate::flow_membership::FLOW_MEMBERSHIP_FILE),
            &text,
        )
        .unwrap();
        text.lines().filter(|line| !line.trim().is_empty()).count()
    }

    fn ledger_lines(paths: &DaemonPaths) -> usize {
        std::fs::read_to_string(
            paths
                .data_dir
                .join(crate::flow_membership::FLOW_MEMBERSHIP_FILE),
        )
        .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count())
        .unwrap_or(0)
    }

    fn ledger_inode(paths: &DaemonPaths) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(
            paths
                .data_dir
                .join(crate::flow_membership::FLOW_MEMBERSHIP_FILE),
        )
        .unwrap()
        .ino()
    }

    /// The HIGH-2 regression, at the level it actually bit.
    ///
    /// The daemon used to rebuild its cached index as *pre-append cache plus the
    /// new record*, never from the compacted set. So once the ledger passed its
    /// bound the cache stayed permanently over it, every later admission decided
    /// it had to compact, and each one serialised and fsynced the entire ledger
    /// — measured at 977 ms per admission against 151 ms for the identical file
    /// after a restart. The cache also grew without limit and answered queries
    /// with runs the file no longer contained.
    ///
    /// Three things are asserted, and the first is the root cause: the index the
    /// daemon holds is the index the file holds.
    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_380_the_daemons_index_never_diverges_from_the_ledger_it_writes() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let bound = crate::flow_membership::FLOW_MEMBERSHIP_MAX_RECORDS;
                // Seed exactly to the bound, in whole runs, so the next
                // admission is the one that crosses it.
                let seeded = seed_membership_ledger(&paths, bound / 4, 4);
                assert_eq!(seeded, bound);

                let daemon = fs1_daemon(&paths).await;
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();

                // Two runs, because `monitor_node_payload` pins `maxNodes: 8`
                // and the flow-node cap is not this test's subject.
                let runs = [Uuid::new_v4().to_string(), Uuid::new_v4().to_string()];
                let mut inodes = Vec::new();
                for ordinal in 0..12 {
                    daemon
                        .handler
                        .enqueue_as_client(Some(monitor_node_payload(
                            &runs[ordinal / 6],
                            ordinal % 6,
                        )))
                        .await
                        .unwrap();
                    inodes.push(ledger_inode(&paths));

                    // The file is bounded...
                    assert!(
                        ledger_lines(&paths) <= bound,
                        "ordinal {ordinal}: ledger grew past its bound"
                    );
                    // ...and the daemon's own index is exactly what the file
                    // holds, every single time. This is the assertion the first
                    // draft would have failed on the very first admission.
                    let held = daemon.handler.flow_membership().await.unwrap();
                    let on_disk = crate::flow_membership::FlowMembership::read(
                        &paths
                            .data_dir
                            .join(crate::flow_membership::FLOW_MEMBERSHIP_FILE),
                    )
                    .unwrap();
                    assert_eq!(
                        *held, on_disk,
                        "ordinal {ordinal}: the daemon's index diverged from the ledger \
                         ({} records held vs {} on disk)",
                        held.record_count(),
                        on_disk.record_count()
                    );
                }

                // A rewrite renames a new file into place, so the inode changes
                // once per compaction. Twelve admissions past the bound must
                // produce exactly one, not twelve.
                inodes.dedup();
                assert_eq!(
                    inodes.len(),
                    1,
                    "the ledger was rewritten more than once in twelve admissions: \
                     that is the every-append-rewrites loop, back again"
                );

                // And the runs still resolve, on a compacted ledger.
                for run in runs {
                    let window = daemon
                        .handler
                        .query("query.log", Some(json!({"flowRun": run, "limit": 1000})))
                        .await
                        .unwrap();
                    assert_eq!(window["flowRunTasks"], 6, "{window}");
                }
            })
            .await;
    }

    /// A long-lived run whose membership is old must not lose it to a bound
    /// crossed by unrelated traffic — and the record an admission is told is
    /// durable must actually be in the ledger.
    ///
    /// This is the shape the divergence test above structurally cannot express,
    /// because that one mints its runs with `Uuid::new_v4()` at test time, so
    /// its runs are always the newest in the ledger and compaction never
    /// considers them. A fixture wrong in the same direction as the code is the
    /// class `AUGUST-02` §3 names, and this is the correction for it: the run
    /// under test is planted as the *oldest* thing in the ledger, holds only
    /// row-less nodes (so the scan half cannot rescue it), and is still live.
    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_380_compaction_never_evicts_a_live_run_or_the_record_it_is_writing() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let bound = crate::flow_membership::FLOW_MEMBERSHIP_MAX_RECORDS;
                let watched = Uuid::new_v4().to_string();

                // Four row-less nodes for the watched run, dated years before
                // anything else. Row-less is the point: these have no capsule on
                // any durable row, so if the ledger forgets them they are gone.
                let mut prefix = String::new();
                for node in 0..4 {
                    prefix.push_str(&format!(
                        "{{\"schemaVersion\":1,\"flowRunId\":\"{watched}\",\
                         \"taskUuid\":\"attached-{node:04}\",\"disposition\":\"attached\",\
                         \"nodeOrdinal\":{node},\"recordedAt\":\"2019-01-01T00:00:0{node}.000Z\"}}\n"
                    ));
                }
                // Pad to exactly the bound, so the watched run's next admission
                // is the one that crosses it.
                let padding_runs = (bound - 4) / 4;
                let seeded = seed_membership_ledger_with(&paths, padding_runs, 4, &prefix);
                assert_eq!(seeded, bound);

                let daemon = fs1_daemon(&paths).await;
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();

                // The still-live run admits its next node.
                let response = daemon
                    .handler
                    .enqueue_as_client(Some(monitor_node_payload(&watched, 4)))
                    .await
                    .unwrap();
                let task_uuid = response["task_uuid"].as_str().unwrap().to_owned();

                // If the write could not be honoured, the caller must be told;
                // if it says nothing, the record has to be there.
                assert!(
                    response["membershipDegraded"].is_null(),
                    "the admission reported degraded membership: {response}"
                );
                let ledger = crate::flow_membership::FlowMembership::read(
                    &paths
                        .data_dir
                        .join(crate::flow_membership::FLOW_MEMBERSHIP_FILE),
                )
                .unwrap();
                assert!(
                    ledger.contains(&watched, &task_uuid),
                    "the record the admission claimed to write is not in the ledger"
                );

                // And the run is whole, not half-present: its four old row-less
                // nodes are still members alongside the one just admitted.
                let mut members = ledger.tasks(&watched).collect::<Vec<_>>();
                members.sort_unstable();
                let mut expected = vec![
                    "attached-0000",
                    "attached-0001",
                    "attached-0002",
                    "attached-0003",
                    task_uuid.as_str(),
                ];
                expected.sort_unstable();
                assert_eq!(
                    members, expected,
                    "the live run lost membership to compaction"
                );

                // The count an operator actually reads.
                let window = daemon
                    .handler
                    .query(
                        "query.log",
                        Some(json!({"flowRun": watched.clone(), "limit": 1000})),
                    )
                    .await
                    .unwrap();
                assert_eq!(window["flowRunTasks"], 5, "{window}");

                // Compaction did happen -- otherwise this proves nothing.
                assert!(
                    ledger_lines(&paths) < bound,
                    "the bound was never crossed, so nothing was under test"
                );
            })
            .await;
    }

    /// The per-admission cost of membership bookkeeping, at several ledger
    /// sizes. Ignored by default because it seeds tens of thousands of records
    /// and reports rather than asserts; run it with
    /// `cargo test -p tally-core --lib membership_admission_cost_sweep
    /// -- --ignored --nocapture`.
    ///
    /// It exists because "measure the hot path you added" is the lesson #379 is
    /// open about, and because the numbers are the only honest way to size
    /// `FLOW_MEMBERSHIP_MAX_RECORDS`.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "seeds a large ledger; run explicitly with --ignored"]
    async fn membership_admission_cost_sweep() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let bound = crate::flow_membership::FLOW_MEMBERSHIP_MAX_RECORDS;
                for (label, records) in [
                    ("empty", 0),
                    ("quarter", bound / 4),
                    ("at the bound", bound),
                    ("past the bound", bound + bound / 4),
                ] {
                    let temp = tempdir().unwrap();
                    let paths = fs1_paths(temp.path());
                    if records > 0 {
                        seed_membership_ledger(&paths, records / 4, 4);
                    }
                    let opened = std::time::Instant::now();
                    let daemon = fs1_daemon(&paths).await;
                    daemon
                        .handler
                        .pause(Some(json!({"all": true})))
                        .await
                        .unwrap();
                    // The first flow admission is what parses the ledger.
                    let warm_run = Uuid::new_v4().to_string();
                    daemon
                        .handler
                        .enqueue_as_client(Some(monitor_node_payload(&warm_run, 0)))
                        .await
                        .unwrap();
                    let first = opened.elapsed();

                    // Every iteration must be a *distinct* `(run, task)` pair.
                    // The first version of this harness re-submitted the same
                    // six pairs, so 21 of its 30 samples took the `AlreadyHeld`
                    // early return — no open, no flock, no fsync — and the
                    // reported figure was ~2.8x too low. The ledger line count
                    // in the output is the check: it must grow by `admissions`.
                    let admissions = 30_usize;
                    let before_lines = ledger_lines(&paths);
                    let started = std::time::Instant::now();
                    for _ in 0..admissions {
                        let run = Uuid::new_v4().to_string();
                        daemon
                            .handler
                            .enqueue_as_client(Some(monitor_node_payload(&run, 0)))
                            .await
                            .unwrap();
                    }
                    let elapsed = started.elapsed();
                    let grew = ledger_lines(&paths).saturating_sub(before_lines);
                    eprintln!(
                        "MEMBERSHIP-COST {label:>14}: seeded {records:>6} records, \
                         open+first admission {first:>12.3?}, \
                         {admissions} admissions in {elapsed:>12.3?} \
                         = {:>10.3?}/admission, ledger {} lines (+{grew})",
                        elapsed / admissions as u32,
                        ledger_lines(&paths),
                    );
                    assert!(
                        grew == admissions || ledger_lines(&paths) < before_lines,
                        "{label}: only {grew} of {admissions} admissions actually \
                         appended; this harness is measuring the wrong path again"
                    );
                }
            })
            .await;
    }

    /// The frozen kernel's other half: an admission that names no run touches
    /// none of this. No ledger, no file, no new failure mode.
    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_380_a_non_flow_admission_writes_no_membership_at_all() {
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
                    fs1_full_payload("no-flow-380", &["true"], ["exit:0".to_owned()]);
                let created = client
                    .call("queue.enqueue", Some(payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(created["disposition"], "created");
                assert_eq!(fs1_wait(&client, &created).await["verdict"], "pass");
                let reused = client.call("queue.enqueue", Some(payload)).await.unwrap();
                assert_eq!(reused["disposition"], "reused");
                assert_eq!(reused["task_uuid"], created["task_uuid"]);

                assert!(
                    membership_ledger(&paths).is_empty(),
                    "a non-flow admission must not create the membership ledger"
                );
                assert!(
                    !paths
                        .data_dir
                        .join(crate::flow_membership::FLOW_MEMBERSHIP_FILE)
                        .exists(),
                    "and must not even create the file"
                );

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    /// Membership is durable, not cached: it survives a restart. And it is
    /// exactly the *delta* over the old scan — remove the ledger and the
    /// pre-#380 answer comes back, node for node, which is what makes an
    /// estate that advances its pin across this commit safe in both
    /// directions.
    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_380_membership_is_durable_and_is_exactly_the_delta_over_the_scan() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let first_run = Uuid::new_v4().to_string();
                let second_run = Uuid::new_v4().to_string();
                let shared_task;
                {
                    let daemon = fs1_daemon(&paths).await;
                    daemon
                        .handler
                        .pause(Some(json!({"all": true})))
                        .await
                        .unwrap();
                    let created = daemon
                        .handler
                        .enqueue_as_client(Some(monitor_node_payload(&first_run, 0)))
                        .await
                        .unwrap();
                    assert_eq!(created["disposition"], "created");
                    shared_task = created["task_uuid"].as_str().unwrap().to_owned();
                    let mut resubmitted = monitor_node_payload(&first_run, 0);
                    resubmitted["orchestration"]["flowRunId"] = json!(second_run);
                    let attached = daemon
                        .handler
                        .enqueue_as_client(Some(resubmitted))
                        .await
                        .unwrap();
                    assert_eq!(attached["disposition"], "attached");
                }

                let count = |handler: &DaemonHandler, flow_run: String| {
                    let handler = handler.clone();
                    async move {
                        handler
                            .query(
                                "query.log",
                                Some(json!({"flowRun": flow_run, "limit": 1000})),
                            )
                            .await
                            .unwrap()["flowRunTasks"]
                            .as_u64()
                            .unwrap()
                    }
                };

                // A fresh daemon on the same paths reads the ledger off disk.
                let restarted = fs1_daemon(&paths).await;
                assert_eq!(count(&restarted.handler, second_run.clone()).await, 1);
                assert_eq!(count(&restarted.handler, first_run.clone()).await, 1);
                drop(restarted);

                // Now take the ledger away. The run whose row carries the
                // capsule is unaffected -- the scan half is untouched -- and
                // the attaching run falls back to exactly the pre-#380 answer.
                std::fs::remove_file(
                    paths
                        .data_dir
                        .join(crate::flow_membership::FLOW_MEMBERSHIP_FILE),
                )
                .unwrap();
                let scan_only = fs1_daemon(&paths).await;
                assert_eq!(
                    count(&scan_only.handler, first_run).await,
                    1,
                    "the row scan must answer exactly as it did before #380"
                );
                assert_eq!(
                    count(&scan_only.handler, second_run.clone()).await,
                    0,
                    "and the ledger must be the whole of the difference"
                );
                let window = scan_only
                    .handler
                    .query(
                        "query.log",
                        Some(json!({"flowRun": second_run, "limit": 1000})),
                    )
                    .await
                    .unwrap();
                assert!(window["items"].as_array().unwrap().is_empty());
                assert!(!shared_task.is_empty());
            })
            .await;
    }

    /// A refusal has to leave nothing behind, and this test's whole job is to
    /// ask what the daemon *kept* — the question the first version of it never
    /// asked, which is how it certified a "refusal" that had already committed a
    /// durable row, emitted the `enqueued` journal event, registered the job
    /// with the dispatcher, and let it run to a passing witness.
    ///
    /// Both reachable faults are asserted: a ledger that cannot be opened at
    /// all, and the one complete-but-unusable record that
    /// `repair-flow-membership-ledger` actually exists for, which is the far
    /// likelier trigger.
    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_380_a_membership_fault_refuses_before_the_kernel_commits_anything() {
        let local = LocalSet::new();
        local
            .run_until(async {
                for fault in ["unopenable", "malformed-record"] {
                    let temp = tempdir().unwrap();
                    let paths = fs1_paths(temp.path());
                    let daemon = fs1_daemon(&paths).await;
                    // The pool is NOT paused: if the kernel committed, the node
                    // would dispatch and run, which is exactly what must not
                    // survive the refusal.
                    let ledger = paths
                        .data_dir
                        .join(crate::flow_membership::FLOW_MEMBERSHIP_FILE);
                    std::fs::create_dir_all(&paths.data_dir).unwrap();
                    match fault {
                        "unopenable" => std::fs::create_dir(&ledger).unwrap(),
                        _ => std::fs::write(
                            &ledger,
                            "{\"schemaVersion\":1,\"flowRunId\":\"run-a\",\
                             \"taskUuid\":\"task-1\",\"disposition\":\"attached\",\
                             \"recordedAt\":\"not-a-timestamp\"}\n",
                        )
                        .unwrap(),
                    }

                    let flow_run = Uuid::new_v4().to_string();
                    let error = daemon
                        .handler
                        .enqueue_as_client(Some(monitor_node_payload(&flow_run, 0)))
                        .await
                        .unwrap_err();
                    assert!(
                        error.message.contains("flow membership"),
                        "{fault}: the refusal must name the store that failed: {error:?}"
                    );
                    assert_eq!(
                        error.data.as_ref().unwrap()["resolution"],
                        "repair-flow-membership-ledger",
                        "{fault}"
                    );

                    // What the daemon kept: nothing.
                    let mut context = daemon.handler.context.write().await;
                    assert!(
                        context.rows.is_empty(),
                        "{fault}: a refused admission left a durable row: {:?}",
                        context.rows.keys().collect::<Vec<_>>()
                    );
                    assert!(
                        context.jobs.is_empty(),
                        "{fault}: a refused admission left a live job"
                    );
                    assert!(
                        context.query_details.snapshot().is_empty(),
                        "{fault}: a refused admission left a query projection"
                    );
                    drop(context);
                    assert!(
                        read_acknowledged_events(&paths.events_dir()).unwrap().is_empty(),
                        "{fault}: a refused admission left a durable enqueue event"
                    );
                    assert!(
                        daemon.handler.history.borrow().snapshot().records.is_empty(),
                        "{fault}: a refused admission emitted a lifecycle event"
                    );

                    // And nothing observable claims the run exists.
                    let window = daemon
                        .handler
                        .query(
                            "query.log",
                            Some(json!({"flowRun": flow_run.clone(), "limit": 1000})),
                        )
                        .await;
                    match window {
                        // A run-scoped query over a damaged ledger fails loudly;
                        // it must never answer with a smaller run.
                        Err(error) => assert!(error.message.contains("flow membership")),
                        Ok(window) => {
                            assert_eq!(window["flowRunTasks"], 0, "{fault}: {window}");
                            assert!(window["items"].as_array().unwrap().is_empty());
                        }
                    }

                    // A non-flow admission is unaffected either way.
                    let unrelated = daemon
                        .handler
                        .enqueue_as_client(Some(fs1_full_payload(
                            "no-flow-membership-fault",
                            &["true"],
                            ["exit:0".to_owned()],
                        )))
                        .await
                        .unwrap();
                    assert_eq!(unrelated["disposition"], "created", "{fault}");
                }
            })
            .await;
    }

    /// The residue the preflight cannot take: the ledger becomes unusable
    /// *between* the preflight and the append, so by the time the failure is
    /// known the admission has already happened.
    ///
    /// The fault is injected at that seam by hand, and deliberately so. The
    /// preflight catches the faults that are reachable *before* an admission —
    /// an unopenable ledger and an unusable record, which the test above proves
    /// — but it is a check, not a guarantee, and it does not cover everything.
    /// Two known gaps: a ledger that is writable under a read-only parent
    /// passes both the probe and an ordinary append and fails only when
    /// compaction tries to create its temp file; and the data directory can
    /// fill, or the file be replaced, in the window between the check and the
    /// write. Both land here, which is why this path has to exist and be
    /// correct rather than be treated as unreachable.
    ///
    /// What must hold: the caller is told the truth. The work was admitted, here
    /// is its task UUID, and this one node's membership is missing until the
    /// ledger is repaired. It is not told the admission failed while its node
    /// executes.
    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_380_a_membership_fault_after_the_commit_degrades_rather_than_lying() {
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
                let ledger = paths
                    .data_dir
                    .join(crate::flow_membership::FLOW_MEMBERSHIP_FILE);
                let flow_run = Uuid::new_v4().to_string();

                // The preflight passes: the ledger is fine at this point.
                daemon.handler.preflight_flow_membership().await.unwrap();

                // The kernel commits. This is a real admission with a real row.
                let mut response = daemon
                    .handler
                    .enqueue_as_client(Some(monitor_node_payload(&flow_run, 0)))
                    .await
                    .unwrap();
                let task_uuid = response["task_uuid"].as_str().unwrap().to_owned();

                // ...and only now does the ledger become unwritable.
                std::fs::remove_file(&ledger).ok();
                std::fs::create_dir(&ledger).unwrap();

                let orchestration = crate::provenance::Orchestration::new(
                    monitor_node_payload(&flow_run, 1)["orchestration"].clone(),
                )
                .unwrap();
                let error = daemon
                    .handler
                    .record_admission_membership(&orchestration, &response)
                    .await
                    .expect_err("the membership write must fail for this test to mean anything");
                daemon.handler.disclose_degraded_membership(
                    &orchestration,
                    &mut response,
                    &error,
                );

                // The response still says what actually happened.
                assert_eq!(response["disposition"], "created", "{response}");
                assert_eq!(
                    response["task_uuid"], json!(task_uuid),
                    "an acknowledged admission must still hand back its task UUID: {response}"
                );
                let degraded = &response["membershipDegraded"];
                assert_eq!(degraded["admitted"], true, "{response}");
                assert_eq!(degraded["flowRunId"], json!(flow_run), "{response}");
                assert_eq!(degraded["taskUuid"], json!(task_uuid), "{response}");
                assert_eq!(
                    degraded["resolution"], "repair-flow-membership-ledger",
                    "{response}"
                );
                assert!(
                    degraded["reason"].as_str().unwrap().contains("flow membership"),
                    "{response}"
                );

                // The admission is real, and the daemon kept it -- which is the
                // whole reason the caller must not be told it failed.
                let context = daemon.handler.context.read().await;
                assert_eq!(context.rows.len(), 1, "the admission did happen");
                assert!(context.jobs.contains_key(&Uuid::parse_str(&task_uuid).unwrap()));
            })
            .await;
    }

    /// `positionGap` keeps meaning what it meant. It is decided by the held
    /// position against retained history, never by the filtered window, so a
    /// run whose membership just became durable reports the same gap it always
    /// would have -- "history is missing" is not quietly replaced by "the
    /// window is now reachable".
    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_380_position_gap_is_retention_evidence_not_membership_evidence() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let first_run = Uuid::new_v4().to_string();
                let second_run = Uuid::new_v4().to_string();
                let shared_task;
                let mark;
                {
                    let daemon = fs1_daemon(&paths).await;
                    daemon
                        .handler
                        .pause(Some(json!({"all": true})))
                        .await
                        .unwrap();
                    let created = daemon
                        .handler
                        .enqueue_as_client(Some(monitor_node_payload(&first_run, 0)))
                        .await
                        .unwrap();
                    shared_task = created["task_uuid"].as_str().unwrap().to_owned();
                    let mut resubmitted = monitor_node_payload(&first_run, 0);
                    resubmitted["orchestration"]["flowRunId"] = json!(second_run);
                    let attached = daemon
                        .handler
                        .enqueue_as_client(Some(resubmitted))
                        .await
                        .unwrap();
                    assert_eq!(attached["disposition"], "attached");

                    // Everything above this mark becomes unretained history.
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    mark = Utc::now();
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    for event in [TallyEvent::Started, TallyEvent::Heartbeat] {
                        let mut emit = crate::journal::EmitEvent::enqueued(
                            shared_task.clone(),
                            Priority::High,
                            EnqueueSource::Orchestrator,
                        );
                        emit.event = event;
                        emit.agent = Some("shell".to_owned());
                        emit.attempt = Some(1);
                        emit.lease_epoch = Some(1);
                        emit.job_id = Some(shared_task.clone());
                        emit.unit = Some(format!("tally-job-{shared_task}.service"));
                        daemon
                            .handler
                            .history
                            .borrow_mut()
                            .append_now(emit.into_fields().unwrap())
                            .unwrap();
                    }
                }

                // Drop the oldest lifecycle prefix for real, so a held position
                // at the origin genuinely predates retained history.
                let compaction =
                    crate::history::compact_lifecycle(&paths.state_dir, &paths.data_dir, 0, mark)
                        .unwrap();
                assert!(compaction.dropped > 0, "{compaction:?}");

                let daemon = fs1_daemon(&paths).await;
                let origin = "log-v1:00000000000000000000:00000000000000000000";
                let gapped = daemon
                    .handler
                    .query(
                        "query.log",
                        Some(json!({
                            "flowRun": second_run.clone(),
                            "after": origin,
                            "limit": 1000,
                        })),
                    )
                    .await
                    .unwrap();
                assert!(
                    gapped["positionGap"].is_object(),
                    "a held position before the retained floor is still a gap on a run \
                     whose membership is durable: {gapped}"
                );
                assert_eq!(
                    gapped["flowRunTasks"], 1,
                    "the run still resolves to its member; the gap is about history, \
                     not about membership"
                );

                // The gap decision is identical with and without the run
                // filter: it is computed from the held position against the
                // retained floor and never from the window the filter produced.
                let unfiltered = daemon
                    .handler
                    .query(
                        "query.log",
                        Some(json!({"after": origin, "limit": 1000})),
                    )
                    .await
                    .unwrap();
                assert_eq!(gapped["positionGap"], unfiltered["positionGap"]);

                // And a position at the head reports no gap, on the same run.
                let head = daemon
                    .handler
                    .query(
                        "query.log",
                        Some(json!({"flowRun": second_run.clone(), "limit": 1000})),
                    )
                    .await
                    .unwrap()["position"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let ungapped = daemon
                    .handler
                    .query(
                        "query.log",
                        Some(json!({"flowRun": second_run, "after": head, "limit": 1000})),
                    )
                    .await
                    .unwrap();
                assert!(ungapped.get("positionGap").is_none(), "{ungapped}");
            })
            .await;
    }

    /// `--after` is a durable stream coordinate, not a page cursor and not the
    /// `--since` time filter. Two successive polls over a quiet run must be
    /// byte-identical apart from the timestamp that dates the projection.
    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_316_after_is_a_durable_position_and_since_stays_a_time_filter() {
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
                let flow_run_id = Uuid::new_v4().to_string();
                for ordinal in 0..3 {
                    daemon
                        .handler
                        .enqueue_as_client(Some(monitor_node_payload(&flow_run_id, ordinal)))
                        .await
                        .unwrap();
                }
                let filter = json!({"flowRun": flow_run_id, "limit": 1000});
                let seed = daemon
                    .handler
                    .query("query.log", Some(filter.clone()))
                    .await
                    .unwrap();
                assert!(!seed["items"].as_array().unwrap().is_empty());
                let position = seed["position"].as_str().unwrap().to_owned();
                assert!(position.starts_with("log-v1:"), "{position}");

                let poll = |after: String| {
                    let mut params = filter.clone();
                    params["after"] = json!(after);
                    let handler = &daemon.handler;
                    async move { handler.query("query.log", Some(params)).await.unwrap() }
                };
                let strip = |mut value: Value| {
                    value["snapshot"]["createdAt"] = Value::Null;
                    value
                };

                // Quiet run: empty items, unchanged position, and the two
                // responses differ only in the timestamp that dates the
                // projection -- what a poller diffs is identical.
                let first = poll(position.clone()).await;
                let second = poll(position.clone()).await;
                assert!(first["items"].as_array().unwrap().is_empty());
                assert_eq!(first["position"], json!(position));
                assert_eq!(second["position"], json!(position));
                assert_eq!(strip(first), strip(second));

                // One more node: `--after` returns only what is new.
                daemon
                    .handler
                    .enqueue_as_client(Some(monitor_node_payload(&flow_run_id, 3)))
                    .await
                    .unwrap();
                let incremental = poll(position.clone()).await;
                let fresh = incremental["items"].as_array().unwrap();
                assert!(!fresh.is_empty(), "the new node produced no visible events");
                let already_seen = seed["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|item| item["eventId"].as_str().unwrap().to_owned())
                    .collect::<BTreeSet<_>>();
                assert!(
                    fresh
                        .iter()
                        .all(|item| !already_seen.contains(item["eventId"].as_str().unwrap())),
                    "an event at or before the held position was replayed: {fresh:?}"
                );
                assert!(
                    fresh.iter().all(|item| item["nodeLabel"] == "node-3"),
                    "--after leaked events from nodes the caller had already seen: {fresh:?}"
                );
                assert_ne!(incremental["position"], json!(position));

                // `--since` is untouched: still a wall-clock filter, and it
                // composes with `--after` rather than replacing it.
                let mut future = filter.clone();
                future["since"] = json!("2099-01-01T00:00:00Z");
                let future = daemon
                    .handler
                    .query("query.log", Some(future))
                    .await
                    .unwrap();
                assert!(future["items"].as_array().unwrap().is_empty());
                let mut past = filter.clone();
                past["since"] = json!("2000-01-01T00:00:00Z");
                let past = daemon.handler.query("query.log", Some(past)).await.unwrap();
                assert!(past["items"].as_array().unwrap().len() > 3);

                // A page cursor is not a position: feeding one back must be
                // refused by name, not silently misread as a stream offset.
                let mut confused = filter.clone();
                confused["after"] = json!("page-v1:00000000000000000001:00000000000000000000");
                let error = daemon
                    .handler
                    .query("query.log", Some(confused))
                    .await
                    .unwrap_err();
                assert!(
                    format!("{error:?}").contains("invalid lifecycle stream position"),
                    "{error:?}"
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

    /// A dispatch that cannot take the capture lock never launched the unit, so
    /// it must not be recorded as the agent having failed. Before this the
    /// bounded deadline reached the catch-all executor-error arm and produced a
    /// `Failed` witness with exit code 1 — a burnt attempt and, with
    /// `postFailureEvidence` on, a public failure receipt with no evidence in
    /// it — for a daemon-side file-locking condition.
    #[tokio::test(flavor = "current_thread")]
    async fn a_contended_capture_lock_preempts_the_dispatch_instead_of_failing_the_job() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let program = temp.path().join("failing-agent");
                crate::test_support::install_shell_program(&program, "#!/bin/sh\nexit 1\n");
                let executor = direct_executor(&paths.state_dir)
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

                // A client cannot preassign a task UUID, so run one attempt to
                // learn the identity, then wedge its lock and retry: attempt 2
                // dispatches under the same unit UUID.
                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": [program],
                        "pool": "slot",
                    })))
                    .await
                    .unwrap();
                let finished = await_positive_progress(
                    "capture-lock source completion",
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();

                let lock_dir = paths.state_dir.join(crate::executor::CAPTURE_LOCK_DIRECTORY);
                fs::create_dir_all(&lock_dir).unwrap();
                let holder = fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(lock_dir.join(format!(
                        "{task_uuid}{}",
                        crate::executor::CAPTURE_LOCK_SUFFIX
                    )))
                    .unwrap();
                FileExt::lock_exclusive(&holder).unwrap();

                daemon
                    .handler
                    .retry_job(Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();

                // The dispatch is now waiting out the capture-lock deadline.
                // `execute_raw` runs under `spawn_local` on this single-threaded
                // runtime, so waiting inline would park the daemon's only thread
                // for the whole deadline: no RPC answered, no timer fired. Drive
                // the local set for a window well inside the deadline and watch
                // for a step that does not come back.
                let mut worst_stall = Duration::ZERO;
                let watch = Instant::now();
                while watch.elapsed() < Duration::from_millis(1_500) {
                    let step = Instant::now();
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    worst_stall = worst_stall.max(step.elapsed());
                }
                assert!(
                    worst_stall < Duration::from_secs(1),
                    "the dispatch parked the runtime thread for {worst_stall:?}"
                );

                // And the daemon is still answering, mid-deadline. This short
                // bound is semantic: response latency is the fact under test,
                // unlike the positive-progress waits above and below.
                let answered = tokio::time::timeout(
                    Duration::from_secs(2),
                    daemon.handler.query("query.jobs", Some(json!({"limit": 1}))),
                )
                .await
                .expect("a contended dispatch must not park the daemon's runtime thread")
                .unwrap();
                assert_eq!(answered["items"].as_array().unwrap().len(), 1);

                let finished = await_positive_progress(
                    "capture-lock deadline completion",
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap();
                daemon.finish_job(finished).await.unwrap();
                drop(holder);

                let result = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": task_uuid, "attempt": 2})))
                    .await
                    .unwrap();
                assert_eq!(result["verdict"], "preempted");

                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 2);
                assert_eq!(records[0].verdict, Verdict::Failed);
                let records = &records[1..];
                assert_eq!(records[0].verdict, Verdict::Preempted);
                // Not charged to the agent, and re-runnable rather than terminal.
                assert!(!crate::witness::counts_toward_canonical_gpu_seconds(
                    &records[0]
                ));
                assert_eq!(
                    crate::evidence::retry_trigger(records[0].verdict),
                    Some(crate::evidence::RetryTrigger::ResourceReturn)
                );
                // A preempted attempt is not a failure, so no failure receipt.
                assert_ne!(
                    terminal_lifecycle_event(
                        records[0].verdict,
                        records[0].artifact_content_hash.is_some()
                    ),
                    TallyEvent::Failed
                );
            })
            .await;
    }

    /// The whole rollover contract at the daemon boundary: the old run is
    /// preserved, the successor is durable, repeats are safe, contradictions are
    /// refused, and every query surface says which generation is current.
    #[tokio::test(flavor = "current_thread")]
    async fn a_supersede_records_durable_lineage_idempotently_and_refuses_contradictions() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let old_run = "00000000-0000-4000-8000-000000000251";
                let new_run = "00000000-0000-4000-8000-000000000252";
                let other_run = "00000000-0000-4000-8000-000000000253";
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let mut payload =
                    fs1_full_payload("flow:supersede:0", &["true"], ["exit:0".to_owned()]);
                payload["source"] = json!("orchestrator");
                payload["orchestration"] = json!({
                    "flowName": "generation-a",
                    "flowRunId": old_run,
                    "scriptHash": "sha256:generation-a-script",
                    "argsHash": "sha256:generation-a-args",
                    "nodeOrdinal": 0,
                    "maxNodes": 2
                });
                let created = client.call("queue.enqueue", Some(payload)).await.unwrap();
                assert_eq!(fs1_wait(&client, &created).await["verdict"], "pass");
                let before = await_positive_progress("post-ack usage publication", async {
                    loop {
                        let jobs = client
                            .call("query.jobs", Some(json!({"flowRun": old_run})))
                            .await
                            .unwrap();
                        if jobs["items"][0].get("usageAccounting").is_some() {
                            break jobs;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await;

                // Nothing is recorded until an operator asks for it.
                let empty = client
                    .call("query.lineage", Some(json!({"flowRun": old_run})))
                    .await
                    .unwrap();
                assert_eq!(empty["superseded"], false);
                assert_eq!(empty["chain"], json!([old_run]));
                assert_eq!(empty["currentFlowRunId"], old_run);

                let recorded = client
                    .call(
                        "flow.supersede",
                        Some(json!({
                            "flowRunId": old_run,
                            "successorFlowRunId": new_run,
                            "reason": "generation-change"
                        })),
                    )
                    .await
                    .unwrap();
                assert_eq!(recorded["disposition"], "recorded");
                // The abandoned generation's own pins are the audit record, and
                // they come from its rows rather than from the caller.
                assert_eq!(
                    recorded["record"]["predecessorScriptHash"],
                    "sha256:generation-a-script"
                );
                assert_eq!(
                    recorded["record"]["predecessorArgsHash"],
                    "sha256:generation-a-args"
                );

                // Idempotent across supervisor restarts: same call, same answer,
                // one durable record.
                let repeated = client
                    .call(
                        "flow.supersede",
                        Some(json!({
                            "flowRunId": old_run,
                            "successorFlowRunId": new_run,
                            "reason": "generation-change"
                        })),
                    )
                    .await
                    .unwrap();
                assert_eq!(repeated["disposition"], "reused");
                assert_eq!(repeated["record"], recorded["record"]);
                assert_eq!(
                    fs::read_to_string(paths.flow_lineage_path())
                        .unwrap()
                        .lines()
                        .count(),
                    1
                );

                // The predecessor's history is preserved byte for byte.
                let after = client
                    .call("query.jobs", Some(json!({"flowRun": old_run})))
                    .await
                    .unwrap();
                assert_eq!(after["items"], before["items"]);

                // A contradicting rollover is refused rather than rewritten.
                let conflict = client
                    .call(
                        "flow.supersede",
                        Some(json!({
                            "flowRunId": old_run,
                            "successorFlowRunId": other_run,
                            "reason": "generation-change"
                        })),
                    )
                    .await
                    .unwrap_err();
                assert!(
                    matches!(
                        conflict,
                        WireIoError::Rpc(WireErrorCode::FlowLineageConflict, _, _)
                    ),
                    "{conflict:?}"
                );

                // Both ends of the boundary, from both directions.
                let predecessor = client
                    .call("query.lineage", Some(json!({"flowRun": old_run})))
                    .await
                    .unwrap();
                assert_eq!(predecessor["superseded"], true);
                assert_eq!(predecessor["supersededBy"]["successorFlowRunId"], new_run);
                assert_eq!(predecessor["supersededBy"]["reason"], "generation-change");
                assert_eq!(predecessor["currentFlowRunId"], new_run);
                let successor = client
                    .call("query.lineage", Some(json!({"flowRun": new_run})))
                    .await
                    .unwrap();
                assert_eq!(successor["superseded"], false);
                assert_eq!(successor["supersedes"]["flowRunId"], old_run);
                assert_eq!(successor["chain"], json!([old_run, new_run]));

                // The run view is unambiguous: terminal, with its successor named.
                let view = client
                    .call("query.run", Some(json!({"id": old_run})))
                    .await
                    .unwrap();
                assert_eq!(view["state"], "superseded");
                assert_eq!(view["supersededBy"]["successorFlowRunId"], new_run);

                // The record survives the daemon that wrote it.
                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
                drop(client);
                let reopened = fs1_daemon(&paths).await;
                let (second_shutdown, second_shutdown_rx) = watch::channel(false);
                let second_task = tokio::task::spawn_local(reopened.run_until(second_shutdown_rx));
                let restarted = RpcClient::connect(&paths.socket).await.unwrap();
                let durable = restarted
                    .call("query.lineage", Some(json!({"flowRun": old_run})))
                    .await
                    .unwrap();
                assert_eq!(durable["supersededBy"]["successorFlowRunId"], new_run);
                second_shutdown.send(true).unwrap();
                second_task.await.unwrap().unwrap();
            })
            .await;
    }

    /// A rollover may not strand live work, and a successor starts fresh.
    #[tokio::test(flavor = "current_thread")]
    async fn a_supersede_refuses_a_live_predecessor_and_a_started_successor() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let old_run = "00000000-0000-4000-8000-000000000261";
                let new_run = "00000000-0000-4000-8000-000000000262";
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let node = |run: &str, key: &str, argv: &[&str]| {
                    let mut payload = fs1_full_payload(key, argv, ["exit:0".to_owned()]);
                    payload["source"] = json!("orchestrator");
                    payload["orchestration"] = json!({
                        "flowName": "generation",
                        "flowRunId": run,
                        "scriptHash": "sha256:generation-script",
                        "nodeOrdinal": 0,
                        "maxNodes": 2
                    });
                    payload
                };

                let live = client
                    .call("queue.enqueue", Some(node(old_run, "flow:live:0", &["sleep", "30"])))
                    .await
                    .unwrap();
                let refused = client
                    .call(
                        "flow.supersede",
                        Some(json!({
                            "flowRunId": old_run,
                            "successorFlowRunId": new_run,
                            "reason": "generation-change"
                        })),
                    )
                    .await
                    .unwrap_err();
                assert!(
                    matches!(
                        refused,
                        WireIoError::Rpc(WireErrorCode::FlowLineageConflict, _, _)
                    ),
                    "{refused:?}"
                );
                assert!(!paths.flow_lineage_path().exists());

                client
                    .call(
                        "queue.cancel",
                        Some(json!({"task_uuid": live["task_uuid"], "force": true})),
                    )
                    .await
                    .unwrap();

                // The successor must not have started: a rollover mints a fresh
                // run, it does not adopt one already in flight.
                client
                    .call("queue.enqueue", Some(node(new_run, "flow:started:0", &["true"])))
                    .await
                    .unwrap();
                let started = client
                    .call(
                        "flow.supersede",
                        Some(json!({
                            "flowRunId": old_run,
                            "successorFlowRunId": new_run,
                            "reason": "generation-change"
                        })),
                    )
                    .await
                    .unwrap_err();
                assert!(
                    matches!(
                        started,
                        WireIoError::Rpc(WireErrorCode::FlowLineageConflict, _, _)
                    ),
                    "{started:?}"
                );
                assert!(!paths.flow_lineage_path().exists());

                // A run started under an earlier daemon epoch is still a started
                // run: the freshness question is asked of the durable rows, not
                // of whichever jobs this process happens to hold in memory.
                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
                drop(client);
                let reopened = fs1_daemon(&paths).await;
                let (second_shutdown, second_shutdown_rx) = watch::channel(false);
                let second_task = tokio::task::spawn_local(reopened.run_until(second_shutdown_rx));
                let restarted = RpcClient::connect(&paths.socket).await.unwrap();
                let after_restart = restarted
                    .call(
                        "flow.supersede",
                        Some(json!({
                            "flowRunId": old_run,
                            "successorFlowRunId": new_run,
                            "reason": "generation-change"
                        })),
                    )
                    .await
                    .unwrap_err();
                assert!(
                    matches!(
                        after_restart,
                        WireIoError::Rpc(WireErrorCode::FlowLineageConflict, _, _)
                    ),
                    "{after_restart:?}"
                );
                assert!(!paths.flow_lineage_path().exists());
                second_shutdown.send(true).unwrap();
                second_task.await.unwrap().unwrap();
            })
            .await;
    }

    /// The repair's two halves at the daemon boundary: a rollover must name a
    /// run that exists, and one run must have exactly one key however its ID is
    /// spelled.
    #[tokio::test(flavor = "current_thread")]
    async fn a_supersede_refuses_an_unknown_predecessor_and_keys_every_rendering_alike() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let old_run = "00000000-0000-4000-8000-000000000271";
                let new_run = "00000000-0000-4000-8000-000000000272";
                let unknown = "00000000-0000-4000-8000-000000000273";
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let supersede = |predecessor: &str, successor: &str| {
                    client.call(
                        "flow.supersede",
                        Some(json!({
                            "flowRunId": predecessor,
                            "successorFlowRunId": successor,
                            "reason": "generation-change"
                        })),
                    )
                };

                // A run that never existed cannot have tripped an identity pin,
                // so it can never need retiring. It used to answer ok:true.
                let invented = supersede(unknown, new_run).await.unwrap_err();
                assert!(
                    matches!(invented, WireIoError::Rpc(WireErrorCode::NotFound, _, _)),
                    "{invented:?}"
                );
                assert!(
                    !paths.flow_lineage_path().exists(),
                    "a refused rollover writes nothing"
                );

                let mut payload =
                    fs1_full_payload("flow:rendering:0", &["true"], ["exit:0".to_owned()]);
                payload["source"] = json!("orchestrator");
                payload["orchestration"] = json!({
                    "flowName": "generation-a",
                    "flowRunId": old_run,
                    "scriptHash": "sha256:generation-a-script",
                    "argsHash": "sha256:generation-a-args",
                    "nodeOrdinal": 0,
                    "maxNodes": 2
                });
                let created = client.call("queue.enqueue", Some(payload)).await.unwrap();
                assert_eq!(fs1_wait(&client, &created).await["verdict"], "pass");

                // Recorded through an upper-case rendering; stored canonically.
                let recorded = supersede(&old_run.to_uppercase(), &new_run.to_uppercase())
                    .await
                    .unwrap();
                assert_eq!(recorded["disposition"], "recorded");
                assert_eq!(recorded["record"]["flowRunId"], old_run);
                assert_eq!(recorded["record"]["successorFlowRunId"], new_run);
                // The promised pins are present, not silently omitted.
                assert_eq!(
                    recorded["record"]["predecessorScriptHash"],
                    "sha256:generation-a-script"
                );

                // The rendering the runner actually presents sees the rollover.
                for rendering in [old_run.to_owned(), old_run.to_uppercase()] {
                    let view = client
                        .call("query.lineage", Some(json!({"flowRun": rendering})))
                        .await
                        .unwrap();
                    assert_eq!(view["superseded"], true, "rendering {rendering}");
                    assert_eq!(view["flowRunId"], old_run);
                    assert_eq!(view["currentFlowRunId"], new_run);
                }
                let run_view = client
                    .call("query.run", Some(json!({"id": old_run.to_uppercase()})))
                    .await
                    .unwrap();
                assert_eq!(run_view["supersededBy"]["successorFlowRunId"], new_run);

                // The honest retry in the canonical rendering is a reuse, not a
                // conflict against a successor its own typo already burned.
                assert_eq!(
                    supersede(old_run, new_run).await.unwrap()["disposition"],
                    "reused"
                );

                // A run ID that is not a UUID is an error, not a well-formed
                // "not superseded" answer that hides a mis-rendered lookup.
                let bogus = client
                    .call("query.lineage", Some(json!({"flowRun": "not-a-uuid"})))
                    .await
                    .unwrap_err();
                assert!(
                    matches!(bogus, WireIoError::Rpc(WireErrorCode::InvalidParams, _, _)),
                    "{bogus:?}"
                );

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    /// An unreadable lineage ledger stops flow starts, so it must never look
    /// like an anonymous internal fault to the supervisor reading the contract
    /// in `errors.md`.
    #[tokio::test(flavor = "current_thread")]
    async fn an_unusable_lineage_ledger_reports_typed_recovery_facts() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let run = "00000000-0000-4000-8000-000000000281";
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                // A complete but unusable record — a hand edit, not a torn
                // append. Skipping it could resurrect a retired run, so it fails
                // closed; the facts say the failure is permanent and bounded.
                fs::write(
                    paths.flow_lineage_path(),
                    "{\"schemaVersion\":1,\"flowRunId\":\"nope\",\"successorFlowRunId\":\"nope2\",\
                      \"reason\":\"operator\",\"recordedAt\":\"2026-08-02T00:00:00.000Z\"}\n",
                )
                .unwrap();
                let error = client
                    .call("query.lineage", Some(json!({"flowRun": run})))
                    .await
                    .unwrap_err();
                let WireIoError::Rpc(code, message, data) = error else {
                    panic!("expected a typed RPC error");
                };
                assert_eq!(code, WireErrorCode::FlowLineageUnusable);
                assert!(message.contains("line 1"), "{message}");
                let data = data.expect("an unusable ledger carries recovery facts");
                assert_eq!(data["transient"], false);
                assert_eq!(data["resolution"], "repair-lineage-ledger");

                // Repairing the file by hand is enough; the cache revalidates
                // against the bytes rather than pinning the failure until the
                // daemon restarts.
                fs::remove_file(paths.flow_lineage_path()).unwrap();
                let view = client
                    .call("query.lineage", Some(json!({"flowRun": run})))
                    .await
                    .unwrap();
                assert_eq!(view["superseded"], false);

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    /// #389: `query.jobs`/`query.run`/`query.standup` filter on archived
    /// reader-state, and no *daemon* code path writes that file -- only
    /// `crate::reader_state::set_reader_state`, called here exactly the way
    /// the `tally reader-state` CLI calls it, off the daemon entirely.
    #[tokio::test(flavor = "current_thread")]
    async fn query_run_jobs_and_standup_hide_archived_runs_by_default_and_expose_the_flag() {
        let local = LocalSet::new();
        local
            .run_until(async {
                const ARCHIVED_RUN: &str = "00000000-0000-4000-8000-000000000389";
                const LIVE_RUN: &str = "00000000-0000-4000-8000-00000000038a";
                const MEMBERSHIP_ONLY_TASK: &str = "00000000-0000-4000-8000-00000000038b";

                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let mut daemon = fs1_daemon(&paths).await;

                let enqueue_node = |flow_run: &'static str, node_label: &'static str| {
                    let handler = &daemon.handler;
                    async move {
                        let admitted = handler
                            .enqueue_as_client(Some(json!({
                                "argv": ["true"],
                                "pool": "slot",
                                "adapter": "shell",
                                "source": "manual",
                                "evidence": ["exit:0"],
                                "orchestration": {
                                    "flowName": "test-flow",
                                    "flowRunId": flow_run,
                                    "nodeOrdinal": 1,
                                    "nodeLabel": node_label
                                }
                            })))
                            .await
                            .unwrap();
                        admitted["task_uuid"].as_str().unwrap().to_owned()
                    }
                };
                let archived_task = enqueue_node(ARCHIVED_RUN, "agent-archived").await;
                let live_task = enqueue_node(LIVE_RUN, "agent-live").await;
                for _ in 0..2 {
                    let finished = await_positive_progress(
                        "archived-run fixture completion",
                        daemon.completion_rx.recv(),
                    )
                    .await
                    .unwrap();
                    daemon.finish_job(finished).await.unwrap();
                }
                daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": archived_task})))
                    .await
                    .unwrap();
                daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": live_task})))
                    .await
                    .unwrap();

                // A durable run member need not have any row, lifecycle, live,
                // or witness fact of its own. This is the identity-only shape
                // that an explicit lookup must still project.
                daemon
                    .handler
                    .record_flow_membership(
                        crate::flow_membership::FlowMembershipRecord::new(
                            ARCHIVED_RUN.to_owned(),
                            MEMBERSHIP_ONLY_TASK.to_owned(),
                            crate::flow_membership::MembershipDisposition::Attached,
                            Some(2),
                            Some("membership-only".to_owned()),
                        ),
                    )
                    .await
                    .unwrap();

                // Written exactly the way the CLI writes it: a direct call
                // against the data-dir file, off the daemon's own RPC path.
                crate::reader_state::set_reader_state(
                    &crate::reader_state::reader_state_path(&paths.data_dir),
                    ARCHIVED_RUN,
                    crate::reader_state::ReaderStateUpdate {
                        archived: Some(true),
                        triage_tag: Some(Some("flaky-fixture".to_owned())),
                    },
                )
                .unwrap();

                // `query.run` always exposes the flag and tag; it never
                // suppresses the single run an operator explicitly asked for.
                let archived_view = daemon
                    .handler
                    .query("query.run", Some(json!({"id": ARCHIVED_RUN})))
                    .await
                    .unwrap();
                assert_eq!(archived_view["archived"], true);
                assert_eq!(archived_view["triageTag"], "flaky-fixture");
                assert!(
                    archived_view.get("tasks").is_none(),
                    "an empty reconciliation board must not shadow durable members"
                );
                let archived_members = archived_view
                    .get("tasks")
                    .or_else(|| archived_view.get("items"))
                    .and_then(Value::as_array)
                    .unwrap();
                assert_eq!(archived_members.len(), 2);
                for expected in [archived_task.as_str(), MEMBERSHIP_ONLY_TASK] {
                    assert!(archived_members
                        .iter()
                        .any(|member| member["taskUuid"].as_str() == Some(expected)));
                }
                let live_view = daemon
                    .handler
                    .query("query.run", Some(json!({"id": LIVE_RUN})))
                    .await
                    .unwrap();
                assert_eq!(live_view["archived"], false);
                assert!(live_view["triageTag"].is_null());

                // `query.jobs` defaults to hiding the archived run's job.
                let default_jobs = daemon
                    .handler
                    .query("query.jobs", Some(json!({})))
                    .await
                    .unwrap();
                let default_anchors = default_jobs["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|item| item["anchor"].as_str().unwrap().to_owned())
                    .collect::<Vec<_>>();
                assert!(!default_anchors.contains(&archived_task));
                assert!(default_anchors.contains(&live_task));

                // `--archived` includes it, and flags it.
                let all_jobs = daemon
                    .handler
                    .query("query.jobs", Some(json!({"archived": true})))
                    .await
                    .unwrap();
                let flagged = all_jobs["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["anchor"] == archived_task)
                    .expect("archived job present when opted in");
                assert_eq!(flagged["archived"], true);

                // A flow-run filter is an explicit by-ID inspection, like
                // `query.run`: it keeps the archived member and annotates it
                // even though the same member is hidden from the broad
                // default query above.
                let archived_run_jobs = daemon
                    .handler
                    .query(
                        "query.jobs",
                        Some(json!({"flowRun": ARCHIVED_RUN})),
                    )
                    .await
                    .unwrap();
                assert_eq!(archived_run_jobs["flowRunTasks"], 2);
                let archived_items = archived_run_jobs["items"].as_array().unwrap();
                assert_eq!(archived_items.len(), 2);
                for expected in [archived_task.as_str(), MEMBERSHIP_ONLY_TASK] {
                    let item = archived_items
                        .iter()
                        .find(|item| item["anchor"].as_str() == Some(expected))
                        .unwrap_or_else(|| panic!("explicit lookup omitted {expected}"));
                    assert_eq!(item["taskUuid"], expected);
                    assert_eq!(item["archived"], true);
                }
                let identity_only = archived_items
                    .iter()
                    .find(|item| item["anchor"] == MEMBERSHIP_ONLY_TASK)
                    .unwrap();
                assert!(identity_only["orchestration"].is_null());

                // `query.standup` hides the archived run's entry and its run
                // rollup by default. `archivedHidden` (task entries) and
                // `archivedRunsHidden` (per-run cost rows) are two separate
                // counts -- both say exactly one here -- each computed from
                // the same pass that filters the list it describes, not a
                // separate recount.
                let default_standup = daemon
                    .handler
                    .query("query.standup", Some(json!({})))
                    .await
                    .unwrap();
                assert_eq!(default_standup["archivedHidden"], 1);
                assert_eq!(default_standup["archivedRunsHidden"], 1);
                let completed = default_standup["completed"].as_array().unwrap();
                assert!(completed
                    .iter()
                    .all(|entry| entry["taskUuid"] != archived_task));
                assert!(completed.iter().any(|entry| entry["taskUuid"] == live_task));
                let runs = default_standup["runs"].as_array().unwrap();
                assert!(runs
                    .iter()
                    .all(|run| run["flowRunId"] != ARCHIVED_RUN));

                let all_standup = daemon
                    .handler
                    .query("query.standup", Some(json!({"archived": true})))
                    .await
                    .unwrap();
                assert_eq!(all_standup["archivedHidden"], 0);
                assert_eq!(all_standup["archivedRunsHidden"], 0);
                assert!(all_standup["completed"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["taskUuid"] == archived_task));

                // Corruption of the reader-state file degrades every reader
                // above to "nothing is archived" -- it never fails the query.
                fs::write(
                    crate::reader_state::reader_state_path(&paths.data_dir),
                    b"not json at all\n",
                )
                .unwrap();
                let degraded = daemon
                    .handler
                    .query("query.run", Some(json!({"id": ARCHIVED_RUN})))
                    .await
                    .unwrap();
                assert_eq!(degraded["archived"], false);
                let degraded_standup = daemon
                    .handler
                    .query("query.standup", Some(json!({})))
                    .await
                    .unwrap();
                assert_eq!(degraded_standup["archivedHidden"], 0);
                assert_eq!(degraded_standup["archivedRunsHidden"], 0);

                // And the witness ledger is entirely indifferent: corrupting
                // reader-state does not touch it or its verification.
                let (report, _) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(report.ok);
            })
            .await;
    }

    /// #389 MEDIUM-6: `query.jobs`'s pagination cache-key fingerprint must
    /// include `archived`, so a cursor minted under one archived selection
    /// can never be followed under the other -- an operator would otherwise
    /// be served rows their own `--archived`/`--no-archived` filter says are
    /// absent. Nothing short of driving a real cursor through the daemon
    /// pins this: `PageCache`'s own fingerprint-mismatch mechanism is
    /// generic (`pagination::tests::cursors_are_snapshot_bound_and_expire_explicitly`
    /// proves the cache itself), but nothing previously asserted that the
    /// `query.jobs` RPC handler actually feeds `archived` into that
    /// mechanism -- deleting `"archived": params.archived,` from its
    /// fingerprint `json!` block passed all 676 tests.
    #[tokio::test(flavor = "current_thread")]
    async fn query_jobs_cursor_from_one_archived_selection_is_refused_under_the_other() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;

                // Two jobs, so a `limit: 1` page always mints a `nextCursor`.
                for _ in 0..2 {
                    daemon
                        .handler
                        .enqueue_as_client(Some(json!({
                            "argv": ["true"],
                            "pool": "slot",
                            "adapter": "shell",
                            "source": "manual",
                            "evidence": ["exit:0"]
                        })))
                        .await
                        .unwrap();
                }

                let first_page = daemon
                    .handler
                    .query(
                        "query.jobs",
                        Some(json!({"limit": 1, "archived": false})),
                    )
                    .await
                    .unwrap();
                let cursor = first_page["nextCursor"]
                    .as_str()
                    .expect("two rows at limit 1 must mint a continuation cursor")
                    .to_owned();

                // Following that cursor under the SAME archived selection
                // works.
                let same_selection = daemon
                    .handler
                    .query(
                        "query.jobs",
                        Some(json!({"cursor": cursor, "archived": false})),
                    )
                    .await
                    .unwrap();
                assert_eq!(same_selection["items"].as_array().unwrap().len(), 1);

                // Following it under the OPPOSITE archived selection must be
                // refused, not silently served -- exactly the property MEDIUM-6
                // found unbound.
                let error = daemon
                    .handler
                    .query(
                        "query.jobs",
                        Some(json!({"cursor": cursor, "archived": true})),
                    )
                    .await
                    .unwrap_err();
                assert_eq!(error.code, WireErrorCode::InvalidParams);
                assert!(
                    error.message.contains("different query"),
                    "{}",
                    error.message
                );
            })
            .await;
    }

    /// Seed a durable corpus at estate scale: `terminal` completed rows (event
    /// file + lifecycle trail + witness Pass) and `queued` rows with no
    /// witness, which recovery re-presents as live jobs. Event files and
    /// lifecycle lines are written directly in their durable formats — the
    /// fixture needs the bytes on disk, not the writers' per-line fsyncs.
    fn seed_scale_corpus(root: &Path, terminal: usize, queued: usize) -> (DaemonPaths, Vec<Uuid>) {
        let paths = DaemonPaths {
            socket: root.join("run/tally.sock"),
            state_dir: root.join("state"),
            data_dir: root.join("data"),
        };
        prepare_paths(&paths).unwrap();
        assert_eq!(bump_epoch(&paths.state_dir).unwrap(), 1);
        let events_dir = paths.events_dir();
        fs::create_dir_all(&events_dir).unwrap();

        let mut ledger = WitnessLedger::open(paths.witness_path()).unwrap();
        let mut lifecycle = Vec::new();
        let mut sequence = 0_u64;
        let mut realtime = 1_786_000_000_000_000_u64;
        let mut uuids = Vec::with_capacity(terminal + queued);
        let mut append_lifecycle = |row: &RowSeed, event: TallyEvent, sequence: &mut u64, realtime: &mut u64| {
            let terminal_event = matches!(event, TallyEvent::Completed | TallyEvent::Failed);
            let fields = EmitEvent {
                event,
                task_uuid: row.uuid.to_string(),
                task_ref: None,
                journal_scope: None,
                projection_scope: None,
                class: row.priority,
                source: row.source,
                message: Some(format!("scale fixture {event}")),
                agent: Some(row.adapter.clone()),
                session_ref: None,
                unit: Some(format!("tally-job-{}.service", row.uuid)),
                exit_code: terminal_event.then_some(0),
                stderr_tail: None,
                stderr_truncated: None,
                gpu_seconds: terminal_event.then_some(0.0),
                context_tokens: None,
                context_window: None,
                artifact_hash: (event == TallyEvent::Completed)
                    .then(|| format!("sha256:{}", "a".repeat(64))),
                evidence: event.is_evidence().then(|| "exit:0".to_owned()),
                attempt: Some(1),
                lease_epoch: Some(1),
                labor_class: Some(LaborClass::Fresh),
                job_id: Some(row.uuid.to_string()),
                parent: None,
                pools: Some(row.pools.clone()),
                executor: None,
            }
            .into_fields()
            .unwrap();
            *sequence += 1;
            *realtime += 1;
            let cursor = crate::history::lifecycle_cursor(*sequence);
            let record = crate::history::LifecycleRecord {
                schema_version: crate::history::LIFECYCLE_SCHEMA_VERSION,
                sequence: *sequence,
                event_id: cursor.clone(),
                cursor,
                observed_at: chrono::DateTime::<Utc>::from_timestamp_micros(
                    i64::try_from(*realtime).unwrap(),
                )
                .unwrap()
                .to_rfc3339_opts(SecondsFormat::Micros, true),
                realtime_us: *realtime,
                fields,
            };
            lifecycle.extend_from_slice(&serde_json::to_vec(&record).unwrap());
            lifecycle.push(b'\n');
        };

        for index in 0..(terminal + queued) {
            let uuid = Uuid::new_v4();
            uuids.push(uuid);
            let row = durable_row(uuid, &format!("scale-{index}"), 1);
            let event = DurableEnqueueEvent::new(row.clone()).unwrap();
            fs::write(
                events_dir.join(format!("{}.enqueue.json", event.event_id)),
                serde_json::to_vec(&event).unwrap(),
            )
            .unwrap();
            if index < terminal {
                for lifecycle_event in [
                    TallyEvent::Enqueued,
                    TallyEvent::Dispatched,
                    TallyEvent::Started,
                    TallyEvent::EvidencePass,
                    TallyEvent::Completed,
                ] {
                    append_lifecycle(&row, lifecycle_event, &mut sequence, &mut realtime);
                }
                append_fixture_witness(
                    &mut ledger,
                    &row,
                    "2026-08-05T12:00:00.000Z",
                    Verdict::Pass,
                    0,
                    1,
                    1,
                );
            } else {
                append_lifecycle(&row, TallyEvent::Enqueued, &mut sequence, &mut realtime);
            }
        }
        drop(ledger);
        fs::write(paths.data_dir.join(crate::history::LIFECYCLE_FILE), lifecycle).unwrap();
        (paths, uuids)
    }

    /// #431 acceptance: with a ~30k-row durable corpus, the dispatch loop's
    /// maximum absence over a sustained scheduling period stays under a
    /// single-digit-second bound, and RPCs issued during that period answer
    /// within the 10 s client deadline the estate's readers use.
    ///
    /// Shape of the proof:
    /// - The corpus is 30,000 terminal rows plus 40 queued rows that recovery
    ///   re-presents; the queued cohort dispatches, runs, and completes through
    ///   the live loop, and twelve more admissions land mid-storm, so
    ///   admission, scheduling, and completion are all exercised while the
    ///   query storm runs.
    /// - Clients run on their own OS threads with their own runtimes, the way
    ///   estate clients are their own processes, so client-side JSON decoding
    ///   cannot pollute the loop measurement.
    /// - Loop absence is measured at tick boundaries with the loop's own lease
    ///   tick (100 ms cadence when live): the hook stamps an instant per tick,
    ///   and the maximum inter-tick gap is the loop's maximum absence as its
    ///   own watchdog witness would see it.
    /// - The bound is 5 s. That is deliberately far above the healthy ~100 ms
    ///   cadence (this suite shares hosts with compiling siblings and the
    ///   corpus makes some O(live)+fsync arm bodies real) and far below the
    ///   60–183 s absences measured live on 2026-08-07 — which one inline
    ///   corpus-scale query reproduces, so the unfixed loop fails this test by
    ///   minutes, not by margin.
    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_loop_stays_live_under_query_storm_at_estate_scale() {
        const STORM: Duration = Duration::from_secs(6);
        const ABSENCE_BOUND: Duration = Duration::from_secs(5);
        const CLIENT_DEADLINE: Duration = Duration::from_secs(10);
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let (paths, uuids) = seed_scale_corpus(temp.path(), 30_000, 40);
                let executor = direct_executor(&paths.state_dir)
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
                let (tick_tx, mut tick_rx) = mpsc::unbounded_channel();
                let (_release_tx, release_rx) = watch::channel(true);
                daemon.lease_tick_hook = Some(LeaseTickHook {
                    started: tick_tx,
                    release: release_rx,
                });
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));

                let ticks = Rc::new(RefCell::new(Vec::<Instant>::new()));
                let collected = ticks.clone();
                let collector = tokio::task::spawn_local(async move {
                    while tick_rx.recv().await.is_some() {
                        collected.borrow_mut().push(Instant::now());
                    }
                });

                // Bracket the wall-clock storm with the loop's own samples.
                // Without a sample before the client threads start, shutting
                // down immediately after they finish can leave the observed
                // span one tick short even though the loop stayed live.
                await_positive_progress("initial lease tick before query storm", async {
                    while ticks.borrow().is_empty() {
                        tokio::task::yield_now().await;
                    }
                })
                .await;

                // Query storm: corpus-scale reads back to back for the whole
                // period, from a dedicated OS thread.
                let storm_socket = paths.socket.clone();
                let probe_uuid = uuids[0].to_string();
                let storm_uuid = probe_uuid.clone();
                let storm = std::thread::spawn(move || {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap()
                        .block_on(async move {
                            let client = RpcClient::connect(&storm_socket).await.unwrap();
                            let started = Instant::now();
                            let mut completed = 0_usize;
                            while started.elapsed() < STORM {
                                client
                                    .call("query.jobs", Some(json!({"limit": 100})))
                                    .await
                                    .unwrap();
                                client
                                    .call("query.job", Some(json!({"id": storm_uuid})))
                                    .await
                                    .unwrap();
                                client.call("query.status", Some(json!({}))).await.unwrap();
                                completed += 3;
                            }
                            completed
                        })
                });
                // Scheduling activity: admissions landing mid-storm, each of
                // which must not wait behind a corpus-scale query.
                let sched_socket = paths.socket.clone();
                let scheduler = std::thread::spawn(move || {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap()
                        .block_on(async move {
                            let client = RpcClient::connect(&sched_socket).await.unwrap();
                            let mut worst = Duration::ZERO;
                            for round in 0..12 {
                                let started = Instant::now();
                                client
                                    .call(
                                        "queue.enqueue",
                                        Some(json!({
                                            "argv": ["true"],
                                            "pool": "slot",
                                            "adapter": "shell",
                                            "source": "manual",
                                            "evidence": ["exit:0"],
                                            "dedupKey": format!("storm-{round}"),
                                        })),
                                    )
                                    .await
                                    .unwrap();
                                worst = worst.max(started.elapsed());
                                tokio::time::sleep(Duration::from_millis(300)).await;
                            }
                            worst
                        })
                });
                // The acceptance probe: a `query.job` read issued in the middle
                // of the storm, with the client deadline the estate's readers
                // use. On the unfixed loop this is the read that timed out.
                let probe_socket = paths.socket.clone();
                let probe = std::thread::spawn(move || {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap()
                        .block_on(async move {
                            let client = RpcClient::connect(&probe_socket).await.unwrap();
                            tokio::time::sleep(STORM / 3).await;
                            let mut elapsed = Vec::new();
                            for _ in 0..2 {
                                let started = Instant::now();
                                client
                                    .call_with_deadline(
                                        "query.job",
                                        Some(json!({"id": probe_uuid})),
                                        CLIENT_DEADLINE,
                                    )
                                    .await
                                    .unwrap();
                                elapsed.push(started.elapsed());
                                tokio::time::sleep(STORM / 3).await;
                            }
                            elapsed
                        })
                });

                let queries = tokio::task::spawn_blocking(move || storm.join().unwrap())
                    .await
                    .unwrap();
                let worst_enqueue = tokio::task::spawn_blocking(move || scheduler.join().unwrap())
                    .await
                    .unwrap();
                let probe_elapsed = tokio::task::spawn_blocking(move || probe.join().unwrap())
                    .await
                    .unwrap();

                // Likewise retain the daemon until its samples cover the full
                // interval. The max-gap assertion below remains the liveness
                // verdict; this wait only makes its measurement window real.
                await_positive_progress("lease ticks covering the query storm", async {
                    loop {
                        let covers_storm = {
                            let ticks = ticks.borrow();
                            ticks.len() >= 2
                                && *ticks.last().unwrap() - *ticks.first().unwrap() >= STORM
                        };
                        if covers_storm {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await;

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
                collector.abort();

                assert!(queries >= 3, "the storm must actually run corpus-scale queries");
                for elapsed in &probe_elapsed {
                    assert!(
                        *elapsed < CLIENT_DEADLINE,
                        "a query.job issued during the storm took {elapsed:?}, past the \
                         {CLIENT_DEADLINE:?} client deadline"
                    );
                }
                assert!(
                    worst_enqueue < CLIENT_DEADLINE,
                    "an admission waited {worst_enqueue:?} behind corpus-scale query work"
                );
                let ticks = ticks.borrow();
                // The instrument must have measured across the storm window;
                // how many samples that took is the host's business. A fixed
                // sample count flaked on a loaded gate host (19 live ticks in
                // ~10 s under a full parallel suite), while a deaf loop is
                // caught by the max-gap bound below regardless of how many
                // ticks it eventually produced.
                assert!(
                    ticks.len() >= 2,
                    "the tick instrument produced too few samples to measure absence ({} ticks)",
                    ticks.len()
                );
                let span = *ticks.last().unwrap() - *ticks.first().unwrap();
                assert!(
                    span >= STORM,
                    "tick samples span {span:?}, less than the {STORM:?} storm window"
                );
                let mut max_gap = Duration::ZERO;
                for pair in ticks.windows(2) {
                    max_gap = max_gap.max(pair[1] - pair[0]);
                }
                assert!(
                    max_gap < ABSENCE_BOUND,
                    "the dispatch loop was absent for {max_gap:?} during the storm \
                     (bound {ABSENCE_BOUND:?}, {} ticks observed)",
                    ticks.len()
                );
            })
            .await;
    }

    /// #431: an RPC read that races a scheduling mutation answers from the
    /// frozen snapshot its projection took — the mutation is either entirely
    /// invisible to that answer or entirely visible to a later one, never
    /// partially visible. This is the torn-read discipline that moving query
    /// construction off the dispatch thread must keep.
    #[tokio::test(flavor = "current_thread")]
    async fn query_projection_is_frozen_against_racing_mutations() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;

                let before = daemon.handler.query_projection().await.unwrap();
                let rows_before = before.rows.len();

                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let new_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();

                // The projection taken before the admission is immutable: the
                // mutation cannot appear in any of its facts, so a query built
                // from it cannot observe the admission half-applied.
                assert_eq!(before.rows.len(), rows_before);
                assert!(!before.rows.iter().any(|row| row.task_uuid == new_uuid));
                assert!(!before
                    .live
                    .iter()
                    .any(|fact| fact.anchor == new_uuid));
                let frozen = query_jobs_v2(
                    &before.details,
                    &before.live,
                    &before.history,
                    &before.witness,
                    &BTreeMap::new(),
                    &JobsFilter::default(),
                    &before.membership,
                )
                .unwrap();
                assert!(!frozen.items.iter().any(|item| item.anchor == new_uuid));

                // A projection taken after the admission sees it whole.
                let after = daemon.handler.query_projection().await.unwrap();
                assert_eq!(after.rows.len(), rows_before + 1);
                assert!(after.rows.iter().any(|row| row.task_uuid == new_uuid));
                assert!(after.live.iter().any(|fact| fact.anchor == new_uuid));
            })
            .await;
    }

    /// #431 repair (eval finding D1-1): the trace decoration on job envelopes
    /// is pinned. `query.jobs` decorates every item through the per-anchor
    /// lane index and `query.job` decorates its single summary; both surfaces
    /// were rewritten by #431 and neither was asserted anywhere, so emptying
    /// the index — or deleting `query.job`'s assignment outright — shipped
    /// every job as "no trace metadata" with the whole suite green.
    ///
    /// Positive shape: a completed job under a trace-declaring adapter with
    /// retained captures reports `trace.available == true` on both verbs.
    /// Negative shape: a job under an adapter with no declared trace reports
    /// exactly `adapter-does-not-declare-a-provider-trace` — the reason that
    /// proves its lanes were found and consulted, where a broken index would
    /// report `no-attempt-trace-metadata` instead.
    #[tokio::test(flavor = "current_thread")]
    async fn query_envelopes_report_trace_availability_from_the_lane_index() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let program = temp.path().join("traced-agent");
                crate::test_support::install_shell_program(
                    &program,
                    concat!(
                        "#!/bin/sh\n",
                        "printf '%s\\n' '{\"event\":{\"session_id\":\"trace-session\"}}'\n"
                    ),
                );
                let mut config = one_pool_config();
                let mut adapter = structured_adapter(&program);
                adapter.trace = Some(AdapterTrace {
                    stream: ScrapeStream::Stdout,
                    framing: TraceFraming::JsonLines,
                });
                config.adapters.insert("traced".to_owned(), adapter);
                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();

                let mut finished_uuids = Vec::new();
                for adapter in ["traced", "shell"] {
                    let admitted = daemon
                        .handler
                        .enqueue_as_client(Some(json!({
                            "argv": if adapter == "traced" {
                                json!(["ignored-by-template"])
                            } else {
                                json!(["true"])
                            },
                            "pool": "slot",
                            "adapter": adapter,
                            "source": "manual",
                            "evidence": ["exit:0"]
                        })))
                        .await
                        .unwrap();
                    let finished = await_positive_progress(
                        "trace-index fixture completion",
                        daemon.completion_rx.recv(),
                    )
                    .await
                    .unwrap();
                    daemon.finish_job(finished).await.unwrap();
                    let terminal = daemon
                        .handler
                        .await_job(Some(json!({"task_uuid": admitted["task_uuid"]})))
                        .await
                        .unwrap();
                    assert_eq!(terminal["verdict"], "pass");
                    finished_uuids.push(admitted["task_uuid"].as_str().unwrap().to_owned());
                }
                let (traced_uuid, plain_uuid) = (&finished_uuids[0], &finished_uuids[1]);

                let job = daemon
                    .handler
                    .query("query.job", Some(json!({"id": traced_uuid})))
                    .await
                    .unwrap();
                assert_eq!(
                    job["job"]["trace"]["available"], true,
                    "query.job must report the retained trace: {}",
                    job["job"]["trace"]
                );

                let jobs = daemon
                    .handler
                    .query("query.jobs", Some(json!({})))
                    .await
                    .unwrap();
                let item = |uuid: &str| {
                    jobs["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| item["taskUuid"] == uuid)
                        .unwrap()
                        .clone()
                };
                assert_eq!(
                    item(traced_uuid)["trace"]["available"],
                    true,
                    "query.jobs must decorate items from the lane index: {}",
                    item(traced_uuid)["trace"]
                );
                assert_eq!(
                    item(plain_uuid)["trace"]["reason"],
                    "adapter-does-not-declare-a-provider-trace",
                    "an adapter without a trace still has its lanes consulted: {}",
                    item(plain_uuid)["trace"]
                );
            })
            .await;
    }


    /// #420 (2): `cancel`'s already-terminal answer derives `was` from the
    /// query fact that admitted the retired job, instead of asserting
    /// `completed`. A row recovered as `Deleted` — latest witness verdict
    /// `cancelled` — answers with the same `deleted-cache` label its query
    /// projection uses; reporting it `completed` claimed a verdict the daemon
    /// does not hold.
    #[tokio::test(flavor = "current_thread")]
    async fn cancel_reports_deleted_cache_for_a_row_recovered_as_deleted() {
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
                assert_eq!(bump_epoch(&paths.state_dir).unwrap(), 1);
                let row = durable_row(Uuid::new_v4(), "deleted-cache-answer", 1);
                write_enqueue_event_atomic(
                    &paths.events_dir(),
                    &DurableEnqueueEvent::new(row.clone()).unwrap(),
                )
                .unwrap();
                let mut ledger = WitnessLedger::open(paths.witness_path()).unwrap();
                append_fixture_witness(
                    &mut ledger,
                    &row,
                    "2026-08-06T12:00:00.000Z",
                    Verdict::Cancelled,
                    1,
                    1,
                    1,
                );
                drop(ledger);

                let executor = direct_executor(&paths.state_dir)
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
                {
                    let context = daemon.handler.context.read().await;
                    assert!(!context.jobs.contains_key(&row.uuid));
                    assert_eq!(context.query_rows[&row.uuid].status, RowStatus::Deleted);
                }

                let already = daemon
                    .handler
                    .cancel(Some(json!({"task_uuid": row.uuid.to_string(), "force": true})))
                    .await
                    .unwrap();
                assert_eq!(already["already_terminal"], true);
                assert_eq!(already["affected"], 0);
                assert_eq!(
                    already["was"], "deleted-cache",
                    "the label is the query fact's status, not a constant"
                );
            })
            .await;
    }

    /// Timing probe for #431: what does each dispatch-loop-resident operation
    /// cost against a corpus at estate scale? Run with
    /// `TALLY_EXP_CORPUS=30000 cargo test -p tally-core exp_corpus -- --ignored --nocapture`.
    #[tokio::test(flavor = "current_thread")]
    #[ignore]
    async fn exp_corpus_scale_timing() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let terminal = std::env::var("TALLY_EXP_CORPUS")
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(30_000);
                let queued = 60;
                let temp = tempdir().unwrap();
                let started = Instant::now();
                let (paths, uuids) = seed_scale_corpus(temp.path(), terminal, queued);
                eprintln!("seed: {terminal} terminal + {queued} queued rows in {:?}", started.elapsed());

                let executor = direct_executor(&paths.state_dir)
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let started = Instant::now();
                let daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                eprintln!("open: {:?}", started.elapsed());
                eprintln!(
                    "phases: {:?}",
                    daemon.startup.as_ref().map(|timeline| timeline.phase_names())
                );

                for method in ["query.status", "query.standup", "query.render"] {
                    let started = Instant::now();
                    let result = daemon.handler.query(method, Some(json!({}))).await;
                    eprintln!("{method}: {:?} ok={}", started.elapsed(), result.is_ok());
                }
                let started = Instant::now();
                let result = daemon.handler.query("query.jobs", Some(json!({}))).await;
                eprintln!("query.jobs: {:?} ok={}", started.elapsed(), result.is_ok());
                let started = Instant::now();
                let result = daemon
                    .handler
                    .query("query.job", Some(json!({"id": uuids[0].to_string()})))
                    .await;
                eprintln!("query.job: {:?} ok={}", started.elapsed(), result.is_ok());
                let started = Instant::now();
                let result = daemon
                    .handler
                    .query("query.run", Some(json!({"id": uuids[0].to_string()})))
                    .await;
                eprintln!("query.run(unknown): {:?} ok={}", started.elapsed(), result.is_ok());

                // Repeat, now warm.
                for method in ["query.status", "query.standup"] {
                    let started = Instant::now();
                    let result = daemon.handler.query(method, Some(json!({}))).await;
                    eprintln!("warm {method}: {:?} ok={}", started.elapsed(), result.is_ok());
                }

                // Admission with the corpus resident.
                let started = Instant::now();
                let admitted = daemon
                    .handler
                    .enqueue_as_client(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await;
                eprintln!("enqueue: {:?} ok={}", started.elapsed(), admitted.is_ok());

                // A query right after the mutation (cold snapshot rebuild).
                let started = Instant::now();
                let result = daemon.handler.query("query.status", Some(json!({}))).await;
                eprintln!("post-mutation query.status: {:?} ok={}", started.elapsed(), result.is_ok());

                // One lease tick with the queued cohort resident.
                let started = Instant::now();
                Daemon::tick_leases_at(daemon.handler.clone(), Utc::now())
                    .await
                    .unwrap();
                eprintln!("tick_leases: {:?}", started.elapsed());
                let started = Instant::now();
                Daemon::tick_leases_at(daemon.handler.clone(), Utc::now())
                    .await
                    .unwrap();
                eprintln!("tick_leases again: {:?}", started.elapsed());
            })
            .await;
    }
}
