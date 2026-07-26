use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::Serialize;
use thiserror::Error;

use crate::nix_store::GcRootBackend;
use crate::witness::{is_nix_store_path, read_verified_records, WitnessError, WitnessRecord};

const ROOT_DIRECTORY_PREFIX: &str = "witness-";

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
    pub collected: bool,
}

#[derive(Debug, Error)]
pub enum RetentionError {
    #[error("invalid retention horizon {value:?}: {reason}")]
    InvalidHorizon { value: String, reason: String },
    #[error("witness ledger error: {0}")]
    Witness(#[from] WitnessError),
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

#[allow(clippy::too_many_arguments)]
pub fn run_gc(
    data_dir: &Path,
    horizon_text: &str,
    now: DateTime<Utc>,
    dry_run: bool,
    collect: bool,
    backend: &impl GcRootBackend,
) -> Result<GcReport, RetentionError> {
    let horizon = parse_horizon(horizon_text)?;
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
        collected,
    })
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
    use crate::taskdb::{AdmissionOrigin, EnqueueSource};
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
        let dry_report = run_gc(data, "30d", now, true, true, &backend).unwrap();
        assert_eq!(dry_report.roots_pruned, 1);
        assert!(!dry_report.collected);
        assert!(
            fs::symlink_metadata(old_roots.join(Path::new(OLD_ONLY).file_name().unwrap())).is_ok()
        );
        assert_eq!(*backend.collections.borrow(), 0);

        let report = run_gc(data, "30d", now, false, true, &backend).unwrap();
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
            run_gc(data, "30d", now, false, true, &backend),
            Err(RetentionError::LiveRootRegistration { sequence: 2, .. })
        ));
        assert!(fs::symlink_metadata(old_link).is_ok());
        assert_eq!(*backend.collections.borrow(), 0);
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
            extensions: serde_json::Map::new(),
            seq: 1,
            prev_hash: crate::witness::GENESIS_PREV_HASH.to_owned(),
            hash: String::new(),
        }
    }
}
