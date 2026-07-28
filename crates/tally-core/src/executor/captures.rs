use super::*;

impl Executor {
    pub fn capture_generation_matches(
        &self,
        identity: &ExecutionIdentity,
        attempt: u32,
        lease_epoch: u64,
    ) -> Result<bool, ExecutorError> {
        let path = self.paths(identity).capture_generation;
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(source) => return Err(io_error(&path, source)),
        };
        let metadata = file.metadata().map_err(|source| io_error(&path, source))?;
        if !metadata.file_type().is_file() || metadata.len() > 1024 {
            return Err(ExecutorError::InvalidRequest(format!(
                "capture generation {} is not a bounded regular file",
                path.display()
            )));
        }
        let generation: CaptureGeneration = serde_json::from_reader(file)?;
        Ok(generation.attempt == attempt && generation.lease_epoch == lease_epoch)
    }

    pub fn retained_capture_paths(
        &self,
        identity: &ExecutionIdentity,
        attempt: u32,
        lease_epoch: u64,
    ) -> Result<Option<RetainedCapturePaths>, ExecutorError> {
        if self.capture_generation_matches(identity, attempt, lease_epoch)? {
            let paths = self.paths(identity);
            return Ok(Some(RetainedCapturePaths {
                stdout: paths.stdout,
                stderr: paths.stderr,
                current: true,
            }));
        }
        let paths = self.archived_capture_paths(identity, attempt, lease_epoch);
        if paths.stdout.exists() || paths.stderr.exists() {
            Ok(Some(paths))
        } else {
            Ok(None)
        }
    }

    pub(super) fn archived_capture_paths(
        &self,
        identity: &ExecutionIdentity,
        attempt: u32,
        lease_epoch: u64,
    ) -> RetainedCapturePaths {
        let directory = self
            .state_dir
            .join(CAPTURE_ARCHIVE_DIRECTORY)
            .join(identity.unit_uuid().to_string());
        let stem = format!("attempt-{attempt:010}-epoch-{lease_epoch:020}");
        RetainedCapturePaths {
            stdout: directory.join(format!("{stem}.out")),
            stderr: directory.join(format!("{stem}.err")),
            current: false,
        }
    }

    pub(super) fn archive_current_capture(
        &self,
        identity: &ExecutionIdentity,
    ) -> Result<(), ExecutorError> {
        let current = self.paths(identity);
        for path in [
            &current.stdout,
            &current.stderr,
            &current.capture_generation,
        ] {
            let metadata = match std::fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(source) => return Err(io_error(path, source)),
            };
            if !metadata.file_type().is_file() || metadata.nlink() != 1 {
                // A partial or replaced capture set has no trustworthy generation.
                // prepare_paths will atomically replace it without following links.
                return Ok(());
            }
        }
        let Some(generation) = read_capture_generation(&current.capture_generation)? else {
            return Ok(());
        };
        let archived =
            self.archived_capture_paths(identity, generation.attempt, generation.lease_epoch);
        let archive_directory = archived
            .stdout
            .parent()
            .expect("archive capture paths always have a parent");
        create_private_directory(archive_directory)?;
        for (source, destination) in [
            (&current.stdout, &archived.stdout),
            (&current.stderr, &archived.stderr),
        ] {
            match std::fs::symlink_metadata(destination) {
                Ok(metadata) => {
                    if !metadata.file_type().is_file()
                        || metadata.nlink() != 1
                        || metadata.permissions().mode() & 0o077 != 0
                    {
                        return Err(ExecutorError::InvalidRequest(format!(
                            "attempt capture archive {} is not a private regular file",
                            destination.display()
                        )));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    copy_private_file_exclusive(source, destination)?;
                }
                Err(source_error) => return Err(io_error(destination, source_error)),
            }
        }
        sync_directory(archive_directory)?;
        std::fs::remove_file(&current.capture_generation)
            .map_err(|source| io_error(&current.capture_generation, source))?;
        sync_directory(
            current
                .capture_generation
                .parent()
                .expect("capture generation always has a parent"),
        )?;
        for source in [&current.stdout, &current.stderr] {
            match std::fs::remove_file(source) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source_error) => return Err(io_error(source, source_error)),
            }
        }
        sync_directory(
            current
                .stdout
                .parent()
                .expect("capture stream always has a parent"),
        )?;
        Ok(())
    }
}

pub(super) fn write_capture_generation(
    path: &Path,
    generation: CaptureGeneration,
) -> Result<(), ExecutorError> {
    replace_private_file(path, &serde_json::to_vec(&generation)?)
}

pub(super) fn read_capture_generation(
    path: &Path,
) -> Result<Option<CaptureGeneration>, ExecutorError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(path, source)),
    };
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !metadata.file_type().is_file() || metadata.len() > 1024 {
        return Err(ExecutorError::InvalidRequest(format!(
            "capture generation {} is not a bounded regular file",
            path.display()
        )));
    }
    serde_json::from_reader(file).map(Some).map_err(Into::into)
}

pub(super) fn replace_private_file(path: &Path, contents: &[u8]) -> Result<(), ExecutorError> {
    let parent = path.parent().ok_or_else(|| {
        ExecutorError::InvalidRequest("private file path has no parent".to_owned())
    })?;
    create_private_directory(parent)?;
    let file_name = path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        ExecutorError::InvalidRequest("private file name is not valid UTF-8".to_owned())
    })?;
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .map_err(|source| io_error(&temporary, source))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|source| io_error(&temporary, source))?;
        file.write_all(contents)
            .map_err(|source| io_error(&temporary, source))?;
        file.sync_all()
            .map_err(|source| io_error(&temporary, source))?;
        std::fs::rename(&temporary, path).map_err(|source| io_error(path, source))?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub(super) fn copy_private_file_exclusive(
    source: &Path,
    destination: &Path,
) -> Result<(), ExecutorError> {
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(source)
        .map_err(|error| io_error(source, error))?;
    let metadata = input.metadata().map_err(|error| io_error(source, error))?;
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err(ExecutorError::InvalidRequest(format!(
            "capture {} is not a private regular file",
            source.display()
        )));
    }
    let parent = destination.parent().ok_or_else(|| {
        ExecutorError::InvalidRequest("capture archive path has no parent".to_owned())
    })?;
    create_private_directory(parent)?;
    let file_name = destination
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| {
            ExecutorError::InvalidRequest("capture archive name is not valid UTF-8".to_owned())
        })?;
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let result = (|| {
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .map_err(|error| io_error(&temporary, error))?;
        std::io::copy(&mut input, &mut output).map_err(|error| io_error(&temporary, error))?;
        output
            .sync_all()
            .map_err(|error| io_error(&temporary, error))?;
        std::fs::hard_link(&temporary, destination)
            .map_err(|error| io_error(destination, error))?;
        std::fs::remove_file(&temporary).map_err(|error| io_error(&temporary, error))?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub(super) fn sync_directory(path: &Path) -> Result<(), ExecutorError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(path, source))
}

pub fn write_exit_record(path: &Path, record: &UnitExitRecord) -> Result<(), ExecutorError> {
    let parent = path.parent().ok_or_else(|| {
        ExecutorError::InvalidRequest("exit record path has no parent".to_owned())
    })?;
    create_private_directory(parent)?;
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let file_name = path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        ExecutorError::InvalidRequest("exit record file name is not valid UTF-8".to_owned())
    })?;
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let mut renamed = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|source| io_error(&temporary, source))?;
        let mut encoded = serde_json::to_vec(record)?;
        encoded.push(b'\n');
        file.write_all(&encoded)
            .map_err(|source| io_error(&temporary, source))?;
        file.sync_all()
            .map_err(|source| io_error(&temporary, source))?;
        std::fs::rename(&temporary, path).map_err(|source| io_error(path, source))?;
        renamed = true;
        sync_directory(parent)
    })();
    if result.is_err() {
        if renamed {
            let _ = std::fs::remove_file(path);
            let _ = sync_directory(parent);
        } else {
            let _ = std::fs::remove_file(&temporary);
        }
    }
    result
}

pub fn read_exit_record(path: &Path, expected_unit: &str) -> Result<UnitExitRecord, ExecutorError> {
    let bytes = std::fs::read(path).map_err(|source| io_error(path, source))?;
    let record: UnitExitRecord = serde_json::from_slice(&bytes)?;
    record.validate(expected_unit)?;
    Ok(record)
}

pub fn persist_exit_record_from_env(
    path: &Path,
    expected_unit: &str,
) -> Result<UnitExitRecord, ExecutorError> {
    let mut values = HashMap::new();
    for name in [
        "INVOCATION_ID",
        "SERVICE_RESULT",
        "TALLY_ATTEMPT",
        "TALLY_LEASE_EPOCH",
    ] {
        let value = std::env::var(name).map_err(|_| ExecutorError::MissingExitEnvironment(name))?;
        values.insert(name, value);
    }
    for name in ["EXIT_CODE", "EXIT_STATUS"] {
        match std::env::var(name) {
            Ok(value) => {
                values.insert(name, value);
            }
            Err(std::env::VarError::NotPresent) => {}
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(ExecutorError::MissingExitEnvironment(name));
            }
        }
    }
    persist_exit_record(path, expected_unit, &values)
}

pub(super) fn persist_exit_record(
    path: &Path,
    expected_unit: &str,
    environment: &HashMap<&str, String>,
) -> Result<UnitExitRecord, ExecutorError> {
    let required = |name: &'static str| {
        environment
            .get(name)
            .filter(|value| !value.is_empty())
            .cloned()
            .ok_or(ExecutorError::MissingExitEnvironment(name))
    };
    let optional = |name: &'static str| match environment.get(name) {
        Some(value) if value.is_empty() => Err(ExecutorError::InvalidExitRecord(format!(
            "{name} must not be empty when present"
        ))),
        Some(value) => Ok(Some(value.clone())),
        None => Ok(None),
    };
    let record = UnitExitRecord {
        schema_version: UNIT_EXIT_SCHEMA_VERSION,
        unit: expected_unit.to_owned(),
        invocation_id: required("INVOCATION_ID")?,
        attempt: required("TALLY_ATTEMPT")?.parse().map_err(|_| {
            ExecutorError::InvalidExitRecord("TALLY_ATTEMPT must be a positive u32".to_owned())
        })?,
        lease_epoch: required("TALLY_LEASE_EPOCH")?.parse().map_err(|_| {
            ExecutorError::InvalidExitRecord("TALLY_LEASE_EPOCH must be a positive u64".to_owned())
        })?,
        service_result: required("SERVICE_RESULT")?,
        exit_code: optional("EXIT_CODE")?,
        exit_status: optional("EXIT_STATUS")?,
    };
    record.validate(expected_unit)?;
    write_exit_record(path, &record)?;
    Ok(record)
}

pub(super) fn classify_termination(
    record: &UnitExitRecord,
) -> Result<ExecutionTermination, ExecutorError> {
    if record.service_result == "timeout" {
        return Ok(ExecutionTermination::RuntimeExceeded);
    }
    if matches!(record.service_result.as_str(), "success" | "exit-code")
        && record.exit_code.as_deref() == Some("exited")
    {
        let status = record
            .exit_status
            .as_deref()
            .expect("validated exited records carry an exitStatus")
            .parse::<i32>()
            .map_err(|_| {
                ExecutorError::InvalidExitRecord(format!(
                    "exitStatus {:?} is not numeric for exitCode=exited",
                    record.exit_status
                ))
            })?;
        return Ok(ExecutionTermination::Exited(status));
    }
    match (
        record.service_result.as_str(),
        &record.exit_code,
        &record.exit_status,
    ) {
        ("success" | "signal" | "core-dump" | "oom-kill", Some(code), Some(status)) => {
            Ok(ExecutionTermination::Signaled {
                code: code.clone(),
                status: status.clone(),
            })
        }
        _ => Ok(ExecutionTermination::ServiceFailed {
            service_result: record.service_result.clone(),
            exit_code: record.exit_code.clone(),
            exit_status: record.exit_status.clone(),
        }),
    }
}

pub(super) fn direct_completion(
    status: std::process::ExitStatus,
    invocation_id: String,
) -> (UnitExitRecord, ExecutionTermination) {
    if let Some(code) = status.code() {
        return (
            UnitExitRecord {
                schema_version: UNIT_EXIT_SCHEMA_VERSION,
                unit: String::new(),
                invocation_id,
                attempt: 0,
                lease_epoch: 0,
                service_result: if code == 0 { "success" } else { "exit-code" }.to_owned(),
                exit_code: Some("exited".to_owned()),
                exit_status: Some(code.to_string()),
            },
            ExecutionTermination::Exited(code),
        );
    }
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal().unwrap_or(0)
    };
    #[cfg(not(unix))]
    let signal = 0;
    (
        UnitExitRecord {
            schema_version: UNIT_EXIT_SCHEMA_VERSION,
            unit: String::new(),
            invocation_id,
            attempt: 0,
            lease_epoch: 0,
            service_result: "signal".to_owned(),
            exit_code: Some("killed".to_owned()),
            exit_status: Some(signal.to_string()),
        },
        ExecutionTermination::Signaled {
            code: "killed".to_owned(),
            status: signal.to_string(),
        },
    )
}

pub(super) fn is_not_found(error: &ExecutorError) -> bool {
    matches!(
        error,
        ExecutorError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound
    )
}

pub(crate) fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[(third & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}
