use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

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

fn reopen(path: &Path) -> Result<File, ChangeError> {
    OpenOptions::new()
        .read(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|source| io_error(path, source))
}

#[derive(Debug)]
pub struct ChangeStore {
    path: PathBuf,
    file: File,
    records: VecDeque<ChangeRecord>,
    capacity: usize,
    // Records currently on disk. The durable file is allowed to hold up to
    // 2*capacity records so that steady-state appends are O(1); the whole-file
    // rewrite runs only when this counter reaches that threshold, dropping the
    // file back to the newest `capacity` records. The in-memory window served
    // by watch() is always trimmed to exactly `capacity`.
    disk_records: usize,
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
        // The watch feed is bounded convenience state, not evidence or a
        // recovery input. Discard the whole feed when complete records cannot
        // be decoded or validated rather than presenting the failure as
        // corruption of durable state that an operator must preserve.
        let mut records = match decode_records(&bytes) {
            Ok(records) => records,
            Err(ChangeError::Json(_) | ChangeError::Invalid(_)) => {
                rewrite(&path, std::iter::empty())?;
                file = reopen(&path)?;
                Vec::new()
            }
            Err(error) => return Err(error),
        };
        let disk_records = if records.len() >= capacity.saturating_mul(2) {
            // A crash may have interrupted the previous owner between the
            // threshold append and its rewrite; finish the drop to the newest
            // `capacity` records now.
            records = records.split_off(records.len() - capacity);
            rewrite(&path, records.iter())?;
            file = reopen(&path)?;
            capacity
        } else {
            file.seek(SeekFrom::End(0))
                .map_err(|source| io_error(&path, source))?;
            records.len()
        };
        if records.len() > capacity {
            records = records.split_off(records.len() - capacity);
        }
        Ok(Self {
            path,
            file,
            records: records.into(),
            capacity,
            disk_records,
        })
    }

    pub fn append_now(
        &mut self,
        kind: ChangeKind,
        payload: Value,
    ) -> Result<ChangeRecord, ChangeError> {
        let sequence = self
            .records
            .back()
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
        self.records.push_back(record.clone());
        if self.records.len() > self.capacity {
            self.records.pop_front();
        }
        self.disk_records += 1;
        if self.disk_records >= self.capacity.saturating_mul(2) {
            rewrite(&self.path, self.records.iter())?;
            self.file = reopen(&self.path)?;
            self.disk_records = self.records.len();
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
        let earliest = self.records.front().map(|record| record.sequence);
        let latest = self.records.back().map(|record| record.sequence);
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
            earliest_available_cursor: self.records.front().map(|record| record.cursor.clone()),
            resume_after_cursor: (status == WatchStatus::CursorExpired).then(|| {
                change_cursor(
                    self.records
                        .front()
                        .map_or(0, |record| record.sequence.saturating_sub(1)),
                )
            }),
            latest_cursor: self.records.back().map(|record| record.cursor.clone()),
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

fn decode_records(bytes: &[u8]) -> Result<Vec<ChangeRecord>, ChangeError> {
    let records = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<ChangeRecord>)
        .collect::<Result<Vec<_>, _>>()?;
    validate_records(&records)?;
    Ok(records)
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

fn rewrite<'a>(
    path: &Path,
    records: impl Iterator<Item = &'a ChangeRecord> + Clone,
) -> Result<(), ChangeError> {
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
        for record in records.clone() {
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

    fn inode(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).unwrap().ino()
    }

    fn test_record(sequence: u64) -> ChangeRecord {
        ChangeRecord {
            schema_version: CHANGE_SCHEMA_VERSION,
            protocol_version: QUERY_PROTOCOL_VERSION,
            sequence,
            cursor: change_cursor(sequence),
            observed_at: "2026-07-29T00:00:00Z".to_owned(),
            kind: ChangeKind::Job,
            payload: serde_json::json!({"sequence": sequence}),
        }
    }

    fn encode_records(records: &[ChangeRecord]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for record in records {
            serde_json::to_writer(&mut bytes, record).unwrap();
            bytes.push(b'\n');
        }
        bytes
    }

    #[test]
    fn invalid_or_foreign_change_log_is_discarded_on_open() {
        let mut foreign_schema = test_record(1);
        foreign_schema.schema_version += 1;
        let cases = [
            ("foreign schema", encode_records(&[foreign_schema])),
            ("foreign shape", b"{\"legacy\":true}\n".to_vec()),
            ("malformed JSON", b"{not-json}\n".to_vec()),
            (
                "non-contiguous sequence",
                encode_records(&[test_record(1), test_record(3)]),
            ),
        ];

        for (case, bytes) in cases {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join(CHANGE_FILE);
            std::fs::write(&path, bytes).unwrap();

            let mut store = ChangeStore::open_with_capacity(temp.path(), 4)
                .unwrap_or_else(|error| panic!("{case}: open failed: {error}"));
            assert!(
                std::fs::read(&path).unwrap().is_empty(),
                "{case}: unusable feed was not discarded"
            );
            let seed = store.watch(None, None).unwrap();
            assert!(seed.items.is_empty(), "{case}: reset feed served records");
            assert_eq!(
                seed.next_cursor.as_deref(),
                Some(change_cursor(0).as_str()),
                "{case}: reset feed did not return the genesis cursor"
            );
            let appended = store
                .append_now(ChangeKind::Job, serde_json::json!({"case": case}))
                .unwrap();
            assert_eq!(
                appended.sequence, 1,
                "{case}: reset feed did not restart at sequence 1"
            );
        }
    }

    #[test]
    fn three_capacities_of_appends_cause_at_most_two_rewrites() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(CHANGE_FILE);
        let mut store = ChangeStore::open_with_capacity(temp.path(), 8).unwrap();
        // The whole-file rewrite is observable as an inode change: it writes a
        // temp file and renames it over the log.
        let mut current = inode(&path);
        let mut rewrites = 0;
        for index in 0..24 {
            store
                .append_now(ChangeKind::Job, serde_json::json!({"index": index}))
                .unwrap();
            let now = inode(&path);
            if now != current {
                rewrites += 1;
                current = now;
            }
        }
        assert!(rewrites <= 2, "expected at most 2 rewrites, saw {rewrites}");
        // The in-memory watch window still serves exactly the newest capacity.
        let expired = store.watch(Some(&change_cursor(0)), None).unwrap();
        assert_eq!(expired.status, WatchStatus::CursorExpired);
        let resumed = store
            .watch(expired.resume_after_cursor.as_deref(), Some(1_000))
            .unwrap();
        assert_eq!(
            resumed
                .items
                .iter()
                .map(|record| record.sequence)
                .collect::<Vec<_>>(),
            (17..=24).collect::<Vec<_>>()
        );
    }

    #[test]
    fn crash_between_threshold_append_and_rewrite_reopens_gap_free() {
        let temp = tempfile::tempdir().unwrap();
        // Produce a durable file holding exactly 2*8 records without a rewrite
        // by writing under a larger capacity: this is byte-identical to a
        // crash after the threshold append but before the amortized rewrite.
        {
            let mut store = ChangeStore::open_with_capacity(temp.path(), 100).unwrap();
            for index in 0..16 {
                store
                    .append_now(ChangeKind::Lifecycle, serde_json::json!({"index": index}))
                    .unwrap();
            }
        }
        let store = ChangeStore::open_with_capacity(temp.path(), 8).unwrap();
        let bytes = std::fs::read(temp.path().join(CHANGE_FILE)).unwrap();
        let sequences = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                serde_json::from_slice::<ChangeRecord>(line)
                    .unwrap()
                    .sequence
            })
            .collect::<Vec<_>>();
        assert_eq!(sequences, (9..=16).collect::<Vec<_>>());
        let page = store.watch(Some(&change_cursor(8)), Some(1_000)).unwrap();
        assert_eq!(page.status, WatchStatus::Ok);
        assert_eq!(
            page.items
                .iter()
                .map(|record| record.sequence)
                .collect::<Vec<_>>(),
            (9..=16).collect::<Vec<_>>()
        );
        let done = store
            .watch(Some(page.next_cursor.as_deref().unwrap()), Some(1_000))
            .unwrap();
        assert!(done.items.is_empty(), "resume produced duplicates");
    }

    // Characterization: observable durable behavior only. These tests assert
    // what a reopened store serves (envelopes and the durable tail), never how
    // the store schedules its rewrites, so a rewrite-amortization change must
    // keep them green.
    #[test]
    fn characterization_default_capacity_rollover_serves_exactly_the_newest_window() {
        let temp = tempfile::tempdir().unwrap();
        let total = CHANGE_RETENTION_RECORDS + 4;
        {
            let mut store = ChangeStore::open(temp.path()).unwrap();
            for index in 0..total {
                store
                    .append_now(ChangeKind::Lifecycle, serde_json::json!({"index": index}))
                    .unwrap();
            }
        }
        let store = ChangeStore::open(temp.path()).unwrap();
        let expired = store.watch(Some(&change_cursor(0)), Some(1)).unwrap();
        assert_eq!(expired.status, WatchStatus::CursorExpired);
        assert_eq!(expired.retention_limit, CHANGE_RETENTION_RECORDS);
        assert_eq!(
            expired.earliest_available_cursor.as_deref(),
            Some(change_cursor((total - CHANGE_RETENTION_RECORDS) as u64 + 1).as_str())
        );
        assert_eq!(
            expired.latest_cursor.as_deref(),
            Some(change_cursor(total as u64).as_str())
        );

        let mut after = expired.resume_after_cursor.unwrap();
        let mut observed = Vec::new();
        loop {
            let page = store.watch(Some(&after), Some(1_000)).unwrap();
            assert_eq!(page.status, WatchStatus::Ok);
            if page.items.is_empty() {
                break;
            }
            observed.extend(
                page.items
                    .iter()
                    .map(|record| record.payload["index"].as_u64().unwrap()),
            );
            after = page.next_cursor.unwrap();
        }
        let expected =
            ((total - CHANGE_RETENTION_RECORDS) as u64..total as u64).collect::<Vec<_>>();
        assert_eq!(observed, expected);
    }

    #[test]
    fn characterization_durable_tail_survives_reopen_gap_free_and_duplicate_free() {
        let temp = tempfile::tempdir().unwrap();
        {
            let mut store = ChangeStore::open_with_capacity(temp.path(), 8).unwrap();
            for index in 0..25 {
                store
                    .append_now(ChangeKind::Job, serde_json::json!({"index": index}))
                    .unwrap();
            }
        }
        // The durable file must always end with the newest `capacity` records
        // in contiguous order, whatever rewrite strategy produced it.
        let bytes = std::fs::read(temp.path().join(CHANGE_FILE)).unwrap();
        let records = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<ChangeRecord>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(records.len() >= 8, "durable tail lost records");
        let sequences = records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>();
        let newest = sequences[sequences.len() - 8..].to_vec();
        assert_eq!(newest, (18..=25).collect::<Vec<_>>());
        assert!(sequences.windows(2).all(|pair| pair[1] == pair[0] + 1));

        let reopened = ChangeStore::open_with_capacity(temp.path(), 8).unwrap();
        let page = reopened
            .watch(Some(&change_cursor(17)), Some(1_000))
            .unwrap();
        assert_eq!(page.status, WatchStatus::Ok);
        assert_eq!(
            page.items
                .iter()
                .map(|record| record.sequence)
                .collect::<Vec<_>>(),
            (18..=25).collect::<Vec<_>>()
        );
        let next = reopened
            .watch(Some(page.next_cursor.as_deref().unwrap()), Some(1_000))
            .unwrap();
        assert!(next.items.is_empty(), "resume produced duplicates");
    }

    #[test]
    fn characterization_crash_artifacts_do_not_break_reopen() {
        let temp = tempfile::tempdir().unwrap();
        {
            let mut store = ChangeStore::open_with_capacity(temp.path(), 4).unwrap();
            for index in 0..6 {
                store
                    .append_now(ChangeKind::Trace, serde_json::json!({"index": index}))
                    .unwrap();
            }
        }
        // A crash between the temp-file write and the rename leaves a stale
        // sibling; a crash mid-append leaves a torn last line. Reopen must
        // absorb both without losing acknowledged records.
        std::fs::write(
            temp.path().join("changes.jsonl.tmp-stale"),
            b"{\"partial\":",
        )
        .unwrap();
        let path = temp.path().join(CHANGE_FILE);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"schemaVersion\":1,\"torn").unwrap();
        drop(file);

        let store = ChangeStore::open_with_capacity(temp.path(), 4).unwrap();
        let page = store.watch(Some(&change_cursor(2)), Some(1_000)).unwrap();
        assert_eq!(page.status, WatchStatus::Ok);
        assert_eq!(
            page.items
                .iter()
                .map(|record| record.payload["index"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![2, 3, 4, 5]
        );
    }

    #[test]
    fn characterization_open_with_capacity_edges() {
        let temp = tempfile::tempdir().unwrap();
        assert!(matches!(
            ChangeStore::open_with_capacity(temp.path(), 0),
            Err(ChangeError::Invalid(_))
        ));

        {
            let mut store = ChangeStore::open_with_capacity(temp.path(), 1).unwrap();
            for index in 0..3 {
                store
                    .append_now(ChangeKind::Pool, serde_json::json!({"index": index}))
                    .unwrap();
            }
            let latest = store.watch(Some(&change_cursor(2)), None).unwrap();
            assert_eq!(latest.items.len(), 1);
            assert_eq!(latest.items[0].sequence, 3);
        }

        // Reopening with a smaller capacity than the durable record count must
        // keep the newest window and the append sequence intact.
        {
            let mut wide = ChangeStore::open_with_capacity(temp.path(), 16).unwrap();
            for index in 3..9 {
                wide.append_now(ChangeKind::Pool, serde_json::json!({"index": index}))
                    .unwrap();
            }
        }
        let mut narrow = ChangeStore::open_with_capacity(temp.path(), 2).unwrap();
        let expired = narrow.watch(Some(&change_cursor(1)), None).unwrap();
        assert_eq!(expired.status, WatchStatus::CursorExpired);
        assert_eq!(
            expired.earliest_available_cursor.as_deref(),
            Some(change_cursor(8).as_str())
        );
        let appended = narrow
            .append_now(ChangeKind::Pool, serde_json::json!({"index": 9}))
            .unwrap();
        assert_eq!(appended.sequence, 10);
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
