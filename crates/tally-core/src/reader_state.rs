//! Operator reader-state on flow runs: `archived` and a free-form triage tag.
//!
//! This is the last strict-superset residue against SSSF's store —
//! `sessions.archived`, "set by the UI; never by a run." A daily-driving
//! operator needs "I have dealt with this run" as state the system holds,
//! without polluting evidence: reader-state is not a fact about execution.
//!
//! Three properties make that separation real, not just a naming convention:
//!
//! 1. **A different file.** This store never shares a path with
//!    `witness.jsonl`, `attestations.jsonl`, or any hash-chained ledger, and
//!    nothing in this module reads or writes those files.
//! 2. **Written only by an explicit operator verb.** Every mutation here
//!    flows through [`set_reader_state`], called from the `tally reader-state`
//!    CLI family and nowhere else — never from the daemon's admission,
//!    reconcile, or exit-recording paths, and never automatically.
//! 3. **Advisory on read.** [`ReaderState::read_advisory`] degrades a
//!    corrupt, truncated, or missing store to "nothing is archived" rather
//!    than failing the query that asked. A reader-state file an operator
//!    never touched, or clobbered by hand, must never take a `query` command
//!    down with it — and must never be consulted by ledger verification at
//!    all, which is the property that keeps this advisory.
//!
//! The store is a flat JSONL log, latest record per `flowRunId` wins on
//! read — the same shape as [`crate::flow_lineage`], chosen so a toggle
//! (archive, then later unarchive, then re-tag) is a plain append rather than
//! an in-place rewrite race. Unlike the lineage ledger this one is folded to
//! one record per run once it grows past [`READER_STATE_COMPACT_THRESHOLD`],
//! because a mutable flag an operator can flip repeatedly has no natural
//! append-only bound the way a one-shot rollover does.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::flow_lineage::canonical_flow_run_id;

pub const READER_STATE_SCHEMA_VERSION: u32 = 1;
pub const READER_STATE_FILE: &str = "reader-state.jsonl";

/// Records kept as literal lines before a write folds the store to one
/// record per `flowRunId`. Chosen well above ordinary interactive use (an
/// operator toggling a handful of runs a day) so compaction is rare, and well
/// below "unbounded" so a scripted archive loop cannot grow this file
/// forever the way an unbounded per-dispatch write would.
pub const READER_STATE_COMPACT_THRESHOLD: usize = 512;

/// The reader-state file living beside the other durable, non-witness stores
/// in a daemon's data directory.
#[must_use]
pub fn reader_state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(READER_STATE_FILE)
}

fn lookup_key(value: &str) -> String {
    canonical_flow_run_id(value).unwrap_or_else(|_| value.to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReaderStateRecord {
    pub schema_version: u32,
    pub flow_run_id: String,
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage_tag: Option<String>,
    pub recorded_at: String,
}

impl ReaderStateRecord {
    fn validate(&self) -> Result<(), String> {
        if self.schema_version != READER_STATE_SCHEMA_VERSION {
            return Err(format!(
                "schemaVersion must be the integer {READER_STATE_SCHEMA_VERSION}"
            ));
        }
        canonical_flow_run_id(&self.flow_run_id)
            .map_err(|_| "flowRunId is not a UUID".to_owned())?;
        chrono::DateTime::parse_from_rfc3339(&self.recorded_at)
            .map_err(|_| "recordedAt is not an RFC 3339 timestamp".to_owned())?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ReaderStateError {
    #[error("reader-state I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("reader-state store {path} line {line} is unusable: {reason}")]
    Malformed {
        path: PathBuf,
        line: usize,
        reason: String,
    },
    #[error("{0}")]
    Invalid(String),
}

fn io_error(path: &Path, source: std::io::Error) -> ReaderStateError {
    ReaderStateError::Io {
        path: path.to_owned(),
        source,
    }
}

fn durable_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Decode every complete record, ignoring a torn final line (an interrupted
/// append), the same tolerance [`crate::flow_lineage`] applies.
fn read_records(path: &Path) -> Result<(Vec<(usize, ReaderStateRecord)>, bool), ReaderStateError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), false))
        }
        Err(error) => return Err(io_error(path, error)),
    };
    let torn = !bytes.is_empty() && bytes.last() != Some(&b'\n');
    let complete = if torn {
        bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |position| position + 1)
    } else {
        bytes.len()
    };
    let mut records = Vec::new();
    for (index, line) in BufReader::new(&bytes[..complete]).lines().enumerate() {
        let line = line.map_err(|source| io_error(path, source))?;
        if line.trim().is_empty() {
            continue;
        }
        let malformed = |reason: String| ReaderStateError::Malformed {
            path: path.to_owned(),
            line: index + 1,
            reason,
        };
        let record: ReaderStateRecord =
            serde_json::from_str(&line).map_err(|error| malformed(error.to_string()))?;
        record.validate().map_err(malformed)?;
        records.push((index + 1, record));
    }
    Ok((records, torn))
}

/// An in-memory view over the store: latest record per flow run wins.
#[derive(Debug, Clone, Default)]
pub struct ReaderState {
    by_run: BTreeMap<String, ReaderStateRecord>,
}

impl ReaderState {
    /// Read the store. A store that does not exist yet is empty state — no
    /// run has ever been archived or tagged.
    pub fn read(path: &Path) -> Result<Self, ReaderStateError> {
        let (records, _) = read_records(path)?;
        let mut state = Self::default();
        for (_, record) in records {
            state.by_run.insert(lookup_key(&record.flow_run_id), record);
        }
        Ok(state)
    }

    /// The read every `query` command uses. Reader-state is advisory
    /// convenience layered on top of the canonical evidence surface, never
    /// load-bearing for it, so a store this degrades to empty rather than
    /// failing a query that has nothing to do with archiving.
    // This is a diagnostic on a degrade-to-empty path with no `Result` to
    // return to a caller who could otherwise never learn their store is
    // corrupt; the same shape as `read_attestations_advisory`
    // (`daemon/rpc/query.rs`), which lives under `daemon`'s blanket allow.
    #[allow(clippy::disallowed_macros)]
    #[must_use]
    pub fn read_advisory(path: &Path) -> Self {
        match Self::read(path) {
            Ok(state) => state,
            Err(error) => {
                eprintln!("tally: reader-state store unreadable, treating as empty: {error}");
                Self::default()
            }
        }
    }

    #[must_use]
    pub fn is_archived(&self, flow_run_id: &str) -> bool {
        self.by_run
            .get(&lookup_key(flow_run_id))
            .is_some_and(|record| record.archived)
    }

    #[must_use]
    pub fn triage_tag(&self, flow_run_id: &str) -> Option<&str> {
        self.by_run
            .get(&lookup_key(flow_run_id))
            .and_then(|record| record.triage_tag.as_deref())
    }

    #[must_use]
    pub fn record(&self, flow_run_id: &str) -> Option<&ReaderStateRecord> {
        self.by_run.get(&lookup_key(flow_run_id))
    }
}

/// What to change about one run's reader-state. `None` leaves a field as it
/// was; `Some(None)` on `triage_tag` clears it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReaderStateUpdate {
    pub archived: Option<bool>,
    pub triage_tag: Option<Option<String>>,
}

/// Append one reader-state change, merging unset fields from the run's
/// current record so `tally reader-state archive` never clobbers a triage tag
/// set by an earlier `tally reader-state tag`, and vice versa.
pub fn set_reader_state(
    path: &Path,
    flow_run_id: &str,
    update: ReaderStateUpdate,
) -> Result<ReaderStateRecord, ReaderStateError> {
    let flow_run_id = canonical_flow_run_id(flow_run_id)
        .map_err(|_| ReaderStateError::Invalid(format!("{flow_run_id:?} is not a UUID")))?;
    let parent = durable_parent(path);
    std::fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    let created = !path.exists();
    // 0600 like every other durable, non-witness store this daemon owns
    // (`flow-lineage.jsonl`, `flow-membership.jsonl`): not sensitive, but a
    // data-dir store should not be world readable by accident.
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    file.lock_exclusive()
        .map_err(|source| io_error(path, source))?;
    let (existing_records, torn) = read_records(path)?;
    let previous = existing_records
        .iter()
        .rev()
        .find(|(_, record)| record.flow_run_id == flow_run_id)
        .map(|(_, record)| record.clone());
    let archived = update
        .archived
        .unwrap_or_else(|| previous.as_ref().is_some_and(|record| record.archived));
    let triage_tag = match update.triage_tag {
        Some(tag) => tag,
        None => previous.and_then(|record| record.triage_tag),
    };
    let record = ReaderStateRecord {
        schema_version: READER_STATE_SCHEMA_VERSION,
        flow_run_id,
        archived,
        triage_tag,
        recorded_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
    };
    record.validate().map_err(ReaderStateError::Invalid)?;
    if existing_records.len() + 1 > READER_STATE_COMPACT_THRESHOLD {
        let mut folded = BTreeMap::new();
        for (_, existing) in existing_records {
            folded.insert(existing.flow_run_id.clone(), existing);
        }
        folded.insert(record.flow_run_id.clone(), record.clone());
        rewrite_compacted(path, parent, &folded.into_values().collect::<Vec<_>>())?;
    } else {
        if torn {
            truncate_torn_tail(&mut file, path)?;
        }
        let mut line = serde_json::to_vec(&record)
            .map_err(|error| ReaderStateError::Invalid(error.to_string()))?;
        line.push(b'\n');
        file.write_all(&line)
            .map_err(|source| io_error(path, source))?;
        file.sync_all().map_err(|source| io_error(path, source))?;
    }
    if created {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| io_error(parent, source))?;
    }
    Ok(record)
}

fn truncate_torn_tail(file: &mut File, path: &Path) -> Result<(), ReaderStateError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_error(path, source))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.is_empty() || bytes.last() == Some(&b'\n') {
        return Ok(());
    }
    let complete = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position + 1);
    file.set_len(complete as u64)
        .map_err(|source| io_error(path, source))?;
    file.sync_all().map_err(|source| io_error(path, source))
}

/// Replace the store with one record per flow run, folded latest-wins.
fn rewrite_compacted(
    path: &Path,
    parent: &Path,
    kept: &[ReaderStateRecord],
) -> Result<(), ReaderStateError> {
    let temporary = path.with_extension("jsonl.compact");
    let mut bytes = Vec::new();
    for record in kept {
        serde_json::to_writer(&mut bytes, record)
            .map_err(|error| ReaderStateError::Invalid(error.to_string()))?;
        bytes.push(b'\n');
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|source| io_error(&temporary, source))?;
    file.write_all(&bytes)
        .map_err(|source| io_error(&temporary, source))?;
    file.sync_all()
        .map_err(|source| io_error(&temporary, source))?;
    drop(file);
    std::fs::rename(&temporary, path).map_err(|source| io_error(path, source))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(parent, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "00000000-0000-4000-8000-0000000000a1";
    const B: &str = "00000000-0000-4000-8000-0000000000b2";

    #[test]
    fn missing_store_reads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(READER_STATE_FILE);
        let state = ReaderState::read(&path).unwrap();
        assert!(!state.is_archived(A));
        assert_eq!(state.triage_tag(A), None);
    }

    #[test]
    fn archive_then_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(READER_STATE_FILE);
        set_reader_state(
            &path,
            A,
            ReaderStateUpdate {
                archived: Some(true),
                triage_tag: None,
            },
        )
        .unwrap();
        let state = ReaderState::read(&path).unwrap();
        assert!(state.is_archived(A));
        assert!(!state.is_archived(B));
    }

    #[test]
    fn setting_tag_does_not_clobber_archived() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(READER_STATE_FILE);
        set_reader_state(
            &path,
            A,
            ReaderStateUpdate {
                archived: Some(true),
                triage_tag: None,
            },
        )
        .unwrap();
        set_reader_state(
            &path,
            A,
            ReaderStateUpdate {
                archived: None,
                triage_tag: Some(Some("needs-followup".to_owned())),
            },
        )
        .unwrap();
        let state = ReaderState::read(&path).unwrap();
        assert!(state.is_archived(A));
        assert_eq!(state.triage_tag(A), Some("needs-followup"));
    }

    #[test]
    fn unarchive_clears_flag_but_keeps_tag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(READER_STATE_FILE);
        set_reader_state(
            &path,
            A,
            ReaderStateUpdate {
                archived: Some(true),
                triage_tag: Some(Some("flaky".to_owned())),
            },
        )
        .unwrap();
        set_reader_state(
            &path,
            A,
            ReaderStateUpdate {
                archived: Some(false),
                triage_tag: None,
            },
        )
        .unwrap();
        let state = ReaderState::read(&path).unwrap();
        assert!(!state.is_archived(A));
        assert_eq!(state.triage_tag(A), Some("flaky"));
    }

    #[test]
    fn untag_clears_tag_explicitly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(READER_STATE_FILE);
        set_reader_state(
            &path,
            A,
            ReaderStateUpdate {
                archived: None,
                triage_tag: Some(Some("flaky".to_owned())),
            },
        )
        .unwrap();
        set_reader_state(
            &path,
            A,
            ReaderStateUpdate {
                archived: None,
                triage_tag: Some(None),
            },
        )
        .unwrap();
        let state = ReaderState::read(&path).unwrap();
        assert_eq!(state.triage_tag(A), None);
    }

    #[test]
    fn rejects_non_uuid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(READER_STATE_FILE);
        let error = set_reader_state(
            &path,
            "not-a-uuid",
            ReaderStateUpdate {
                archived: Some(true),
                triage_tag: None,
            },
        )
        .unwrap_err();
        assert!(matches!(error, ReaderStateError::Invalid(_)));
    }

    #[test]
    fn corrupt_store_degrades_to_empty_on_advisory_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(READER_STATE_FILE);
        set_reader_state(
            &path,
            A,
            ReaderStateUpdate {
                archived: Some(true),
                triage_tag: None,
            },
        )
        .unwrap();
        std::fs::write(&path, b"{not valid json at all\n").unwrap();
        assert!(ReaderState::read(&path).is_err());
        let state = ReaderState::read_advisory(&path);
        assert!(!state.is_archived(A));
    }

    #[test]
    fn compaction_bounds_growth_under_repeated_toggles() {
        // Three threshold-widths of repeated toggles on ONE run: an unbounded
        // append-only store would carry every one of these lines forever.
        // Compaction folds on crossing the threshold, so the store never
        // grows past roughly one threshold's worth of lines even after many
        // multiples of that many writes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(READER_STATE_FILE);
        let toggles = READER_STATE_COMPACT_THRESHOLD * 3;
        for toggle in 0..toggles {
            set_reader_state(
                &path,
                A,
                ReaderStateUpdate {
                    archived: Some(toggle % 2 == 0),
                    triage_tag: None,
                },
            )
            .unwrap();
        }
        let (records, _) = read_records(&path).unwrap();
        assert!(
            records.len() <= READER_STATE_COMPACT_THRESHOLD,
            "expected compaction to bound the store well under {} writes, got {} records",
            toggles,
            records.len()
        );
        let state = ReaderState::read(&path).unwrap();
        // The last toggle index is `toggles - 1`, odd -> not archived.
        assert!(!state.is_archived(A));
    }

    #[test]
    fn compaction_folds_many_runs_to_one_record_each() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(READER_STATE_FILE);
        // Distinct runs, each toggled twice: compaction folds by key, so this
        // should settle at one record per distinct flow_run_id, not one per
        // write.
        let run_count = READER_STATE_COMPACT_THRESHOLD + 5;
        for index in 0..run_count {
            let run = format!("00000000-0000-4000-8000-{index:012x}");
            set_reader_state(
                &path,
                &run,
                ReaderStateUpdate {
                    archived: Some(true),
                    triage_tag: None,
                },
            )
            .unwrap();
        }
        let (records, _) = read_records(&path).unwrap();
        let mut folded = BTreeMap::new();
        for (_, record) in records {
            folded.insert(record.flow_run_id.clone(), record);
        }
        assert_eq!(folded.len(), run_count);
        assert!(folded.values().all(|record| record.archived));
    }
}
