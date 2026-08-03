//! Durable predecessor/successor lineage for flow runs.
//!
//! A flow run's identity-bearing inputs — script bytes, serialized arguments,
//! catalog bytes — are pinned for the life of the run. Replaying a run whose
//! inputs moved is refused, and that refusal is correct: the alternative is one
//! run whose first half and second half came from different programs.
//!
//! What the refusal alone cannot express is the *transition*. A supervised
//! runner that persists one `flowRunId` per work item and retries it across
//! declarative deployments can only ever re-observe the same refusal, because
//! nothing durable says the old generation was abandoned, why, or which run
//! replaced it. This ledger is that missing statement: an append-only chain of
//! `predecessor → successor` records with a closed reason vocabulary.
//!
//! The ledger is deliberately not hash-chained. It is not a proof surface — the
//! witness ledger remains the only canonical one — it is a small durable
//! index that makes an unattended rollover safe to automate and later auditable.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const FLOW_LINEAGE_SCHEMA_VERSION: u32 = 1;
pub const FLOW_LINEAGE_FILE: &str = "flow-lineage.jsonl";

/// Newest records kept when the ledger is compacted on append.
///
/// The bound exists because a declarative estate retires one run per work item
/// per activation: a 3,000-item worklist on a daily cadence writes ~1.1 M
/// records a year, and the file is read whole. Compaction is safe here for the
/// reason stated at the top of this module — this is an index, not a proof
/// chain — and it drops the *oldest* generations, whose runs are long past any
/// possibility of replay. Shaped after `changes.jsonl`'s count bound rather
/// than inventing a new retention mechanism.
pub const FLOW_LINEAGE_MAX_RECORDS: usize = 100_000;

/// One canonical rendering for a run ID: hyphenated, lowercase.
///
/// `Uuid::parse_str` also accepts upper case, the unhyphenated simple form, and
/// the braced form. Each of those is a *different* `String` key, so storing the
/// caller's raw spelling would let a rollover be recorded against a run nobody
/// ever replays — the exact silent-no-op this whole mechanism exists to
/// eliminate. Every write and every lookup goes through here.
pub fn canonical_flow_run_id(value: &str) -> Result<String, FlowLineageError> {
    Uuid::parse_str(value)
        .map(|uuid| uuid.hyphenated().to_string())
        .map_err(|_| FlowLineageError::Invalid(format!("{value:?} is not a UUID")))
}

/// The lookup key for a possibly non-canonical run ID.
///
/// A value that is not a UUID cannot name a flow run at all, so it keys to
/// itself and simply matches nothing.
fn lookup_key(value: &str) -> String {
    canonical_flow_run_id(value).unwrap_or_else(|_| value.to_owned())
}

/// Why a run was abandoned in favour of a successor.
///
/// Closed on purpose. The whole point of recording a reason is that a generic
/// supervisor can branch on it without reading prose, and an operator auditing
/// a generation boundary months later can group by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SupersedeReason {
    /// A declarative activation replaced the script and/or argument store paths
    /// under a supervisor that kept retrying the old run ID.
    GenerationChange,
    /// The pinned script bytes changed.
    ScriptChanged,
    /// The pinned serialized arguments changed.
    ArgsChanged,
    /// The pinned catalog bytes changed, appeared, or were removed.
    CatalogChanged,
    /// An operator decision that is none of the above.
    Operator,
}

impl SupersedeReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GenerationChange => "generation-change",
            Self::ScriptChanged => "script-changed",
            Self::ArgsChanged => "args-changed",
            Self::CatalogChanged => "catalog-changed",
            Self::Operator => "operator",
        }
    }
}

impl std::fmt::Display for SupersedeReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One durable rollover: `flowRunId` is terminal, `successorFlowRunId` replaces it.
///
/// The predecessor's pinned hashes are recorded by the daemon from the
/// predecessor's own durable rows, never supplied by the caller. They are the
/// frozen fingerprint of the abandoned generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FlowSupersedeRecord {
    pub schema_version: u32,
    pub flow_run_id: String,
    pub successor_flow_run_id: String,
    pub reason: SupersedeReason,
    pub recorded_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor_script_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor_args_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor_catalog_hash: Option<String>,
}

impl FlowSupersedeRecord {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != FLOW_LINEAGE_SCHEMA_VERSION {
            return Err(format!(
                "schemaVersion must be the integer {FLOW_LINEAGE_SCHEMA_VERSION}"
            ));
        }
        Uuid::parse_str(&self.flow_run_id).map_err(|_| "flowRunId is not a UUID".to_owned())?;
        Uuid::parse_str(&self.successor_flow_run_id)
            .map_err(|_| "successorFlowRunId is not a UUID".to_owned())?;
        if self.flow_run_id == self.successor_flow_run_id {
            return Err("successorFlowRunId must differ from flowRunId".to_owned());
        }
        chrono::DateTime::parse_from_rfc3339(&self.recorded_at)
            .map_err(|_| "recordedAt is not an RFC 3339 timestamp".to_owned())?;
        Ok(())
    }

    /// True when this record supersedes `flow_run_id` with `successor` for `reason`.
    ///
    /// The predecessor hashes are excluded: they are daemon-observed context,
    /// not part of the caller's request, so a re-observation must not turn a
    /// retry into a conflict.
    fn matches_request(&self, successor: &str, reason: SupersedeReason) -> bool {
        self.successor_flow_run_id == successor && self.reason == reason
    }
}

/// What one supersede call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SupersedeDisposition {
    /// A new durable record was appended.
    Recorded,
    /// The exact same rollover was already durable; nothing was written.
    Reused,
}

impl SupersedeDisposition {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Reused => "reused",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SupersedeOutcome {
    pub ok: bool,
    pub disposition: SupersedeDisposition,
    pub record: FlowSupersedeRecord,
}

/// The lineage of one flow run as a query surface answers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FlowLineageView {
    pub schema_version: u32,
    pub flow_run_id: String,
    /// True once this run has been superseded: it is terminal and replay is refused.
    pub superseded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<FlowSupersedeRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<FlowSupersedeRecord>,
    /// The whole generation chain oldest-first, always containing `flowRunId`.
    pub chain: Vec<String>,
    /// The last run in the chain: the one an operator or supervisor should run.
    pub current_flow_run_id: String,
}

#[derive(Debug, Error)]
pub enum FlowLineageError {
    #[error("flow lineage I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("flow lineage ledger {path} line {line} is unusable: {reason}")]
    Malformed {
        path: PathBuf,
        line: usize,
        reason: String,
    },
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
}

fn io_error(path: &Path, source: std::io::Error) -> FlowLineageError {
    FlowLineageError::Io {
        path: path.to_owned(),
        source,
    }
}

fn durable_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Decode every complete record in the ledger, ignoring a torn final line.
///
/// Returns the `(line number, record)` pairs and whether a torn tail was seen,
/// so the write path can physically truncate what the read path merely skips.
fn read_records(
    path: &Path,
) -> Result<(Vec<(usize, FlowSupersedeRecord)>, bool), FlowLineageError> {
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
        let malformed = |reason: String| FlowLineageError::Malformed {
            path: path.to_owned(),
            line: index + 1,
            reason,
        };
        let record: FlowSupersedeRecord =
            serde_json::from_str(&line).map_err(|error| malformed(error.to_string()))?;
        record.validate().map_err(malformed)?;
        records.push((index + 1, record));
    }
    Ok((records, torn))
}

/// An in-memory index over the append-only ledger.
#[derive(Debug, Clone, Default)]
pub struct FlowLineage {
    by_predecessor: BTreeMap<String, FlowSupersedeRecord>,
    by_successor: BTreeMap<String, FlowSupersedeRecord>,
}

impl FlowLineage {
    /// Read the ledger. A ledger that does not exist yet is an empty lineage.
    ///
    /// An unterminated final line is an interrupted append — a crash, a power
    /// loss, or a short write under ENOSPC — and is ignored, as the attestation
    /// chain already does with its own torn tail. A *complete* record that is
    /// unusable is still a hard failure: skipping it could resurrect a run an
    /// operator durably retired, which is the one outcome this store exists to
    /// prevent. Repair is one line out of a plain JSONL index, documented in
    /// the troubleshooting chapter.
    pub fn read(path: &Path) -> Result<Self, FlowLineageError> {
        let (records, _) = read_records(path)?;
        let mut lineage = Self::default();
        for (line, record) in records {
            lineage
                .insert(record)
                .map_err(|reason| FlowLineageError::Malformed {
                    path: path.to_owned(),
                    line,
                    reason,
                })?;
        }
        Ok(lineage)
    }

    fn insert(&mut self, mut record: FlowSupersedeRecord) -> Result<(), String> {
        // Read-side canonicalization absorbs a record written before run IDs
        // were canonicalized on the way in, so a pre-repair ledger entry in an
        // upper-case or unhyphenated rendering still answers the lookup the
        // runner actually makes.
        record.flow_run_id = lookup_key(&record.flow_run_id);
        record.successor_flow_run_id = lookup_key(&record.successor_flow_run_id);
        if let Some(existing) = self.by_predecessor.get(&record.flow_run_id) {
            return Err(format!(
                "flow run {} is already superseded by {}",
                record.flow_run_id, existing.successor_flow_run_id
            ));
        }
        if let Some(existing) = self.by_successor.get(&record.successor_flow_run_id) {
            return Err(format!(
                "flow run {} already succeeds {}",
                record.successor_flow_run_id, existing.flow_run_id
            ));
        }
        self.by_predecessor
            .insert(record.flow_run_id.clone(), record.clone());
        self.by_successor
            .insert(record.successor_flow_run_id.clone(), record);
        Ok(())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_predecessor.is_empty()
    }

    /// The record that made `flow_run_id` terminal, if any.
    #[must_use]
    pub fn superseded_by(&self, flow_run_id: &str) -> Option<&FlowSupersedeRecord> {
        self.by_predecessor.get(&lookup_key(flow_run_id))
    }

    /// The record naming `flow_run_id` as a successor, if any.
    #[must_use]
    pub fn supersedes(&self, flow_run_id: &str) -> Option<&FlowSupersedeRecord> {
        self.by_successor.get(&lookup_key(flow_run_id))
    }

    /// The whole generation chain, oldest first. Always contains `flow_run_id`.
    #[must_use]
    pub fn chain(&self, flow_run_id: &str) -> Vec<String> {
        let mut root = lookup_key(flow_run_id);
        let mut guard = 0_usize;
        while let Some(record) = self.by_successor.get(&root) {
            root = record.flow_run_id.clone();
            guard += 1;
            if guard > self.by_predecessor.len() {
                break;
            }
        }
        let mut chain = vec![root.clone()];
        let mut cursor = root;
        while let Some(record) = self.by_predecessor.get(&cursor) {
            cursor = record.successor_flow_run_id.clone();
            chain.push(cursor.clone());
            if chain.len() > self.by_predecessor.len() + 1 {
                break;
            }
        }
        chain
    }

    #[must_use]
    pub fn view(&self, flow_run_id: &str) -> FlowLineageView {
        let key = lookup_key(flow_run_id);
        let chain = self.chain(&key);
        let current = chain.last().cloned().unwrap_or_else(|| key.clone());
        FlowLineageView {
            schema_version: FLOW_LINEAGE_SCHEMA_VERSION,
            // The view echoes the canonical rendering, never the caller's, so
            // two spellings of one run cannot read as two runs.
            flow_run_id: key.clone(),
            superseded: self.by_predecessor.contains_key(&key),
            superseded_by: self.by_predecessor.get(&key).cloned(),
            supersedes: self.by_successor.get(&key).cloned(),
            chain,
            current_flow_run_id: current,
        }
    }

    /// True when superseding `predecessor` with `successor` would close a cycle.
    fn would_cycle(&self, predecessor: &str, successor: &str) -> bool {
        let mut cursor = lookup_key(successor);
        let mut guard = 0_usize;
        while let Some(record) = self.by_predecessor.get(&cursor) {
            if record.successor_flow_run_id == predecessor {
                return true;
            }
            cursor = record.successor_flow_run_id.clone();
            guard += 1;
            if guard > self.by_predecessor.len() {
                return true;
            }
        }
        false
    }

    /// Decide what a supersede request means against the durable ledger without
    /// writing anything.
    ///
    /// `Ok(None)` means the request is new and may be appended; `Ok(Some(record))`
    /// means the exact rollover is already durable and the call is a no-op.
    pub fn classify(
        &self,
        predecessor: &str,
        successor: &str,
        reason: SupersedeReason,
    ) -> Result<Option<&FlowSupersedeRecord>, FlowLineageError> {
        let predecessor = &canonical_flow_run_id(predecessor)?;
        let successor = &canonical_flow_run_id(successor)?;
        if predecessor == successor {
            return Err(FlowLineageError::Invalid(
                "successorFlowRunId must differ from flowRunId".to_owned(),
            ));
        }
        if let Some(existing) = self.by_predecessor.get(predecessor) {
            if existing.matches_request(successor, reason) {
                return Ok(Some(existing));
            }
            return Err(FlowLineageError::Conflict(format!(
                "flow run {predecessor} is already superseded by {} for reason {}; \
                 a durable rollover is never rewritten",
                existing.successor_flow_run_id, existing.reason
            )));
        }
        if let Some(existing) = self.by_successor.get(successor) {
            return Err(FlowLineageError::Conflict(format!(
                "flow run {successor} already succeeds {}; a successor belongs to one predecessor",
                existing.flow_run_id
            )));
        }
        if self.would_cycle(predecessor, successor) {
            return Err(FlowLineageError::Conflict(format!(
                "superseding {predecessor} with {successor} would close a lineage cycle"
            )));
        }
        Ok(None)
    }
}

/// Predecessor hashes the daemon observed on the abandoned run's own rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PredecessorPins {
    pub script_hash: Option<String>,
    pub args_hash: Option<String>,
    pub catalog_hash: Option<String>,
}

/// Append one rollover, or recognize that it is already durable.
///
/// Idempotent by construction: the identical `(predecessor, successor, reason)`
/// triple returns [`SupersedeDisposition::Reused`] and writes nothing, so a
/// supervisor that crashes between recording the rollover and acting on it can
/// simply call again.
pub fn record_supersede(
    path: &Path,
    predecessor: &str,
    successor: &str,
    reason: SupersedeReason,
    pins: &PredecessorPins,
) -> Result<SupersedeOutcome, FlowLineageError> {
    record_supersede_bounded(
        path,
        predecessor,
        successor,
        reason,
        pins,
        FLOW_LINEAGE_MAX_RECORDS,
    )
}

fn record_supersede_bounded(
    path: &Path,
    predecessor: &str,
    successor: &str,
    reason: SupersedeReason,
    pins: &PredecessorPins,
    max_records: usize,
) -> Result<SupersedeOutcome, FlowLineageError> {
    let predecessor = canonical_flow_run_id(predecessor)?;
    let successor = canonical_flow_run_id(successor)?;
    let parent = durable_parent(path);
    std::fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    let created = !path.exists();
    // 0600 like `lifecycle.jsonl` and `changes.jsonl`; the contents are not
    // sensitive, but a data-dir store should not be the one that is world
    // readable by accident.
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    file.lock_exclusive()
        .map_err(|source| io_error(path, source))?;
    // Re-read under the lock: another connection may have recorded the same
    // rollover between this caller's read and its write. This also decides the
    // torn tail — the read path skips an interrupted append, and here, holding
    // the write lock, it is truncated for good.
    let (existing_records, torn) = read_records(path)?;
    let mut lineage = FlowLineage::default();
    for (line, record) in &existing_records {
        lineage
            .insert(record.clone())
            .map_err(|reason| FlowLineageError::Malformed {
                path: path.to_owned(),
                line: *line,
                reason,
            })?;
    }
    if let Some(existing) = lineage.classify(&predecessor, &successor, reason)? {
        return Ok(SupersedeOutcome {
            ok: true,
            disposition: SupersedeDisposition::Reused,
            record: existing.clone(),
        });
    }
    let record = FlowSupersedeRecord {
        schema_version: FLOW_LINEAGE_SCHEMA_VERSION,
        flow_run_id: predecessor,
        successor_flow_run_id: successor,
        reason,
        recorded_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        predecessor_script_hash: pins.script_hash.clone(),
        predecessor_args_hash: pins.args_hash.clone(),
        predecessor_catalog_hash: pins.catalog_hash.clone(),
    };
    record.validate().map_err(FlowLineageError::Invalid)?;
    if existing_records.len() >= max_records {
        // Keep the newest `max_records - 1` and let the new record complete the bound.
        let dropped = existing_records.len() + 1 - max_records;
        let kept = existing_records
            .into_iter()
            .skip(dropped)
            .map(|(_, record)| record)
            .chain(std::iter::once(record.clone()))
            .collect::<Vec<_>>();
        rewrite_compacted(path, parent, &kept)?;
    } else {
        if torn {
            truncate_torn_tail(&mut file, path)?;
        }
        let mut line = serde_json::to_vec(&record)
            .map_err(|error| FlowLineageError::Invalid(error.to_string()))?;
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
    Ok(SupersedeOutcome {
        ok: true,
        disposition: SupersedeDisposition::Recorded,
        record,
    })
}

/// Drop an interrupted final append, keeping every complete record.
fn truncate_torn_tail(file: &mut File, path: &Path) -> Result<(), FlowLineageError> {
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

/// Replace the ledger with `kept`, oldest first, through a temporary file.
///
/// A concurrent reader without the write lock observes either the whole old
/// file or the whole new one, never a half-written ledger. Writers are
/// serialized above this by the daemon: one daemon owns a data directory
/// (`daemon.lock`) and its RPC loop is single-threaded, so the `flock` this
/// runs under is defence in depth rather than the mechanism that makes the
/// rename safe.
fn rewrite_compacted(
    path: &Path,
    parent: &Path,
    kept: &[FlowSupersedeRecord],
) -> Result<(), FlowLineageError> {
    let temporary = path.with_extension("jsonl.compact");
    let mut bytes = Vec::new();
    for record in kept {
        serde_json::to_writer(&mut bytes, record)
            .map_err(|error| FlowLineageError::Invalid(error.to_string()))?;
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
    const C: &str = "00000000-0000-4000-8000-0000000000c3";

    fn pins() -> PredecessorPins {
        PredecessorPins {
            script_hash: Some("sha256:aa".to_owned()),
            args_hash: Some("sha256:bb".to_owned()),
            catalog_hash: None,
        }
    }

    #[test]
    fn a_missing_ledger_reads_as_an_empty_lineage() {
        let temp = tempfile::tempdir().unwrap();
        let lineage = FlowLineage::read(&temp.path().join(FLOW_LINEAGE_FILE)).unwrap();
        assert!(lineage.is_empty());
        let view = lineage.view(A);
        assert!(!view.superseded);
        assert_eq!(view.chain, vec![A.to_owned()]);
        assert_eq!(view.current_flow_run_id, A);
    }

    #[test]
    fn recording_the_same_rollover_twice_writes_one_record() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FLOW_LINEAGE_FILE);
        let first =
            record_supersede(&path, A, B, SupersedeReason::GenerationChange, &pins()).unwrap();
        assert_eq!(first.disposition, SupersedeDisposition::Recorded);
        let second = record_supersede(
            &path,
            A,
            B,
            SupersedeReason::GenerationChange,
            &PredecessorPins::default(),
        )
        .unwrap();
        assert_eq!(second.disposition, SupersedeDisposition::Reused);
        // The reused answer is the original durable record, pins included.
        assert_eq!(second.record, first.record);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            1,
            "an idempotent retry must not append a second record"
        );
    }

    #[test]
    fn a_second_successor_for_one_predecessor_is_a_conflict() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FLOW_LINEAGE_FILE);
        record_supersede(&path, A, B, SupersedeReason::GenerationChange, &pins()).unwrap();
        let error =
            record_supersede(&path, A, C, SupersedeReason::GenerationChange, &pins()).unwrap_err();
        assert!(matches!(error, FlowLineageError::Conflict(_)), "{error}");
        let reason_change =
            record_supersede(&path, A, B, SupersedeReason::Operator, &pins()).unwrap_err();
        assert!(
            matches!(reason_change, FlowLineageError::Conflict(_)),
            "{reason_change}"
        );
    }

    #[test]
    fn one_successor_may_not_serve_two_predecessors() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FLOW_LINEAGE_FILE);
        record_supersede(&path, A, C, SupersedeReason::GenerationChange, &pins()).unwrap();
        let error =
            record_supersede(&path, B, C, SupersedeReason::GenerationChange, &pins()).unwrap_err();
        assert!(matches!(error, FlowLineageError::Conflict(_)), "{error}");
    }

    #[test]
    fn a_chain_reports_every_generation_and_its_tip() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FLOW_LINEAGE_FILE);
        record_supersede(&path, A, B, SupersedeReason::GenerationChange, &pins()).unwrap();
        record_supersede(&path, B, C, SupersedeReason::ScriptChanged, &pins()).unwrap();
        let lineage = FlowLineage::read(&path).unwrap();
        for id in [A, B, C] {
            assert_eq!(
                lineage.chain(id),
                vec![A.to_owned(), B.to_owned(), C.to_owned()],
                "chain seen from {id}"
            );
        }
        let middle = lineage.view(B);
        assert!(middle.superseded);
        assert_eq!(middle.superseded_by.unwrap().successor_flow_run_id, C);
        assert_eq!(middle.supersedes.unwrap().flow_run_id, A);
        assert_eq!(middle.current_flow_run_id, C);
        let tip = lineage.view(C);
        assert!(!tip.superseded);
        assert_eq!(tip.current_flow_run_id, C);
    }

    #[test]
    fn a_cycle_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FLOW_LINEAGE_FILE);
        record_supersede(&path, A, B, SupersedeReason::GenerationChange, &pins()).unwrap();
        record_supersede(&path, B, C, SupersedeReason::GenerationChange, &pins()).unwrap();
        let error =
            record_supersede(&path, C, A, SupersedeReason::GenerationChange, &pins()).unwrap_err();
        assert!(matches!(error, FlowLineageError::Conflict(_)), "{error}");
    }

    #[test]
    fn a_run_may_not_supersede_itself() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FLOW_LINEAGE_FILE);
        let error =
            record_supersede(&path, A, A, SupersedeReason::GenerationChange, &pins()).unwrap_err();
        assert!(matches!(error, FlowLineageError::Invalid(_)), "{error}");
        assert!(!path.exists() || std::fs::read_to_string(&path).unwrap().is_empty());
    }

    #[test]
    fn a_non_uuid_run_identifier_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FLOW_LINEAGE_FILE);
        let error = record_supersede(&path, "not-a-uuid", B, SupersedeReason::Operator, &pins())
            .unwrap_err();
        assert!(matches!(error, FlowLineageError::Invalid(_)), "{error}");
    }

    #[test]
    fn a_contradictory_ledger_is_reported_rather_than_silently_indexed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FLOW_LINEAGE_FILE);
        record_supersede(&path, A, B, SupersedeReason::GenerationChange, &pins()).unwrap();
        let smuggled = FlowSupersedeRecord {
            schema_version: FLOW_LINEAGE_SCHEMA_VERSION,
            flow_run_id: A.to_owned(),
            successor_flow_run_id: C.to_owned(),
            reason: SupersedeReason::Operator,
            recorded_at: "2026-08-02T00:00:00.000Z".to_owned(),
            predecessor_script_hash: None,
            predecessor_args_hash: None,
            predecessor_catalog_hash: None,
        };
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&serde_json::to_vec(&smuggled).unwrap())
            .unwrap();
        file.write_all(b"\n").unwrap();
        let error = FlowLineage::read(&path).unwrap_err();
        assert!(
            matches!(error, FlowLineageError::Malformed { .. }),
            "{error}"
        );
    }

    #[test]
    fn every_valid_uuid_rendering_names_one_run() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FLOW_LINEAGE_FILE);
        // The braced, upper-case rendering on the way in.
        let recorded = record_supersede(
            &path,
            "{00000000-0000-4000-8000-0000000000A1}",
            "00000000000040008000000000000B2A".to_owned().as_str(),
            SupersedeReason::GenerationChange,
            &pins(),
        )
        .unwrap();
        assert_eq!(recorded.disposition, SupersedeDisposition::Recorded);
        assert_eq!(recorded.record.flow_run_id, A);
        assert_eq!(
            recorded.record.successor_flow_run_id,
            "00000000-0000-4000-8000-000000000b2a"
        );

        // The hyphenated lower-case rendering the runner actually presents
        // finds it, and so does the upper-case one an operator might paste.
        let lineage = FlowLineage::read(&path).unwrap();
        for rendering in [
            A,
            "00000000-0000-4000-8000-0000000000A1",
            "{00000000-0000-4000-8000-0000000000a1}",
            "000000000000400080000000000000a1",
        ] {
            let view = lineage.view(rendering);
            assert!(view.superseded, "rendering {rendering} lost its rollover");
            assert_eq!(view.flow_run_id, A, "rendering {rendering}");
            assert_eq!(
                view.current_flow_run_id,
                "00000000-0000-4000-8000-000000000b2a"
            );
        }

        // And the retry in yet another rendering is still the same rollover,
        // not a second one against a burned successor.
        let repeated = record_supersede(
            &path,
            "00000000-0000-4000-8000-0000000000A1",
            "00000000-0000-4000-8000-000000000B2A",
            SupersedeReason::GenerationChange,
            &PredecessorPins::default(),
        )
        .unwrap();
        assert_eq!(repeated.disposition, SupersedeDisposition::Reused);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            1,
            "a differently rendered retry must not append a second record"
        );
    }

    #[test]
    fn a_pre_repair_ledger_entry_is_absorbed_on_read() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FLOW_LINEAGE_FILE);
        // Exactly what the merged-but-unrepaired build could write: the
        // caller's own upper-case spelling stored verbatim.
        let legacy = FlowSupersedeRecord {
            schema_version: FLOW_LINEAGE_SCHEMA_VERSION,
            flow_run_id: "00000000-0000-4000-8000-0000000000A1".to_owned(),
            successor_flow_run_id: "00000000-0000-4000-8000-0000000000B2".to_owned(),
            reason: SupersedeReason::GenerationChange,
            recorded_at: "2026-08-02T00:00:00.000Z".to_owned(),
            predecessor_script_hash: None,
            predecessor_args_hash: None,
            predecessor_catalog_hash: None,
        };
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&legacy).unwrap()),
        )
        .unwrap();

        let lineage = FlowLineage::read(&path).unwrap();
        let view = lineage.view(A);
        assert!(view.superseded, "the runner's rendering must still match");
        assert_eq!(view.superseded_by.unwrap().successor_flow_run_id, B);
        assert_eq!(view.chain, vec![A.to_owned(), B.to_owned()]);
        // It is one rollover, so re-recording it is a reuse rather than a
        // conflict against a successor that looks unclaimed.
        assert_eq!(
            record_supersede(&path, A, B, SupersedeReason::GenerationChange, &pins())
                .unwrap()
                .disposition,
            SupersedeDisposition::Reused
        );
    }

    #[test]
    fn an_interrupted_append_never_blocks_an_unrelated_run() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FLOW_LINEAGE_FILE);
        record_supersede(&path, A, B, SupersedeReason::GenerationChange, &pins()).unwrap();
        // A crash, a power loss, or a short write under ENOSPC between
        // `write_all` and `sync_all`.
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"schemaVersion":1,"flowRunId":"00000000-0000-4000-8"#)
            .unwrap();
        drop(file);

        let lineage = FlowLineage::read(&path).expect("a torn tail must not fail the read");
        assert!(lineage.view(A).superseded);
        assert!(!lineage.view(C).superseded);

        // The next write cleans it up rather than appending after the debris.
        record_supersede(&path, B, C, SupersedeReason::ScriptChanged, &pins()).unwrap();
        let repaired = std::fs::read_to_string(&path).unwrap();
        assert_eq!(repaired.lines().count(), 2);
        assert!(repaired.ends_with('\n'));
        assert_eq!(
            FlowLineage::read(&path).unwrap().chain(A),
            vec![A.to_owned(), B.to_owned(), C.to_owned()]
        );
    }

    #[test]
    fn the_ledger_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FLOW_LINEAGE_FILE);
        record_supersede(&path, A, B, SupersedeReason::GenerationChange, &pins()).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn the_ledger_compacts_to_its_record_bound() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FLOW_LINEAGE_FILE);
        let run = |index: u32| format!("00000000-0000-4000-8000-{index:012}");
        // A chain long enough to cross a small bound twice over.
        for index in 0..8_u32 {
            record_supersede_bounded(
                &path,
                &run(index),
                &run(index + 1),
                SupersedeReason::GenerationChange,
                &pins(),
                5,
            )
            .unwrap();
        }
        let lines = std::fs::read_to_string(&path).unwrap();
        assert_eq!(lines.lines().count(), 5, "the bound holds");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "compaction keeps the ledger owner-only"
        );
        let lineage = FlowLineage::read(&path).unwrap();
        // The newest generations survive; the oldest are the ones dropped.
        assert!(lineage.view(&run(7)).superseded);
        assert!(!lineage.view(&run(0)).superseded);
        assert_eq!(lineage.view(&run(7)).current_flow_run_id, run(8));
        assert_eq!(lineage.chain(&run(8)).len(), 6);
    }

    #[test]
    fn records_round_trip_through_their_wire_shape() {
        let record = FlowSupersedeRecord {
            schema_version: FLOW_LINEAGE_SCHEMA_VERSION,
            flow_run_id: A.to_owned(),
            successor_flow_run_id: B.to_owned(),
            reason: SupersedeReason::GenerationChange,
            recorded_at: "2026-08-02T00:00:00.000Z".to_owned(),
            predecessor_script_hash: Some("sha256:aa".to_owned()),
            predecessor_args_hash: None,
            predecessor_catalog_hash: None,
        };
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["reason"], "generation-change");
        assert_eq!(value["successorFlowRunId"], B);
        assert!(value.get("predecessorArgsHash").is_none());
        assert_eq!(
            serde_json::from_value::<FlowSupersedeRecord>(value).unwrap(),
            record
        );
    }
}
