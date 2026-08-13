use super::*;

impl Executor {
    pub fn build_systemd_argv(
        &self,
        request: &ExecutionRequest,
    ) -> Result<Vec<OsString>, ExecutorError> {
        self.validate_request(request)?;
        let paths = self.paths(&request.identity);
        let unit_stem = self.unit_stem(&request.identity);
        let unit_name = self.unit_name(&request.identity);
        let exec_stop_post = self.exec_stop_post(&paths.exit_record, &unit_name)?;
        let mut args = vec![
            "--user".into(),
            "--wait".into(),
            "--collect".into(),
            "--unit".into(),
            unit_stem.into(),
            "--quiet".into(),
            "--expand-environment=no".into(),
        ];
        push_pair(&mut args, "--property", "Type=exec");
        push_pair(
            &mut args,
            "--property",
            format!("CPUWeight={}", request.limits.cpu_weight),
        );
        push_pair(
            &mut args,
            "--property",
            format!("MemoryMax={}", request.limits.memory_max_bytes),
        );
        if let Some(seconds) = request.runtime_max_sec {
            push_pair(&mut args, "--property", format!("RuntimeMaxSec={seconds}s"));
        }
        push_pair(
            &mut args,
            "--property",
            format!("StandardOutput=append:{}", display_path(&paths.stdout)?),
        );
        push_pair(
            &mut args,
            "--property",
            format!("StandardError=append:{}", display_path(&paths.stderr)?),
        );
        push_pair(
            &mut args,
            "--property",
            format!("ExecStopPost={exec_stop_post}"),
        );
        self.push_hardening_properties(&mut args, request)?;
        for (name, source) in &request.credentials {
            push_pair(
                &mut args,
                "--property",
                format!("LoadCredential={name}:{}", display_path(source)?),
            );
        }
        if let Some(cwd) = &request.cwd {
            push_pair(&mut args, "--working-directory", cwd.as_os_str());
        }
        let environment = execution_environment(request)?;
        for (name, value) in environment {
            push_pair(&mut args, "--setenv", format!("{name}={value}"));
        }
        let unset_environment = environment_to_unset(request);
        if !unset_environment.is_empty() {
            push_pair(
                &mut args,
                "--property",
                format!("UnsetEnvironment={}", unset_environment.join(" ")),
            );
        }
        args.push("--".into());
        args.extend(self.execution_argv(request));
        Ok(args)
    }

    pub(super) fn execution_argv(&self, request: &ExecutionRequest) -> Vec<OsString> {
        let Some(attestation) = &request.exec_attestation else {
            return request.argv.iter().map(OsString::from).collect();
        };
        let mut argv = vec![
            self.recorder_program.as_os_str().to_owned(),
            "attest".into(),
            "exec".into(),
            "--task-uuid".into(),
            request.identity.unit_uuid().to_string().into(),
            "--attempt".into(),
            request.attempt.to_string().into(),
            "--lease-epoch".into(),
            request.lease_epoch.to_string().into(),
            "--adapter".into(),
            attestation.adapter.clone().into(),
        ];
        if let Some(executor) = &attestation.executor {
            argv.extend(["--executor".into(), executor.clone().into()]);
        }
        if let Some(payload_hash) = &attestation.payload_hash {
            argv.extend(["--payload-hash".into(), payload_hash.clone().into()]);
        }
        if let Some(brief_hash) = &attestation.brief_hash {
            argv.extend(["--brief-hash".into(), brief_hash.clone().into()]);
        }
        for evidence in &attestation.evidence {
            argv.extend(["--evidence".into(), evidence.clone().into()]);
        }
        argv.extend([
            "--ledger".into(),
            self.state_dir
                .join(EXEC_ATTESTATION_LEDGER)
                .into_os_string(),
            "--".into(),
        ]);
        argv.extend(request.argv.iter().map(OsString::from));
        argv
    }

    pub(super) fn push_hardening_properties(
        &self,
        args: &mut Vec<OsString>,
        request: &ExecutionRequest,
    ) -> Result<(), ExecutorError> {
        if request.hardening == AdapterHardening::None {
            return Ok(());
        }
        let strict = matches!(
            request.hardening,
            AdapterHardening::Strict | AdapterHardening::Production
        );
        if strict {
            push_pair(args, "--property", "ProtectHome=read-only");
        }
        push_pair(args, "--property", "PrivateTmp=yes");
        if strict {
            push_pair(args, "--property", "ProtectSystem=strict");
            push_pair(args, "--property", "NoNewPrivileges=yes");
            push_pair(
                args,
                "--property",
                "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6",
            );
        }
        if request.hardening == AdapterHardening::Production {
            for property in [
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
            ] {
                push_pair(args, "--property", property);
            }
        }
        let mut writable = Vec::new();
        if let Some(workspace) = &request.workspace {
            writable.push(workspace.worktree_path.clone());
        }
        if strict {
            // Keep the transient unit's state writers explicit. systemd opens
            // the two capture files, ExecStopPost atomically replaces a record
            // in unit-exit, the optional exec wrapper appends its ledger, and a
            // job may write its declared gate manifest. The yield hook only
            // calls the daemon socket and receives no state-directory-wide
            // write exception.
            let paths = self.paths(&request.identity);
            writable.push(
                paths
                    .exit_record
                    .parent()
                    .expect("exit record always has a parent")
                    .to_owned(),
            );
            writable.push(paths.stdout);
            writable.push(paths.stderr);
            if request.exec_attestation.is_some() {
                writable.push(self.state_dir.join(EXEC_ATTESTATION_LEDGER));
            }
            if let Some(manifest) = &request.gate_manifest {
                writable.push(manifest.path.clone());
            }
        } else {
            writable.push(self.state_dir.clone());
        }
        writable.extend(request.extra_writable_paths.iter().cloned());
        let mut unique_writable = Vec::new();
        for path in writable {
            if !unique_writable.contains(&path) {
                unique_writable.push(path);
            }
        }
        let writable = unique_writable
            .into_iter()
            .map(|path| quote_systemd_exec_word(path.as_os_str()))
            .collect::<Result<Vec<_>, _>>()?
            .join(" ");
        push_pair(args, "--property", format!("ReadWritePaths={writable}"));
        Ok(())
    }
}

pub(super) fn push_pair(
    args: &mut Vec<OsString>,
    option: impl Into<OsString>,
    value: impl Into<OsString>,
) {
    args.push(option.into());
    args.push(value.into());
}

pub(super) fn priority_name(priority: Priority) -> &'static str {
    match priority {
        Priority::Interrupt => "interrupt",
        Priority::High => "high",
        Priority::Medium => "medium",
        Priority::Low => "low",
    }
}

pub(super) fn execution_environment(
    request: &ExecutionRequest,
) -> Result<Vec<(String, String)>, ExecutorError> {
    let mut environment = request
        .environment
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<Vec<_>>();
    environment.push((
        "TALLY_JOB_ID".to_owned(),
        request.identity.job_id.to_string(),
    ));
    if let Some(task_uuid) = &request.identity.task_uuid {
        environment.push(("TALLY_TASK_UUID".to_owned(), task_uuid.to_string()));
    }
    if let Some(task_ref) = &request.identity.task_ref {
        environment.push(("TALLY_TASK_REF".to_owned(), task_ref.to_string()));
    }
    if let Some(parent) = &request.parent {
        environment.push(("TALLY_PARENT".to_owned(), parent.to_string()));
    }
    environment.extend([
        (
            "TALLY_POOL".to_owned(),
            crate::poolset::encoded(&request.pools)?,
        ),
        (
            "TALLY_LEASE_EPOCH".to_owned(),
            request.lease_epoch.to_string(),
        ),
        ("TALLY_ATTEMPT".to_owned(), request.attempt.to_string()),
        (
            "TALLY_CLASS".to_owned(),
            priority_name(request.priority).to_owned(),
        ),
    ]);
    if request.no_enqueue {
        environment.push(("TALLY_NO_ENQUEUE".to_owned(), "1".to_owned()));
    }
    if !request.credentials.is_empty() {
        let names = request.credentials.keys().collect::<Vec<_>>();
        environment.push((
            "TALLY_CREDENTIALS".to_owned(),
            serde_json::to_string(&names)?,
        ));
    }
    if let Some(hook) = &request.yield_hook {
        environment.push(("TALLY_YIELD_HOOK".to_owned(), serde_json::to_string(hook)?));
    }
    if let Some(socket) = &request.tally_socket {
        environment.push(("TALLY_SOCKET".to_owned(), socket.clone()));
    }
    if let Some(token) = &request.job_token {
        environment.push(("TALLY_JOB_TOKEN".to_owned(), token.clone()));
    }
    if let Some(path) = &request.brief_path {
        environment.push(("TALLY_BRIEF".to_owned(), display_path(path)?.to_owned()));
    }
    if let Some(hash) = &request.brief_hash {
        environment.push(("TALLY_BRIEF_HASH".to_owned(), hash.clone()));
    }
    if let Some(manifest) = &request.gate_manifest {
        environment.push((
            "TALLY_GATE_MANIFEST".to_owned(),
            display_path(&manifest.path)?.to_owned(),
        ));
    }
    if let Some(workspace) = &request.workspace {
        environment.extend([
            ("TALLY_WORKSPACE_REPO".to_owned(), workspace.repo.clone()),
            (
                "TALLY_WORKSPACE_BASE_REV".to_owned(),
                workspace.base_rev.clone(),
            ),
            (
                "TALLY_WORKSPACE_BRANCH".to_owned(),
                workspace.branch.clone(),
            ),
            (
                "TALLY_WORKSPACE_PATH".to_owned(),
                workspace.worktree_path.to_string_lossy().into_owned(),
            ),
        ]);
    }
    Ok(environment)
}

pub(super) fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

pub(super) fn environment_to_unset(request: &ExecutionRequest) -> Vec<&'static str> {
    let mut names = Vec::new();
    if request.identity.task_uuid.is_none() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[0]);
    }
    if request.identity.task_ref.is_none() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[1]);
    }
    if request.parent.is_none() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[2]);
    }
    if !request.no_enqueue {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[3]);
    }
    if request.credentials.is_empty() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[4]);
        names.push("CREDENTIALS_DIRECTORY");
    }
    if request.yield_hook.is_none() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[5]);
    }
    if request.tally_socket.is_none() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[6]);
    }
    if request.job_token.is_none() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[7]);
    }
    if request.workspace.is_none() {
        names.extend(OPTIONAL_TALLY_ENVIRONMENT[8..12].iter().copied());
    }
    if request.brief_path.is_none() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[12]);
    }
    if request.brief_hash.is_none() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[13]);
    }
    if request.gate_manifest.is_none() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[14]);
    }
    names
}

pub(super) fn validate_credential_name(name: &str) -> Result<(), ExecutorError> {
    let valid = !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(ExecutorError::InvalidRequest(format!(
            "invalid credential name {name:?}"
        )))
    }
}

pub(super) fn display_path(path: &Path) -> Result<&str, ExecutorError> {
    path.to_str().ok_or_else(|| {
        ExecutorError::InvalidRequest(format!("path {} is not valid UTF-8", path.display()))
    })
}

pub(super) fn validate_systemd_path(path: &Path, label: &str) -> Result<(), ExecutorError> {
    let path = display_path(path)?;
    if path.chars().any(char::is_control) {
        return Err(ExecutorError::InvalidRequest(format!(
            "{label} must not contain control characters"
        )));
    }
    if path.contains('%') {
        return Err(ExecutorError::InvalidRequest(format!(
            "{label} must not contain systemd specifier character %"
        )));
    }
    Ok(())
}

pub(super) fn quote_systemd_exec_word(word: &OsStr) -> Result<String, ExecutorError> {
    let word = word.to_str().ok_or_else(|| {
        ExecutorError::InvalidRequest("ExecStopPost argument is not valid UTF-8".to_owned())
    })?;
    if word.chars().any(char::is_control) {
        return Err(ExecutorError::InvalidRequest(
            "ExecStopPost arguments must not contain control characters".to_owned(),
        ));
    }
    let mut quoted = String::with_capacity(word.len() + 2);
    quoted.push('"');
    for character in word.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    Ok(quoted)
}

pub(super) fn create_private_directory(path: &Path) -> Result<(), ExecutorError> {
    std::fs::create_dir_all(path).map_err(|source| io_error(path, source))?;
    let metadata = std::fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if !metadata.file_type().is_dir() {
        return Err(ExecutorError::InvalidRequest(format!(
            "private directory {} must not be a symbolic link",
            path.display()
        )));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|source| io_error(path, source))
}

pub(super) fn create_private_file(path: &Path) -> Result<(), ExecutorError> {
    replace_private_file(path, &[])
}
