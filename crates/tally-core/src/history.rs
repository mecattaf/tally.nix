use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::journal::{validate_fields, TallyFields};

pub const LIFECYCLE_FILE: &str = "lifecycle.jsonl";
pub const LIFECYCLE_RETENTION_FILE: &str = "lifecycle-retention.json";
pub const LIFECYCLE_SCHEMA_VERSION: u32 = 1;
pub const LIFECYCLE_RETENTION_SCHEMA_VERSION: u32 = 1;
pub const LIFECYCLE_RETENTION_POLICY: &str = "unbounded";

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

        if let Some(boundary) = repaired_boundary {
            retention.complete = false;
            retention.truncation_boundary = Some(boundary);
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
        let sequence = self.records.len() as u64 + 1;
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
            Ok(record)
        })();
        let unlock = FileExt::unlock(&self.file).map_err(|source| io_error(&self.path, source));
        match (result, unlock) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(record), Ok(())) => Ok(record),
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

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn retention_path(&self) -> &Path {
        &self.retention_path
    }
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

    let mut records = Vec::new();
    for (index, line) in BufReader::new(bytes.as_slice()).lines().enumerate() {
        let line = line.map_err(|source| io_error(path, source))?;
        if line.trim().is_empty() {
            return Err(HistoryError::Invalid(format!(
                "lifecycle line {} is empty",
                index + 1
            )));
        }
        let record: LifecycleRecord = serde_json::from_str(&line)?;
        record.validate(index as u64 + 1)?;
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
