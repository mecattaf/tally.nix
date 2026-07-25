use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use taskchampion::Uuid;
use thiserror::Error;

use crate::query::{QUERY_PROTOCOL_VERSION, QUERY_SCHEMA_VERSION};

pub const CHANGE_FILE: &str = "changes.jsonl";
pub const CHANGE_SCHEMA_VERSION: u32 = 1;
pub const CHANGE_RETENTION_RECORDS: usize = 4_096;
const MAX_WATCH_PAGE_ITEMS: usize = 1_000;
const DEFAULT_WATCH_PAGE_ITEMS: usize = 100;
const WATCH_RESULT_CAP_BYTES: usize = 48 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeKind {
    Job,
    Lifecycle,
    Trace,
    Proof,
    Pool,
    Producer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ChangeRecord {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub sequence: u64,
    pub cursor: String,
    pub observed_at: String,
    pub kind: ChangeKind,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WatchStatus {
    Ok,
    CursorExpired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WatchTermination {
    pub condition: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WatchEnvelope {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub status: WatchStatus,
    pub items: Vec<ChangeRecord>,
    pub next_cursor: Option<String>,
    pub earliest_available_cursor: Option<String>,
    pub resume_after_cursor: Option<String>,
    pub latest_cursor: Option<String>,
    pub retention_limit: usize,
    pub termination: Option<WatchTermination>,
}

#[derive(Debug, Error)]
pub enum ChangeError {
    #[error("change log I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("change log JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid change log: {0}")]
    Invalid(String),
}

fn io_error(path: &Path, source: std::io::Error) -> ChangeError {
    ChangeError::Io {
        path: path.to_owned(),
        source,
    }
}

#[derive(Debug)]
pub struct ChangeStore {
    path: PathBuf,
    file: File,
    records: Vec<ChangeRecord>,
    capacity: usize,
}

impl ChangeStore {
    pub fn open(data_dir: &Path) -> Result<Self, ChangeError> {
        Self::open_with_capacity(data_dir, CHANGE_RETENTION_RECORDS)
    }

    pub fn open_with_capacity(data_dir: &Path, capacity: usize) -> Result<Self, ChangeError> {
        if capacity == 0 {
            return Err(ChangeError::Invalid(
                "change retention capacity must be positive".to_owned(),
            ));
        }
        std::fs::create_dir_all(data_dir).map_err(|source| io_error(data_dir, source))?;
        let path = data_dir.join(CHANGE_FILE);
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|source| io_error(&path, source))?;
        let metadata = file.metadata().map_err(|source| io_error(&path, source))?;
        if !metadata.file_type().is_file() {
            return Err(ChangeError::Invalid(
                "change log is not a regular file".to_owned(),
            ));
        }
        ensure_private(&path)?;
        let mut bytes = Vec::new();
        file.seek(SeekFrom::Start(0))
            .and_then(|_| file.read_to_end(&mut bytes))
            .map_err(|source| io_error(&path, source))?;
        if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
            let complete = bytes
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |position| position + 1);
            file.set_len(complete as u64)
                .and_then(|_| file.sync_all())
                .map_err(|source| io_error(&path, source))?;
            bytes.truncate(complete);
        }
        let mut records = Vec::new();
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            records.push(serde_json::from_slice::<ChangeRecord>(line)?);
        }
        validate_records(&records)?;
        if records.len() > capacity {
            records = records.split_off(records.len() - capacity);
            rewrite(&path, &records)?;
            file = OpenOptions::new()
                .read(true)
                .append(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)
                .map_err(|source| io_error(&path, source))?;
        } else {
            file.seek(SeekFrom::End(0))
                .map_err(|source| io_error(&path, source))?;
        }
        Ok(Self {
            path,
            file,
            records,
            capacity,
        })
    }

    pub fn append_now(
        &mut self,
        kind: ChangeKind,
        payload: Value,
    ) -> Result<ChangeRecord, ChangeError> {
        let sequence = self
            .records
            .last()
            .map_or(1, |record| record.sequence.saturating_add(1));
        let record = ChangeRecord {
            schema_version: CHANGE_SCHEMA_VERSION,
            protocol_version: QUERY_PROTOCOL_VERSION,
            sequence,
            cursor: change_cursor(sequence),
            observed_at: Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true),
            kind,
            payload,
        };
        let mut encoded = serde_json::to_vec(&record)?;
        encoded.push(b'\n');
        self.file
            .write_all(&encoded)
            .and_then(|_| self.file.sync_all())
            .map_err(|source| io_error(&self.path, source))?;
        self.records.push(record.clone());
        if self.records.len() > self.capacity {
            self.records.remove(0);
            rewrite(&self.path, &self.records)?;
            self.file = OpenOptions::new()
                .read(true)
                .append(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&self.path)
                .map_err(|source| io_error(&self.path, source))?;
        }
        Ok(record)
    }

    pub fn watch(
        &self,
        after: Option<&str>,
        limit: Option<usize>,
    ) -> Result<WatchEnvelope, ChangeError> {
        let limit = limit.unwrap_or(DEFAULT_WATCH_PAGE_ITEMS);
        if !(1..=MAX_WATCH_PAGE_ITEMS).contains(&limit) {
            return Err(ChangeError::Invalid(format!(
                "watch limit must be between 1 and {MAX_WATCH_PAGE_ITEMS}"
            )));
        }
        let earliest = self.records.first().map(|record| record.sequence);
        let latest = self.records.last().map(|record| record.sequence);
        if after.is_none() {
            return Ok(self.envelope(
                WatchStatus::Ok,
                Vec::new(),
                Some(latest.map_or_else(|| change_cursor(0), change_cursor)),
                None,
            ));
        }
        let after = parse_change_cursor(after.unwrap())?;
        if latest.is_none_or(|latest| after > latest) && after != 0 {
            return Err(ChangeError::Invalid(
                "watch cursor is ahead of the durable change log".to_owned(),
            ));
        }
        if earliest.is_some_and(|earliest| after < earliest.saturating_sub(1)) {
            return Ok(self.envelope(
                WatchStatus::CursorExpired,
                Vec::new(),
                None,
                Some(WatchTermination {
                    condition: "gap".to_owned(),
                    reason: "cursor-expired".to_owned(),
                }),
            ));
        }
        let mut items = Vec::new();
        for record in self.records.iter().filter(|record| record.sequence > after) {
            if items.len() == limit {
                break;
            }
            items.push(record.clone());
            let candidate = self.envelope(
                WatchStatus::Ok,
                items.clone(),
                Some(record.cursor.clone()),
                None,
            );
            if serde_json::to_vec(&candidate)?.len() > WATCH_RESULT_CAP_BYTES {
                items.pop();
                if items.is_empty() {
                    return Err(ChangeError::Invalid(
                        "one watch change exceeds the bounded response size".to_owned(),
                    ));
                }
                break;
            }
        }
        let next = items
            .last()
            .map(|record| record.cursor.clone())
            .or_else(|| Some(change_cursor(after)));
        Ok(self.envelope(WatchStatus::Ok, items, next, None))
    }

    fn envelope(
        &self,
        status: WatchStatus,
        items: Vec<ChangeRecord>,
        next_cursor: Option<String>,
        termination: Option<WatchTermination>,
    ) -> WatchEnvelope {
        WatchEnvelope {
            schema_version: QUERY_SCHEMA_VERSION,
            protocol_version: QUERY_PROTOCOL_VERSION,
            status,
            items,
            next_cursor,
            earliest_available_cursor: self.records.first().map(|record| record.cursor.clone()),
            resume_after_cursor: (status == WatchStatus::CursorExpired).then(|| {
                change_cursor(
                    self.records
                        .first()
                        .map_or(0, |record| record.sequence.saturating_sub(1)),
                )
            }),
            latest_cursor: self.records.last().map(|record| record.cursor.clone()),
            retention_limit: self.capacity,
            termination,
        }
    }
}

pub fn change_cursor(sequence: u64) -> String {
    format!("change:{sequence:020}")
}

fn parse_change_cursor(cursor: &str) -> Result<u64, ChangeError> {
    cursor
        .strip_prefix("change:")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| ChangeError::Invalid("invalid watch cursor".to_owned()))
}

fn validate_records(records: &[ChangeRecord]) -> Result<(), ChangeError> {
    for (index, record) in records.iter().enumerate() {
        if record.schema_version != CHANGE_SCHEMA_VERSION
            || record.protocol_version != QUERY_PROTOCOL_VERSION
            || record.cursor != change_cursor(record.sequence)
        {
            return Err(ChangeError::Invalid(format!(
                "change record {} has an invalid schema or cursor",
                record.sequence
            )));
        }
        if index > 0 && records[index - 1].sequence.saturating_add(1) != record.sequence {
            return Err(ChangeError::Invalid(format!(
                "change sequence {} is not contiguous",
                record.sequence
            )));
        }
    }
    Ok(())
}

fn ensure_private(path: &Path) -> Result<(), ChangeError> {
    let mut permissions = std::fs::metadata(path)
        .map_err(|source| io_error(path, source))?
        .permissions();
    if permissions.mode() & 0o077 != 0 {
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions).map_err(|source| io_error(path, source))?;
    }
    Ok(())
}

fn rewrite(path: &Path, records: &[ChangeRecord]) -> Result<(), ChangeError> {
    let parent = path
        .parent()
        .ok_or_else(|| ChangeError::Invalid("change path has no parent".to_owned()))?;
    let temporary = path.with_extension(format!("jsonl.tmp-{}", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .map_err(|source| io_error(&temporary, source))?;
        for record in records {
            serde_json::to_writer(&mut file, record)?;
            file.write_all(b"\n")
                .map_err(|source| io_error(&temporary, source))?;
        }
        file.sync_all()
            .map_err(|source| io_error(&temporary, source))?;
        std::fs::rename(&temporary, path).map_err(|source| io_error(path, source))?;
        ensure_private(path)?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| io_error(parent, source))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acceptance_24_8_disconnect_resume_is_gap_free_and_duplicate_free_across_restart() {
        let temp = tempfile::tempdir().unwrap();
        {
            let mut store = ChangeStore::open_with_capacity(temp.path(), 16).unwrap();
            for index in 0..10 {
                store
                    .append_now(ChangeKind::Lifecycle, serde_json::json!({"index": index}))
                    .unwrap();
            }
        }
        let store = ChangeStore::open_with_capacity(temp.path(), 16).unwrap();
        let mut after = change_cursor(0);
        let mut observed = Vec::new();
        loop {
            let page = store.watch(Some(&after), Some(3)).unwrap();
            assert_eq!(page.status, WatchStatus::Ok);
            observed.extend(
                page.items
                    .iter()
                    .map(|record| record.payload["index"].as_u64().unwrap()),
            );
            after = page.next_cursor.unwrap();
            if observed.len() == 10 {
                break;
            }
        }
        assert_eq!(observed, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn acceptance_24_8_expired_slow_reader_gets_an_explicit_gap_and_resume_cursor() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = ChangeStore::open_with_capacity(temp.path(), 3).unwrap();
        for index in 0..5 {
            store
                .append_now(ChangeKind::Job, serde_json::json!({"index": index}))
                .unwrap();
        }
        let expired = store.watch(Some(&change_cursor(0)), None).unwrap();
        assert_eq!(expired.status, WatchStatus::CursorExpired);
        assert_eq!(
            expired.earliest_available_cursor.as_deref(),
            Some("change:00000000000000000003")
        );
        assert_eq!(
            expired.resume_after_cursor.as_deref(),
            Some("change:00000000000000000002")
        );
        assert_eq!(expired.termination.as_ref().unwrap().condition, "gap");
        assert!(expired.items.is_empty());
        let resumed = store
            .watch(expired.resume_after_cursor.as_deref(), None)
            .unwrap();
        assert_eq!(resumed.status, WatchStatus::Ok);
        assert_eq!(
            resumed
                .items
                .iter()
                .map(|record| record.payload["index"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
    }
}
