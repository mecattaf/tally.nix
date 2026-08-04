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
use crate::executor::{
    Uuid, CAPTURE_ARCHIVE_DIRECTORY, CAPTURE_LOCK_DIRECTORY, CAPTURE_LOCK_SUFFIX,
    LEGACY_CAPTURE_LOCK_DIRECTORY,
};
use crate::nix_store::GcRootBackend;
use crate::producers::{pending_ingress_brief_paths, ProducerError, INGRESS_LOCK_FILE_NAME};
use crate::taskdb::{read_acknowledged_events, TaskDbError};
use crate::witness::{is_nix_store_path, read_verified_records, WitnessError, WitnessRecord};

const ROOT_DIRECTORY_PREFIX: &str = "witness-";
const EVENTS_DIRECTORY: &str = "events";

/// The state-directory root under which `tally adapter smoke --assert-commit`
/// seeds its throwaway git repositories, and the per-run directory prefix it
/// mints inside it. Both are exported so the producer of those directories and
/// the sweep that reaps them cannot name them differently.
pub const ADAPTER_SMOKE_DIRECTORY: &str = "adapter-smoke";
pub const ADAPTER_SMOKE_PROBE_PREFIX: &str = "probe-";

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
/// Producer marker files are per-dispatch idempotency state, so they outlive
/// the item they refer to by design and are only collectable long after the
/// thread they guard has gone quiet. The default deliberately matches the
/// `events/done` audit envelope rather than the shorter archive one.
pub const DEFAULT_PRODUCER_MARKER_MAX_AGE: &str = "180d";

/// The `producers/<set>` directories one sweep collects.
///
/// Every one of them is written once per dispatch and read back to make a
/// forge mutation idempotent, and until now not one of them had a sweeper, a
/// retention entry, or a tmpfiles rule. They are collected as one class rather
/// than one at a time, because "anything written per dispatch with no GC" is
/// the growth surface this tree keeps reproducing.
///
/// `gh-orphaned` is written per dispatch too, though it guards nothing: it is
/// the durable statement that one projection can never be applied, and its
/// only readers are the startup report and `tally producer orphaned`. It joins
/// the class anyway, because the growth argument does not care what a file is
/// for. Collecting one is safe precisely because nothing reads it to decide
/// behaviour, and it does not resurrect: a record can only outlive this
/// horizon if the acknowledged event it describes outlived it first, and once
/// that event is collected at the `events/done` horizon no recovery plan
/// re-derives the projection. Should one be re-derived anyway — an operator
/// running a shorter marker horizon than event horizon — the attestation
/// chain, not the record file, decides whether it has already been witnessed,
/// so a collected record cannot produce a duplicate claim.
const PRODUCER_MARKER_DIRECTORIES: [&str; 5] = [
    "gh-triggers",
    "gh-completed",
    "gh-comments",
    "gh-storage-warnings",
    "gh-orphaned",
];
const PRODUCER_MARKER_DIRECTORY: &str = "producers";
/// The mutual-exclusion file a marker directory keeps for its own writers. It
/// belongs to the directory, not to any one marker, and is never collected.
const PRODUCER_MUTATION_LOCK: &str = "mutations.lock";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRetentionPolicy {
    pub capture_archive_max_age: Duration,
    pub events_done_max_age: Duration,
    pub events_rejected_max_age: Duration,
    pub events_rejected_max_count: usize,
    pub producer_marker_max_age: Duration,
}

impl StateRetentionPolicy {
    pub fn parse(
        capture_archive_max_age: &str,
        events_done_max_age: &str,
        events_rejected_max_age: &str,
        events_rejected_max_count: usize,
        producer_marker_max_age: &str,
    ) -> Result<Self, RetentionError> {
        Ok(Self {
            capture_archive_max_age: parse_horizon(capture_archive_max_age)?,
            events_done_max_age: parse_horizon(events_done_max_age)?,
            events_rejected_max_age: parse_horizon(events_rejected_max_age)?,
            events_rejected_max_count,
            producer_marker_max_age: parse_horizon(producer_marker_max_age)?,
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
            DEFAULT_PRODUCER_MARKER_MAX_AGE,
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
    /// Files in the canonical store that carry a managed `<64hex>.json` name but
    /// do not verify against it. They are skipped, never pruned, and counted
    /// here so the condition is visible in every `tally gc` report.
    pub briefs_unverified: usize,
    pub legacy_briefs_examined: usize,
    pub legacy_briefs_pruned: usize,
    pub legacy_briefs_unverified: usize,
    pub state_dir_swept: bool,
    pub capture_archives_examined: usize,
    pub capture_archives_pruned: usize,
    pub capture_archive_directories_pruned: usize,
    pub capture_locks_examined: usize,
    pub capture_locks_pruned: usize,
    /// Retained `adapter smoke --assert-commit` probe repositories seen and
    /// reaped under the capture-archive horizon.
    pub adapter_probes_examined: usize,
    pub adapter_probes_pruned: usize,
    pub events_done_examined: usize,
    pub events_done_pruned: usize,
    pub events_rejected_examined: usize,
    pub events_rejected_pruned: usize,
    pub producer_markers_examined: usize,
    pub producer_markers_pruned: usize,
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
        briefs_unverified: briefs.data_unverified,
        legacy_briefs_examined: briefs.legacy_examined,
        legacy_briefs_pruned: briefs.legacy_pruned,
        legacy_briefs_unverified: briefs.legacy_unverified,
        state_dir_swept: state_dir.is_some(),
        capture_archives_examined: state.capture_archives_examined,
        capture_archives_pruned: state.capture_archives_pruned,
        capture_archive_directories_pruned: state.capture_archive_directories_pruned,
        capture_locks_examined: state.capture_locks_examined,
        capture_locks_pruned: state.capture_locks_pruned,
        adapter_probes_examined: state.adapter_probes_examined,
        adapter_probes_pruned: state.adapter_probes_pruned,
        events_done_examined: state.events_done_examined,
        events_done_pruned: state.events_done_pruned,
        events_rejected_examined: state.events_rejected_examined,
        events_rejected_pruned: state.events_rejected_pruned,
        producer_markers_examined: state.producer_markers_examined,
        producer_markers_pruned: state.producer_markers_pruned,
        collected,
    })
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct BriefSweep {
    data_examined: usize,
    data_retained: usize,
    data_pruned: usize,
    data_unverified: usize,
    legacy_examined: usize,
    legacy_pruned: usize,
    legacy_unverified: usize,
}

/// Marks briefs required by an unwitnessed attempt or a witness inside the
/// configured retention horizon, then sweeps the canonical data store. The
/// state store is the legacy producer location shipped by #250; it receives an
/// age floor as well as the same live-row floor so an upgrade can retire those
/// duplicate and orphaned files safely.
///
/// A file whose bytes do not verify against the hash in its own name is
/// **counted and skipped**, never pruned and never renamed. Two facts decide
/// that: such a file is unaddressable — no live brief hash can resolve to it,
/// so leaving it costs only its own bounded bytes — and it is the one case
/// where the sweep cannot prove what it is looking at. Retention removes bytes
/// it can prove are unreferenced; a file it cannot even parse is an operator's
/// decision, not a timer's. Skipping also keeps a single damaged file from
/// aborting `run_gc` before the state-directory and projection sweeps, which is
/// what propagating the verification error used to do on every subsequent run.
/// The counter rides in every `tally gc` report so the condition stays visible
/// instead of being announced once and then forgotten.
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
    let managed = managed_brief_files(data_dir)?;
    sweep.data_unverified = managed.unverified;
    for (hash, path, _) in managed.files {
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
        let legacy = managed_brief_files(state_dir)?;
        sweep.legacy_unverified = legacy.unverified;
        for (hash, path, modified) in legacy.files {
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

#[derive(Debug, Default)]
struct ManagedBriefs {
    /// Files that verify against the hash in their own name, in name order.
    files: Vec<(String, PathBuf, SystemTime)>,
    /// Managed-looking files that failed `brief::read_verified`. See
    /// [`sweep_brief_stores`] for why they are counted rather than removed.
    unverified: usize,
}

fn managed_brief_files(root: &Path) -> Result<ManagedBriefs, RetentionError> {
    let directory = root.join(brief::BRIEF_DIRECTORY);
    if !directory.is_dir() {
        return Ok(ManagedBriefs::default());
    }
    let mut entries = std::fs::read_dir(&directory)
        .map_err(|source| io_error(&directory, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| io_error(&directory, source))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut managed = ManagedBriefs::default();
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
        if brief::read_verified(&path, &hash).is_err() {
            managed.unverified += 1;
            continue;
        }
        let modified = metadata
            .modified()
            .map_err(|source| io_error(&path, source))?;
        managed.files.push((hash, path, modified));
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
    capture_locks_examined: usize,
    capture_locks_pruned: usize,
    adapter_probes_examined: usize,
    adapter_probes_pruned: usize,
    events_done_examined: usize,
    events_done_pruned: usize,
    events_rejected_examined: usize,
    events_rejected_pruned: usize,
    producer_markers_examined: usize,
    producer_markers_pruned: usize,
}

/// Prunes the unbounded on-disk sets under the daemon state directory.
///
/// Runs inside `run_gc`, under the GC-roots lock it already holds, so there is
/// no second sweep entry point and no second timer. The ingress directories are
/// additionally guarded by the producer ingress lock, because the daemon renames
/// consumed event files into `done`/`rejected` while holding it.
///
/// Capture locks are swept in both places they can be found: the live
/// `capture-lock/` directory, and `unit-exit/`, where they lived before the
/// relocation off the job-writable path. Nothing mints a lock in `unit-exit/`
/// any more, so that population only drains. In `unit-exit/` the sweep touches
/// `<uuid>.capture.lock` names only — the exit records beside them are durable
/// recovery input and keep their "no pruner, do not prune by age" envelope.
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
    for locks in [CAPTURE_LOCK_DIRECTORY, LEGACY_CAPTURE_LOCK_DIRECTORY] {
        prune_capture_locks(&state_dir.join(locks), capture_cutoff, dry_run, &mut sweep)?;
    }
    prune_adapter_smoke_probes(
        &state_dir.join(ADAPTER_SMOKE_DIRECTORY),
        capture_cutoff,
        dry_run,
        &mut sweep,
    )?;

    let marker_cutoff = mtime_cutoff(now, policy.producer_marker_max_age)?;
    let markers_root = state_dir.join(PRODUCER_MARKER_DIRECTORY);
    for set in PRODUCER_MARKER_DIRECTORIES {
        prune_producer_markers(&markers_root.join(set), marker_cutoff, dry_run, &mut sweep)?;
    }

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

/// Prunes retained adapter-smoke commit probe repositories.
///
/// `tally adapter smoke --assert-commit` seeds a throwaway git repository under
/// `<state_dir>/adapter-smoke/` and deliberately keeps it when the probe fails,
/// because a failed probe *is* the evidence. Until this sweep existed nothing in
/// the tree knew that prefix, so every retained repository was permanent and
/// unbounded. Retained evidence gets a horizon here like every other retained
/// artefact: the capture-archive horizon, which is the envelope the rest of the
/// replay material already carries.
///
/// Only real directories named `probe-*` are candidates. Anything else under the
/// root was not written by the probe and is left for an operator rather than
/// deleted, the same rule [`prune_capture_archives`] applies. A probe repository
/// is at most `SMOKE_RUNTIME_MAX_SEC` old while its run is still live, so any
/// capture-archive horizon larger than a few minutes cannot reach one in flight.
fn prune_adapter_smoke_probes(
    probe_root: &Path,
    cutoff: SystemTime,
    dry_run: bool,
    sweep: &mut StateSweep,
) -> Result<(), RetentionError> {
    if !probe_root.is_dir() {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(probe_root)
        .map_err(|source| io_error(probe_root, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| io_error(probe_root, source))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let is_probe = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(ADAPTER_SMOKE_PROBE_PREFIX));
        if !is_probe {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(io_error(&path, source)),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        sweep.adapter_probes_examined += 1;
        if !metadata
            .modified()
            .map(|modified| modified < cutoff)
            .unwrap_or(false)
        {
            continue;
        }
        sweep.adapter_probes_pruned += 1;
        if dry_run {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(&path, source)),
        }
    }
    Ok(())
}

/// Prunes dead `<uuid>.capture.lock` files from one directory under the
/// capture-archive horizon.
///
/// Only the lock files are candidates. In the legacy `unit-exit/` location the
/// exit records sit beside them; those are durable recovery input and keep their
/// standing "do not prune by age" envelope, so this pruner never opens,
/// examines, or removes one.
///
/// Two independent checks gate every unlink, because neither is sufficient
/// alone. `flock` does not touch mtime, and re-opening an existing lock file
/// does not rewrite it either, so an old mtime proves only that nobody created
/// the file recently — never that the lock is free. In the other direction,
/// unlinking a lock somebody still holds is worse than leaking it: the next
/// locker creates a fresh file at the same path, and the two of them then hold
/// exclusive locks on different inodes and run concurrently. So a candidate
/// must be both older than the cutoff *and* provably free right now, where
/// "free" is proven by the non-blocking exclusive lock this sweep takes itself
/// and holds across the unlink. A held lock is skipped whatever its mtime says.
///
/// The remaining window — a locker blocked on the lock while the sweep unlinks
/// it — is closed on the locker's side: it re-stats after `flock` returns and
/// reopens if the name no longer resolves to the inode it holds.
fn prune_capture_locks(
    lock_dir: &Path,
    cutoff: SystemTime,
    dry_run: bool,
    sweep: &mut StateSweep,
) -> Result<(), RetentionError> {
    if !lock_dir.is_dir() {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(lock_dir)
        .map_err(|source| io_error(lock_dir, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| io_error(lock_dir, source))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name();
        // Exactly the executor's own naming: anything else in this directory,
        // exit records included, is not this pruner's business.
        let is_capture_lock = file_name
            .to_str()
            .and_then(|name| name.strip_suffix(CAPTURE_LOCK_SUFFIX))
            .is_some_and(|stem| Uuid::parse_str(stem).is_ok());
        if !is_capture_lock {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(io_error(&path, source)),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        sweep.capture_locks_examined += 1;
        let modified = metadata
            .modified()
            .map_err(|source| io_error(&path, source))?;
        if modified >= cutoff {
            continue;
        }
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(io_error(&path, source)),
        };
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => {}
            // A live capture holds this lock. Its mtime says nothing about
            // that; the contended lock says everything.
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(source) => return Err(io_error(&path, source)),
        }
        sweep.capture_locks_pruned += 1;
        if dry_run {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(&path, source)),
        }
    }
    Ok(())
}

/// Collects expired per-dispatch marker files from one `producers/<set>`
/// directory.
///
/// Each of these directories holds one `<stable-key>.json` per (producer, item,
/// receipt-or-completion id), written once and never removed. A collected
/// marker costs at most a re-publication that the marker scan on the thread
/// already makes idempotent, which is why they are collectable at all — but
/// only long after the item they guard has gone quiet, so the envelope is the
/// long audit horizon rather than the short archive one.
///
/// Two things in these directories are not markers and are never collected:
/// the directory-wide `mutations.lock`, and a `<stable-key>.lock` whose own
/// marker is still present. A per-marker lock is collected together with the
/// marker it guards, so the lock population drains with the markers instead of
/// becoming the next unbounded set.
fn prune_producer_markers(
    directory: &Path,
    cutoff: SystemTime,
    dry_run: bool,
    sweep: &mut StateSweep,
) -> Result<(), RetentionError> {
    if !directory.is_dir() {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(directory)
        .map_err(|source| io_error(directory, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| io_error(directory, source))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut markers = BTreeSet::new();
    let mut locks = BTreeSet::new();
    let mut expired = BTreeSet::new();
    for entry in &entries {
        let path = entry.path();
        let Some(file_name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        if file_name == PRODUCER_MUTATION_LOCK {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(io_error(&path, source)),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        if let Some(stem) = file_name.strip_suffix(".lock") {
            locks.insert(stem.to_owned());
            continue;
        }
        let Some(stem) = file_name.strip_suffix(".json") else {
            continue;
        };
        markers.insert(stem.to_owned());
        sweep.producer_markers_examined += 1;
        let modified = metadata
            .modified()
            .map_err(|source| io_error(&path, source))?;
        if modified < cutoff {
            expired.insert(stem.to_owned());
        }
    }
    // A per-marker lock goes with its marker. `flock` never moves an mtime, so
    // a lock's own timestamp says nothing; the marker is the signal. An orphan
    // lock — no marker at all — is left over from an interrupted write or an
    // earlier sweep and goes too, or it becomes the next unbounded set.
    let collectable_locks = locks
        .iter()
        .filter(|stem| expired.contains(*stem) || !markers.contains(*stem))
        .cloned()
        .collect::<Vec<_>>();
    for stem in collectable_locks {
        let path = directory.join(format!("{stem}.lock"));
        if !try_claim_exclusively(&path)? {
            // A live writer holds it. Its marker keeps its lock this round.
            expired.remove(&stem);
            continue;
        }
        if dry_run {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(&path, source)),
        }
    }
    for stem in expired {
        sweep.producer_markers_pruned += 1;
        if dry_run {
            continue;
        }
        let path = directory.join(format!("{stem}.json"));
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(&path, source)),
        }
    }
    Ok(())
}

/// Can this sweep take the file's own lock right now?
///
/// `false` means a live writer holds it, which is the only signal that matters:
/// an idle lock file's mtime is whatever it was when it was created.
fn try_claim_exclusively(path: &Path) -> Result<bool, RetentionError> {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        // Already gone: there is nothing for anyone to be holding.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(source) => return Err(io_error(path, source)),
    };
    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
        Err(source) => Err(io_error(path, source)),
    }
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
            usage: None,
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

    fn set_dir_mtime(path: &Path, at: DateTime<Utc>) {
        // A directory cannot be opened for writing, so the read handle carries
        // the timestamps; the caller owns the tree, which is what `futimens`
        // requires when explicit times are supplied.
        let times = fs::FileTimes::new()
            .set_accessed(SystemTime::from(at))
            .set_modified(SystemTime::from(at));
        fs::File::open(path).unwrap().set_times(times).unwrap();
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
    fn a_corrupt_brief_is_counted_and_skipped_without_aborting_the_rest_of_the_sweep() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&state).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap();
        ledger_only_state(&data, now);

        let live = brief::PreparedBrief::from_value(serde_json::json!({"kind": "live"})).unwrap();
        let orphan =
            brief::PreparedBrief::from_value(serde_json::json!({"kind": "orphan"})).unwrap();
        for prepared in [&live, &orphan] {
            brief::store(&data, prepared).unwrap();
        }
        let live_task = Uuid::new_v4();
        write_enqueue_event_atomic(&state.join("events"), &brief_event(live_task, live.hash()))
            .unwrap();

        // A managed-looking name whose bytes do not hash to it. Before this
        // sweep counted the condition, `read_verified` propagated out of
        // `managed_brief_files` and killed `run_gc` before every later pruner,
        // forever, because GC never removed the offending file either.
        let corrupt = data
            .join(brief::BRIEF_DIRECTORY)
            .join(format!("{}.json", "d".repeat(64)));
        fs::write(&corrupt, b"{\"kind\":\"not-what-my-name-says\"}").unwrap();

        // Both state-directory sets have something to prune, so a completed
        // sweep is provable rather than merely non-erroring.
        let stale_archive = state
            .join(CAPTURE_ARCHIVE_DIRECTORY)
            .join("unit-a")
            .join("attempt-0000000001-epoch-1.out");
        write_aged(&stale_archive, chrono::TimeDelta::days(31), now);
        let done_old = state.join("events/done/old.json");
        write_aged(&done_old, chrono::TimeDelta::days(181), now);

        let request = GcRequest {
            data_dir: &data,
            state_dir: Some(&state),
            horizon_text: "30d",
            state_retention: StateRetentionPolicy::default(),
            now,
            dry_run: false,
            collect: false,
        };
        let first = run_gc(request.clone(), &FakeBackend::default()).unwrap();
        assert_eq!(first.briefs_unverified, 1);
        assert_eq!(first.briefs_examined, 2);
        assert_eq!(first.briefs_retained, 1);
        assert_eq!(first.briefs_pruned, 1);
        assert!(brief::content_path(&data, live.hash()).unwrap().exists());
        assert!(!brief::content_path(&data, orphan.hash()).unwrap().exists());
        // Skipped, not deleted and not renamed: the sweep cannot parse it, so
        // an operator decides its fate.
        assert!(corrupt.exists());
        // The pruners downstream of the brief sweep all ran.
        assert!(first.state_dir_swept);
        assert_eq!(first.capture_archives_examined, 1);
        assert_eq!(first.capture_archives_pruned, 1);
        assert_eq!(first.events_done_examined, 1);
        assert_eq!(first.events_done_pruned, 1);
        assert!(!stale_archive.exists());
        assert!(!done_old.exists());

        // The timer runs again tomorrow over the same unreadable file.
        let second = run_gc(request, &FakeBackend::default()).unwrap();
        assert_eq!(second.briefs_unverified, 1);
        assert_eq!(second.briefs_examined, 1);
        assert_eq!(second.briefs_pruned, 0);
        assert!(corrupt.exists());
    }

    #[test]
    fn capture_locks_expire_by_age_only_when_no_holder_has_them() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state");
        fs::create_dir_all(&data).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap();
        ledger_only_state(&data, now);

        // The live location plus the pre-relocation one: the sweep drains both,
        // and the legacy population only ever shrinks because nothing mints
        // there any more.
        let unit_exit = state.join(LEGACY_CAPTURE_LOCK_DIRECTORY);
        let live_locks = state.join(CAPTURE_LOCK_DIRECTORY);
        let dead = Uuid::new_v4();
        let held = Uuid::new_v4();
        let young = Uuid::new_v4();
        let live_dead = Uuid::new_v4();
        let dead_lock = unit_exit.join(format!("{dead}{CAPTURE_LOCK_SUFFIX}"));
        let held_lock = unit_exit.join(format!("{held}{CAPTURE_LOCK_SUFFIX}"));
        let young_lock = unit_exit.join(format!("{young}{CAPTURE_LOCK_SUFFIX}"));
        let live_dead_lock = live_locks.join(format!("{live_dead}{CAPTURE_LOCK_SUFFIX}"));
        write_aged(&dead_lock, chrono::TimeDelta::days(31), now);
        // Older than the dead one and still live: mtime alone would condemn it.
        write_aged(&held_lock, chrono::TimeDelta::days(40), now);
        write_aged(&young_lock, chrono::TimeDelta::days(29), now);
        write_aged(&live_dead_lock, chrono::TimeDelta::days(31), now);

        // Exit records and capture generations are durable recovery input.
        let exit_record = unit_exit.join(format!("{dead}.json"));
        let generation = unit_exit.join(format!("{dead}.capture.json"));
        let foreign = unit_exit.join(format!("not-a-uuid{CAPTURE_LOCK_SUFFIX}"));
        for path in [&exit_record, &generation, &foreign] {
            write_aged(path, chrono::TimeDelta::days(400), now);
        }

        let holder = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&held_lock)
            .unwrap();
        FileExt::lock_exclusive(&holder).unwrap();

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
        assert_eq!(dry.capture_locks_examined, 4);
        assert_eq!(dry.capture_locks_pruned, 2);
        assert!(dead_lock.exists());
        assert!(live_dead_lock.exists());

        let report = run_gc(
            GcRequest {
                dry_run: false,
                ..request.clone()
            },
            &FakeBackend::default(),
        )
        .unwrap();
        assert_eq!(report.capture_locks_examined, 4);
        assert_eq!(report.capture_locks_pruned, 2);
        assert!(!dead_lock.exists());
        assert!(!live_dead_lock.exists());
        assert!(held_lock.exists());
        assert!(young_lock.exists());
        assert!(exit_record.exists());
        assert!(generation.exists());
        assert!(foreign.exists());

        // Once the holder is gone the same file becomes collectable, which is
        // the only reason the age floor is not the whole rule.
        drop(holder);
        let released = run_gc(
            GcRequest {
                dry_run: false,
                ..request
            },
            &FakeBackend::default(),
        )
        .unwrap();
        assert_eq!(released.capture_locks_pruned, 1);
        assert!(!held_lock.exists());
        assert!(young_lock.exists());
    }

    /// A retained commit probe is evidence, and evidence gets a horizon. Before
    /// this sweep nothing in the tree knew the `adapter-smoke/probe-*` prefix,
    /// so every failed `adapter smoke --assert-commit` leaked a whole git
    /// repository permanently.
    #[test]
    fn retained_adapter_smoke_probes_expire_on_the_capture_archive_horizon() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&state).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap();
        ledger_only_state(&data, now);

        let probe_root = state.join(ADAPTER_SMOKE_DIRECTORY);
        let seed = |name: &str, age: chrono::TimeDelta| {
            let root = probe_root.join(name);
            // A real tree, not an empty directory: the sweep has to remove the
            // repository a failed probe left, not just its top-level inode.
            fs::create_dir_all(root.join(".git/objects")).unwrap();
            fs::write(root.join("tally-commit-probe.txt"), b"ok\n").unwrap();
            set_dir_mtime(&root, now - age);
            root
        };
        let expired = seed("probe-4242-1", chrono::TimeDelta::days(31));
        let recent = seed("probe-4242-2", chrono::TimeDelta::days(1));
        // Not written by the probe: left for an operator, never deleted.
        let foreign = seed("operator-notes", chrono::TimeDelta::days(400));

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
        assert_eq!(dry.adapter_probes_examined, 2);
        assert_eq!(dry.adapter_probes_pruned, 1);
        assert!(expired.exists());

        let report = run_gc(
            GcRequest {
                dry_run: false,
                ..request
            },
            &FakeBackend::default(),
        )
        .unwrap();
        assert_eq!(report.adapter_probes_examined, 2);
        assert_eq!(report.adapter_probes_pruned, 1);
        assert!(!expired.exists());
        assert!(recent.join("tally-commit-probe.txt").exists());
        assert!(foreign.exists());
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
        assert_eq!(
            policy.producer_marker_max_age,
            Duration::from_secs(180 * 86_400)
        );
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

    /// Every `producers/*` marker directory was written once per dispatch and
    /// collected by nothing: no sweeper, no retention entry, no tmpfiles rule.
    /// One sweep covers all five, keeps the directory-wide mutation lock, and
    /// takes a per-marker lock away only together with the marker it guards.
    #[test]
    fn expired_producer_markers_are_collected_across_every_marker_directory() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state");
        fs::create_dir_all(&data).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap();
        ledger_only_state(&data, now);

        let markers = state.join("producers");
        for set in PRODUCER_MARKER_DIRECTORIES {
            write_aged(
                &markers.join(set).join("expired.json"),
                chrono::TimeDelta::days(200),
                now,
            );
            write_aged(
                &markers.join(set).join("fresh.json"),
                chrono::TimeDelta::days(2),
                now,
            );
            // The directory-wide mutation lock belongs to the directory, not to
            // any marker, and is old by construction on any real host.
            write_aged(
                &markers.join(set).join(PRODUCER_MUTATION_LOCK),
                chrono::TimeDelta::days(200),
                now,
            );
        }
        // gh-triggers keeps one lock per receipt. `flock` never moves an mtime,
        // so both look ancient; only the one whose marker expired may go.
        let triggers = markers.join("gh-triggers");
        write_aged(
            &triggers.join("expired.lock"),
            chrono::TimeDelta::days(200),
            now,
        );
        write_aged(
            &triggers.join("fresh.lock"),
            chrono::TimeDelta::days(200),
            now,
        );
        write_aged(
            &triggers.join("orphan.lock"),
            chrono::TimeDelta::days(200),
            now,
        );

        let request = |dry_run: bool| GcRequest {
            data_dir: &data,
            state_dir: Some(&state),
            horizon_text: "30d",
            state_retention: StateRetentionPolicy::default(),
            now,
            dry_run,
            collect: false,
        };

        let dry = run_gc(request(true), &FakeBackend::default()).unwrap();
        assert_eq!(dry.producer_markers_examined, 10);
        assert_eq!(dry.producer_markers_pruned, 5);
        assert!(triggers.join("expired.json").exists());

        let report = run_gc(request(false), &FakeBackend::default()).unwrap();
        assert_eq!(report.producer_markers_examined, 10);
        assert_eq!(report.producer_markers_pruned, 5);
        for set in PRODUCER_MARKER_DIRECTORIES {
            assert!(!markers.join(set).join("expired.json").exists());
            assert!(markers.join(set).join("fresh.json").exists());
            assert!(markers.join(set).join(PRODUCER_MUTATION_LOCK).exists());
        }
        assert!(!triggers.join("expired.lock").exists());
        assert!(!triggers.join("orphan.lock").exists());
        assert!(triggers.join("fresh.lock").exists());
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
