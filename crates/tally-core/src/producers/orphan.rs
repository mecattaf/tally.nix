//! Terminal fate for a forge projection whose producer is gone.
//!
//! Removing a producer block from the effective configuration is a documented
//! operation: a retired campaign's producer is deleted and never returns. The
//! completions admitted under it are already settled and witnessed — only the
//! forge-side projection (the COMPLETED comment, the checkbox flip, the
//! storage-warning receipt) is still owed, and it can never be paid, because
//! resolving the projection needs the producer's own configuration.
//!
//! A projection in that state used to retry forever at a one-minute ceiling.
//! This module gives it a defined end instead: one durable record per orphaned
//! projection under `<state-dir>/producers/gh-orphaned/`, written once,
//! enumerable in a single pass, and cleared the moment the same projection
//! actually settles. The record is a statement about the present, not a
//! memory of removed producers: nothing consults it to decide what to do, so
//! re-adding the producer block simply projects the completion after all.
//!
//! A projection that already reached the forge is *not* orphaned, whatever the
//! configuration says now. The `producers/gh-completed/` and
//! `producers/gh-storage-warnings/` idempotency markers are the durable proof
//! that it did, and every path that can write a record here consults them
//! first. Saying a delivered projection was lost would be wrong in the
//! reassuring direction, and it would be wrong on the strongest claim surface
//! in the tree.

use super::*;

pub const ORPHANED_PROJECTION_SCHEMA_VERSION: u32 = 1;

/// Which post-ack forge projection was orphaned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OrphanedProjectionKind {
    /// The terminal COMPLETED mutation for one task generation.
    Completion,
    /// One storage-budget warning receipt.
    StorageWarning,
}

impl OrphanedProjectionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completion => "completion",
            Self::StorageWarning => "storage-warning",
        }
    }
}

/// One projection that can never be applied, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OrphanedProjection {
    pub schema_version: u32,
    pub kind: OrphanedProjectionKind,
    pub producer: String,
    pub source: String,
    pub item_id: String,
    pub completion_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    /// When this projection was first observed to be orphaned. A repeat
    /// observation keeps the first timestamp rather than rewriting it.
    pub observed_at: String,
    /// The producer error that made the projection terminal, rendered.
    pub detail: String,
}

impl OrphanedProjection {
    /// The identity of the projection, independent of when it was observed.
    #[must_use]
    pub fn marker_key(&self) -> String {
        stable_key(&[
            "gh-orphaned",
            self.kind.as_str(),
            &self.producer,
            &self.source,
            &self.item_id,
            &self.completion_id,
        ])
    }

    fn render(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.task_uuid {
            Some(task_uuid) => write!(formatter, "task {task_uuid}")?,
            None => write!(formatter, "{} receipt", self.kind.as_str())?,
        }
        write!(
            formatter,
            " producer {:?} source {:?} item {:?} completion {:?}",
            self.producer, self.source, self.item_id, self.completion_id
        )?;
        if let Some(verdict) = self.verdict {
            let rendered = serde_json::to_value(verdict)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unknown".to_owned());
            write!(formatter, " verdict {rendered}")?;
        }
        write!(
            formatter,
            " — {} (first seen {})",
            self.detail, self.observed_at
        )
    }
}

/// One file under `producers/gh-orphaned/` that could not be read as a record.
///
/// It is carried rather than raised so that one unusable file cannot hide
/// every usable one, which is the discipline `UnitFactFailures` established in
/// `recovery.rs`: a per-item failure never suppresses the rest of the pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnreadableOrphanRecord {
    pub path: PathBuf,
    pub detail: String,
}

/// One pass over `producers/gh-orphaned/`: what it holds and what it could not
/// read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OrphanedProjectionScan {
    pub records: Vec<OrphanedProjection>,
    pub unreadable: Vec<UnreadableOrphanRecord>,
}

impl OrphanedProjectionScan {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty() && self.unreadable.is_empty()
    }
}

/// Every orphaned projection currently recorded, plus where they live.
///
/// This exists so the condition is reported in one pass instead of one line
/// per projection per minute. The completions themselves are settled; the
/// report says what was lost and what, if anything, an operator may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanedProjections {
    pub scan: OrphanedProjectionScan,
    /// The state root whose `producers/gh-orphaned/` directory holds them.
    pub state_dir: PathBuf,
}

impl std::fmt::Display for OrphanedProjections {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} GitHub projection(s) are orphaned: the producer that owns each one no longer \
             resolves to a configured GitHub producer, so the mutation is terminal and is not \
             retried. Each one was checked against its idempotency marker first, so none of \
             these reached the forge. The task completions themselves are settled and witnessed \
             — only the forge-side projection is lost:",
            self.scan.records.len()
        )?;
        for record in &self.scan.records {
            formatter.write_str("\n  ")?;
            record.render(formatter)?;
        }
        if !self.scan.unreadable.is_empty() {
            write!(
                formatter,
                "\n{} record file(s) in the same directory could not be read and are not counted \
                 above:",
                self.scan.unreadable.len()
            )?;
            for unreadable in &self.scan.unreadable {
                write!(
                    formatter,
                    "\n  {}: {}",
                    unreadable.path.display(),
                    unreadable.detail
                )?;
            }
        }
        write!(
            formatter,
            "\nThis is the documented consequence of retiring a producer and needs no repair. To \
             project them after all, restore the named producer block in the configuration and \
             start the daemon again; each record clears itself when its projection settles. \
             Otherwise they retire with the audit trail they describe: `tally gc` collects them \
             at the producer-marker horizon, by which point the durable rows they refer to have \
             aged out and nothing re-derives them. List them at any time with:\n  tally producer \
             orphaned --state-dir {}",
            self.state_dir.display()
        )
    }
}

/// The inputs of one terminal completion projection, bundled so the call site
/// reads as the one decision it is.
pub struct GhCompletionProjection<'a> {
    pub origin: &'a GhOrigin,
    pub completion_id: &'a str,
    pub task_uuid: Option<&'a str>,
    pub verdict: Verdict,
    pub evidence: Option<Value>,
    pub completion: Option<SemanticCompletion>,
}

/// What became of one post-ack forge projection attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhProjectionOutcome {
    /// The projection reached the forge, had already reached it, or was
    /// declined by the producer's own policy. Nothing is owed.
    Settled {
        /// A record that claimed this projection was orphaned, removed because
        /// it has just been shown false. The attestation chain is append-only,
        /// so the caller answers this by appending a retraction rather than by
        /// pretending the claim was never made.
        retracted: Option<Box<OrphanedProjection>>,
    },
    /// The producer that owns this projection no longer resolves to a
    /// configured GitHub producer, so no retry can ever succeed. The
    /// projection is terminal and recorded.
    Orphaned { record: Box<OrphanedProjection> },
}

pub(super) fn orphaned_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("producers/gh-orphaned")
}

/// Record one orphaned projection. Returns whether this observation wrote it.
pub(super) fn write_orphaned_record(
    state_dir: &Path,
    record: &OrphanedProjection,
) -> Result<bool, ProducerError> {
    let directory = orphaned_dir(state_dir);
    create_dir_durable(&directory)?;
    let path = directory.join(format!("{}.json", record.marker_key()));
    if path_lexists(&path)? {
        return Ok(false);
    }
    write_json_atomic(&path, record)?;
    Ok(true)
}

/// Drop the record for a projection that has just been shown to be settled,
/// returning what it had claimed.
pub(super) fn remove_orphaned_record(
    state_dir: &Path,
    record: &OrphanedProjection,
) -> Result<Option<OrphanedProjection>, ProducerError> {
    let path = orphaned_dir(state_dir).join(format!("{}.json", record.marker_key()));
    // Read before unlinking so the retraction can quote the claim it retracts.
    // A file that will not parse is still removed: the claim it made is false
    // either way, and refusing to delete it would keep the falsehood.
    let claimed = match read_bounded_regular(&path, 64 * 1024) {
        Ok(bytes) => serde_json::from_slice::<OrphanedProjection>(&bytes).ok(),
        Err(_) => None,
    };
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(claimed),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ProducerError::Io { path, source }),
    }
}

/// Every recorded orphaned projection, in a stable order, plus every file the
/// pass could not read.
///
/// This reads the state directory alone: an operator inspecting the condition
/// does not need the configuration that no longer mentions the producer. It
/// fails only when the directory itself cannot be listed — one unusable record
/// is reported beside the usable ones rather than instead of them.
pub fn read_orphaned_projections(
    state_dir: &Path,
) -> Result<OrphanedProjectionScan, ProducerError> {
    let directory = orphaned_dir(state_dir);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(OrphanedProjectionScan::default())
        }
        Err(source) => {
            return Err(ProducerError::Io {
                path: directory,
                source,
            })
        }
    };
    let mut scan = OrphanedProjectionScan::default();
    for entry in entries {
        let entry = entry.map_err(|source| ProducerError::Io {
            path: directory.clone(),
            source,
        })?;
        let path = entry.path();
        // `write_json_atomic` stages under a dot-prefixed temporary name in
        // the same directory; a concurrent write must not become a record.
        if path.extension().is_none_or(|extension| extension != "json")
            || path
                .file_name()
                .is_some_and(|name| name.as_bytes().starts_with(b"."))
        {
            continue;
        }
        match read_orphaned_record(&path) {
            Ok(record) => scan.records.push(record),
            Err(detail) => scan
                .unreadable
                .push(UnreadableOrphanRecord { path, detail }),
        }
    }
    scan.records.sort_by(|left, right| {
        (
            &left.producer,
            &left.source,
            &left.item_id,
            &left.completion_id,
        )
            .cmp(&(
                &right.producer,
                &right.source,
                &right.item_id,
                &right.completion_id,
            ))
    });
    scan.unreadable
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(scan)
}

fn read_orphaned_record(path: &Path) -> Result<OrphanedProjection, String> {
    let bytes = read_bounded_regular(path, 64 * 1024).map_err(|error| error.to_string())?;
    let record: OrphanedProjection =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if record.schema_version != ORPHANED_PROJECTION_SCHEMA_VERSION {
        return Err(format!(
            "record has schema version {}, expected {ORPHANED_PROJECTION_SCHEMA_VERSION}",
            record.schema_version
        ));
    }
    Ok(record)
}
