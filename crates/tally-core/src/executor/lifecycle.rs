use super::*;

#[derive(Debug)]
pub(super) struct UnitReservation {
    _file: File,
}

impl Drop for UnitReservation {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self._file);
    }
}

pub(super) struct LaunchingUnitGuard {
    key: Uuid,
    registry: Arc<Mutex<HashMap<Uuid, watch::Receiver<bool>>>>,
    receiver: watch::Receiver<bool>,
    completed: watch::Sender<bool>,
    armed: bool,
}

impl LaunchingUnitGuard {
    fn mark_complete(&mut self) {
        if !self.armed {
            return;
        }
        let _ = self.completed.send(true);
        if let Ok(mut registry) = self.registry.lock() {
            if registry
                .get(&self.key)
                .is_some_and(|receiver| receiver.same_channel(&self.receiver))
            {
                registry.remove(&self.key);
            }
        }
        self.armed = false;
    }
}

impl Drop for LaunchingUnitGuard {
    fn drop(&mut self) {
        self.mark_complete();
    }
}

#[derive(Clone)]
pub(super) struct DirectProcess {
    pgid: i32,
    invocation_id: String,
    stopped: watch::Receiver<bool>,
}

pub(super) struct DirectProcessGuard {
    key: Uuid,
    pgid: i32,
    registry: Arc<Mutex<HashMap<Uuid, DirectProcess>>>,
    stopped: watch::Sender<bool>,
    armed: bool,
}

impl DirectProcessGuard {
    fn mark_stopped(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(mut registry) = self.registry.lock() {
            if registry
                .get(&self.key)
                .is_some_and(|process| process.pgid == self.pgid)
            {
                registry.remove(&self.key);
            }
        }
        let _ = self.stopped.send(true);
        self.armed = false;
    }
}

impl Drop for DirectProcessGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Direct fallback is its own process group. This is also the unwind and
        // daemon-shutdown backstop, so descendants cannot outlive a dropped
        // execution future.
        let result = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                eprintln!(
                    "tally: cannot kill direct process group {}: {error}",
                    self.pgid
                );
            }
        }
        self.mark_stopped();
    }
}

impl Executor {
    pub async fn reclaim_identity_exact(
        &self,
        identity: &ExecutionIdentity,
        expected_invocation_id: Option<&str>,
    ) -> Result<(), ExecutorError> {
        let mut launching = None;
        let mut launch_deadline = None;
        let mut attempt = 0_u16;
        loop {
            let direct = self
                .direct_processes
                .lock()
                .map_err(|_| ExecutorError::UnitControl {
                    unit: self.unit_name(identity),
                    detail: "direct-process registry is poisoned".to_owned(),
                })?
                .get(identity.unit_uuid())
                .cloned();
            if let Some(mut direct) = direct {
                if let Some(expected) = expected_invocation_id {
                    if expected != direct.invocation_id {
                        return Err(ExecutorError::AdoptedInvocationMismatch {
                            unit: self.unit_name(identity),
                            expected: expected.to_owned(),
                            observed: Some(direct.invocation_id),
                        });
                    }
                }
                let result = unsafe { libc::kill(-direct.pgid, libc::SIGKILL) };
                if result != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        return Err(ExecutorError::UnitControl {
                            unit: self.unit_name(identity),
                            detail: format!(
                                "cannot kill direct process group {}: {error}",
                                direct.pgid
                            ),
                        });
                    }
                }
                while !*direct.stopped.borrow() {
                    direct
                        .stopped
                        .changed()
                        .await
                        .map_err(|_| ExecutorError::UnitControl {
                            unit: self.unit_name(identity),
                            detail: format!(
                                "direct process group {} lost its stop acknowledgement",
                                direct.pgid
                            ),
                        })?;
                }
                return Ok(());
            }

            let fact = self.inspect_identity_async(identity).await?;
            if let Some(expected) = expected_invocation_id {
                if fact.state != LocalUnitState::Absent
                    && fact.invocation_id.as_deref() != Some(expected)
                {
                    return Err(ExecutorError::AdoptedInvocationMismatch {
                        unit: fact.unit,
                        expected: expected.to_owned(),
                        observed: fact.invocation_id,
                    });
                }
            }
            if fact.state == LocalUnitState::Running {
                let mut command = Command::new(&self.systemctl);
                command
                    .kill_on_drop(true)
                    .args(["--user", "stop", "--", &fact.unit]);
                let output =
                    command
                        .output()
                        .await
                        .map_err(|source| ExecutorError::UnitControl {
                            unit: fact.unit.clone(),
                            detail: format!("cannot spawn {}: {source}", self.systemctl.display()),
                        })?;
                if !output.status.success() {
                    return Err(ExecutorError::UnitControl {
                        unit: fact.unit,
                        detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                    });
                }
                return Ok(());
            }
            let reserved = self.identity_is_reserved(identity).map_err(|error| {
                ExecutorError::UnitControl {
                    unit: fact.unit.clone(),
                    detail: format!("cannot verify execution reservation: {error}"),
                }
            })?;
            if !reserved {
                return Ok(());
            }
            if launch_deadline.is_none() {
                launching = self
                    .launching_units
                    .lock()
                    .map_err(|_| ExecutorError::UnitControl {
                        unit: fact.unit.clone(),
                        detail: "launch registry is poisoned".to_owned(),
                    })?
                    .get(identity.unit_uuid())
                    .cloned();
                if launching.is_some() {
                    launch_deadline = Some(tokio::time::Instant::now() + LAUNCH_VISIBILITY_TIMEOUT);
                }
            }
            if let Some(deadline) = launch_deadline {
                if tokio::time::Instant::now() >= deadline {
                    return Err(ExecutorError::UnitControl {
                        unit: fact.unit,
                        detail: format!(
                            "execution launch did not become reclaimable within {} seconds",
                            LAUNCH_VISIBILITY_TIMEOUT.as_secs()
                        ),
                    });
                }
            } else if attempt == 200 {
                return Err(ExecutorError::UnitControl {
                    unit: fact.unit,
                    detail: "execution reservation is still held without a reclaimable unit"
                        .to_owned(),
                });
            }
            // The reservation is acquired before either backend becomes
            // externally visible. Give that bounded transition time to publish
            // a systemd unit or direct-process registry entry, then reclaim it.
            if let Some(receiver) = launching.as_mut() {
                let launch_completed = tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(5)) => false,
                    changed = receiver.changed() => {
                        changed.is_err() || *receiver.borrow_and_update()
                    }
                };
                if launch_completed {
                    launching = None;
                }
            } else {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            attempt = attempt.saturating_add(1);
        }
    }

    pub(super) fn identity_is_reserved(
        &self,
        identity: &ExecutionIdentity,
    ) -> Result<bool, ExecutorError> {
        let exits = self
            .paths(identity)
            .exit_record
            .parent()
            .expect("exit path always has a parent")
            .to_owned();
        let lock_path = exits.join(format!("{}.lock", identity.unit_uuid()));
        let file = match OpenOptions::new()
            .create(false)
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(source) => return Err(io_error(&lock_path, source)),
        };
        match file.try_lock_exclusive() {
            Ok(()) => Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
            Err(source) => Err(io_error(&lock_path, source)),
        }
    }

    pub async fn execute(
        &self,
        request: ExecutionRequest,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let (preflight, runtime) = self.prepare_git_ai(&request).await?;
        let result = match self.execute_raw(request.clone(), runtime.as_ref()).await {
            Ok(outcome) => {
                self.finalize_outcome(outcome, &request, preflight.as_ref(), runtime.as_ref())
                    .await
            }
            Err(error) => Err(error),
        };
        if let Some(runtime) = runtime {
            runtime.shutdown().await;
        }
        result
    }

    pub(super) async fn execute_raw(
        &self,
        mut request: ExecutionRequest,
        git_ai_runtime: Option<&git_ai::PrivateDaemon>,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        self.validate_request(&request)?;
        self.materialize_brief(&mut request)?;
        self.validate_request(&request)?;
        let observed = self.inspect_identity_async(&request.identity).await?;
        let absent_without_exit = match observed.state {
            LocalUnitState::Absent => true,
            LocalUnitState::Exited => {
                let record = observed
                    .exit_record
                    .ok_or_else(|| ExecutorError::UnitProbe {
                        unit: observed.unit.clone(),
                        detail: "exited observation has no durable unit-exit record".to_owned(),
                    })?;
                record.validate(&observed.unit)?;
                if record.attempt == request.attempt && record.lease_epoch == request.lease_epoch {
                    let termination = classify_termination(&record)?;
                    return Ok(ExecutionOutcome {
                        unit: observed.unit,
                        backend: ExecutionBackend::Adopted,
                        paths: self.paths(&request.identity),
                        record,
                        termination,
                        evidence_gate: None,
                        semantic_completion: None,
                        result_revision: None,
                        authorship: None,
                        authorship_sessions: None,
                        host_id: self.host_id.clone(),
                        captures_available: true,
                    });
                }
                if observed.loaded {
                    return Err(ExecutorError::ExistingUnit {
                        unit: observed.unit,
                        state: LocalUnitState::Exited,
                    });
                }
                let immediately_precedes = record
                    .attempt
                    .checked_add(1)
                    .is_some_and(|next| next == request.attempt)
                    && record.lease_epoch <= request.lease_epoch;
                if !immediately_precedes {
                    return Err(ExecutorError::ExistingUnit {
                        unit: observed.unit,
                        state: LocalUnitState::Exited,
                    });
                }
                false
            }
            LocalUnitState::Running | LocalUnitState::InactiveWithoutRecord => {
                return Err(ExecutorError::ExistingUnit {
                    unit: observed.unit,
                    state: observed.state,
                });
            }
        };
        let reservation = self.reserve(&request.identity)?;
        let mut launching = self.register_launch(&request.identity)?;
        // This marker is fsynced before systemd-run can create the unit. If a
        // retry finds the same generation with neither a unit nor an exit
        // record, the previous helper may have launched work that was lost
        // with the worker. Replaying argv would be a possible duplicate, so
        // preserve the coordinator's lease and require explicit recovery.
        if absent_without_exit
            && self.capture_generation_matches(
                &request.identity,
                request.attempt,
                request.lease_epoch,
            )?
        {
            return Err(ExecutorError::IndeterminatePriorLaunch {
                unit: self.unit_name(&request.identity),
                attempt: request.attempt,
                lease_epoch: request.lease_epoch,
            });
        }
        let paths = self.prepare_paths(&request.identity)?;
        write_capture_generation(
            &paths.capture_generation,
            CaptureGeneration {
                attempt: request.attempt,
                lease_epoch: request.lease_epoch,
            },
        )?;
        self.materialize_gh_context(&request)?;
        self.prepare_hardening_files(&request)?;
        let args = self.build_systemd_argv_with_git_ai(&request, git_ai_runtime)?;
        let output = match Command::new(&self.systemd_run).args(&args).output().await {
            Ok(output) => {
                launching.mark_complete();
                output
            }
            Err(source)
                if source.kind() == std::io::ErrorKind::NotFound && self.allow_direct_fallback =>
            {
                return self.execute_direct(request, paths, git_ai_runtime).await;
            }
            Err(source) => {
                return Err(ExecutorError::Spawn {
                    program: self.systemd_run.clone(),
                    source,
                });
            }
        };

        let unit = self.unit_name(&request.identity);
        let record = match read_exit_record(&paths.exit_record, &unit) {
            Ok(record) => record,
            Err(error) if is_not_found(&error) => {
                // Losing the systemd-run client must never release the caller's
                // lease while the exact transient unit may still be executing.
                drop(reservation);
                if let Err(error) = self.reclaim_identity(&request.identity).await {
                    eprintln!("tally: cannot reclaim {unit} after launcher failure: {error}");
                }
                return Err(ExecutorError::LauncherFailed {
                    status: output.status.code(),
                    stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                });
            }
            Err(error) => return Err(error),
        };
        let termination = classify_termination(&record)?;
        Ok(ExecutionOutcome {
            unit,
            backend: ExecutionBackend::Systemd,
            paths,
            record,
            termination,
            evidence_gate: None,
            semantic_completion: None,
            result_revision: None,
            authorship: None,
            authorship_sessions: None,
            host_id: self.host_id.clone(),
            captures_available: true,
        })
    }

    /// Consume the exact durable exit of an execution recovered as already
    /// running. Unlike `execute`, this path can never turn an absent or stale
    /// observation into a fresh launch.
    pub async fn adopt(
        &self,
        request: ExecutionRequest,
        expected_invocation_id: &str,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let (preflight, runtime) = self.prepare_git_ai(&request).await?;
        let result = match self
            .adopt_raw(request.clone(), expected_invocation_id)
            .await
        {
            Ok(outcome) => {
                self.finalize_outcome(outcome, &request, preflight.as_ref(), runtime.as_ref())
                    .await
            }
            Err(error) => Err(error),
        };
        if let Some(runtime) = runtime {
            runtime.shutdown().await;
        }
        result
    }

    pub(super) async fn prepare_git_ai(
        &self,
        request: &ExecutionRequest,
    ) -> Result<(Option<git_ai::Preflight>, Option<git_ai::PrivateDaemon>), ExecutorError> {
        let Some(execution) = &request.git_ai else {
            return Ok((None, None));
        };
        let mut preflight = git_ai::preflight(execution)
            .await
            .map_err(ExecutorError::GitAiRequired)?;
        let Some(workspace) = &request.workspace else {
            return Ok((Some(preflight), None));
        };
        if matches!(preflight, git_ai::Preflight::Failed { .. }) {
            return Ok((Some(preflight), None));
        }
        let runtime_key = format!(
            "{}:{}:{}",
            request.identity.unit_uuid(),
            request.attempt,
            request.lease_epoch
        );
        let mut repository_write_paths = git_repository_write_paths(&workspace.worktree_path);
        repository_write_paths.push(workspace.worktree_path.clone());
        match git_ai::start_private_daemon(
            execution,
            &preflight,
            git_ai::PrivateDaemonLaunch {
                state_dir: &self.state_dir,
                runtime_key: &runtime_key,
                worktree: &workspace.worktree_path,
                repository_write_paths: &repository_write_paths,
                systemd_run: &self.systemd_run,
                systemctl: &self.systemctl,
                allow_direct_fallback: self.allow_direct_fallback,
            },
        )
        .await
        {
            Ok(runtime) => Ok((Some(preflight), Some(runtime))),
            Err(reason) => {
                preflight = git_ai::runtime_failure(execution, &preflight, reason)
                    .map_err(ExecutorError::GitAiRequired)?;
                Ok((Some(preflight), None))
            }
        }
    }

    pub(super) async fn adopt_raw(
        &self,
        request: ExecutionRequest,
        expected_invocation_id: &str,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        if request.attempt == 0 || request.lease_epoch == 0 || expected_invocation_id.is_empty() {
            return Err(ExecutorError::InvalidRequest(
                "adopted invocation, attempt, and lease epoch must be present".to_owned(),
            ));
        }
        loop {
            let observed = self.inspect_identity_async(&request.identity).await?;
            if observed.state != LocalUnitState::Absent
                && observed.invocation_id.as_deref() != Some(expected_invocation_id)
            {
                return Err(ExecutorError::AdoptedInvocationMismatch {
                    unit: observed.unit,
                    expected: expected_invocation_id.to_owned(),
                    observed: observed.invocation_id,
                });
            }
            match observed.state {
                LocalUnitState::Running => {
                    if observed.attempt != Some(request.attempt)
                        || observed.lease_epoch != Some(request.lease_epoch)
                    {
                        return Err(ExecutorError::AdoptedGenerationMismatch {
                            unit: observed.unit,
                            expected_attempt: request.attempt,
                            expected_lease_epoch: request.lease_epoch,
                            observed_attempt: observed.attempt.unwrap_or_default(),
                            observed_lease_epoch: observed.lease_epoch.unwrap_or_default(),
                        });
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                LocalUnitState::InactiveWithoutRecord => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                LocalUnitState::Absent => {
                    return Err(ExecutorError::AdoptedUnitUnavailable {
                        unit: observed.unit,
                        state: observed.state,
                    });
                }
                LocalUnitState::Exited => {
                    let record = observed
                        .exit_record
                        .ok_or_else(|| ExecutorError::UnitProbe {
                            unit: observed.unit.clone(),
                            detail: "exited observation has no durable unit-exit record".to_owned(),
                        })?;
                    record.validate(&observed.unit)?;
                    if record.attempt != request.attempt
                        || record.lease_epoch != request.lease_epoch
                    {
                        return Err(ExecutorError::AdoptedGenerationMismatch {
                            unit: observed.unit,
                            expected_attempt: request.attempt,
                            expected_lease_epoch: request.lease_epoch,
                            observed_attempt: record.attempt,
                            observed_lease_epoch: record.lease_epoch,
                        });
                    }
                    let termination = classify_termination(&record)?;
                    return Ok(ExecutionOutcome {
                        unit: observed.unit,
                        backend: ExecutionBackend::Adopted,
                        paths: self.paths(&request.identity),
                        record,
                        termination,
                        evidence_gate: None,
                        semantic_completion: None,
                        result_revision: None,
                        authorship: None,
                        authorship_sessions: None,
                        host_id: self.host_id.clone(),
                        captures_available: true,
                    });
                }
            }
        }
    }

    pub(super) async fn finalize_outcome(
        &self,
        mut outcome: ExecutionOutcome,
        request: &ExecutionRequest,
        preflight: Option<&git_ai::Preflight>,
        git_ai_runtime: Option<&git_ai::PrivateDaemon>,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let Some(spec) = &request.gate_manifest else {
            return Ok(outcome);
        };
        let execution = execution_fact(&outcome.termination);
        let mut completion = evaluate_completion(execution, spec);
        if let (Some(git_ai), Some(preflight)) = (&request.git_ai, preflight) {
            let worktree = request
                .workspace
                .as_ref()
                .map(|workspace| workspace.worktree_path.as_path());
            let binding =
                git_ai::bind(git_ai, preflight, &completion, worktree, git_ai_runtime).await;
            outcome.result_revision = binding.result_revision;
            outcome.authorship = binding.authorship;
            outcome.authorship_sessions = binding.authorship_sessions;
            if let Some(reason) = binding.required_failure {
                completion = evaluate_completion(ExecutionFact::failed(reason), spec);
            }
        }
        outcome.semantic_completion = Some(completion);
        Ok(outcome)
    }

    pub(super) fn validate_request(&self, request: &ExecutionRequest) -> Result<(), ExecutorError> {
        if !self.state_dir.is_absolute() {
            return Err(ExecutorError::InvalidRequest(
                "state directory must be absolute".to_owned(),
            ));
        }
        validate_systemd_path(&self.state_dir, "state directory")?;
        if !self.recorder_program.is_absolute() {
            return Err(ExecutorError::InvalidRequest(
                "exit recorder program must be absolute".to_owned(),
            ));
        }
        validate_systemd_path(&self.recorder_program, "exit recorder program")?;
        let mut canonical_pools = request.pools.clone();
        crate::poolset::canonicalize(&mut canonical_pools)
            .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
        if canonical_pools != request.pools {
            return Err(ExecutorError::InvalidRequest(
                "pool set must be in canonical order".to_owned(),
            ));
        }
        if request.lease_epoch == 0 {
            return Err(ExecutorError::InvalidRequest(
                "lease epoch must be positive".to_owned(),
            ));
        }
        if request.attempt == 0 {
            return Err(ExecutorError::InvalidRequest(
                "attempt must be positive".to_owned(),
            ));
        }
        if let Some(git_ai) = &request.git_ai {
            git_ai.validate().map_err(ExecutorError::InvalidRequest)?;
        }
        if let Some(attestation) = &request.exec_attestation {
            attestation
                .validate()
                .map_err(ExecutorError::InvalidRequest)?;
        }
        if request.argv.is_empty() || request.argv[0].is_empty() {
            return Err(ExecutorError::InvalidRequest(
                "argv must contain a non-empty executable".to_owned(),
            ));
        }
        if request.argv.iter().any(|argument| argument.contains('\0')) {
            return Err(ExecutorError::InvalidRequest(
                "argv must not contain NUL bytes".to_owned(),
            ));
        }
        if let Some(hook) = &request.yield_hook {
            if hook.is_empty() || hook[0].is_empty() {
                return Err(ExecutorError::InvalidRequest(
                    "yield hook must contain a non-empty executable".to_owned(),
                ));
            }
            if hook.iter().any(|argument| argument.contains('\0')) {
                return Err(ExecutorError::InvalidRequest(
                    "yield hook must not contain NUL bytes".to_owned(),
                ));
            }
        }
        if request
            .tally_socket
            .as_ref()
            .is_some_and(|socket| socket.is_empty() || socket.contains('\0'))
        {
            return Err(ExecutorError::InvalidRequest(
                "tally socket must be non-empty and contain no NUL bytes".to_owned(),
            ));
        }
        if request.job_token.as_ref().is_some_and(|token| {
            token.len() != 64
                || !token
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err(ExecutorError::InvalidRequest(
                "job token must be exactly 256 bits of lowercase hex".to_owned(),
            ));
        }
        match (
            request.brief_hash.as_deref(),
            request.brief_path.as_deref(),
            request.brief_document.as_ref(),
        ) {
            (None, None, None) => {}
            (Some(hash), Some(path), None) => {
                brief::content_path(&self.state_dir, hash)
                    .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
                if !path.is_absolute() {
                    return Err(ExecutorError::InvalidRequest(
                        "briefPath must be absolute".to_owned(),
                    ));
                }
                validate_systemd_path(path, "brief path")?;
            }
            (Some(hash), None, Some(document)) => {
                let prepared = PreparedBrief::from_value(document.clone())
                    .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
                if prepared.hash() != hash {
                    return Err(ExecutorError::InvalidRequest(format!(
                        "briefDocument hashes to {}, expected {hash}",
                        prepared.hash()
                    )));
                }
            }
            _ => {
                return Err(ExecutorError::InvalidRequest(
                    "briefHash requires exactly one of briefPath or briefDocument".to_owned(),
                ));
            }
        }
        for (name, value) in &request.environment {
            if !valid_environment_name(name)
                || name.starts_with("TALLY_")
                || name == "CREDENTIALS_DIRECTORY"
            {
                return Err(ExecutorError::InvalidRequest(format!(
                    "adapter environment name {name:?} is invalid or reserved"
                )));
            }
            if value.contains('\0') {
                return Err(ExecutorError::InvalidRequest(format!(
                    "adapter environment {name:?} contains a NUL byte"
                )));
            }
        }
        if !(1..=10_000).contains(&request.limits.cpu_weight) {
            return Err(ExecutorError::InvalidRequest(
                "CPUWeight must be in 1..=10000".to_owned(),
            ));
        }
        if request.limits.memory_max_bytes == 0 || request.limits.memory_max_bytes == u64::MAX {
            return Err(ExecutorError::InvalidRequest(
                "MemoryMax must be positive and finite".to_owned(),
            ));
        }
        if let Some(seconds) = request.runtime_max_sec {
            if seconds == 0 || seconds >= u64::MAX / 1_000_000 {
                return Err(ExecutorError::InvalidRequest(
                    "runtimeMaxSec must be positive and fit systemd's microsecond range".to_owned(),
                ));
            }
        }
        if let Some(cwd) = &request.cwd {
            if !cwd.is_absolute() {
                return Err(ExecutorError::InvalidRequest(
                    "working directory must be absolute".to_owned(),
                ));
            }
            validate_systemd_path(cwd, "working directory")?;
        }
        if let Some(gate_manifest) = &request.gate_manifest {
            gate_manifest
                .validate()
                .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
        }
        if let Some(workspace) = &request.workspace {
            workspace
                .validate()
                .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
        }
        for (name, source) in &request.credentials {
            validate_credential_name(name)?;
            if !source.is_absolute() {
                return Err(ExecutorError::InvalidRequest(format!(
                    "credential {name:?} source must be absolute"
                )));
            }
            validate_systemd_path(source, "credential source")?;
        }
        if let Some(origin) = &request.gh_origin {
            origin
                .validate()
                .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
        }
        for path in &request.extra_writable_paths {
            if !path.is_absolute() {
                return Err(ExecutorError::InvalidRequest(format!(
                    "extra writable path {} must be absolute",
                    path.display()
                )));
            }
            validate_systemd_path(path, "extra writable path")?;
        }
        Ok(())
    }

    pub(super) fn materialize_gh_context(
        &self,
        request: &ExecutionRequest,
    ) -> Result<Option<PathBuf>, ExecutorError> {
        let Some(origin) = request
            .gh_origin
            .as_ref()
            .filter(|origin| origin.is_current())
        else {
            return Ok(None);
        };
        let context = origin.context.as_ref().ok_or_else(|| {
            ExecutorError::InvalidRequest("current GitHub origin omitted context".to_owned())
        })?;
        context
            .validate()
            .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
        let path = self.gh_context_path(&request.identity);
        replace_private_file(&path, &serde_json::to_vec(context)?)?;
        Ok(Some(path))
    }

    fn prepare_hardening_files(&self, request: &ExecutionRequest) -> Result<(), ExecutorError> {
        if !matches!(
            request.hardening,
            AdapterHardening::Strict | AdapterHardening::Production
        ) {
            return Ok(());
        }
        if request.exec_attestation.is_some() {
            ensure_private_file(&self.state_dir.join(EXEC_ATTESTATION_LEDGER))?;
        }
        if let Some(manifest) = &request.gate_manifest {
            ensure_private_file(&manifest.path)?;
        }
        Ok(())
    }

    pub(super) fn exec_stop_post(
        &self,
        record: &Path,
        unit: &str,
    ) -> Result<String, ExecutorError> {
        [
            self.recorder_program.as_os_str(),
            OsStr::new("__record-unit-exit"),
            OsStr::new("--record"),
            record.as_os_str(),
            OsStr::new("--unit"),
            OsStr::new(unit),
        ]
        .into_iter()
        .map(quote_systemd_exec_word)
        .collect::<Result<Vec<_>, _>>()
        .map(|words| format!(":{}", words.join(" ")))
    }

    pub(super) fn reserve(
        &self,
        identity: &ExecutionIdentity,
    ) -> Result<UnitReservation, ExecutorError> {
        let paths = self.paths(identity);
        let exits = paths
            .exit_record
            .parent()
            .expect("exit path always has a parent");
        create_private_directory(exits)?;
        let lock_path = exits.join(format!("{}.lock", identity.unit_uuid()));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock_path)
            .map_err(|source| io_error(&lock_path, source))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|source| io_error(&lock_path, source))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(UnitReservation { _file: file }),
            Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                Err(ExecutorError::AlreadyRunning(self.unit_name(identity)))
            }
            Err(source) => Err(io_error(&lock_path, source)),
        }
    }

    pub(super) fn register_launch(
        &self,
        identity: &ExecutionIdentity,
    ) -> Result<LaunchingUnitGuard, ExecutorError> {
        let key = *identity.unit_uuid();
        let (completed, receiver) = watch::channel(false);
        let mut registry = self
            .launching_units
            .lock()
            .map_err(|_| ExecutorError::UnitControl {
                unit: self.unit_name(identity),
                detail: "launch registry is poisoned".to_owned(),
            })?;
        if registry.contains_key(&key) {
            return Err(ExecutorError::UnitControl {
                unit: self.unit_name(identity),
                detail: "execution identity is already registered as launching".to_owned(),
            });
        }
        registry.insert(key, receiver.clone());
        drop(registry);
        Ok(LaunchingUnitGuard {
            key,
            registry: self.launching_units.clone(),
            receiver,
            completed,
            armed: true,
        })
    }

    pub(super) fn prepare_paths(
        &self,
        identity: &ExecutionIdentity,
    ) -> Result<ExecutionPaths, ExecutorError> {
        self.archive_current_capture(identity)?;
        let paths = self.paths(identity);
        let capture = paths
            .stdout
            .parent()
            .expect("capture path always has a parent");
        let exits = paths
            .exit_record
            .parent()
            .expect("exit path always has a parent");
        create_private_directory(capture)?;
        create_private_directory(exits)?;
        create_private_file(&paths.stdout)?;
        create_private_file(&paths.stderr)?;
        match std::fs::remove_file(&paths.exit_record) {
            Ok(()) => sync_directory(exits)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(&paths.exit_record, source)),
        }
        Ok(paths)
    }

    pub(super) async fn execute_direct(
        &self,
        request: ExecutionRequest,
        paths: ExecutionPaths,
        git_ai_runtime: Option<&git_ai::PrivateDaemon>,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        if !request.credentials.is_empty() {
            return Err(ExecutorError::CredentialedFallback);
        }
        let stdout = OpenOptions::new()
            .append(true)
            .open(&paths.stdout)
            .map_err(|source| io_error(&paths.stdout, source))?;
        let stderr = OpenOptions::new()
            .append(true)
            .open(&paths.stderr)
            .map_err(|source| io_error(&paths.stderr, source))?;
        let execution_argv = self.execution_argv(&request);
        let mut command = Command::new(&execution_argv[0]);
        command
            .args(&execution_argv[1..])
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .process_group(0);
        if let Some(cwd) = &request.cwd {
            command.current_dir(cwd);
        }
        for name in environment_to_unset(&request) {
            command.env_remove(name);
        }
        let gh_context_path = request
            .gh_origin
            .as_ref()
            .filter(|origin| origin.is_current())
            .map(|_| self.gh_context_path(&request.identity));
        let mut environment = execution_environment(&request, gh_context_path.as_deref())?;
        if let Some(runtime) = git_ai_runtime {
            environment.extend(runtime.child_environment());
        }
        for (name, value) in environment {
            command.env(name, value);
        }
        let mut child = command.spawn().map_err(|source| ExecutorError::Spawn {
            program: PathBuf::from(&execution_argv[0]),
            source,
        })?;
        let child_pid = child.id();
        let pgid = child_pid
            .and_then(|pid| i32::try_from(pid).ok())
            .ok_or_else(|| ExecutorError::UnitControl {
                unit: self.unit_name(&request.identity),
                detail: "direct child has no representable process-group id".to_owned(),
            })?;
        let (stopped, stopped_rx) = watch::channel(false);
        let key = *request.identity.unit_uuid();
        let invocation_id = format!("direct-{}", child_pid.unwrap_or(0));
        {
            let mut registry =
                self.direct_processes
                    .lock()
                    .map_err(|_| ExecutorError::UnitControl {
                        unit: self.unit_name(&request.identity),
                        detail: "direct-process registry is poisoned".to_owned(),
                    })?;
            if registry.contains_key(&key) {
                let _ = unsafe { libc::kill(-pgid, libc::SIGKILL) };
                return Err(ExecutorError::UnitControl {
                    unit: self.unit_name(&request.identity),
                    detail: "direct-process identity was already registered".to_owned(),
                });
            }
            registry.insert(
                key,
                DirectProcess {
                    pgid,
                    invocation_id: invocation_id.clone(),
                    stopped: stopped_rx,
                },
            );
        }
        let mut direct_guard = DirectProcessGuard {
            key,
            pgid,
            registry: self.direct_processes.clone(),
            stopped,
            armed: true,
        };
        let (record, termination) = if let Some(seconds) = request.runtime_max_sec {
            match tokio::time::timeout(Duration::from_secs(seconds), child.wait()).await {
                Ok(status) => direct_completion(
                    status.map_err(|source| ExecutorError::Spawn {
                        program: PathBuf::from(&execution_argv[0]),
                        source,
                    })?,
                    invocation_id,
                ),
                Err(_) => {
                    terminate_direct_process_group(&mut child, child_pid)
                        .await
                        .map_err(|source| ExecutorError::Spawn {
                            program: PathBuf::from(&execution_argv[0]),
                            source,
                        })?;
                    let record = UnitExitRecord {
                        schema_version: UNIT_EXIT_SCHEMA_VERSION,
                        unit: self.unit_name(&request.identity),
                        invocation_id,
                        attempt: request.attempt,
                        lease_epoch: request.lease_epoch,
                        service_result: "timeout".to_owned(),
                        exit_code: Some("killed".to_owned()),
                        exit_status: Some("KILL".to_owned()),
                    };
                    (record, ExecutionTermination::RuntimeExceeded)
                }
            }
        } else {
            direct_completion(
                child.wait().await.map_err(|source| ExecutorError::Spawn {
                    program: PathBuf::from(&execution_argv[0]),
                    source,
                })?,
                invocation_id,
            )
        };
        // child.wait (or the timeout termination helper) has reaped the group
        // leader. Disarm before any exit-record I/O so PID reuse cannot make a
        // later Drop signal an unrelated process group.
        direct_guard.mark_stopped();
        let mut record = record;
        record.unit = self.unit_name(&request.identity);
        record.attempt = request.attempt;
        record.lease_epoch = request.lease_epoch;
        write_exit_record(&paths.exit_record, &record)?;
        Ok(ExecutionOutcome {
            unit: record.unit.clone(),
            backend: ExecutionBackend::Direct,
            paths,
            record,
            termination,
            evidence_gate: None,
            semantic_completion: None,
            result_revision: None,
            authorship: None,
            authorship_sessions: None,
            host_id: self.host_id.clone(),
            captures_available: true,
        })
    }
}

pub(super) async fn terminate_direct_process_group(
    child: &mut tokio::process::Child,
    pid: Option<u32>,
) -> std::io::Result<()> {
    if let Some(pid) = pid.and_then(|value| i32::try_from(value).ok()) {
        // The child was spawned as its own process-group leader. A group kill prevents
        // descendants from outliving a direct-fallback runtime deadline.
        let result = unsafe { libc::kill(-pid, libc::SIGKILL) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        child.wait().await?;
        return Ok(());
    }
    child.kill().await
}
