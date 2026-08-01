use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::Serialize;
use thiserror::Error;

use crate::brief::{self, BriefError};
use crate::executor::CAPTURE_ARCHIVE_DIRECTORY;
use crate::nix_store::GcRootBackend;
use crate::producers::{pending_ingress_brief_paths, ProducerError, INGRESS_LOCK_FILE_NAME};
use crate::taskdb::{read_acknowledged_events, TaskDbError};
use crate::witness::{is_nix_store_path, read_verified_records, WitnessError, WitnessRecord};

const ROOT_DIRECTORY_PREFIX: &str = "witness-";
const EVENTS_DIRECTORY: &str = "events";

/// Ratified state-directory retention envelope.
///
/// `events/rejected` is the adversarially drivable set and carries both an age
/// and a count bound; whichever is exceeded first prunes oldest-first.
/// `events/done` is the audit trail and carries only the longer age bound.
/// Capture archives expire on their own age bound and are deliberately *not*
/// pinned by the witness ledger: the witness record is the durable evidence and
/// an archive is only replay material.
pub const DEFAULT_CAPTURE_ARCHIVE_MAX_AGE: &str = "30d";
pub const DEFAULT_EVENTS_DONE_MAX_AGE: &str = "180d";
pub const DEFAULT_EVENTS_REJECTED_MAX_AGE: &str = "30d";
pub const DEFAULT_EVENTS_REJECTED_MAX_COUNT: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRetentionPolicy {
    pub capture_archive_max_age: Duration,
    pub events_done_max_age: Duration,
    pub events_rejected_max_age: Duration,
    pub events_rejected_max_count: usize,
}

impl StateRetentionPolicy {
    pub fn parse(
        capture_archive_max_age: &str,
        events_done_max_age: &str,
        events_rejected_max_age: &str,
        events_rejected_max_count: usize,
    ) -> Result<Self, RetentionError> {
        Ok(Self {
            capture_archive_max_age: parse_horizon(capture_archive_max_age)?,
            events_done_max_age: parse_horizon(events_done_max_age)?,
            events_rejected_max_age: parse_horizon(events_rejected_max_age)?,
            events_rejected_max_count,
        })
    }
}

impl Default for StateRetentionPolicy {
    fn default() -> Self {
        Self::parse(
            DEFAULT_CAPTURE_ARCHIVE_MAX_AGE,
            DEFAULT_EVENTS_DONE_MAX_AGE,
            DEFAULT_EVENTS_REJECTED_MAX_AGE,
            DEFAULT_EVENTS_REJECTED_MAX_COUNT,
        )
        .expect("ratified default retention horizons are valid systemd timespans")
    }
}

/// Everything one sweep needs. There is exactly one sweep entry point
/// (`run_gc`) and exactly one timer driving it; the state-directory pruners run
/// under the same GC-roots lock as the Nix GC-root reconciliation.
#[derive(Debug, Clone)]
pub struct GcRequest<'a> {
    pub data_dir: &'a Path,
    /// Daemon state directory. `None` skips the state-directory pruners
    /// entirely, which is what a data-dir-only invocation wants.
    pub state_dir: Option<&'a Path>,
    pub horizon_text: &'a str,
    pub state_retention: StateRetentionPolicy,
    pub now: DateTime<Utc>,
    pub dry_run: bool,
    pub collect: bool,
}

pub struct GcRootsLock {
    _file: File,
}

pub fn gcroots_lock_path(gcroots_dir: &Path) -> PathBuf {
    gcroots_dir.with_extension("lock")
}

pub fn acquire_registration_lock(gcroots_dir: &Path) -> std::io::Result<GcRootsLock> {
    acquire_roots_lock(gcroots_dir, false)
}

fn acquire_gc_lock(gcroots_dir: &Path) -> std::io::Result<GcRootsLock> {
    acquire_roots_lock(gcroots_dir, true)
}

fn acquire_roots_lock(gcroots_dir: &Path, exclusive: bool) -> std::io::Result<GcRootsLock> {
    let path = gcroots_lock_path(gcroots_dir);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    if exclusive {
        FileExt::lock_exclusive(&file)?;
    } else {
        FileExt::lock_shared(&file)?;
    }
    Ok(GcRootsLock { _file: file })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootRegistrationFailure {
    pub link: PathBuf,
    pub target: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootRegistrationReport {
    pub attempted: usize,
    pub registered: usize,
    pub failures: Vec<RootRegistrationFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GcReport {
    pub horizon: String,
    pub dry_run: bool,
    pub collect_requested: bool,
    pub live_paths: usize,
    pub roots_examined: usize,
    pub roots_pruned: usize,
    pub root_directories_pruned: usize,
    pub brief_stores_swept: bool,
    pub briefs_examined: usize,
    pub briefs_retained: usize,
    pub briefs_pruned: usize,
    pub legacy_briefs_examined: usize,
    pub legacy_briefs_pruned: usize,
    pub state_dir_swept: bool,
    pub capture_archives_examined: usize,
    pub capture_archives_pruned: usize,
    pub capture_archive_directories_pruned: usize,
    pub events_done_examined: usize,
    pub events_done_pruned: usize,
    pub events_rejected_examined: usize,
    pub events_rejected_pruned: usize,
    pub collected: bool,
}

#[derive(Debug, Error)]
pub enum RetentionError {
    #[error("invalid retention horizon {value:?}: {reason}")]
    InvalidHorizon { value: String, reason: String },
    #[error("witness ledger error: {0}")]
    Witness(#[from] WitnessError),
    #[error("brief retention error: {0}")]
    Brief(#[from] BriefError),
    #[error("durable row retention error: {0}")]
    TaskDb(#[from] TaskDbError),
    #[error("producer ingress retention error: {0}")]
    Producer(#[from] ProducerError),
    #[error("witness ledger verification failed; GC roots were left untouched")]
    InvalidLedger,
    #[error("retention I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("unsafe tally GC-root entry at {path}: {reason}")]
    UnsafeRoot { path: PathBuf, reason: String },
    #[error("nix store garbage collection failed: {0}")]
    Collect(String),
    #[error(
        "cannot secure live GC root for witness {sequence} path {target}: {reason}; expired roots were left untouched"
    )]
    LiveRootRegistration {
        sequence: u64,
        target: PathBuf,
        reason: String,
    },
}

fn io_error(path: &Path, source: std::io::Error) -> RetentionError {
    RetentionError::Io {
        path: path.to_owned(),
        source,
    }
}

pub fn parse_horizon(value: &str) -> Result<Duration, RetentionError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid_horizon(value, "value must be non-empty"));
    }

    let bytes = value.as_bytes();
    let mut cursor = 0;
    let mut total_seconds = 0.0_f64;
    let mut components = 0_u32;
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor == bytes.len() {
            break;
        }
        let number_start = cursor;
        let mut dots = 0_u8;
        while cursor < bytes.len() && (bytes[cursor].is_ascii_digit() || bytes[cursor] == b'.') {
            if bytes[cursor] == b'.' {
                dots += 1;
            }
            cursor += 1;
        }
        if cursor == number_start || dots > 1 {
            return Err(invalid_horizon(
                value,
                "expected a positive number followed by a systemd time unit",
            ));
        }
        let amount = value[number_start..cursor]
            .parse::<f64>()
            .map_err(|_| invalid_horizon(value, "time component is not a finite number"))?;
        if !amount.is_finite() || amount < 0.0 {
            return Err(invalid_horizon(
                value,
                "time components must be non-negative and finite",
            ));
        }

        let unit_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_alphabetic() {
            cursor += 1;
        }
        let unit = &value[unit_start..cursor];
        let multiplier = match unit {
            "" | "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            "ms" | "msec" | "msecs" => 0.001,
            "us" | "usec" | "usecs" => 0.000_001,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600.0,
            "d" | "day" | "days" => 86_400.0,
            "w" | "week" | "weeks" => 604_800.0,
            "month" | "months" => 2_629_800.0,
            "y" | "year" | "years" => 31_557_600.0,
            _ => {
                return Err(invalid_horizon(
                    value,
                    format!("unsupported systemd time unit {unit:?}"),
                ))
            }
        };
        total_seconds += amount * multiplier;
        components += 1;
    }
    if components == 0 || !total_seconds.is_finite() || total_seconds < 0.0 {
        return Err(invalid_horizon(
            value,
            "duration must be non-negative and finite",
        ));
    }
    Duration::try_from_secs_f64(total_seconds)
        .map_err(|_| invalid_horizon(value, "duration is outside the supported range"))
}

fn invalid_horizon(value: &str, reason: impl Into<String>) -> RetentionError {
    RetentionError::InvalidHorizon {
        value: value.to_owned(),
        reason: reason.into(),
    }
}

pub fn referenced_store_paths(record: &WitnessRecord) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();
    if let Some(store_paths) = &record.store_paths {
        paths.extend(store_paths.iter().map(PathBuf::from));
    }
    if let Some(drv) = &record.drv {
        paths.insert(PathBuf::from(&drv.drv_path));
        paths.extend(drv.outputs.iter().map(|output| PathBuf::from(&output.path)));
    }
    paths
}

pub fn register_record_roots(
    gcroots_dir: &Path,
    record: &WitnessRecord,
    backend: &impl GcRootBackend,
) -> RootRegistrationReport {
    let mut report = RootRegistrationReport::default();
    let root_dir = root_directory(gcroots_dir, record.seq);
    for target in referenced_store_paths(record) {
        report.attempted += 1;
        let basename = target
            .file_name()
            .expect("validated Nix store paths always have a basename");
        let link = root_dir.join(basename);
        match backend.add_root(&link, &target) {
            Ok(()) => report.registered += 1,
            Err(reason) => report.failures.push(RootRegistrationFailure {
                link,
                target,
                reason,
            }),
        }
    }
    report
}

pub fn reconcile_recent_roots(
    gcroots_dir: &Path,
    records: &[WitnessRecord],
    now: DateTime<Utc>,
    horizon: Duration,
    backend: &impl GcRootBackend,
) -> Result<Vec<(u64, RootRegistrationReport)>, RetentionError> {
    let cutoff = cutoff(now, horizon)?;
    let mut reports = Vec::new();
    for record in records {
        if record_timestamp(record)? >= cutoff && !referenced_store_paths(record).is_empty() {
            reports.push((
                record.seq,
                register_record_roots(gcroots_dir, record, backend),
            ));
        }
    }
    Ok(reports)
}

pub fn run_gc(
    request: GcRequest<'_>,
    backend: &impl GcRootBackend,
) -> Result<GcReport, RetentionError> {
    let GcRequest {
        data_dir,
        state_dir,
        horizon_text,
        state_retention,
        now,
        dry_run,
        collect,
    } = request;
    let horizon = parse_horizon(horizon_text)?;
    // Brief admission takes the shared side before it publishes a durable row
    // or witness. Take the exclusive side first, then the GC-roots lock, in the
    // same order as brief-bearing substitution admission.
    let _brief_lock = state_dir
        .map(|_| brief::acquire_exclusive(data_dir))
        .transpose()?;
    let gcroots_dir = data_dir.join("gcroots");
    let lock_path = gcroots_lock_path(&gcroots_dir);
    let _lock = acquire_gc_lock(&gcroots_dir).map_err(|source| io_error(&lock_path, source))?;
    let witness_path = data_dir.join("witness.jsonl");
    let (verification, records) = read_verified_records(&witness_path)?;
    if !verification.ok {
        return Err(RetentionError::InvalidLedger);
    }
    let cutoff = cutoff(now, horizon)?;
    let by_seq = records
        .iter()
        .map(|record| (record.seq, record))
        .collect::<BTreeMap<_, _>>();
    let mut live = BTreeSet::new();
    for record in &records {
        if record_timestamp(record)? >= cutoff {
            live.extend(referenced_store_paths(record));
        }
    }
    if !dry_run {
        for (sequence, report) in
            reconcile_recent_roots(&gcroots_dir, &records, now, horizon, backend)?
        {
            if let Some(failure) = report.failures.into_iter().next() {
                return Err(RetentionError::LiveRootRegistration {
                    sequence,
                    target: failure.target,
                    reason: failure.reason,
                });
            }
        }
    }

    let mut roots_examined = 0;
    let mut roots_pruned = 0;
    let mut directories_pruned = 0;
    if gcroots_dir.exists() {
        let mut directories = std::fs::read_dir(&gcroots_dir)
            .map_err(|source| io_error(&gcroots_dir, source))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| io_error(&gcroots_dir, source))?;
        directories.sort_by_key(std::fs::DirEntry::file_name);
        for directory in directories {
            let path = directory.path();
            let Some(sequence) = managed_sequence(&directory.file_name()) else {
                continue;
            };
            let Some(record) = by_seq.get(&sequence) else {
                continue;
            };
            if record_timestamp(record)? >= cutoff {
                continue;
            }
            let metadata =
                std::fs::symlink_metadata(&path).map_err(|source| io_error(&path, source))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(RetentionError::UnsafeRoot {
                    path,
                    reason: "managed witness root is not a real directory".to_owned(),
                });
            }
            let expected = referenced_store_paths(record);
            let mut links = std::fs::read_dir(&path)
                .map_err(|source| io_error(&path, source))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| io_error(&path, source))?;
            links.sort_by_key(std::fs::DirEntry::file_name);
            let mut all_removed = true;
            for entry in links {
                let link = entry.path();
                let metadata =
                    std::fs::symlink_metadata(&link).map_err(|source| io_error(&link, source))?;
                if !metadata.file_type().is_symlink() {
                    return Err(RetentionError::UnsafeRoot {
                        path: link,
                        reason: "managed root entry is not a symlink".to_owned(),
                    });
                }
                let target = std::fs::read_link(&link).map_err(|source| io_error(&link, source))?;
                if !target.to_str().is_some_and(is_nix_store_path) || !expected.contains(&target) {
                    return Err(RetentionError::UnsafeRoot {
                        path: link,
                        reason: format!(
                            "symlink target {} is not referenced by witness {sequence}",
                            target.display()
                        ),
                    });
                }
                roots_examined += 1;
                if live.contains(&target) {
                    all_removed = false;
                    continue;
                }
                roots_pruned += 1;
                if !dry_run {
                    std::fs::remove_file(&link).map_err(|source| io_error(&link, source))?;
                }
            }
            if all_removed {
                directories_pruned += 1;
                if !dry_run {
                    match std::fs::remove_dir(&path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(source) => return Err(io_error(&path, source)),
                    }
                }
            }
        }
    }

    let briefs = match state_dir {
        Some(state_dir) => sweep_brief_stores(data_dir, state_dir, &records, cutoff, dry_run)?,
        None => BriefSweep::default(),
    };
    let state = match state_dir {
        Some(state_dir) => sweep_state_directory(state_dir, &state_retention, now, dry_run)?,
        None => StateSweep::default(),
    };

    let collected = collect && !dry_run;
    if collected {
        backend.collect_garbage().map_err(RetentionError::Collect)?;
    }
    Ok(GcReport {
        horizon: horizon_text.to_owned(),
        dry_run,
        collect_requested: collect,
        live_paths: live.len(),
        roots_examined,
        roots_pruned,
        root_directories_pruned: directories_pruned,
        brief_stores_swept: state_dir.is_some(),
        briefs_examined: briefs.data_examined,
        briefs_retained: briefs.data_retained,
        briefs_pruned: briefs.data_pruned,
        legacy_briefs_examined: briefs.legacy_examined,
        legacy_briefs_pruned: briefs.legacy_pruned,
        state_dir_swept: state_dir.is_some(),
        capture_archives_examined: state.capture_archives_examined,
        capture_archives_pruned: state.capture_archives_pruned,
        capture_archive_directories_pruned: state.capture_archive_directories_pruned,
        events_done_examined: state.events_done_examined,
        events_done_pruned: state.events_done_pruned,
        events_rejected_examined: state.events_rejected_examined,
        events_rejected_pruned: state.events_rejected_pruned,
        collected,
    })
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct BriefSweep {
    data_examined: usize,
    data_retained: usize,
    data_pruned: usize,
    legacy_examined: usize,
    legacy_pruned: usize,
}

/// Marks briefs required by an unwitnessed attempt or a witness inside the
/// configured retention horizon, then sweeps the canonical data store. The
/// state store is the legacy producer location shipped by #250; it receives an
/// age floor as well as the same live-row floor so an upgrade can retire those
/// duplicate and orphaned files safely.
fn sweep_brief_stores(
    data_dir: &Path,
    state_dir: &Path,
    records: &[WitnessRecord],
    retained_cutoff: DateTime<Utc>,
    dry_run: bool,
) -> Result<BriefSweep, RetentionError> {
    let events_dir = state_dir.join(EVENTS_DIRECTORY);
    let _ingress_lock = events_dir
        .is_dir()
        .then(|| lock_ingress_for_sweep(&events_dir))
        .transpose()?;
    let pending_paths = pending_ingress_brief_paths(&events_dir)?;
    let events = read_acknowledged_events(&events_dir)?;

    let witnessed_attempts = records
        .iter()
        .filter_map(|record| {
            record
                .task_uuid
                .as_ref()
                .map(|task| (task.clone(), record.attempt))
        })
        .collect::<BTreeSet<_>>();
    let mut retained = records
        .iter()
        .filter(|record| {
            record_timestamp(record).is_ok_and(|timestamp| timestamp >= retained_cutoff)
        })
        .filter_map(|record| record.brief_hash.clone())
        .collect::<BTreeSet<_>>();

    for event in &events {
        let Some(hash) = event.row.brief_hash.as_ref() else {
            continue;
        };
        let attempt = event
            .retries
            .iter()
            .map(|retry| retry.attempt)
            .max()
            .unwrap_or(event.row.attempt);
        let identity = (event.row.uuid.to_string(), attempt);
        if !witnessed_attempts.contains(&identity) {
            retained.insert(hash.clone());
        }
    }
    for path in &pending_paths {
        if let Ok(prepared) = brief::PreparedBrief::from_path(path) {
            retained.insert(prepared.hash().to_owned());
        }
    }

    let mut sweep = BriefSweep::default();
    for (hash, path, _) in managed_brief_files(data_dir)? {
        sweep.data_examined += 1;
        if retained.contains(&hash) {
            sweep.data_retained += 1;
        } else {
            sweep.data_pruned += 1;
            remove_managed_brief(&path, dry_run)?;
        }
    }

    if data_dir != state_dir {
        let legacy_cutoff = SystemTime::from(retained_cutoff);
        for (hash, path, modified) in managed_brief_files(state_dir)? {
            sweep.legacy_examined += 1;
            if retained.contains(&hash)
                || pending_paths.contains(&path)
                || modified >= legacy_cutoff
            {
                continue;
            }
            sweep.legacy_pruned += 1;
            remove_managed_brief(&path, dry_run)?;
        }
    }
    Ok(sweep)
}

fn managed_brief_files(root: &Path) -> Result<Vec<(String, PathBuf, SystemTime)>, RetentionError> {
    let directory = root.join(brief::BRIEF_DIRECTORY);
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = std::fs::read_dir(&directory)
        .map_err(|source| io_error(&directory, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| io_error(&directory, source))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut managed = Vec::new();
    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name();
        let Some(digest) = file_name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        else {
            continue;
        };
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|source| io_error(&path, source))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        let hash = format!("sha256:{digest}");
        brief::read_verified(&path, &hash)?;
        let modified = metadata
            .modified()
            .map_err(|source| io_error(&path, source))?;
        managed.push((hash, path, modified));
    }
    Ok(managed)
}

fn remove_managed_brief(path: &Path, dry_run: bool) -> Result<(), RetentionError> {
    if dry_run {
        return Ok(());
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error(path, source)),
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct StateSweep {
    capture_archives_examined: usize,
    capture_archives_pruned: usize,
    capture_archive_directories_pruned: usize,
    events_done_examined: usize,
    events_done_pruned: usize,
    events_rejected_examined: usize,
    events_rejected_pruned: usize,
}

/// Prunes the two unbounded on-disk sets under the daemon state directory.
///
/// Runs inside `run_gc`, under the GC-roots lock it already holds, so there is
/// no second sweep entry point and no second timer. The ingress directories are
/// additionally guarded by the producer ingress lock, because the daemon renames
/// consumed event files into `done`/`rejected` while holding it.
fn sweep_state_directory(
    state_dir: &Path,
    policy: &StateRetentionPolicy,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<StateSweep, RetentionError> {
    let mut sweep = StateSweep::default();
    let archive_root = state_dir.join(CAPTURE_ARCHIVE_DIRECTORY);
    let capture_cutoff = mtime_cutoff(now, policy.capture_archive_max_age)?;
    prune_capture_archives(&archive_root, capture_cutoff, dry_run, &mut sweep)?;

    let events_dir = state_dir.join(EVENTS_DIRECTORY);
    if !events_dir.is_dir() {
        return Ok(sweep);
    }
    let _ingress_lock = lock_ingress_for_sweep(&events_dir)?;

    let done_cutoff = mtime_cutoff(now, policy.events_done_max_age)?;
    let done = prune_directory(
        &events_dir.join("done"),
        done_cutoff,
        // The audit trail has no count bound; the ruling deliberately gives the
        // two event sets separate envelopes.
        usize::MAX,
        dry_run,
    )?;
    sweep.events_done_examined = done.examined;
    sweep.events_done_pruned = done.pruned;

    let rejected_cutoff = mtime_cutoff(now, policy.events_rejected_max_age)?;
    let rejected = prune_directory(
        &events_dir.join("rejected"),
        rejected_cutoff,
        policy.events_rejected_max_count,
        dry_run,
    )?;
    sweep.events_rejected_examined = rejected.examined;
    sweep.events_rejected_pruned = rejected.pruned;
    Ok(sweep)
}

fn lock_ingress_for_sweep(events_dir: &Path) -> Result<File, RetentionError> {
    let path = events_dir.join(INGRESS_LOCK_FILE_NAME);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|source| io_error(&path, source))?;
    FileExt::lock_exclusive(&file).map_err(|source| io_error(&path, source))?;
    Ok(file)
}

fn prune_capture_archives(
    archive_root: &Path,
    cutoff: SystemTime,
    dry_run: bool,
    sweep: &mut StateSweep,
) -> Result<(), RetentionError> {
    if !archive_root.is_dir() {
        return Ok(());
    }
    let mut units = std::fs::read_dir(archive_root)
        .map_err(|source| io_error(archive_root, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| io_error(archive_root, source))?;
    units.sort_by_key(std::fs::DirEntry::file_name);
    for unit in units {
        let unit_dir = unit.path();
        let metadata = match std::fs::symlink_metadata(&unit_dir) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(io_error(&unit_dir, source)),
        };
        // Anything that is not a plain per-unit directory was not written by
        // the archiver; leave it for an operator rather than deleting it.
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        // Sampled before pruning: removing entries bumps the directory mtime,
        // which would otherwise make an emptied directory look freshly touched.
        let directory_expired = metadata
            .modified()
            .map(|modified| modified < cutoff)
            .unwrap_or(false);
        let outcome = prune_directory(&unit_dir, cutoff, usize::MAX, dry_run)?;
        sweep.capture_archives_examined += outcome.examined;
        sweep.capture_archives_pruned += outcome.pruned;
        if !(directory_expired && outcome.skipped == 0 && outcome.examined == outcome.pruned) {
            continue;
        }
        if dry_run {
            sweep.capture_archive_directories_pruned += 1;
            continue;
        }
        match std::fs::remove_dir(&unit_dir) {
            Ok(()) => sweep.capture_archive_directories_pruned += 1,
            // A concurrent archive write can repopulate the directory between
            // the file prune and this rmdir. Leaving it to the next sweep is
            // correct; failing the whole sweep is not.
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(libc::ENOTEMPTY) => {}
            Err(source) => return Err(io_error(&unit_dir, source)),
        }
    }
    Ok(())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct PruneOutcome {
    examined: usize,
    pruned: usize,
    skipped: usize,
}

/// Prunes regular files in `directory` whose mtime predates `cutoff`, then
/// prunes the oldest survivors until at most `max_count` remain.
fn prune_directory(
    directory: &Path,
    cutoff: SystemTime,
    max_count: usize,
    dry_run: bool,
) -> Result<PruneOutcome, RetentionError> {
    let mut outcome = PruneOutcome::default();
    if !directory.is_dir() {
        return Ok(outcome);
    }
    let entries = std::fs::read_dir(directory)
        .map_err(|source| io_error(directory, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| io_error(directory, source))?;

    let mut expired = Vec::new();
    let mut retained: Vec<(SystemTime, PathBuf)> = Vec::new();
    for entry in entries {
        let path = entry.path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(io_error(&path, source)),
        };
        // Directories, symlinks and device nodes are not archiver or ingress
        // output; count them as skipped and never unlink them.
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            outcome.skipped += 1;
            continue;
        }
        outcome.examined += 1;
        let modified = metadata
            .modified()
            .map_err(|source| io_error(&path, source))?;
        if modified < cutoff {
            expired.push(path);
        } else {
            retained.push((modified, path));
        }
    }

    if retained.len() > max_count {
        retained.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        let overflow = retained.len() - max_count;
        expired.extend(retained.drain(..overflow).map(|(_, path)| path));
    }

    expired.sort();
    for path in expired {
        outcome.pruned += 1;
        if dry_run {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(&path, source)),
        }
    }
    Ok(outcome)
}

fn mtime_cutoff(now: DateTime<Utc>, max_age: Duration) -> Result<SystemTime, RetentionError> {
    Ok(SystemTime::from(cutoff(now, max_age)?))
}

fn cutoff(now: DateTime<Utc>, horizon: Duration) -> Result<DateTime<Utc>, RetentionError> {
    let horizon = chrono::TimeDelta::from_std(horizon)
        .map_err(|_| invalid_horizon("<parsed>", "duration is outside chrono's range"))?;
    now.checked_sub_signed(horizon)
        .ok_or_else(|| invalid_horizon("<parsed>", "duration predates the timestamp range"))
}

fn record_timestamp(record: &WitnessRecord) -> Result<DateTime<Utc>, RetentionError> {
    DateTime::parse_from_rfc3339(&record.transition_timestamp)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|_| RetentionError::InvalidLedger)
}

fn root_directory(gcroots_dir: &Path, sequence: u64) -> PathBuf {
    gcroots_dir.join(format!("{ROOT_DIRECTORY_PREFIX}{sequence}"))
}

fn managed_sequence(name: &std::ffi::OsStr) -> Option<u64> {
    let suffix = name.to_str()?.strip_prefix(ROOT_DIRECTORY_PREFIX)?;
    (!suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| suffix.parse().ok())
        .flatten()
        .filter(|sequence| *sequence > 0)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::os::unix::fs::symlink;

    use chrono::{SecondsFormat, TimeZone};

    use super::*;
    use crate::adapters::AdapterJobOptions;
    use crate::config::Priority;
    use crate::executor::Uuid;
    use crate::taskdb::{
        write_enqueue_event_atomic, AdmissionOrigin, DurableEnqueueEvent, EnqueueSource, RowSeed,
        CURRENT_ROW_VERSION,
    };
    use crate::witness::{
        Derivation, DerivationOutput, LaborClass, Verdict, WitnessBody, WitnessLedger,
    };

    const SHARED: &str = "/nix/store/00000000000000000000000000000000-shared";
    const OLD_ONLY: &str = "/nix/store/11111111111111111111111111111111-old";
    const DRV: &str = "/nix/store/22222222222222222222222222222222-build.drv";

    #[derive(Default)]
    struct FakeBackend {
        roots: RefCell<Vec<(PathBuf, PathBuf)>>,
        collections: RefCell<u32>,
        fail_targets: BTreeSet<PathBuf>,
    }

    impl GcRootBackend for FakeBackend {
        fn add_root(&self, link: &Path, target: &Path) -> Result<(), String> {
            self.roots
                .borrow_mut()
                .push((link.to_owned(), target.to_owned()));
            if self.fail_targets.contains(target) {
                Err("injected root registration failure".to_owned())
            } else {
                Ok(())
            }
        }

        fn collect_garbage(&self) -> Result<(), String> {
            *self.collections.borrow_mut() += 1;
            Ok(())
        }
    }

    fn append(
        ledger: &mut WitnessLedger,
        timestamp: DateTime<Utc>,
        store_paths: Vec<String>,
        drv: Option<Derivation>,
    ) -> WitnessRecord {
        ledger
            .append(WitnessBody {
                task_uuid: None,
                transition_timestamp: timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
                verdict: Verdict::Pass,
                exit_code: 0,
                artifact_content_hash: None,
                store_paths: (!store_paths.is_empty()).then_some(store_paths),
                drv,
                gpu_seconds: None,
                wall_clock: 1.0,
                attempt: 1,
                lease_epoch: 1,
                dedup_key: None,
                payload_hash: None,
                brief_hash: None,
                origin: AdmissionOrigin::direct(EnqueueSource::Manual),
                orchestration: None,
                labor_class: LaborClass::Fresh,
                trace_ref: None,
                pools: vec!["build".to_owned()],
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
            .unwrap()
    }

    fn append_brief_witness(
        ledger: &mut WitnessLedger,
        timestamp: DateTime<Utc>,
        task_uuid: Uuid,
        brief_hash: &str,
    ) -> WitnessRecord {
        ledger
            .append(WitnessBody {
                task_uuid: Some(task_uuid.to_string()),
                transition_timestamp: timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
                verdict: Verdict::Pass,
                exit_code: 0,
                artifact_content_hash: None,
                store_paths: None,
                drv: None,
                gpu_seconds: None,
                wall_clock: 1.0,
                attempt: 1,
                lease_epoch: 1,
                dedup_key: None,
                payload_hash: None,
                brief_hash: Some(brief_hash.to_owned()),
                origin: AdmissionOrigin::direct(EnqueueSource::Manual),
                orchestration: None,
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
            .unwrap()
    }

    fn brief_event(task_uuid: Uuid, brief_hash: &str) -> DurableEnqueueEvent {
        DurableEnqueueEvent::new(RowSeed {
            row_version: CURRENT_ROW_VERSION,
            uuid: task_uuid,
            description: "brief retention fixture".to_owned(),
            priority: Priority::Medium,
            source: EnqueueSource::Manual,
            adapter: "shell".to_owned(),
            pools: vec!["slot".to_owned()],
            executor: None,
            model: None,
            cwd: None,
            workspace: None,
            adapter_options: AdapterJobOptions::default(),
            gate_manifest: None,
            resumed_from: None,
            dedup_key: None,
            payload_hash: None,
            brief_hash: Some(brief_hash.to_owned()),
            orchestration: None,
            session_ref: None,
            final_message: None,
            job_token_hash: None,
            lease_epoch: 1,
            attempt: 1,
            argv: vec!["true".to_owned()],
            evidence: Vec::new(),
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
        })
        .unwrap()
    }

    #[test]
    fn systemd_timespan_subset_accepts_composition_and_rejects_ambiguity() {
        assert_eq!(
            parse_horizon("30d").unwrap(),
            Duration::from_secs(2_592_000)
        );
        assert_eq!(
            parse_horizon("1h 30min").unwrap(),
            Duration::from_secs(5_400)
        );
        assert_eq!(parse_horizon("1.5h").unwrap(), Duration::from_secs(5_400));
        assert_eq!(parse_horizon("0s").unwrap(), Duration::ZERO);
        for invalid in ["", "infinity", "1fortnight", "1..5h", "-1s"] {
            assert!(
                parse_horizon(invalid).is_err(),
                "{invalid:?} unexpectedly parsed"
            );
        }
    }

    #[test]
    fn registration_unions_store_drv_and_output_paths_once() {
        let record = WitnessRecord {
            seq: 7,
            store_paths: Some(vec![SHARED.to_owned()]),
            drv: Some(Derivation {
                drv_path: DRV.to_owned(),
                outputs: vec![DerivationOutput {
                    name: "out".to_owned(),
                    path: SHARED.to_owned(),
                }],
            }),
            ..append_record_template()
        };
        let backend = FakeBackend::default();
        let report = register_record_roots(Path::new("/data/gcroots"), &record, &backend);
        assert_eq!(report.attempted, 2);
        assert_eq!(report.registered, 2);
        assert_eq!(backend.roots.borrow().len(), 2);
        assert!(backend.roots.borrow().iter().all(|(link, _)| link
            .parent()
            .is_some_and(|parent| parent.ends_with("witness-7"))));
    }

    #[test]
    fn pruning_preserves_live_floor_collects_old_only_and_never_edits_ledger() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path();
        let now = Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap();
        let mut ledger = WitnessLedger::open(data.join("witness.jsonl")).unwrap();
        append(
            &mut ledger,
            now - chrono::TimeDelta::days(40),
            vec![SHARED.to_owned(), OLD_ONLY.to_owned()],
            None,
        );
        append(
            &mut ledger,
            now - chrono::TimeDelta::days(1),
            vec![SHARED.to_owned()],
            None,
        );
        drop(ledger);
        let ledger_before = fs::read(data.join("witness.jsonl")).unwrap();

        let old_roots = data.join("gcroots/witness-1");
        let live_roots = data.join("gcroots/witness-2");
        fs::create_dir_all(&old_roots).unwrap();
        fs::create_dir_all(&live_roots).unwrap();
        symlink(
            SHARED,
            old_roots.join(Path::new(SHARED).file_name().unwrap()),
        )
        .unwrap();
        symlink(
            OLD_ONLY,
            old_roots.join(Path::new(OLD_ONLY).file_name().unwrap()),
        )
        .unwrap();
        symlink(
            SHARED,
            live_roots.join(Path::new(SHARED).file_name().unwrap()),
        )
        .unwrap();

        let backend = FakeBackend::default();
        let dry_report = run_gc(gc_request(data, now, true), &backend).unwrap();
        assert_eq!(dry_report.roots_pruned, 1);
        assert!(!dry_report.collected);
        assert!(
            fs::symlink_metadata(old_roots.join(Path::new(OLD_ONLY).file_name().unwrap())).is_ok()
        );
        assert_eq!(*backend.collections.borrow(), 0);

        let report = run_gc(gc_request(data, now, false), &backend).unwrap();
        assert_eq!(report.live_paths, 1);
        assert_eq!(report.roots_examined, 2);
        assert_eq!(report.roots_pruned, 1);
        assert_eq!(*backend.collections.borrow(), 1);
        assert!(
            fs::symlink_metadata(old_roots.join(Path::new(SHARED).file_name().unwrap())).is_ok()
        );
        assert!(
            fs::symlink_metadata(old_roots.join(Path::new(OLD_ONLY).file_name().unwrap())).is_err()
        );
        assert!(
            fs::symlink_metadata(live_roots.join(Path::new(SHARED).file_name().unwrap())).is_ok()
        );
        assert_eq!(fs::read(data.join("witness.jsonl")).unwrap(), ledger_before);
    }

    #[test]
    fn live_root_registration_failure_aborts_before_pruning_or_collection() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path();
        let now = Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap();
        let mut ledger = WitnessLedger::open(data.join("witness.jsonl")).unwrap();
        append(
            &mut ledger,
            now - chrono::TimeDelta::days(40),
            vec![OLD_ONLY.to_owned()],
            None,
        );
        append(
            &mut ledger,
            now - chrono::TimeDelta::days(1),
            vec![SHARED.to_owned()],
            None,
        );
        drop(ledger);

        let old_roots = data.join("gcroots/witness-1");
        fs::create_dir_all(&old_roots).unwrap();
        let old_link = old_roots.join(Path::new(OLD_ONLY).file_name().unwrap());
        symlink(OLD_ONLY, &old_link).unwrap();
        let backend = FakeBackend {
            fail_targets: BTreeSet::from([PathBuf::from(SHARED)]),
            ..FakeBackend::default()
        };

        assert!(matches!(
            run_gc(gc_request(data, now, false), &backend),
            Err(RetentionError::LiveRootRegistration { sequence: 2, .. })
        ));
        assert!(fs::symlink_metadata(old_link).is_ok());
        assert_eq!(*backend.collections.borrow(), 0);
    }

    fn gc_request(data: &Path, now: DateTime<Utc>, dry_run: bool) -> GcRequest<'_> {
        GcRequest {
            data_dir: data,
            state_dir: None,
            horizon_text: "30d",
            state_retention: StateRetentionPolicy::default(),
            now,
            dry_run,
            collect: true,
        }
    }

    fn write_aged(path: &Path, age: chrono::TimeDelta, now: DateTime<Utc>) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"payload").unwrap();
        set_mtime(path, now - age);
    }

    fn set_mtime(path: &Path, at: DateTime<Utc>) {
        // Age is injected through mtimes rather than slept for, so the sweep is
        // exercised against real files without a wall-clock dependency.
        let times = fs::FileTimes::new()
            .set_accessed(SystemTime::from(at))
            .set_modified(SystemTime::from(at));
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(times)
            .unwrap();
    }

    fn ledger_only_state(data: &Path, now: DateTime<Utc>) {
        let mut ledger = WitnessLedger::open(data.join("witness.jsonl")).unwrap();
        append(
            &mut ledger,
            now - chrono::TimeDelta::days(1),
            vec![SHARED.to_owned()],
            None,
        );
    }

    #[test]
    fn brief_sweep_marks_live_and_recent_rows_and_collects_legacy_duplicates() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&state).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap();

        let live = brief::PreparedBrief::from_value(serde_json::json!({"kind": "live"})).unwrap();
        let recent =
            brief::PreparedBrief::from_value(serde_json::json!({"kind": "recent"})).unwrap();
        let expired =
            brief::PreparedBrief::from_value(serde_json::json!({"kind": "expired"})).unwrap();
        let orphan =
            brief::PreparedBrief::from_value(serde_json::json!({"kind": "orphan"})).unwrap();
        for prepared in [&live, &recent, &expired, &orphan] {
            brief::store(&data, prepared).unwrap();
        }

        let live_task = Uuid::new_v4();
        let recent_task = Uuid::new_v4();
        let expired_task = Uuid::new_v4();
        for event in [
            brief_event(live_task, live.hash()),
            brief_event(recent_task, recent.hash()),
            brief_event(expired_task, expired.hash()),
        ] {
            write_enqueue_event_atomic(&state.join("events"), &event).unwrap();
        }
        let mut ledger = WitnessLedger::open(data.join("witness.jsonl")).unwrap();
        append_brief_witness(
            &mut ledger,
            now - chrono::TimeDelta::days(1),
            recent_task,
            recent.hash(),
        );
        append_brief_witness(
            &mut ledger,
            now - chrono::TimeDelta::days(40),
            expired_task,
            expired.hash(),
        );
        drop(ledger);

        let legacy_live = brief::store(&state, &live).unwrap();
        let legacy_expired = brief::store(&state, &expired).unwrap();
        let legacy_fresh_orphan = brief::store(&state, &orphan).unwrap();
        set_mtime(&legacy_live, now - chrono::TimeDelta::days(40));
        set_mtime(&legacy_expired, now - chrono::TimeDelta::days(40));
        set_mtime(&legacy_fresh_orphan, now - chrono::TimeDelta::days(1));

        let request = GcRequest {
            data_dir: &data,
            state_dir: Some(&state),
            horizon_text: "30d",
            state_retention: StateRetentionPolicy::default(),
            now,
            dry_run: true,
            collect: false,
        };
        let dry = run_gc(request.clone(), &FakeBackend::default()).unwrap();
        assert!(dry.brief_stores_swept);
        assert_eq!(dry.briefs_examined, 4);
        assert_eq!(dry.briefs_retained, 2);
        assert_eq!(dry.briefs_pruned, 2);
        assert_eq!(dry.legacy_briefs_examined, 3);
        assert_eq!(dry.legacy_briefs_pruned, 1);
        assert!(brief::content_path(&data, expired.hash()).unwrap().exists());

        let report = run_gc(
            GcRequest {
                dry_run: false,
                ..request
            },
            &FakeBackend::default(),
        )
        .unwrap();
        assert_eq!(report.briefs_pruned, 2);
        assert_eq!(report.legacy_briefs_pruned, 1);
        assert!(brief::content_path(&data, live.hash()).unwrap().exists());
        assert!(brief::content_path(&data, recent.hash()).unwrap().exists());
        assert!(!brief::content_path(&data, expired.hash()).unwrap().exists());
        assert!(!brief::content_path(&data, orphan.hash()).unwrap().exists());
        assert!(legacy_live.exists());
        assert!(!legacy_expired.exists());
        assert!(legacy_fresh_orphan.exists());
    }

    #[test]
    fn ratified_defaults_match_the_retention_envelope() {
        let policy = StateRetentionPolicy::default();
        assert_eq!(
            policy.capture_archive_max_age,
            Duration::from_secs(30 * 86_400)
        );
        assert_eq!(
            policy.events_done_max_age,
            Duration::from_secs(180 * 86_400)
        );
        assert_eq!(
            policy.events_rejected_max_age,
            Duration::from_secs(30 * 86_400)
        );
        assert_eq!(policy.events_rejected_max_count, 10_000);
    }

    #[test]
    fn state_sweep_expires_by_age_without_pinning_archives_to_the_ledger() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state");
        fs::create_dir_all(&data).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap();
        ledger_only_state(&data, now);

        // The archive belongs to the attempt the live witness record describes,
        // and still expires: witness records do not pin archives.
        let unit = state.join(CAPTURE_ARCHIVE_DIRECTORY).join("unit-a");
        let stale_out = unit.join("attempt-0000000001-epoch-1.out");
        let fresh_out = unit.join("attempt-0000000002-epoch-1.err");
        write_aged(&stale_out, chrono::TimeDelta::days(31), now);
        write_aged(&fresh_out, chrono::TimeDelta::days(29), now);

        let done_old = state.join("events/done/old.json");
        let done_fresh = state.join("events/done/fresh.json");
        write_aged(&done_old, chrono::TimeDelta::days(181), now);
        write_aged(&done_fresh, chrono::TimeDelta::days(179), now);

        let rejected_old = state.join("events/rejected/old.json");
        let rejected_fresh = state.join("events/rejected/fresh.json");
        write_aged(&rejected_old, chrono::TimeDelta::days(31), now);
        write_aged(&rejected_fresh, chrono::TimeDelta::days(29), now);

        let processing = state.join("events/processing/inflight.json");
        write_aged(&processing, chrono::TimeDelta::days(400), now);

        let backend = FakeBackend::default();
        let request = GcRequest {
            data_dir: &data,
            state_dir: Some(&state),
            horizon_text: "30d",
            state_retention: StateRetentionPolicy::default(),
            now,
            dry_run: true,
            collect: false,
        };
        let dry = run_gc(request.clone(), &backend).unwrap();
        assert!(dry.state_dir_swept);
        assert_eq!(dry.capture_archives_pruned, 1);
        assert_eq!(dry.events_done_pruned, 1);
        assert_eq!(dry.events_rejected_pruned, 1);
        assert!(stale_out.exists());

        let report = run_gc(
            GcRequest {
                dry_run: false,
                ..request
            },
            &backend,
        )
        .unwrap();
        assert_eq!(report.capture_archives_examined, 2);
        assert_eq!(report.capture_archives_pruned, 1);
        assert_eq!(report.capture_archive_directories_pruned, 0);
        assert_eq!(report.events_done_examined, 2);
        assert_eq!(report.events_done_pruned, 1);
        assert_eq!(report.events_rejected_examined, 2);
        assert_eq!(report.events_rejected_pruned, 1);
        assert!(!stale_out.exists());
        assert!(fresh_out.exists());
        assert!(!done_old.exists());
        assert!(done_fresh.exists());
        assert!(!rejected_old.exists());
        assert!(rejected_fresh.exists());
        // In-flight claims are never retention material.
        assert!(processing.exists());
    }

    #[test]
    fn rejected_count_bound_prunes_oldest_first_and_done_has_no_count_bound() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state");
        fs::create_dir_all(&data).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap();
        ledger_only_state(&data, now);

        for index in 0..6_i64 {
            write_aged(
                &state.join(format!("events/rejected/hostile-{index}.json")),
                chrono::TimeDelta::hours(index),
                now,
            );
            write_aged(
                &state.join(format!("events/done/audit-{index}.json")),
                chrono::TimeDelta::hours(index),
                now,
            );
        }

        let report = run_gc(
            GcRequest {
                data_dir: &data,
                state_dir: Some(&state),
                horizon_text: "30d",
                state_retention: StateRetentionPolicy {
                    events_rejected_max_count: 2,
                    ..StateRetentionPolicy::default()
                },
                now,
                dry_run: false,
                collect: false,
            },
            &FakeBackend::default(),
        )
        .unwrap();

        assert_eq!(report.events_rejected_examined, 6);
        assert_eq!(report.events_rejected_pruned, 4);
        assert_eq!(report.events_done_examined, 6);
        assert_eq!(report.events_done_pruned, 0);
        // Newest two survive: index 0 is the youngest.
        for index in 0..2_i64 {
            assert!(state
                .join(format!("events/rejected/hostile-{index}.json"))
                .exists());
        }
        for index in 2..6_i64 {
            assert!(!state
                .join(format!("events/rejected/hostile-{index}.json"))
                .exists());
        }
    }

    #[test]
    fn fully_expired_archive_directory_is_removed_and_foreign_entries_are_left_alone() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state");
        fs::create_dir_all(&data).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap();
        ledger_only_state(&data, now);

        let archive_root = state.join(CAPTURE_ARCHIVE_DIRECTORY);
        let expired_unit = archive_root.join("unit-expired");
        write_aged(
            &expired_unit.join("attempt-0000000001-epoch-1.out"),
            chrono::TimeDelta::days(45),
            now,
        );
        write_aged(
            &expired_unit.join("attempt-0000000001-epoch-1.err"),
            chrono::TimeDelta::days(45),
            now,
        );
        let expired_times = fs::FileTimes::new()
            .set_accessed(SystemTime::from(now - chrono::TimeDelta::days(45)))
            .set_modified(SystemTime::from(now - chrono::TimeDelta::days(45)));
        File::open(&expired_unit)
            .unwrap()
            .set_times(expired_times)
            .unwrap();

        let guarded_unit = archive_root.join("unit-guarded");
        fs::create_dir_all(&guarded_unit).unwrap();
        let dangling = guarded_unit.join("dangling.out");
        symlink("/nonexistent-capture", &dangling).unwrap();

        let report = run_gc(
            GcRequest {
                data_dir: &data,
                state_dir: Some(&state),
                horizon_text: "30d",
                state_retention: StateRetentionPolicy::default(),
                now,
                dry_run: false,
                collect: false,
            },
            &FakeBackend::default(),
        )
        .unwrap();

        assert_eq!(report.capture_archives_pruned, 2);
        assert_eq!(report.capture_archive_directories_pruned, 1);
        assert!(!expired_unit.exists());
        // A symlink in an archive directory was never written by the archiver;
        // it is neither unlinked nor allowed to drag its directory away.
        assert!(fs::symlink_metadata(&dangling).is_ok());
        assert!(guarded_unit.exists());
    }

    fn append_record_template() -> WitnessRecord {
        WitnessRecord {
            schema_version: crate::witness::WITNESS_SCHEMA_VERSION,
            record_type: crate::witness::RecordType::Verdict,
            transition_timestamp: "2026-07-26T00:00:00.000Z".to_owned(),
            task_uuid: None,
            verdict: Verdict::Pass,
            exit_code: 0,
            artifact_content_hash: None,
            store_paths: None,
            drv: None,
            gpu_seconds: None,
            wall_clock: 1.0,
            attempt: 1,
            lease_epoch: 1,
            dedup_key: None,
            payload_hash: None,
            brief_hash: None,
            origin: AdmissionOrigin::direct(EnqueueSource::Manual),
            orchestration: None,
            labor_class: LaborClass::Fresh,
            trace_ref: None,
            pools: vec!["build".to_owned()],
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
            extensions: serde_json::Map::new(),
            seq: 1,
            prev_hash: crate::witness::GENESIS_PREV_HASH.to_owned(),
            hash: String::new(),
        }
    }
}
