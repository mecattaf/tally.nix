//! Durable flow-run membership: which task UUIDs a run was actually handed.
//!
//! Run membership used to be *recomputed* on every query by scanning durable
//! rows and witness records for an orchestration capsule naming the run. That
//! works only for admissions that write a row of their own. Three do not —
//! `attached`, and full-mode `reused` and `terminal` — and each of them hands
//! the caller a task UUID for work that is real and running while the row, and
//! therefore the scanned membership, stays with whichever run created it. The
//! submitting run's own window then filters its own node out: same items,
//! `nextCursor: null`, no page cap in sight. That is waiver W-316, and #247
//! before it.
//!
//! This ledger is the missing fact. One record per `(flowRunId, taskUuid)`
//! pair, appended synchronously with the admission decision and durable before
//! the admission is acknowledged, so a run that was handed a task UUID can
//! always resolve it back to itself.
//!
//! Deliberately shaped after [`crate::flow_lineage`]: an append-only JSONL
//! index, not hash-chained, not a proof surface. The witness ledger remains the
//! only canonical one. What this store owns is a membership question that the
//! witness ledger cannot answer, because a row-less admission appends no
//! witness under the submitting run either.
//!
//! **It is additive, never authoritative-by-itself.** Query membership is the
//! union of this ledger and the original scan, which is what makes the store
//! safe to introduce: a row written by an older binary carries its capsule and
//! is still found by the scan with no record here, and a ledger that is missing
//! or empty degrades exactly to the pre-#380 behaviour rather than to an empty
//! run.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const FLOW_MEMBERSHIP_SCHEMA_VERSION: u32 = 1;
pub const FLOW_MEMBERSHIP_FILE: &str = "flow-membership.jsonl";

/// Records kept when the ledger is compacted.
///
/// One record per admitted flow node. At the pinned `maxNodes` of 51 that is
/// roughly two thousand whole campaigns, and the file is a few megabytes at the
/// bound. The bound also caps the in-memory index the daemon keeps hot.
///
/// Compaction drops **whole runs**, oldest first, never individual records.
/// A run that is half-present would report a membership count lower than the
/// truth — a number that is wrong in the reassuring direction, which is the one
/// outcome this whole store exists to remove. A run that is wholly absent falls
/// back to the row scan, which is exactly what an operator got before this
/// ledger existed and is what the observability chapter already documents.
pub const FLOW_MEMBERSHIP_MAX_RECORDS: usize = 100_000;

/// How much of the tail is scanned backwards to find the last record boundary.
/// A single record is a few hundred bytes, so this never fails to find one in
/// practice; the full-scan fallback exists so that it cannot be wrong when it
/// does.
const TAIL_SCAN_BYTES: u64 = 64 * 1024;

/// Which admission handed the run this task.
///
/// Open on the read side on purpose. A closed vocabulary that hard-fails on an
/// unknown string would mean a ledger written by a newer daemon crashes an
/// older one's queries on a pin rollback — the #371 failure, imported into a
/// store whose whole job is to keep a window honest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MembershipDisposition {
    Created,
    Attached,
    Reused,
    Terminal,
    Substituted,
    /// `queue.retry` re-admitted a node the run already owned.
    Retried,
    /// Written by a daemon that knows a disposition this one does not.
    #[serde(other)]
    Unknown,
}

impl MembershipDisposition {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Attached => "attached",
            Self::Reused => "reused",
            Self::Terminal => "terminal",
            Self::Substituted => "substituted",
            Self::Retried => "retried",
            Self::Unknown => "unknown",
        }
    }

    /// Parse the `disposition` string a daemon admission response carries.
    #[must_use]
    pub fn from_response(value: &str) -> Self {
        match value {
            "created" => Self::Created,
            "attached" => Self::Attached,
            "reused" => Self::Reused,
            "terminal" => Self::Terminal,
            "substituted" => Self::Substituted,
            "retried" => Self::Retried,
            _ => Self::Unknown,
        }
    }
}

impl std::fmt::Display for MembershipDisposition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One durable membership fact: run `flowRunId` was handed task `taskUuid`.
///
/// `nodeOrdinal`/`nodeLabel` are the *submitting* run's, taken from the capsule
/// on the request. For a row-less admission they are precisely what the durable
/// row cannot say, because that row carries the creating run's capsule instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FlowMembershipRecord {
    pub schema_version: u32,
    pub flow_run_id: String,
    pub task_uuid: String,
    pub disposition: MembershipDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_ordinal: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_label: Option<String>,
    pub recorded_at: String,
}

impl FlowMembershipRecord {
    #[must_use]
    pub fn new(
        flow_run_id: String,
        task_uuid: String,
        disposition: MembershipDisposition,
        node_ordinal: Option<u64>,
        node_label: Option<String>,
    ) -> Self {
        Self {
            schema_version: FLOW_MEMBERSHIP_SCHEMA_VERSION,
            flow_run_id,
            task_uuid,
            disposition,
            node_ordinal,
            node_label,
            recorded_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != FLOW_MEMBERSHIP_SCHEMA_VERSION {
            return Err(format!(
                "schemaVersion must be the integer {FLOW_MEMBERSHIP_SCHEMA_VERSION}"
            ));
        }
        // Neither field is canonicalized. The scan this ledger is unioned with
        // compares the capsule's `flowRunId` to the caller's `--flow-run` byte
        // for byte, so canonicalizing here would let one spelling of a run be
        // found by the ledger and not by the scan, or the reverse.
        if self.flow_run_id.trim().is_empty() {
            return Err("flowRunId must not be empty".to_owned());
        }
        if self.task_uuid.trim().is_empty() {
            return Err("taskUuid must not be empty".to_owned());
        }
        chrono::DateTime::parse_from_rfc3339(&self.recorded_at)
            .map_err(|_| "recordedAt is not an RFC 3339 timestamp".to_owned())?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum FlowMembershipError {
    #[error("flow membership I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("flow membership ledger {path} line {line} is unusable: {reason}")]
    Malformed {
        path: PathBuf,
        line: usize,
        reason: String,
    },
    #[error("{0}")]
    Invalid(String),
}

fn io_error(path: &Path, source: std::io::Error) -> FlowMembershipError {
    FlowMembershipError::Io {
        path: path.to_owned(),
        source,
    }
}

fn durable_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// An in-memory index over the append-only ledger.
///
/// Keyed run-first because every reader asks the same question: "which tasks
/// does this one run hold?"
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlowMembership {
    by_run: BTreeMap<String, BTreeMap<String, FlowMembershipRecord>>,
    records: usize,
}

impl FlowMembership {
    /// Read the ledger. A ledger that does not exist yet is empty membership.
    ///
    /// An unterminated final line is an interrupted append — a crash, a power
    /// loss, or a short write under ENOSPC — and is skipped, as the lineage
    /// ledger and the attestation chain already do with their own torn tails.
    ///
    /// A *complete* record that is unusable is a hard failure, and deliberately
    /// so. Skipping it would drop members from a run and answer a membership
    /// question with a number smaller than the truth, which reads to an
    /// operator as "this run is smaller/quieter than you thought" — the exact
    /// reassuring-direction lie this ledger was written to remove. Failing the
    /// query instead is loud, and repair is one line out of plain JSONL.
    pub fn read(path: &Path) -> Result<Self, FlowMembershipError> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(io_error(path, error)),
        };
        let complete = complete_prefix(&bytes);
        let mut membership = Self::default();
        for (index, line) in BufReader::new(&bytes[..complete]).lines().enumerate() {
            let line = line.map_err(|source| io_error(path, source))?;
            if line.trim().is_empty() {
                continue;
            }
            let malformed = |reason: String| FlowMembershipError::Malformed {
                path: path.to_owned(),
                line: index + 1,
                reason,
            };
            let record: FlowMembershipRecord =
                serde_json::from_str(&line).map_err(|error| malformed(error.to_string()))?;
            record.validate().map_err(malformed)?;
            membership.insert(record);
        }
        Ok(membership)
    }

    /// Record one fact in the index. Returns true when it was not already held.
    ///
    /// First writer wins: a run that is handed the same task twice keeps the
    /// disposition that first joined it, which is the one that explains how the
    /// run came to hold it.
    pub fn insert(&mut self, record: FlowMembershipRecord) -> bool {
        let tasks = self.by_run.entry(record.flow_run_id.clone()).or_default();
        if tasks.contains_key(&record.task_uuid) {
            return false;
        }
        tasks.insert(record.task_uuid.clone(), record);
        self.records += 1;
        true
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records == 0
    }

    #[must_use]
    pub fn record_count(&self) -> usize {
        self.records
    }

    #[must_use]
    pub fn contains(&self, flow_run_id: &str, task_uuid: &str) -> bool {
        self.by_run
            .get(flow_run_id)
            .is_some_and(|tasks| tasks.contains_key(task_uuid))
    }

    /// Every task UUID this run was durably handed, ascending.
    pub fn tasks(&self, flow_run_id: &str) -> impl Iterator<Item = &str> {
        self.by_run
            .get(flow_run_id)
            .into_iter()
            .flat_map(|tasks| tasks.keys().map(String::as_str))
    }

    /// The node ordinal *this* run admitted the task under, which is not
    /// necessarily the ordinal on the task's durable row.
    #[must_use]
    pub fn node_ordinal(&self, flow_run_id: &str, task_uuid: &str) -> Option<u64> {
        self.by_run.get(flow_run_id)?.get(task_uuid)?.node_ordinal
    }

    #[must_use]
    pub fn record(&self, flow_run_id: &str, task_uuid: &str) -> Option<&FlowMembershipRecord> {
        self.by_run.get(flow_run_id)?.get(task_uuid)
    }

    /// Drop whole runs, oldest first, until the index fits `max_records`.
    /// Returns the retained records in replay order when anything was dropped.
    fn compacted(&self, max_records: usize) -> Option<Vec<FlowMembershipRecord>> {
        if self.records <= max_records {
            return None;
        }
        let mut runs = self.by_run.values().collect::<Vec<_>>();
        runs.sort_by(|left, right| run_age(left).cmp(&run_age(right)));
        let mut total = self.records;
        let mut dropped = 0_usize;
        for tasks in &runs {
            if total <= max_records {
                break;
            }
            total -= tasks.len();
            dropped += 1;
        }
        Some(
            runs.into_iter()
                .skip(dropped)
                .flat_map(|tasks| tasks.values().cloned())
                .collect(),
        )
    }
}

/// A run's age is its earliest membership record: the moment it first held a node.
fn run_age(tasks: &BTreeMap<String, FlowMembershipRecord>) -> (&str, &str) {
    tasks
        .values()
        .map(|record| (record.recorded_at.as_str(), record.flow_run_id.as_str()))
        .min()
        .unwrap_or(("", ""))
}

/// Byte length of the complete, newline-terminated prefix of `bytes`.
fn complete_prefix(bytes: &[u8]) -> usize {
    if bytes.is_empty() || bytes.last() == Some(&b'\n') {
        return bytes.len();
    }
    bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position + 1)
}

/// What one membership write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembershipWrite {
    /// A new durable record was appended.
    Appended,
    /// The run already durably held this task; nothing was written.
    AlreadyHeld,
}

/// Append one membership fact and fsync it.
///
/// Idempotent against `held`, the caller's already-parsed index: a run that is
/// handed the same task twice writes once. The check is deliberately the
/// caller's cached index rather than a re-read of the ledger, because this runs
/// on the admission path and a re-read would make every admission linear in the
/// ledger. A duplicate written by a racing second writer is harmless — the read
/// path is set-valued and collapses it.
pub fn record_membership(
    path: &Path,
    record: &FlowMembershipRecord,
    held: &FlowMembership,
) -> Result<MembershipWrite, FlowMembershipError> {
    record_membership_bounded(path, record, held, FLOW_MEMBERSHIP_MAX_RECORDS)
}

fn record_membership_bounded(
    path: &Path,
    record: &FlowMembershipRecord,
    held: &FlowMembership,
    max_records: usize,
) -> Result<MembershipWrite, FlowMembershipError> {
    record.validate().map_err(FlowMembershipError::Invalid)?;
    if held.contains(&record.flow_run_id, &record.task_uuid) {
        return Ok(MembershipWrite::AlreadyHeld);
    }
    let parent = durable_parent(path);
    std::fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    // 0600 like `lifecycle.jsonl`, `changes.jsonl`, and `flow-lineage.jsonl`.
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    file.lock_exclusive()
        .map_err(|source| io_error(path, source))?;

    // A projected index that already accounts for this record decides the
    // bound, so the ledger is compacted *before* it can exceed it.
    let mut projected = held.clone();
    projected.insert(record.clone());
    if let Some(kept) = projected.compacted(max_records) {
        rewrite_locked(&mut file, path, parent, &kept)?;
        return Ok(MembershipWrite::Appended);
    }

    // The read path skips a torn final line; here, holding the write lock, it
    // is removed for good. Appending behind it would splice the interrupted
    // bytes onto this record and turn a skipped tail into a hard read failure.
    truncate_torn_tail(&mut file, path)?;
    let mut line = serde_json::to_vec(record)
        .map_err(|error| FlowMembershipError::Invalid(error.to_string()))?;
    line.push(b'\n');
    file.write_all(&line)
        .map_err(|source| io_error(path, source))?;
    file.sync_all().map_err(|source| io_error(path, source))?;
    Ok(MembershipWrite::Appended)
}

/// Remove an unterminated final line, scanning the tail rather than the file.
fn truncate_torn_tail(file: &mut std::fs::File, path: &Path) -> Result<(), FlowMembershipError> {
    let len = file
        .metadata()
        .map_err(|source| io_error(path, source))?
        .len();
    if len == 0 {
        return Ok(());
    }
    let window = len.min(TAIL_SCAN_BYTES);
    let start = len - window;
    file.seek(SeekFrom::Start(start))
        .map_err(|source| io_error(path, source))?;
    let mut tail = vec![0_u8; usize::try_from(window).unwrap_or(usize::MAX)];
    file.read_exact(&mut tail)
        .map_err(|source| io_error(path, source))?;
    if tail.last() == Some(&b'\n') {
        return Ok(());
    }
    let keep = match tail.iter().rposition(|byte| *byte == b'\n') {
        Some(position) => start + position as u64 + 1,
        // No boundary in the scanned window: either the whole file is one torn
        // line, or a record longer than the window. Read the whole file rather
        // than guess.
        None => {
            let bytes = std::fs::read(path).map_err(|source| io_error(path, source))?;
            complete_prefix(&bytes) as u64
        }
    };
    file.set_len(keep)
        .map_err(|source| io_error(path, source))?;
    file.seek(SeekFrom::End(0))
        .map_err(|source| io_error(path, source))?;
    file.sync_all().map_err(|source| io_error(path, source))?;
    Ok(())
}

/// Replace the ledger contents atomically while holding the append lock.
fn rewrite_locked(
    file: &mut std::fs::File,
    path: &Path,
    parent: &Path,
    kept: &[FlowMembershipRecord],
) -> Result<(), FlowMembershipError> {
    let mut buffer = Vec::new();
    for record in kept {
        serde_json::to_writer(&mut buffer, record)
            .map_err(|error| FlowMembershipError::Invalid(error.to_string()))?;
        buffer.push(b'\n');
    }
    file.set_len(0).map_err(|source| io_error(path, source))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_error(path, source))?;
    file.write_all(&buffer)
        .map_err(|source| io_error(path, source))?;
    file.sync_all().map_err(|source| io_error(path, source))?;
    // The truncate-in-place keeps the inode, so the lock other writers are
    // blocked on stays the one guarding these bytes; syncing the directory is
    // still worth it for the file that may have just been created.
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn record(run: &str, task: &str, disposition: MembershipDisposition) -> FlowMembershipRecord {
        FlowMembershipRecord::new(
            run.to_owned(),
            task.to_owned(),
            disposition,
            Some(0),
            Some("node-0".to_owned()),
        )
    }

    #[test]
    fn a_missing_ledger_is_empty_membership_rather_than_an_error() {
        let temp = tempdir().unwrap();
        let membership = FlowMembership::read(&temp.path().join(FLOW_MEMBERSHIP_FILE)).unwrap();
        assert!(membership.is_empty());
        assert_eq!(membership.tasks("any-run").count(), 0);
    }

    #[test]
    fn appending_is_idempotent_against_the_callers_index() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        let mut held = FlowMembership::default();
        let first = record("run-a", "task-1", MembershipDisposition::Attached);
        assert_eq!(
            record_membership(&path, &first, &held).unwrap(),
            MembershipWrite::Appended
        );
        held.insert(first.clone());
        assert_eq!(
            record_membership(&path, &first, &held).unwrap(),
            MembershipWrite::AlreadyHeld
        );
        let reread = FlowMembership::read(&path).unwrap();
        assert_eq!(reread.record_count(), 1);
        assert_eq!(reread.tasks("run-a").collect::<Vec<_>>(), vec!["task-1"]);
        assert_eq!(reread.node_ordinal("run-a", "task-1"), Some(0));
    }

    #[test]
    fn one_task_can_be_held_by_more_than_one_run() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        let mut held = FlowMembership::default();
        for run in ["run-a", "run-b"] {
            let entry = record(run, "shared-task", MembershipDisposition::Attached);
            record_membership(&path, &entry, &held).unwrap();
            held.insert(entry);
        }
        let reread = FlowMembership::read(&path).unwrap();
        assert!(reread.contains("run-a", "shared-task"));
        assert!(reread.contains("run-b", "shared-task"));
        assert_eq!(reread.record_count(), 2);
    }

    #[test]
    fn a_torn_final_line_is_skipped_on_read_and_removed_on_the_next_append() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        let first = record("run-a", "task-1", MembershipDisposition::Created);
        record_membership(&path, &first, &FlowMembership::default()).unwrap();
        let mut raw = std::fs::read(&path).unwrap();
        raw.extend_from_slice(br#"{"schemaVersion":1,"flowRunId":"run-a","taskUu"#);
        std::fs::write(&path, &raw).unwrap();

        let after_tear = FlowMembership::read(&path).unwrap();
        assert_eq!(after_tear.record_count(), 1);

        let second = record("run-a", "task-2", MembershipDisposition::Attached);
        record_membership(&path, &second, &after_tear).unwrap();
        let repaired = FlowMembership::read(&path).unwrap();
        assert_eq!(repaired.record_count(), 2);
        assert_eq!(
            repaired.tasks("run-a").collect::<Vec<_>>(),
            vec!["task-1", "task-2"]
        );
        assert!(
            std::fs::read(&path).unwrap().ends_with(b"\n"),
            "the torn tail must be gone, not spliced onto the new record"
        );
    }

    #[test]
    fn a_complete_but_unusable_record_fails_the_read_rather_than_shrinking_a_run() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        std::fs::write(
            &path,
            "{\"schemaVersion\":1,\"flowRunId\":\"run-a\",\"taskUuid\":\"task-1\",\
             \"disposition\":\"attached\",\"recordedAt\":\"not-a-timestamp\"}\n",
        )
        .unwrap();
        let error = FlowMembership::read(&path).unwrap_err();
        assert!(
            matches!(error, FlowMembershipError::Malformed { line: 1, .. }),
            "{error}"
        );
    }

    /// N-1 fixture. These bytes are what today's binary writes; a later binary
    /// must still read them. Held inline rather than as a file so the fixture
    /// cannot be lost from the packaged source fileset.
    #[test]
    fn a_schema_version_1_ledger_written_today_still_reads() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        std::fs::write(
            &path,
            concat!(
                r#"{"schemaVersion":1,"flowRunId":"7c2f6f0e-0000-4000-8000-000000000001","#,
                r#""taskUuid":"7c2f6f0e-0000-4000-8000-0000000000a1","disposition":"created","#,
                r#""nodeOrdinal":0,"nodeLabel":"node-0","#,
                r#""recordedAt":"2026-08-04T00:00:00.000Z"}"#,
                "\n",
                r#"{"schemaVersion":1,"flowRunId":"7c2f6f0e-0000-4000-8000-000000000002","#,
                r#""taskUuid":"7c2f6f0e-0000-4000-8000-0000000000a1","disposition":"attached","#,
                r#""recordedAt":"2026-08-04T00:00:01.000Z"}"#,
                "\n",
                // A disposition this binary does not know must degrade to
                // `unknown`, never fail the read: a pin rollback must not turn
                // a newer ledger into a daemon-wide query outage.
                r#"{"schemaVersion":1,"flowRunId":"7c2f6f0e-0000-4000-8000-000000000002","#,
                r#""taskUuid":"7c2f6f0e-0000-4000-8000-0000000000a2","disposition":"teleported","#,
                r#""recordedAt":"2026-08-04T00:00:02.000Z"}"#,
                "\n",
            ),
        )
        .unwrap();
        let membership = FlowMembership::read(&path).unwrap();
        assert_eq!(membership.record_count(), 3);
        assert_eq!(
            membership
                .tasks("7c2f6f0e-0000-4000-8000-000000000002")
                .collect::<Vec<_>>(),
            vec![
                "7c2f6f0e-0000-4000-8000-0000000000a1",
                "7c2f6f0e-0000-4000-8000-0000000000a2"
            ]
        );
        assert_eq!(
            membership
                .record(
                    "7c2f6f0e-0000-4000-8000-000000000002",
                    "7c2f6f0e-0000-4000-8000-0000000000a2"
                )
                .unwrap()
                .disposition,
            MembershipDisposition::Unknown
        );
        assert_eq!(
            membership.node_ordinal(
                "7c2f6f0e-0000-4000-8000-000000000001",
                "7c2f6f0e-0000-4000-8000-0000000000a1"
            ),
            Some(0)
        );
    }

    #[test]
    fn compaction_drops_whole_runs_oldest_first_and_never_half_a_run() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        let mut held = FlowMembership::default();
        // Three runs of two nodes each, written oldest run first.
        for (index, run) in ["run-a", "run-b", "run-c"].into_iter().enumerate() {
            for node in 0..2 {
                let mut entry = record(
                    run,
                    &format!("{run}-task-{node}"),
                    MembershipDisposition::Created,
                );
                entry.recorded_at = format!("2026-08-04T00:00:{:02}.000Z", index * 2 + node);
                record_membership_bounded(&path, &entry, &held, 6).unwrap();
                held.insert(entry);
            }
        }
        assert_eq!(FlowMembership::read(&path).unwrap().record_count(), 6);

        // The seventh record crosses the bound: the oldest run goes whole.
        let mut overflow = record("run-d", "run-d-task-0", MembershipDisposition::Created);
        overflow.recorded_at = "2026-08-04T00:00:09.000Z".to_owned();
        record_membership_bounded(&path, &overflow, &held, 6).unwrap();

        let compacted = FlowMembership::read(&path).unwrap();
        assert_eq!(compacted.tasks("run-a").count(), 0, "oldest run is gone");
        assert_eq!(compacted.tasks("run-b").count(), 2, "and it went whole");
        assert_eq!(compacted.tasks("run-c").count(), 2);
        assert_eq!(compacted.tasks("run-d").count(), 1);
        assert_eq!(compacted.record_count(), 5);
    }

    #[test]
    fn an_unwritable_ledger_is_an_error_rather_than_a_silent_skip() {
        let temp = tempdir().unwrap();
        // A directory where the ledger file belongs: every open fails.
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        std::fs::create_dir(&path).unwrap();
        let error = record_membership(
            &path,
            &record("run-a", "task-1", MembershipDisposition::Attached),
            &FlowMembership::default(),
        )
        .unwrap_err();
        assert!(matches!(error, FlowMembershipError::Io { .. }), "{error}");
    }
}
