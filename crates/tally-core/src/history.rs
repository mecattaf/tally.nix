//! Canonical lifecycle observations and compaction receipts.
//!
//! `lifecycle.jsonl` records original daemon observations. When an old prefix
//! is intentionally removed, `lifecycle-retention.json` becomes the canonical
//! receipt for that loss; the parsed vector and shared snapshot are derived by
//! replaying the retained suffix.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, SecondsFormat, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::journal::{validate_fields, TallyFields};

pub const LIFECYCLE_FILE: &str = "lifecycle.jsonl";
pub const LIFECYCLE_RETENTION_FILE: &str = "lifecycle-retention.json";
pub const LIFECYCLE_SCHEMA_VERSION: u32 = 1;
pub const LIFECYCLE_RETENTION_SCHEMA_VERSION: u32 = 1;
pub const LIFECYCLE_RETENTION_POLICY: &str = "declared-prefix-compaction";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LifecycleRecord {
    pub schema_version: u32,
    pub sequence: u64,
    pub event_id: String,
    pub cursor: String,
    pub observed_at: String,
    pub realtime_us: u64,
    pub fields: TallyFields,
}

impl LifecycleRecord {
    fn new(sequence: u64, realtime_us: u64, fields: TallyFields) -> Self {
        let cursor = lifecycle_cursor(sequence);
        Self {
            schema_version: LIFECYCLE_SCHEMA_VERSION,
            sequence,
            event_id: cursor.clone(),
            cursor,
            observed_at: timestamp_from_micros(realtime_us),
            realtime_us,
            fields,
        }
    }

    fn validate(&self, expected_sequence: u64) -> Result<(), HistoryError> {
        if self.schema_version != LIFECYCLE_SCHEMA_VERSION {
            return Err(HistoryError::Invalid(format!(
                "lifecycle record {} has unsupported schema version {}",
                self.sequence, self.schema_version
            )));
        }
        if self.sequence != expected_sequence {
            return Err(HistoryError::Invalid(format!(
                "lifecycle sequence {} does not follow {}",
                self.sequence,
                expected_sequence.saturating_sub(1)
            )));
        }
        let expected_cursor = lifecycle_cursor(self.sequence);
        if self.cursor != expected_cursor || self.event_id != expected_cursor {
            return Err(HistoryError::Invalid(format!(
                "lifecycle record {} has an invalid stable event ID/cursor",
                self.sequence
            )));
        }
        let parsed = DateTime::parse_from_rfc3339(&self.observed_at).map_err(|_| {
            HistoryError::Invalid(format!(
                "lifecycle record {} has an invalid observedAt timestamp",
                self.sequence
            ))
        })?;
        if parsed.timestamp_micros() != self.realtime_us as i64 {
            return Err(HistoryError::Invalid(format!(
                "lifecycle record {} timestamp and realtimeUs disagree",
                self.sequence
            )));
        }
        validate_fields(&self.fields).map_err(|error| {
            HistoryError::Invalid(format!(
                "lifecycle record {} has invalid tally fields: {error}",
                self.sequence
            ))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RetentionMetadata {
    pub complete: bool,
    pub policy: String,
    pub earliest_cursor: Option<String>,
    pub latest_cursor: Option<String>,
    pub truncation_boundary: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleSnapshot {
    pub records: Vec<LifecycleRecord>,
    pub retention: RetentionMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RetentionState {
    schema_version: u32,
    complete: bool,
    truncation_boundary: Option<String>,
    reason: Option<String>,
}

impl Default for RetentionState {
    fn default() -> Self {
        Self {
            schema_version: LIFECYCLE_RETENTION_SCHEMA_VERSION,
            complete: true,
            truncation_boundary: None,
            reason: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("lifecycle history I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("lifecycle history JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid lifecycle history: {0}")]
    Invalid(String),
}

fn io_error(path: &Path, source: std::io::Error) -> HistoryError {
    HistoryError::Io {
        path: path.to_owned(),
        source,
    }
}

pub fn lifecycle_cursor(sequence: u64) -> String {
    format!("lifecycle:{sequence:020}")
}

fn timestamp_from_micros(realtime_us: u64) -> String {
    let micros = i64::try_from(realtime_us).expect("current timestamps fit in i64");
    DateTime::<Utc>::from_timestamp_micros(micros)
        .expect("current timestamps are representable")
        .to_rfc3339_opts(SecondsFormat::Micros, true)
}

pub struct LifecycleStore {
    path: PathBuf,
    retention_path: PathBuf,
    file: File,
    records: Vec<LifecycleRecord>,
    retention: RetentionState,
    /// Cached [`shared_snapshot`](Self::shared_snapshot) value, dropped by the
    /// two mutators (`append_at`, `compact_if_over_limit`) so a snapshot is
    /// deep-built at most once per mutation instead of once per reader.
    shared: Option<Arc<LifecycleSnapshot>>,
}

impl LifecycleStore {
    pub fn open(data_dir: &Path) -> Result<Self, HistoryError> {
        std::fs::create_dir_all(data_dir).map_err(|source| io_error(data_dir, source))?;
        let path = data_dir.join(LIFECYCLE_FILE);
        let retention_path = data_dir.join(LIFECYCLE_RETENTION_FILE);
        let created = !path.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .mode(0o600)
            .open(&path)
            .map_err(|source| io_error(&path, source))?;
        ensure_private(&path)?;
        let mut retention = read_retention_state(&retention_path)?;

        file.lock_exclusive()
            .map_err(|source| io_error(&path, source))?;
        let scan = scan_and_repair(&mut file, &path);
        let unlock = FileExt::unlock(&file).map_err(|source| io_error(&path, source));
        let (records, repaired_boundary) = scan?;
        unlock?;

        if records.first().is_some_and(|record| record.sequence > 1)
            && retention.truncation_boundary.is_none()
        {
            return Err(HistoryError::Invalid(
                "lifecycle history starts past sequence 1 without a recorded truncation boundary"
                    .to_owned(),
            ));
        }
        if let Some(boundary) = repaired_boundary {
            retention.complete = false;
            // Cursors are fixed-width, so string max never regresses a
            // compaction boundary when a torn tail is repaired on an
            // otherwise-empty suffix.
            retention.truncation_boundary = Some(
                retention
                    .truncation_boundary
                    .take()
                    .map_or(boundary.clone(), |existing| existing.max(boundary)),
            );
            retention.reason = Some("incomplete-tail-repaired-after-interrupted-append".to_owned());
            write_retention_state(&retention_path, &retention)?;
        } else if !retention_path.exists() {
            write_retention_state(&retention_path, &retention)?;
        }
        if created {
            File::open(data_dir)
                .and_then(|directory| directory.sync_all())
                .map_err(|source| io_error(data_dir, source))?;
        }
        Ok(Self {
            path,
            retention_path,
            file,
            records,
            retention,
            shared: None,
        })
    }

    pub fn append_now(&mut self, fields: TallyFields) -> Result<LifecycleRecord, HistoryError> {
        let realtime_us = u64::try_from(Utc::now().timestamp_micros()).map_err(|_| {
            HistoryError::Invalid("current time predates the Unix epoch".to_owned())
        })?;
        self.append_at(fields, realtime_us)
    }

    pub fn append_at(
        &mut self,
        fields: TallyFields,
        realtime_us: u64,
    ) -> Result<LifecycleRecord, HistoryError> {
        validate_fields(&fields).map_err(|error| HistoryError::Invalid(error.to_string()))?;
        let sequence = self.next_sequence()?;
        let record = LifecycleRecord::new(sequence, realtime_us, fields);
        record.validate(sequence)?;
        let mut line = serde_json::to_vec(&record)?;
        line.push(b'\n');

        self.file
            .lock_exclusive()
            .map_err(|source| io_error(&self.path, source))?;
        let result = (|| {
            self.file
                .write_all(&line)
                .map_err(|source| io_error(&self.path, source))?;
            self.file
                .sync_all()
                .map_err(|source| io_error(&self.path, source))?;
            self.records.push(record.clone());
            self.shared = None;
            Ok(record)
        })();
        let unlock = FileExt::unlock(&self.file).map_err(|source| io_error(&self.path, source));
        match (result, unlock) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(record), Ok(())) => Ok(record),
        }
    }

    fn next_sequence(&self) -> Result<u64, HistoryError> {
        if let Some(last) = self.records.last() {
            return Ok(last.sequence + 1);
        }
        match &self.retention.truncation_boundary {
            Some(boundary) => parse_lifecycle_cursor(boundary).map(|sequence| sequence + 1),
            None => Ok(1),
        }
    }

    pub fn snapshot(&self) -> LifecycleSnapshot {
        LifecycleSnapshot {
            records: self.records.clone(),
            retention: RetentionMetadata {
                complete: self.retention.complete,
                policy: LIFECYCLE_RETENTION_POLICY.to_owned(),
                earliest_cursor: self.records.first().map(|record| record.cursor.clone()),
                latest_cursor: self.records.last().map(|record| record.cursor.clone()),
                truncation_boundary: self.retention.truncation_boundary.clone(),
                reason: self.retention.reason.clone(),
            },
        }
    }

    /// The same snapshot behind an `Arc`, deep-built at most once per mutation.
    /// [`snapshot`](Self::snapshot) clones every record per call, which on the
    /// daemon put O(all lifecycle records) of copying on the dispatch thread
    /// for every fresh query; readers that only need a frozen view share this
    /// one instead.
    pub fn shared_snapshot(&mut self) -> Arc<LifecycleSnapshot> {
        if self.shared.is_none() {
            self.shared = Some(Arc::new(self.snapshot()));
        }
        Arc::clone(self.shared.as_ref().expect("populated above"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn retention_path(&self) -> &Path {
        &self.retention_path
    }

    /// Compact an old contiguous prefix once the lifecycle log crosses its
    /// declared byte ceiling. The ceiling triggers work; the horizon decides
    /// what is eligible, so recent observability is never discarded merely to
    /// hit a target size.
    pub fn compact_if_over_limit(
        &mut self,
        keep: std::time::Duration,
        max_bytes: u64,
        now: DateTime<Utc>,
    ) -> Result<Option<LifecycleCompaction>, HistoryError> {
        let bytes = self
            .file
            .metadata()
            .map_err(|source| io_error(&self.path, source))?
            .len();
        if bytes <= max_bytes {
            return Ok(None);
        }
        let keep = chrono::Duration::from_std(keep).map_err(|_| {
            HistoryError::Invalid("lifecycle retention horizon is out of range".to_owned())
        })?;
        let cutoff_us = u64::try_from((now - keep).timestamp_micros()).unwrap_or(0);
        let examined = self.records.len();
        let keep_from = self
            .records
            .iter()
            .position(|record| record.realtime_us >= cutoff_us)
            .unwrap_or(examined);
        if keep_from == 0 {
            return Ok(None);
        }

        let kept_records = self.records[keep_from..].to_vec();
        let boundary_sequence = kept_records.first().map_or_else(
            || self.records[keep_from - 1].sequence,
            |record| record.sequence - 1,
        );
        let boundary = lifecycle_cursor(boundary_sequence);
        let temporary = self.path.with_extension(format!(
            "jsonl.tmp-{}-{}",
            std::process::id(),
            now.timestamp_micros()
        ));

        self.file
            .lock_exclusive()
            .map_err(|source| io_error(&self.path, source))?;
        let result = (|| -> Result<File, HistoryError> {
            let mut replacement = OpenOptions::new()
                .create_new(true)
                .read(true)
                .append(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|source| io_error(&temporary, source))?;
            for record in &kept_records {
                serde_json::to_writer(&mut replacement, record)?;
                replacement
                    .write_all(b"\n")
                    .map_err(|source| io_error(&temporary, source))?;
            }
            replacement
                .sync_all()
                .map_err(|source| io_error(&temporary, source))?;

            // Record the conservative incomplete-history boundary before the
            // rename. A crash can therefore leave full history plus an early
            // boundary, but never a truncated history falsely marked complete.
            let mut retention = self.retention.clone();
            retention.complete = false;
            retention.truncation_boundary = Some(boundary.clone());
            retention.reason = Some(format!(
                "automatic-byte-triggered-compaction-max-{max_bytes}"
            ));
            write_retention_state(&self.retention_path, &retention)?;
            std::fs::rename(&temporary, &self.path)
                .map_err(|source| io_error(&self.path, source))?;
            self.retention = retention;
            Ok(replacement)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        let unlock = FileExt::unlock(&self.file).map_err(|source| io_error(&self.path, source));
        let replacement = result?;
        // Once rename succeeded, switch the live append handle even if
        // unlocking the now-unlinked predecessor reports an error. Returning
        // with the old handle would send later lifecycle records into an
        // unreachable inode.
        self.file = replacement;
        self.records = kept_records;
        self.shared = None;
        unlock?;
        File::open(
            self.path
                .parent()
                .expect("lifecycle path always has a parent"),
        )
        .and_then(|directory| directory.sync_all())
        .map_err(|source| {
            io_error(
                self.path
                    .parent()
                    .expect("lifecycle path always has a parent"),
                source,
            )
        })?;
        Ok(Some(LifecycleCompaction {
            examined,
            dropped: keep_from,
            kept: self.records.len(),
            truncation_boundary: Some(boundary),
        }))
    }
}

fn parse_lifecycle_cursor(cursor: &str) -> Result<u64, HistoryError> {
    cursor
        .strip_prefix("lifecycle:")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| {
            HistoryError::Invalid(format!("invalid lifecycle truncation boundary {cursor:?}"))
        })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleCompaction {
    pub examined: usize,
    pub dropped: usize,
    pub kept: usize,
    pub truncation_boundary: Option<String>,
}

/// Offline lifecycle compaction sanctioned by the retention rulings: drop the
/// contiguous prefix older than `keep_days`, recording the cut in the durable
/// retention metadata (complete=false, truncation boundary, reason). Durable
/// enqueue events are recovery inputs and are never touched. Refuses to run
/// while a daemon owns the state directory.
pub fn compact_lifecycle(
    state_dir: &Path,
    data_dir: &Path,
    keep_days: u32,
    now: DateTime<Utc>,
) -> Result<LifecycleCompaction, HistoryError> {
    std::fs::create_dir_all(state_dir).map_err(|source| io_error(state_dir, source))?;
    let lock_path = state_dir.join("daemon.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|source| io_error(&lock_path, source))?;
    lock.try_lock_exclusive().map_err(|source| {
        if source.kind() == std::io::ErrorKind::WouldBlock {
            HistoryError::Invalid(format!(
                "a running daemon owns {}; stop it before compacting",
                lock_path.display()
            ))
        } else {
            io_error(&lock_path, source)
        }
    })?;

    let mut store = LifecycleStore::open(data_dir)?;
    let cutoff = now - chrono::Duration::days(i64::from(keep_days));
    let cutoff_us = u64::try_from(cutoff.timestamp_micros()).unwrap_or(0);
    let examined = store.records.len();
    // Keep a contiguous suffix: cut before the first record inside the window
    // so sequences stay gap-free even if timestamps are not monotonic.
    let keep_from = store
        .records
        .iter()
        .position(|record| record.realtime_us >= cutoff_us)
        .unwrap_or(examined);
    if keep_from == 0 {
        let unlock = FileExt::unlock(&lock);
        unlock.map_err(|source| io_error(&lock_path, source))?;
        return Ok(LifecycleCompaction {
            examined,
            dropped: 0,
            kept: examined,
            truncation_boundary: store.retention.truncation_boundary.clone(),
        });
    }
    let kept_records = store.records.split_off(keep_from);
    let boundary = lifecycle_cursor(kept_records.first().map_or_else(
        || {
            store
                .records
                .last()
                .expect("dropped prefix is nonempty")
                .sequence
        },
        |record| record.sequence - 1,
    ));

    let temporary = store
        .path
        .with_extension(format!("jsonl.tmp-{}", std::process::id()));
    let result = (|| -> Result<(), HistoryError> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|source| io_error(&temporary, source))?;
        for record in &kept_records {
            serde_json::to_writer(&mut file, record)?;
            file.write_all(b"\n")
                .map_err(|source| io_error(&temporary, source))?;
        }
        file.sync_all()
            .map_err(|source| io_error(&temporary, source))?;
        // As in online compaction, publish the conservative boundary before
        // replacing the log so a crash can never expose a truncated file with
        // metadata that still claims completeness.
        store.retention.complete = false;
        store.retention.truncation_boundary = Some(boundary.clone());
        store.retention.reason = Some(format!("compacted-keep-days-{keep_days}"));
        write_retention_state(&store.retention_path, &store.retention)?;
        std::fs::rename(&temporary, &store.path).map_err(|source| io_error(&store.path, source))?;
        File::open(data_dir)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| io_error(data_dir, source))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;

    FileExt::unlock(&lock).map_err(|source| io_error(&lock_path, source))?;
    Ok(LifecycleCompaction {
        examined,
        dropped: keep_from,
        kept: kept_records.len(),
        truncation_boundary: Some(boundary),
    })
}

fn ensure_private(path: &Path) -> Result<(), HistoryError> {
    let permissions = std::fs::metadata(path)
        .map_err(|source| io_error(path, source))?
        .permissions();
    if permissions.mode() & 0o077 != 0 {
        let mut private = permissions;
        private.set_mode(0o600);
        std::fs::set_permissions(path, private).map_err(|source| io_error(path, source))?;
    }
    Ok(())
}

fn scan_and_repair(
    file: &mut File,
    path: &Path,
) -> Result<(Vec<LifecycleRecord>, Option<String>), HistoryError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_error(path, source))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    let complete_len = if bytes.is_empty() || bytes.last() == Some(&b'\n') {
        bytes.len()
    } else {
        bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |position| position + 1)
    };
    let repaired = complete_len != bytes.len();
    if repaired {
        file.set_len(complete_len as u64)
            .map_err(|source| io_error(path, source))?;
        file.sync_all().map_err(|source| io_error(path, source))?;
        bytes.truncate(complete_len);
    }

    let mut records: Vec<LifecycleRecord> = Vec::new();
    let mut base = 1_u64;
    for (index, line) in BufReader::new(bytes.as_slice()).lines().enumerate() {
        let line = line.map_err(|source| io_error(path, source))?;
        if line.trim().is_empty() {
            return Err(HistoryError::Invalid(format!(
                "lifecycle line {} is empty",
                index + 1
            )));
        }
        let record: LifecycleRecord = serde_json::from_str(&line)?;
        if index == 0 {
            // A compacted history legitimately starts past sequence 1; the
            // caller checks that a truncation boundary vouches for the
            // missing prefix.
            base = record.sequence;
        }
        record.validate(base + index as u64)?;
        records.push(record);
    }
    file.seek(SeekFrom::End(0))
        .map_err(|source| io_error(path, source))?;
    let boundary = repaired.then(|| {
        records
            .last()
            .map_or_else(|| lifecycle_cursor(0), |record| record.cursor.clone())
    });
    Ok((records, boundary))
}

fn read_retention_state(path: &Path) -> Result<RetentionState, HistoryError> {
    if !path.exists() {
        return Ok(RetentionState::default());
    }
    let bytes = std::fs::read(path).map_err(|source| io_error(path, source))?;
    let state: RetentionState = serde_json::from_slice(&bytes)?;
    if state.schema_version != LIFECYCLE_RETENTION_SCHEMA_VERSION {
        return Err(HistoryError::Invalid(format!(
            "retention metadata has unsupported schema version {}",
            state.schema_version
        )));
    }
    Ok(state)
}

fn write_retention_state(path: &Path, state: &RetentionState) -> Result<(), HistoryError> {
    let parent = path
        .parent()
        .ok_or_else(|| HistoryError::Invalid("retention path has no parent".to_owned()))?;
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let mut encoded = serde_json::to_vec(state)?;
    encoded.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|source| io_error(&temporary, source))?;
    file.write_all(&encoded)
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error(&temporary, source))?;
    std::fs::rename(&temporary, path).map_err(|source| io_error(path, source))?;
    ensure_private(path)?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(parent, source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Priority;
    use crate::journal::EmitEvent;
    use crate::taskdb::EnqueueSource;
    use proptest::prelude::*;

    fn fields(task: &str) -> TallyFields {
        let mut event = EmitEvent::enqueued(task, Priority::High, EnqueueSource::Manual);
        event.agent = Some("shell".to_owned());
        event.attempt = Some(1);
        event.lease_epoch = Some(7);
        event.into_fields().unwrap()
    }

    #[test]
    fn acceptance_24_2_more_than_4096_events_survive_reopen_without_truncation() {
        let temp = tempfile::tempdir().unwrap();
        {
            let mut store = LifecycleStore::open(temp.path()).unwrap();
            for index in 0..4_129 {
                store
                    .append_at(
                        fields(&format!("task-{index}")),
                        1_700_000_000_000_000 + index,
                    )
                    .unwrap();
            }
            let snapshot = store.snapshot();
            assert_eq!(snapshot.records.len(), 4_129);
            assert!(snapshot.retention.complete);
            assert_eq!(
                snapshot.retention.latest_cursor.as_deref(),
                Some("lifecycle:00000000000000004129")
            );
        }

        let reopened = LifecycleStore::open(temp.path()).unwrap();
        let snapshot = reopened.snapshot();
        assert_eq!(snapshot.records.len(), 4_129);
        assert!(snapshot.retention.complete);
        assert!(snapshot.retention.truncation_boundary.is_none());
        assert_eq!(snapshot.records[4_096].fields.task_uuid, "task-4096");
    }

    #[test]
    fn interrupted_tail_is_repaired_and_remains_explicit_after_another_restart() {
        let temp = tempfile::tempdir().unwrap();
        let path;
        {
            let mut store = LifecycleStore::open(temp.path()).unwrap();
            store
                .append_at(fields("complete"), 1_700_000_000_000_000)
                .unwrap();
            path = store.path().to_owned();
        }
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(br#"{"schemaVersion":1"#)
            .unwrap();

        let repaired = LifecycleStore::open(temp.path()).unwrap();
        let snapshot = repaired.snapshot();
        assert_eq!(snapshot.records.len(), 1);
        assert!(!snapshot.retention.complete);
        assert_eq!(
            snapshot.retention.truncation_boundary.as_deref(),
            Some("lifecycle:00000000000000000001")
        );
        drop(repaired);

        let reopened = LifecycleStore::open(temp.path()).unwrap();
        assert!(!reopened.snapshot().retention.complete);
        assert_eq!(
            reopened.snapshot().retention.reason.as_deref(),
            Some("incomplete-tail-repaired-after-interrupted-append")
        );
    }

    proptest! {
        #[test]
        fn arbitrary_incomplete_tail_repairs_to_the_exact_valid_prefix(
            task_ids in prop::collection::vec(any::<u64>(), 1..9),
            garbage in prop::collection::vec(
                prop_oneof![0_u8..=9, 11_u8..=u8::MAX],
                1..513,
            ),
        ) {
            let temp = tempfile::tempdir().unwrap();
            let path;
            let expected;
            {
                let mut store = LifecycleStore::open(temp.path()).unwrap();
                for (index, task_id) in task_ids.iter().enumerate() {
                    store
                        .append_at(
                            fields(&format!("property-{index}-{task_id}")),
                            1_700_000_000_000_000 + index as u64,
                        )
                        .unwrap();
                }
                path = store.path().to_owned();
                expected = store.snapshot().records;
            }
            let valid_prefix = std::fs::read(&path).unwrap();
            OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(&garbage)
                .unwrap();

            let repaired = LifecycleStore::open(temp.path()).unwrap();
            let snapshot = repaired.snapshot();
            let expected_boundary = lifecycle_cursor(expected.len() as u64);
            prop_assert_eq!(snapshot.records.as_slice(), expected.as_slice());
            prop_assert!(!snapshot.retention.complete);
            prop_assert_eq!(
                snapshot.retention.truncation_boundary.as_deref(),
                Some(expected_boundary.as_str()),
            );
            prop_assert_eq!(
                snapshot.retention.reason.as_deref(),
                Some("incomplete-tail-repaired-after-interrupted-append"),
            );
            prop_assert_eq!(std::fs::read(&path).unwrap(), valid_prefix);
        }
    }

    #[test]
    fn compaction_drops_the_old_prefix_and_records_an_explicit_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let data_dir = temp.path().join("data");
        let base_us = 1_700_000_000_000_000_u64;
        let day_us = 86_400_000_000_u64;
        {
            let mut store = LifecycleStore::open(&data_dir).unwrap();
            for index in 0..10_u64 {
                store
                    .append_at(fields(&format!("task-{index}")), base_us + index * day_us)
                    .unwrap();
            }
        }
        // Keep the newest 3 days relative to just after the last record.
        let now =
            DateTime::<Utc>::from_timestamp_micros((base_us + 9 * day_us + 1) as i64).unwrap();
        let outcome = compact_lifecycle(&state_dir, &data_dir, 3, now).unwrap();
        assert_eq!(outcome.examined, 10);
        assert_eq!(outcome.dropped, 7);
        assert_eq!(outcome.kept, 3);
        assert_eq!(
            outcome.truncation_boundary.as_deref(),
            Some("lifecycle:00000000000000000007")
        );

        let reopened = LifecycleStore::open(&data_dir).unwrap();
        let snapshot = reopened.snapshot();
        assert_eq!(
            snapshot
                .records
                .iter()
                .map(|record| record.sequence)
                .collect::<Vec<_>>(),
            vec![8, 9, 10]
        );
        assert!(!snapshot.retention.complete);
        assert_eq!(snapshot.retention.policy, LIFECYCLE_RETENTION_POLICY);
        assert_eq!(
            snapshot.retention.truncation_boundary.as_deref(),
            Some("lifecycle:00000000000000000007")
        );
        assert_eq!(
            snapshot.retention.reason.as_deref(),
            Some("compacted-keep-days-3")
        );

        // Appends continue the original sequence.
        let mut appending = LifecycleStore::open(&data_dir).unwrap();
        let appended = appending
            .append_at(fields("after-compaction"), base_us + 10 * day_us)
            .unwrap();
        assert_eq!(appended.sequence, 11);
    }

    #[test]
    fn compaction_to_empty_history_preserves_the_sequence_high_water() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let data_dir = temp.path().join("data");
        let base_us = 1_700_000_000_000_000_u64;
        {
            let mut store = LifecycleStore::open(&data_dir).unwrap();
            for index in 0..4_u64 {
                store
                    .append_at(fields(&format!("task-{index}")), base_us + index)
                    .unwrap();
            }
        }
        let now = DateTime::<Utc>::from_timestamp_micros((base_us + 100) as i64).unwrap()
            + chrono::Duration::days(30);
        let outcome = compact_lifecycle(&state_dir, &data_dir, 1, now).unwrap();
        assert_eq!(outcome.dropped, 4);
        assert_eq!(outcome.kept, 0);
        let mut reopened = LifecycleStore::open(&data_dir).unwrap();
        assert!(reopened.snapshot().records.is_empty());
        let appended = reopened
            .append_at(fields("resumes"), base_us + 200)
            .unwrap();
        assert_eq!(appended.sequence, 5);
    }

    #[test]
    fn online_byte_triggered_compaction_keeps_the_declared_recent_suffix() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let base_us = 1_700_000_000_000_000_u64;
        let day_us = 86_400_000_000_u64;
        let mut store = LifecycleStore::open(&data_dir).unwrap();
        for index in 0..8_u64 {
            store
                .append_at(fields(&format!("online-{index}")), base_us + index * day_us)
                .unwrap();
        }
        let now =
            DateTime::<Utc>::from_timestamp_micros((base_us + 7 * day_us + 1) as i64).unwrap();
        let outcome = store
            .compact_if_over_limit(std::time::Duration::from_secs(3 * 86_400), 1, now)
            .unwrap()
            .unwrap();
        assert_eq!(outcome.dropped, 5);
        assert_eq!(outcome.kept, 3);
        assert_eq!(store.snapshot().records[0].sequence, 6);
        assert_eq!(
            store.snapshot().retention.reason.as_deref(),
            Some("automatic-byte-triggered-compaction-max-1")
        );
        assert_eq!(
            store
                .append_at(fields("online-after"), base_us + 8 * day_us)
                .unwrap()
                .sequence,
            9
        );
        drop(store);
        let reopened = LifecycleStore::open(&data_dir).unwrap();
        assert_eq!(reopened.snapshot().records.len(), 4);
        assert_eq!(reopened.snapshot().records[0].sequence, 6);
    }

    #[test]
    fn compaction_is_a_no_op_inside_the_window_and_refuses_a_running_daemon() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let data_dir = temp.path().join("data");
        let now_us = u64::try_from(Utc::now().timestamp_micros()).unwrap();
        {
            let mut store = LifecycleStore::open(&data_dir).unwrap();
            store.append_at(fields("fresh"), now_us).unwrap();
        }
        let outcome = compact_lifecycle(&state_dir, &data_dir, 7, Utc::now()).unwrap();
        assert_eq!(outcome.dropped, 0);
        assert_eq!(outcome.kept, 1);
        assert!(
            LifecycleStore::open(&data_dir)
                .unwrap()
                .snapshot()
                .retention
                .complete
        );

        // A held daemon lock refuses compaction.
        std::fs::create_dir_all(&state_dir).unwrap();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(state_dir.join("daemon.lock"))
            .unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        let refused = compact_lifecycle(&state_dir, &data_dir, 7, Utc::now());
        assert!(matches!(refused, Err(HistoryError::Invalid(_))));
    }

    #[test]
    fn missing_prefix_without_boundary_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().to_owned();
        let path;
        {
            let mut store = LifecycleStore::open(&data_dir).unwrap();
            for index in 0..3_u64 {
                store
                    .append_at(
                        fields(&format!("task-{index}")),
                        1_700_000_000_000_000 + index,
                    )
                    .unwrap();
            }
            path = store.path().to_owned();
        }
        // Strip the first line without recording a truncation boundary.
        let contents = std::fs::read_to_string(&path).unwrap();
        let remainder = contents.split_once('\n').unwrap().1.to_owned();
        std::fs::write(&path, remainder).unwrap();
        std::fs::remove_file(temp.path().join(LIFECYCLE_RETENTION_FILE)).unwrap();
        assert!(LifecycleStore::open(&data_dir).is_err());
    }

    #[test]
    fn complete_corruption_fails_closed_instead_of_pruning() {
        let temp = tempfile::tempdir().unwrap();
        let path;
        {
            let store = LifecycleStore::open(temp.path()).unwrap();
            path = store.path().to_owned();
        }
        std::fs::write(&path, b"{\"not\":\"a lifecycle record\"}\n").unwrap();
        assert!(LifecycleStore::open(temp.path()).is_err());
    }
}
