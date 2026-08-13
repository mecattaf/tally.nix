use super::*;

pub const CAPTURE_EXCERPT_MAX_BYTES: usize = 2 * 1024;
const CAPTURE_EXCERPT_TRUNCATION_MARKER: &str = "[... earlier captured stderr omitted ...]\n";

/// How long a caller will wait for the per-unit capture lock before giving up.
///
/// Every critical section this lock guards is a bounded local file operation —
/// archiving one generation, writing one projection — so a wait beyond this is
/// evidence of a stuck holder, not of honest contention. Waiting forever is the
/// one outcome that is never acceptable: the failure-projection path is reached
/// from the durable-wait RPC handlers, and a daemon that blocks there stops
/// answering.
pub const CAPTURE_LOCK_DEADLINE: Duration = Duration::from_secs(5);
const CAPTURE_LOCK_FIRST_BACKOFF: Duration = Duration::from_millis(1);
const CAPTURE_LOCK_MAX_BACKOFF: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureExcerpt {
    pub text: String,
    pub truncated: bool,
}

impl Executor {
    pub fn capture_lock_path(&self, identity: &ExecutionIdentity) -> PathBuf {
        self.state_dir
            .join(CAPTURE_LOCK_DIRECTORY)
            .join(format!("{}{CAPTURE_LOCK_SUFFIX}", identity.unit_uuid()))
    }

    pub(super) fn lock_capture(&self, identity: &ExecutionIdentity) -> Result<File, ExecutorError> {
        self.lock_capture_within(identity, CAPTURE_LOCK_DEADLINE)
    }

    /// Take the per-unit capture lock, or give up inside `budget`.
    ///
    /// Two properties beyond "hold an exclusive `flock`" are load-bearing here.
    ///
    /// The wait is bounded. `lock_exclusive` is a blocking syscall on a file
    /// that used to live in a job-writable directory; the relocation removes the
    /// hostile holder, but a wedged daemon-side holder would still stall every
    /// caller, including the RPC failure-projection path. Callers already treat
    /// a failure here as "no excerpt", which is strictly better than not
    /// answering.
    ///
    /// The acquisition is revalidated. `flock` follows the inode, not the name,
    /// so a lock granted after the retention sweep unlinked the path guards
    /// nothing: the next caller creates a fresh file and locks it immediately,
    /// and two holders run at once. Re-stat after the lock is granted and, if
    /// the name no longer resolves to the inode under the lock, drop it and open
    /// again.
    fn lock_capture_within(
        &self,
        identity: &ExecutionIdentity,
        budget: Duration,
    ) -> Result<File, ExecutorError> {
        let path = self.capture_lock_path(identity);
        let parent = path
            .parent()
            .expect("capture lock path always has a parent");
        create_private_directory(parent)?;
        let started = Instant::now();
        let mut backoff = CAPTURE_LOCK_FIRST_BACKOFF;
        loop {
            let file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)
                .map_err(|source| io_error(&path, source))?;
            let metadata = file.metadata().map_err(|source| io_error(&path, source))?;
            if !metadata.file_type().is_file() || metadata.nlink() != 1 {
                return Err(ExecutorError::InvalidRequest(format!(
                    "capture lock {} is not a private regular file",
                    path.display()
                )));
            }
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|source| io_error(&path, source))?;
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => {
                    if capture_lock_still_named(&file, &path)? {
                        return Ok(file);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(source) => return Err(io_error(&path, source)),
            }
            drop(file);
            let waited = started.elapsed();
            if waited >= budget {
                return Err(ExecutorError::CaptureLockContended {
                    path,
                    waited_ms: budget.as_millis(),
                });
            }
            std::thread::sleep(backoff.min(budget - waited));
            backoff = (backoff * 2).min(CAPTURE_LOCK_MAX_BACKOFF);
        }
    }

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
                failure_stderr: paths
                    .failure_stderr
                    .exists()
                    .then_some(paths.failure_stderr),
                current: true,
            }));
        }
        let mut paths = self.archived_capture_paths(identity, attempt, lease_epoch);
        paths.failure_stderr = paths.failure_stderr.filter(|path| path.exists());
        if paths.stdout.exists()
            || paths.stderr.exists()
            || paths
                .failure_stderr
                .as_ref()
                .is_some_and(|path| path.exists())
        {
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
            .join(identity.capture_stem());
        let stem = format!("attempt-{attempt:010}-epoch-{lease_epoch:020}");
        RetainedCapturePaths {
            stdout: directory.join(format!("{stem}.out")),
            stderr: directory.join(format!("{stem}.adapter.err")),
            failure_stderr: Some(directory.join(format!("{stem}.err"))),
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
        if current.failure_stderr.exists() {
            let destination = archived
                .failure_stderr
                .as_ref()
                .expect("archive paths always include a failure destination");
            match std::fs::symlink_metadata(destination) {
                Ok(metadata) => {
                    if !metadata.file_type().is_file()
                        || metadata.nlink() != 1
                        || metadata.permissions().mode() & 0o077 != 0
                    {
                        return Err(ExecutorError::InvalidRequest(format!(
                            "attempt failure capture archive {} is not a private regular file",
                            destination.display()
                        )));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    copy_private_file_exclusive(&current.failure_stderr, destination)?;
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
        for source in [&current.stdout, &current.stderr, &current.failure_stderr] {
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

    /// Materialize the operator-facing `.err` projection for an exactly
    /// identified failed generation. The same lock guards retry preparation,
    /// so a late terminal handler can neither inspect one generation and copy
    /// another nor stamp a failure signal onto a newer healthy attempt.
    ///
    /// The generation is checked twice on purpose. The cheap read before the
    /// lock decides nothing about the projection — the read under the lock
    /// does — but it keeps this call from minting a lock file for a task whose
    /// capture generation is already gone. The startup reconciler replays every
    /// failed witness in the ledger at every daemon start; locking first left one
    /// permanent lock file per historically failed task, with a freshly stamped
    /// mtime that the retention sweep could never age out.
    pub fn persist_failure_stderr(
        &self,
        identity: &ExecutionIdentity,
        attempt: u32,
        lease_epoch: u64,
    ) -> Result<Option<CaptureExcerpt>, ExecutorError> {
        if !self.capture_generation_matches(identity, attempt, lease_epoch)? {
            return Ok(None);
        }
        let _capture_lock = self.lock_capture(identity)?;
        if !self.capture_generation_matches(identity, attempt, lease_epoch)? {
            return Ok(None);
        }
        let paths = self.paths(identity);
        let excerpt = read_capture_excerpt(&paths.stderr)?;
        replace_private_file(&paths.failure_stderr, excerpt.text.as_bytes())?;
        Ok(Some(excerpt))
    }
}

/// Does the locked file still answer to the name it was opened under?
///
/// A false answer means somebody unlinked or replaced the path while this
/// caller was waiting for the lock, so the granted lock excludes nobody.
pub(super) fn capture_lock_still_named(file: &File, path: &Path) -> Result<bool, ExecutorError> {
    let held = file.metadata().map_err(|source| io_error(path, source))?;
    if held.nlink() != 1 {
        return Ok(false);
    }
    match std::fs::symlink_metadata(path) {
        Ok(named) => Ok(named.file_type().is_file()
            && named.ino() == held.ino()
            && named.dev() == held.dev()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error(path, source)),
    }
}

/// Read the bounded tail of a retained stream without following links.
///
/// Failure diagnostics favor the tail because command harnesses commonly emit
/// setup chatter before the actionable process error. Invalid UTF-8 is
/// lossily represented for terminal display; the retained capture remains the
/// byte-authoritative copy.
pub fn read_capture_excerpt(path: &Path) -> Result<CaptureExcerpt, ExecutorError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err(ExecutorError::InvalidRequest(format!(
            "capture excerpt source {} is not a private regular file",
            path.display()
        )));
    }
    let max = CAPTURE_EXCERPT_MAX_BYTES as u64;
    let start = metadata.len().saturating_sub(max);
    file.seek(SeekFrom::Start(start))
        .map_err(|source| io_error(path, source))?;
    let mut bytes = Vec::with_capacity((metadata.len() - start) as usize);
    file.take(max)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if start > 0 {
        let partial_prefix = bytes
            .iter()
            .take_while(|byte| **byte & 0b1100_0000 == 0b1000_0000)
            .count();
        bytes.drain(..partial_prefix);
    }
    let mut truncated = start > 0;
    let mut text = String::from_utf8_lossy(&bytes).replace('\0', "�");
    let plain_limit = if truncated {
        CAPTURE_EXCERPT_MAX_BYTES - CAPTURE_EXCERPT_TRUNCATION_MARKER.len()
    } else {
        CAPTURE_EXCERPT_MAX_BYTES
    };
    if text.len() > plain_limit {
        truncated = true;
    }
    if truncated {
        let tail_limit = CAPTURE_EXCERPT_MAX_BYTES - CAPTURE_EXCERPT_TRUNCATION_MARKER.len();
        if text.len() > tail_limit {
            let mut start = text.len() - tail_limit;
            while !text.is_char_boundary(start) {
                start += 1;
            }
            text.drain(..start);
        }
        text.insert_str(0, CAPTURE_EXCERPT_TRUNCATION_MARKER);
    }
    if text.len() > CAPTURE_EXCERPT_MAX_BYTES {
        return Err(ExecutorError::InvalidRequest(format!(
            "capture excerpt exceeded the {CAPTURE_EXCERPT_MAX_BYTES} byte bound"
        )));
    }
    Ok(CaptureExcerpt { text, truncated })
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

pub(crate) fn replace_private_file(path: &Path, contents: &[u8]) -> Result<(), ExecutorError> {
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

pub(super) fn ensure_private_file(path: &Path) -> Result<(), ExecutorError> {
    let parent = path.parent().ok_or_else(|| {
        ExecutorError::InvalidRequest("private file path has no parent".to_owned())
    })?;
    let (file, created) = match OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => (file, true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (
            OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)
                .map_err(|source| io_error(path, source))?,
            false,
        ),
        Err(source) => return Err(io_error(path, source)),
    };
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err(ExecutorError::InvalidRequest(format!(
            "private file {} must be a regular file with one link",
            path.display()
        )));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|source| io_error(path, source))?;
    if created {
        file.sync_all().map_err(|source| io_error(path, source))?;
        sync_directory(parent)?;
    }
    Ok(())
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

/// The exit recorder's entry point, run as `ExecStopPost`. Beyond the
/// environment-derived fields `persist_exit_record` has always written, this
/// issues the one accounting `systemctl show` while the unit is still
/// queryable and embeds the result. A failed probe never fails the exit
/// record: accounting is advisory to the verdict, so the failure is logged to
/// the job's captured stderr (the executor module's diagnostics convention,
/// #315) and the record is written with a typed absence instead.
pub fn persist_exit_record_from_env(
    path: &Path,
    expected_unit: &str,
    systemctl: &Path,
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
    let accounting = match probe_unit_accounting(systemctl, expected_unit) {
        Ok(sample) => Some(sample),
        Err(error) => {
            eprintln!("tally: unit accounting probe failed for {expected_unit}: {error}");
            None
        }
    };
    let record = build_exit_record(expected_unit, &values, accounting)?;
    write_exit_record(path, &record)?;
    Ok(record)
}

/// The environment-only builder, kept for the test suite that exercises
/// `EXIT_CODE`/`EXIT_STATUS` shapes without a `systemctl` binary in play.
/// Production has one entry point, `persist_exit_record_from_env`, which
/// always attempts the accounting probe too.
#[cfg(test)]
pub(super) fn persist_exit_record(
    path: &Path,
    expected_unit: &str,
    environment: &HashMap<&str, String>,
) -> Result<UnitExitRecord, ExecutorError> {
    let record = build_exit_record(expected_unit, environment, None)?;
    write_exit_record(path, &record)?;
    Ok(record)
}

fn build_exit_record(
    expected_unit: &str,
    environment: &HashMap<&str, String>,
    accounting: Option<UnitAccounting>,
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
        accounting,
    };
    record.validate(expected_unit)?;
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
                accounting: None,
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
            accounting: None,
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
