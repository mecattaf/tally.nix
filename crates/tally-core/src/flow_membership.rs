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
//! only proof chain. What this store owns is a membership question that the
//! witness ledger cannot answer, because a row-less admission appends no
//! witness under the submitting run either.
//!
//! **It is additive, never authoritative-by-itself.** Query membership is the
//! union of this ledger and the original scan, which is what makes the store
//! safe to introduce: a row written by an older binary carries its capsule and
//! is still found by the scan with no record here, and a ledger that is missing
//! or empty degrades exactly to the pre-#380 behaviour rather than to an empty
//! run.
//!
//! **Durability class: canonical.** A row-less admission's membership cannot
//! be rebuilt from another store. The parsed [`FlowMembership`] lookup map is
//! the derived surface replayed from these records.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const FLOW_MEMBERSHIP_SCHEMA_VERSION: u32 = 1;
pub const FLOW_MEMBERSHIP_FILE: &str = "flow-membership.jsonl";

/// Records kept before the ledger is compacted.
///
/// **Re-derived for what this store actually accumulates.** The first draft
/// copied 100,000 from [`crate::flow_lineage`], which records *rollover* events
/// — one per retired generation, rare by construction. Membership is one record
/// per **admitted flow node**: a per-dispatch surface. Sizing a per-dispatch
/// store with a rare-event bound is a named recurring class here.
///
/// What the bound is *not* sized by, any more, is the admission path. Appending
/// is one set lookup, one write, and one fsync, so per-admission cost is **flat
/// in the ledger's size**: 2.13 / 1.91 / 2.16 / 2.02 ms across ledgers of 0,
/// 5,000, 20,000, and 25,000 records (debug profile, ext4, via
/// `membership_admission_cost_sweep`).
///
/// The flatness is the claim, not the constant. Absolute numbers move with host
/// load — an independent measurement of the same code on the same host under
/// different load read 3.27 / 3.33 / 2.89 / 3.39 ms — and at an empty ledger the
/// figure is indistinguishable from the pre-#380 admission path, because at zero
/// records there was never anything to improve. What the repair removed is the
/// *growth*: the same sweep against the first draft read 2.8 ms empty, 17.4 ms
/// at 10,000, 77.5 ms at 50,000, and 977 ms past the bound, where every
/// admission rewrote the whole ledger.
///
/// What the bound *is* sized by is the cost that stayed linear:
///
/// - **The one-time parse**, paid at daemon open and again whenever the ledger
///   changes underneath the cache: ~10 µs/record, so ~200 ms at 20,000 records
///   and ~1 s at 100,000. That lands in the budget #379 is open about, and it is
///   the binding constraint.
/// - **Resident memory.** The parsed index is `BTreeMap<run, BTreeMap<task,
///   record>>` over two UUID strings and a timestamp per record, held for the
///   daemon's lifetime.
///
/// 20,000 records is roughly 390 whole campaigns at the pinned `maxNodes` of
/// 51, a few megabytes on disk, ~200 ms to parse, and the horizon past which a
/// run is old enough that its own rows and witnesses are the thing an operator
/// reads anyway.
///
/// Compaction drops **whole runs** — never part of one, because a half-present
/// run reports a membership count lower than the truth, which is the
/// reassuring-direction lie this store exists to remove. It drops
/// least-recently-touched first and never touches a run holding work that has
/// not completed — running, queued, or paused alike; see
/// [`FlowMembership::eviction_plan`]. A run that is wholly
/// evicted falls back to the row scan, which is what an operator got before this
/// ledger existed and is what the observability chapter documents — with the
/// limit that a row-less admission has nothing to fall back to, which is why
/// eviction has to keep away from live runs rather than merely be tidy about
/// whole ones.
pub const FLOW_MEMBERSHIP_MAX_RECORDS: usize = 20_000;

/// What a compaction compacts *down to*, as a fraction of the bound.
///
/// Compacting to exactly the bound is a trap this store fell into: drop the
/// oldest run, land back on the bound, and the very next append compacts again.
/// With single-node runs that degenerates to a full rewrite per admission —
/// which is precisely the failure the first draft shipped, by a different
/// route. Compacting to 90% of the bound means a rewrite is followed by at least
/// `max_records / 10` ordinary appends, so the amortised cost of compaction is
/// one rewrite per two thousand admissions rather than one per admission.
///
/// `flow_lineage` does not need this because its records arrive rarely enough
/// that even the degenerate case is invisible; a per-dispatch store does.
#[must_use]
pub const fn flow_membership_compact_to(max_records: usize) -> usize {
    max_records - max_records / 10
}

/// How much of the tail is scanned backwards to find the last record boundary.
/// A single record is a few hundred bytes, so this never fails to find one in
/// practice; the full-scan fallback exists so that it cannot be wrong when it
/// does.
const TAIL_SCAN_BYTES: u64 = 64 * 1024;

/// Which admission handed the run this task: the parsed view of
/// [`FlowMembershipRecord::disposition`], which is stored as the raw wire string.
///
/// Open on the read side on purpose. A closed vocabulary that hard-fails on an
/// unknown string would mean a ledger written by a newer daemon crashes an
/// older one's queries on a pin rollback — the #371 failure, imported into a
/// store whose whole job is to keep a window honest. `Unknown` is this binary
/// saying it does not recognise the value, never a replacement for it: the
/// record keeps what was written, so a compaction rewrite cannot flatten a newer
/// daemon's disposition into the literal `"unknown"`.
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
///
/// **Deliberately not `deny_unknown_fields`, and deliberately diverging from
/// [`crate::flow_lineage`] here.** This store's stated purpose is surviving a
/// pin move in *both* directions. Rejecting an unknown field would reintroduce
/// the same #371 failure one level down, and worse: `read` fails the whole
/// ledger, so one field written by a newer daemon would take out every
/// run-scoped query and every flow admission on an older one, not one record.
///
/// **Reading is not enough: the record must survive being written back.**
/// Compaction re-serializes every record it retains, so an older daemon that
/// merely *ignored* a newer daemon's fields would permanently strip them the
/// first time it compacted — while leaving the higher `schemaVersion` label in
/// place, so rolling the pin forward again could not even detect the loss. The
/// residual would not be "an older daemon over-reports a member a newer one
/// retracted", which is visible and investigable; it would be "the retraction is
/// gone". So unknown fields are *captured* in [`Self::extra`] and re-emitted
/// verbatim, and `disposition` is stored as the raw wire string rather than as
/// the parsed enum, whose `Unknown` variant would otherwise write back the
/// literal `"unknown"` and destroy the original value.
///
/// What this daemon cannot do is *honour* a field it does not understand. A
/// future field that narrows membership is preserved through a rollback but not
/// applied during one, so an older daemon over-reports until the pin moves
/// forward again — the tolerable direction, and now genuinely recoverable. A
/// future version that needs the older daemon to refuse outright has to say so
/// in a way this reader already enforces: `validate` accepts a `schemaVersion`
/// at or above its own and refuses one below, so the mechanism is "the older
/// daemon's own floor", not a bump. Bumping alone does not make an old reader
/// refuse, by design — that is what keeps a rollback survivable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowMembershipRecord {
    pub schema_version: u32,
    pub flow_run_id: String,
    pub task_uuid: String,
    /// The disposition exactly as written. Parsed through
    /// [`Self::disposition`]; kept raw so a value this binary does not know
    /// survives a compaction rewrite unchanged.
    pub disposition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_ordinal: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_label: Option<String>,
    pub recorded_at: String,
    /// Every field a newer daemon wrote that this one does not know, captured so
    /// a rewrite re-emits it rather than deleting it.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
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
            disposition: disposition.as_str().to_owned(),
            node_ordinal,
            node_label,
            recorded_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            extra: BTreeMap::new(),
        }
    }

    /// The disposition this binary understands, or
    /// [`MembershipDisposition::Unknown`] for one written by a newer daemon.
    /// The wire value stays in [`Self::disposition`] either way.
    #[must_use]
    pub fn disposition(&self) -> MembershipDisposition {
        MembershipDisposition::from_response(&self.disposition)
    }

    pub fn validate(&self) -> Result<(), String> {
        // Forward-tolerant, backward-strict. A record from a *newer* daemon is
        // read on its known fields for the reason given on the struct: refusing
        // it would turn a pin rollback into a daemon-wide query outage. A record
        // claiming a version this binary predates cannot be reinterpreted
        // safely, because the fields below would mean something else, so it is
        // still a hard failure.
        if self.schema_version < FLOW_MEMBERSHIP_SCHEMA_VERSION {
            return Err(format!(
                "schemaVersion must be at least {FLOW_MEMBERSHIP_SCHEMA_VERSION}"
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

    /// Every run that durably holds this task, ascending.
    ///
    /// The index is keyed run-first because that is the question nearly every
    /// reader asks, so this walks it. It exists for the one reader that asks
    /// the reverse question — a stand-up digest holds task UUIDs and needs the
    /// runs they belong to — and a task's own row cannot answer it: a node one
    /// run created and another was handed carries only the creating run's
    /// orchestration capsule.
    pub fn runs_holding<'a>(&'a self, task_uuid: &'a str) -> impl Iterator<Item = &'a str> {
        self.by_run
            .iter()
            .filter(move |(_, tasks)| tasks.contains_key(task_uuid))
            .map(|(flow_run_id, _)| flow_run_id.as_str())
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

    /// Build an index from records already known to be valid.
    fn from_records(records: Vec<FlowMembershipRecord>) -> Self {
        let mut membership = Self::default();
        for record in records {
            membership.insert(record);
        }
        membership
    }

    /// Which whole runs to evict to bring the index down to `target`.
    ///
    /// **Least-recently-*touched* first, never least-recently-born.** Ordering
    /// by a run's earliest record — which is what the first repair did — makes
    /// the eviction key anti-correlated with liveness: the longer a campaign
    /// runs, the older its first record and the sooner it is evicted, so the
    /// policy preferentially deletes the membership of exactly the runs an
    /// operator is still watching. Keying on the run's *latest* record means a
    /// run that is still admitting nodes keeps moving to the back of the queue.
    ///
    /// `protected` is never evicted whatever its age. It carries two things the
    /// timestamps cannot: the run being written right now (a compaction that
    /// evicts its own caller's run would report a durable membership that is not
    /// there), and runs holding a task that has not completed — running, queued,
    /// or paused. The second is what covers a run that admitted everything early
    /// and has been quietly working ever since, which LRU alone would eventually
    /// evict. It is deliberately wider than "executing": a paused queue is a
    /// state an operator can hold indefinitely, and membership for work they are
    /// about to resume is exactly what must not be thrown away.
    ///
    /// When it covers *everything* above the target, nothing is evictable and
    /// the caller appends anyway rather than deleting membership in use — see
    /// [`MembershipWrite::AppendedOverTarget`].
    ///
    /// Returns the run IDs to drop, cheaply and without cloning any record, so
    /// the caller can skip the expensive rewrite when nothing is evictable.
    fn eviction_plan(&self, target: usize, protected: &BTreeSet<String>) -> Vec<String> {
        if self.records <= target {
            return Vec::new();
        }
        let mut candidates = self
            .by_run
            .iter()
            .filter(|(run, _)| !protected.contains(*run))
            .map(|(run, tasks)| (run_last_touched(tasks), run.as_str(), tasks.len()))
            .collect::<Vec<_>>();
        candidates.sort();
        let mut total = self.records;
        let mut plan = Vec::new();
        for (_, run, held) in candidates {
            if total <= target {
                break;
            }
            total -= held;
            plan.push(run.to_owned());
        }
        plan
    }

    /// Runs that compaction must not evict: the one being written, and every
    /// run holding a task that is still executing.
    ///
    /// The liveness half is derived from the membership index itself rather than
    /// from durable rows, and that is the point. A run whose nodes were all
    /// `attached` to another run's tasks owns no rows at all — it is precisely
    /// the run W-316 is about — so a rows-based liveness check would be blind to
    /// exactly the runs this ledger exists to keep visible. Joining the index
    /// against live task UUIDs is not.
    fn protected_runs(&self, writing: &str, live_tasks: &BTreeSet<String>) -> BTreeSet<String> {
        let mut protected = BTreeSet::from([writing.to_owned()]);
        if live_tasks.is_empty() {
            return protected;
        }
        for (run, tasks) in &self.by_run {
            if tasks.keys().any(|task| live_tasks.contains(task)) {
                protected.insert(run.clone());
            }
        }
        protected
    }

    /// Consume the index into the records that survive dropping `evicted`.
    fn without_runs(mut self, evicted: &[String]) -> Vec<FlowMembershipRecord> {
        for run in evicted {
            self.by_run.remove(run);
        }
        self.by_run
            .into_values()
            .flat_map(BTreeMap::into_values)
            .collect()
    }
}

/// When a run was last handed a node: the ordering key for eviction.
fn run_last_touched(tasks: &BTreeMap<String, FlowMembershipRecord>) -> (&str, &str) {
    tasks
        .values()
        .map(|record| (record.recorded_at.as_str(), record.flow_run_id.as_str()))
        .max()
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
    /// Appended, but the ledger is over its target and nothing could be
    /// evicted: every run above the bound holds work that has not completed, or
    /// is the run being written. The ledger grows by one record per admission
    /// rather than deleting membership that is still in use, and compacts on the
    /// next admission after that work finishes. The caller reports it, because
    /// this module does not log — and the report has to say *not completed*
    /// rather than *executing*, since the commonest way to get here is a paused
    /// queue, which does not drain unattended.
    AppendedOverTarget,
    /// The run already durably held this task; nothing was written.
    AlreadyHeld,
}

/// Prove the ledger is readable and appendable, and return the parsed index.
///
/// Called *before* the enqueue kernel commits anything, because the alternative
/// is telling a caller its admission failed while that admission's work is
/// already dispatching. Both faults this catches are the realistic ones: a
/// complete-but-unusable record (the state `repair-flow-membership-ledger`
/// exists for) and a ledger that cannot be opened for append at all. What it
/// cannot catch is a fault that arrives *between* here and the append — for that
/// residue see the degraded acknowledgement in the daemon.
///
/// The read is not extra work: the same index is what the write path dedupes
/// against and what the caller then holds.
pub fn preflight(path: &Path) -> Result<FlowMembership, FlowMembershipError> {
    let membership = FlowMembership::read(path)?;
    probe_appendable(path)?;
    Ok(membership)
}

/// The half of [`preflight`] that does not parse: can this ledger be appended to?
///
/// Split out because a caller whose parsed index is already current must not pay
/// a full re-parse to answer an appendability question. Re-reading on every
/// admission is exactly the linear-in-the-ledger cost the cache exists to avoid,
/// and it does not stop being that cost because it is spelled `preflight`.
pub fn probe_appendable(path: &Path) -> Result<(), FlowMembershipError> {
    let parent = durable_parent(path);
    std::fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    Ok(())
}

/// Append one membership fact, fsync it, and return the index that results.
///
/// Takes the caller's index **by value and hands it back** so the caller's cache
/// is always the index the file now holds. The first draft returned only a
/// disposition and left the caller to patch its own cache, which meant a
/// compaction shrank the file but not the cache: the cache then stayed
/// permanently over the bound and every later admission rewrote the whole
/// ledger. Ownership makes that class unrepresentable.
///
/// Idempotent against the index: a run handed the same task twice writes once.
/// The check is the caller's parsed index rather than a re-read, and on the
/// happy path nothing is cloned — one lookup, one append, one fsync, whatever
/// the ledger's size. A duplicate written by a racing second writer is harmless;
/// the read path is set-valued and collapses it.
///
/// `live_tasks` are task UUIDs still executing. Their runs are exempt from
/// compaction; see [`FlowMembership::eviction_plan`].
pub fn record_membership(
    path: &Path,
    record: &FlowMembershipRecord,
    held: FlowMembership,
    live_tasks: &BTreeSet<String>,
) -> Result<(MembershipWrite, FlowMembership), FlowMembershipError> {
    record_membership_bounded(path, record, held, live_tasks, FLOW_MEMBERSHIP_MAX_RECORDS)
}

fn record_membership_bounded(
    path: &Path,
    record: &FlowMembershipRecord,
    mut held: FlowMembership,
    live_tasks: &BTreeSet<String>,
    max_records: usize,
) -> Result<(MembershipWrite, FlowMembership), FlowMembershipError> {
    record.validate().map_err(FlowMembershipError::Invalid)?;
    if held.contains(&record.flow_run_id, &record.task_uuid) {
        return Ok((MembershipWrite::AlreadyHeld, held));
    }
    let mut over_target = false;
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

    held.insert(record.clone());
    // Counted, not cloned. The bound is a comparison on a `usize`; an earlier
    // draft cloned the whole index to answer it, on every admission, which is
    // what made admission linear in the ledger below the bound as well as above.
    if held.record_count() > max_records {
        // The protected set is computed here rather than per append because
        // this is the only place it is consulted, and deriving it is linear in
        // the index — affordable once per compaction, not once per admission.
        let protected = held.protected_runs(&record.flow_run_id, live_tasks);
        let plan = held.eviction_plan(flow_membership_compact_to(max_records), &protected);
        if !plan.is_empty() {
            let kept = held.without_runs(&plan);
            rewrite_compacted(path, parent, &kept)?;
            let compacted = FlowMembership::from_records(kept);
            // The record was inserted before the plan was made and its run is
            // in `protected`, so this cannot fire — which is exactly why it is
            // asserted rather than assumed. Reporting `Appended` for a record
            // the ledger does not hold is W-316 rebuilt inside its own fix, and
            // silent; the caller must land on the degraded path instead.
            if !compacted.contains(&record.flow_run_id, &record.task_uuid) {
                return Err(FlowMembershipError::Invalid(format!(
                    "compaction dropped the membership record it was asked to write \
                     (run {}, task {}); the ledger was rewritten but this admission's \
                     membership is not durable",
                    record.flow_run_id, record.task_uuid
                )));
            }
            return Ok((MembershipWrite::Appended, compacted));
        }
        // Nothing is evictable: every run over the bound is either live or the
        // one being written. The ledger is allowed to exceed its target rather
        // than delete membership that is still in use — and, critically, it
        // falls through to an ordinary append rather than rewriting the whole
        // file on this and every subsequent admission.
        over_target = true;
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
    Ok((
        if over_target {
            MembershipWrite::AppendedOverTarget
        } else {
            MembershipWrite::Appended
        },
        held,
    ))
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

/// Replace the ledger with the retained set, atomically.
///
/// Write-and-rename, exactly as [`crate::flow_lineage`] does, and for the reason
/// that store states: a reader observes either the whole old file or the whole
/// new one, never a half-written ledger. The first draft truncated in place and
/// reasoned only about lock ownership, which is the wrong axis — the danger is
/// not a concurrent reader racing the lock but a crash mid-rewrite, which leaves
/// a short file that the read path accepts as a *smaller, valid* membership. A
/// silently smaller run set is precisely the reassuring-direction lie this
/// module's own doc comment says the store exists to remove, so the write path
/// must not be able to manufacture it.
fn rewrite_compacted(
    path: &Path,
    parent: &Path,
    kept: &[FlowMembershipRecord],
) -> Result<(), FlowMembershipError> {
    let temporary = path.with_extension("jsonl.compact");
    let mut bytes = Vec::new();
    for record in kept {
        serde_json::to_writer(&mut bytes, record)
            .map_err(|error| FlowMembershipError::Invalid(error.to_string()))?;
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
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(parent, source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// No task is executing, so compaction is free to evict by age alone.
    fn no_live() -> BTreeSet<String> {
        BTreeSet::new()
    }

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
    fn appending_is_idempotent_and_hands_back_the_index_the_file_now_holds() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        let first = record("run-a", "task-1", MembershipDisposition::Attached);
        let (write, held) =
            record_membership(&path, &first, FlowMembership::default(), &no_live()).unwrap();
        assert_eq!(write, MembershipWrite::Appended);
        assert!(held.contains("run-a", "task-1"));
        let (write, held) = record_membership(&path, &first, held, &no_live()).unwrap();
        assert_eq!(write, MembershipWrite::AlreadyHeld);
        assert_eq!(held.record_count(), 1);
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
            held = record_membership(&path, &entry, held, &no_live())
                .unwrap()
                .1;
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
        record_membership(&path, &first, FlowMembership::default(), &no_live()).unwrap();
        let mut raw = std::fs::read(&path).unwrap();
        raw.extend_from_slice(br#"{"schemaVersion":1,"flowRunId":"run-a","taskUu"#);
        std::fs::write(&path, &raw).unwrap();

        let after_tear = FlowMembership::read(&path).unwrap();
        assert_eq!(after_tear.record_count(), 1);

        let second = record("run-a", "task-2", MembershipDisposition::Attached);
        record_membership(&path, &second, after_tear, &no_live()).unwrap();
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
                .disposition(),
            MembershipDisposition::Unknown,
            "an unrecognised disposition parses as Unknown..."
        );
        assert_eq!(
            membership
                .record(
                    "7c2f6f0e-0000-4000-8000-000000000002",
                    "7c2f6f0e-0000-4000-8000-0000000000a2"
                )
                .unwrap()
                .disposition,
            "teleported",
            "...while the record keeps what was actually written"
        );
        assert_eq!(
            membership.node_ordinal(
                "7c2f6f0e-0000-4000-8000-000000000001",
                "7c2f6f0e-0000-4000-8000-0000000000a1"
            ),
            Some(0)
        );
    }

    /// A ledger written by a *newer* daemon must not take an older one's
    /// queries out. Unknown fields are ignored and a higher `schemaVersion` is
    /// read on the fields this binary understands; only a version this binary
    /// predates is refused, because then these field names mean something else.
    #[test]
    fn a_newer_ledger_reads_on_the_fields_this_binary_understands() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        std::fs::write(
            &path,
            concat!(
                r#"{"schemaVersion":2,"flowRunId":"run-future","taskUuid":"task-1","#,
                r#""disposition":"attached","nodeOrdinal":4,"#,
                r#""retractedBy":"some-future-field","supersedes":["x"],"#,
                r#""recordedAt":"2026-08-04T00:00:00.000Z"}"#,
                "\n",
            ),
        )
        .unwrap();
        let membership = FlowMembership::read(&path).unwrap();
        assert_eq!(membership.record_count(), 1);
        assert!(membership.contains("run-future", "task-1"));
        assert_eq!(membership.node_ordinal("run-future", "task-1"), Some(4));

        // A version below this binary's is still a hard failure.
        std::fs::write(
            &path,
            concat!(
                r#"{"schemaVersion":0,"flowRunId":"run-old","taskUuid":"task-1","#,
                r#""disposition":"attached","recordedAt":"2026-08-04T00:00:00.000Z"}"#,
                "\n",
            ),
        )
        .unwrap();
        assert!(matches!(
            FlowMembership::read(&path).unwrap_err(),
            FlowMembershipError::Malformed { line: 1, .. }
        ));
    }

    #[test]
    fn compaction_drops_whole_runs_least_recently_touched_first_and_never_half_a_run() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        let mut held = FlowMembership::default();
        // Ten runs of two nodes each, written oldest run first.
        let runs = (0..10)
            .map(|index| format!("run-{index:02}"))
            .collect::<Vec<_>>();
        for (index, run) in runs.iter().enumerate() {
            for node in 0..2 {
                let mut entry = record(
                    run,
                    &format!("{run}-task-{node}"),
                    MembershipDisposition::Created,
                );
                entry.recorded_at = format!("2026-08-04T00:00:{:02}.000Z", index * 2 + node);
                held = record_membership_bounded(&path, &entry, held, &no_live(), 20)
                    .unwrap()
                    .1;
            }
        }
        assert_eq!(FlowMembership::read(&path).unwrap().record_count(), 20);

        // The twenty-first record crosses the bound. Compaction goes to the
        // low-water mark (18), so two whole two-node runs go: 21 - 2 = 19 is
        // still above it, 19 - 2 = 17 is not.
        let mut overflow = record("run-10", "run-10-task-0", MembershipDisposition::Created);
        overflow.recorded_at = "2026-08-04T00:00:30.000Z".to_owned();
        held = record_membership_bounded(&path, &overflow, held, &no_live(), 20)
            .unwrap()
            .1;

        let compacted = FlowMembership::read(&path).unwrap();
        assert_eq!(compacted.tasks("run-00").count(), 0, "oldest run is gone");
        assert_eq!(compacted.tasks("run-01").count(), 0, "and so is the next");
        assert_eq!(
            compacted.tasks("run-02").count(),
            2,
            "every surviving run survives whole"
        );
        assert_eq!(compacted.tasks("run-10").count(), 1);
        assert_eq!(compacted.record_count(), 17);
        assert_eq!(
            held, compacted,
            "the index handed back must be the one the file holds -- a cache that \
             kept the dropped run would stay over the bound and rewrite forever"
        );
    }

    /// The regression behind HIGH-2: compacting to exactly the bound means the
    /// next append compacts again, forever. The low-water mark is what makes a
    /// rewrite rare, and "rare" has to be asserted, not asserted-about.
    #[test]
    fn a_compaction_is_followed_by_ordinary_appends_rather_than_more_compactions() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        let mut held = FlowMembership::default();
        // Single-node runs: the degenerate case, where dropping the oldest run
        // recovers exactly one record, so compacting to the bound itself would
        // rewrite on every subsequent append.
        let append = |held: FlowMembership, index: usize| {
            let mut entry = record(
                &format!("run-{index:04}"),
                "task-0",
                MembershipDisposition::Created,
            );
            entry.recorded_at = format!("2026-08-04T{:02}:{:02}:00.000Z", index / 60, index % 60);
            record_membership_bounded(&path, &entry, held, &no_live(), 100)
                .unwrap()
                .1
        };
        for index in 0..100 {
            held = append(held, index);
        }
        assert_eq!(held.record_count(), 100);

        // Watch the inode across the next ten appends. A rewrite renames a new
        // file into place, so the inode changes once and only once: the first
        // append compacts to the 90-record low-water mark, and the nine after it
        // are ordinary appends.
        let inode = |path: &Path| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(path).unwrap().ino()
        };
        let mut inodes = vec![inode(&path)];
        for index in 100..110 {
            held = append(held, index);
            inodes.push(inode(&path));
        }
        inodes.dedup();
        assert_eq!(
            inodes.len(),
            2,
            "exactly one rewrite in ten appends past the bound; \
             compacting to the bound itself would rewrite on every one"
        );
        assert_eq!(held.record_count(), 99);
        assert_eq!(held, FlowMembership::read(&path).unwrap());
    }

    /// The eviction key must track when a run was **last** handed a node, not
    /// when it was first. Keying on the earliest record makes the key
    /// anti-correlated with liveness — the longer a campaign runs, the older its
    /// first record — so the policy deletes the membership of exactly the runs
    /// an operator is still watching.
    #[test]
    fn a_run_that_keeps_admitting_outlives_one_that_stopped_however_old_it_is() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        let mut held = FlowMembership::default();
        let write = |held: FlowMembership, run: &str, task: &str, at: &str| {
            let mut entry = record(run, task, MembershipDisposition::Created);
            entry.recorded_at = at.to_owned();
            record_membership_bounded(&path, &entry, held, &no_live(), 4)
                .unwrap()
                .1
        };

        // `long-lived` was born first and is still admitting; `stopped` was born
        // later and has been quiet since. Under a birth-ordered key the
        // long-lived run is evicted first, which is precisely backwards.
        held = write(held, "long-lived", "task-0", "2026-08-01T00:00:00.000Z");
        held = write(held, "stopped", "task-0", "2026-08-02T00:00:00.000Z");
        held = write(held, "stopped", "task-1", "2026-08-02T00:00:01.000Z");
        held = write(held, "long-lived", "task-1", "2026-08-03T00:00:00.000Z");
        assert_eq!(held.record_count(), 4);

        held = write(held, "long-lived", "task-2", "2026-08-04T00:00:00.000Z");
        assert_eq!(
            held.tasks("long-lived").collect::<Vec<_>>(),
            vec!["task-0", "task-1", "task-2"],
            "the run still admitting nodes must survive whole"
        );
        assert_eq!(
            held.tasks("stopped").count(),
            0,
            "the quiet run is the one that ages out"
        );
        assert_eq!(held, FlowMembership::read(&path).unwrap());
    }

    /// A compaction must never evict the run it is being asked to write for,
    /// and must never report `Appended` for a record the ledger does not hold.
    #[test]
    fn a_write_is_never_evicted_by_its_own_compaction() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        let mut held = FlowMembership::default();
        // The writing run is the oldest thing in the ledger by every key: born
        // first, last touched first. Nothing but the write-protects-itself rule
        // keeps it alive.
        for (index, (run, task)) in [
            ("writer", "task-0"),
            ("writer", "task-1"),
            ("other-a", "task-0"),
            ("other-b", "task-0"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut entry = record(run, task, MembershipDisposition::Attached);
            entry.recorded_at = format!("2026-08-0{}T00:00:00.000Z", index + 1);
            held = record_membership_bounded(&path, &entry, held, &no_live(), 4)
                .unwrap()
                .1;
        }

        let mut fresh = record("writer", "task-2", MembershipDisposition::Attached);
        fresh.recorded_at = "2026-08-01T00:00:00.500Z".to_owned();
        let (write, held) = record_membership_bounded(&path, &fresh, held, &no_live(), 4).unwrap();
        assert_eq!(write, MembershipWrite::Appended);
        assert!(
            held.contains("writer", "task-2"),
            "the record the caller was told is durable must be in the index"
        );
        assert!(
            FlowMembership::read(&path)
                .unwrap()
                .contains("writer", "task-2"),
            "...and in the ledger on disk"
        );
        assert_eq!(
            held.tasks("writer").collect::<Vec<_>>(),
            vec!["task-0", "task-1", "task-2"],
            "the writing run survives whole, not half"
        );
    }

    /// A run holding executing work is never evicted, however long it has been
    /// since it last admitted anything. This is the case LRU alone cannot
    /// cover: a campaign that admitted every node up front and has been running
    /// ever since.
    #[test]
    fn a_run_holding_a_live_task_is_never_evicted() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        let live = BTreeSet::from(["task-still-running".to_owned()]);
        let mut held = FlowMembership::default();
        let write = |held: FlowMembership, run: &str, task: &str, at: &str| {
            let mut entry = record(run, task, MembershipDisposition::Attached);
            entry.recorded_at = at.to_owned();
            record_membership_bounded(&path, &entry, held, &live, 4)
                .unwrap()
                .1
        };

        // `ancient` is the least-recently-touched run by a wide margin, but the
        // task it holds is still executing.
        held = write(
            held,
            "ancient",
            "task-still-running",
            "2019-01-01T00:00:00.000Z",
        );
        held = write(held, "recent-a", "task-0", "2026-08-02T00:00:00.000Z");
        held = write(held, "recent-b", "task-0", "2026-08-03T00:00:00.000Z");
        held = write(held, "recent-c", "task-0", "2026-08-04T00:00:00.000Z");
        held = write(held, "recent-d", "task-0", "2026-08-05T00:00:00.000Z");

        assert!(
            held.contains("ancient", "task-still-running"),
            "a run whose work is still executing must not be evicted: {:?}",
            held.tasks("ancient").collect::<Vec<_>>()
        );
        assert_eq!(
            held.tasks("recent-a").count(),
            0,
            "the oldest *idle* run is the one that goes instead"
        );
        assert_eq!(held, FlowMembership::read(&path).unwrap());
    }

    /// When *everything* above the target is protected there is nothing to
    /// evict, and the ledger grows rather than deleting membership that is still
    /// in use. The contract has three parts and all three matter:
    ///
    /// 1. the record is still written — the admission is not failed;
    /// 2. `AppendedOverTarget` is reported, so the daemon can say so on its
    ///    journal (this module does not log);
    /// 3. **no rewrite happens** — the file is appended to, not re-serialized.
    ///    Rewriting the whole ledger on every admission because compaction
    ///    cannot make progress would be round 1's HIGH-2 by a third route.
    ///
    /// The reachable operator state for this is `queue pause --all`: every job
    /// is `Paused`, which counts as not-completed, so every run is protected. It
    /// is deliberately not `Running` here — protecting paused work is the point.
    #[test]
    fn nothing_evictable_grows_the_ledger_and_says_so_instead_of_rewriting_it() {
        use std::os::unix::fs::MetadataExt;
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        // Every task in the ledger is held by a job that has not completed.
        let live = (0..8)
            .map(|index| format!("paused-task-{index}"))
            .collect::<BTreeSet<_>>();
        let mut held = FlowMembership::default();
        for index in 0..4 {
            let mut entry = record(
                &format!("run-{index}"),
                &format!("paused-task-{index}"),
                MembershipDisposition::Created,
            );
            entry.recorded_at = format!("2026-08-0{}T00:00:00.000Z", index + 1);
            held = record_membership_bounded(&path, &entry, held, &live, 4)
                .unwrap()
                .1;
        }
        assert_eq!(held.record_count(), 4);
        let inode_before = std::fs::metadata(&path).unwrap().ino();

        // The fifth crosses the bound with nothing evictable.
        let mut over = record("run-4", "paused-task-4", MembershipDisposition::Attached);
        over.recorded_at = "2026-08-05T00:00:00.000Z".to_owned();
        let (write, held) = record_membership_bounded(&path, &over, held, &live, 4).unwrap();

        assert_eq!(
            write,
            MembershipWrite::AppendedOverTarget,
            "the caller must be able to tell its operator why the ledger is growing"
        );
        assert!(
            held.contains("run-4", "paused-task-4"),
            "the admission's membership must still be durable"
        );
        assert_eq!(held.record_count(), 5, "nothing may be evicted");
        assert_eq!(held, FlowMembership::read(&path).unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().ino(),
            inode_before,
            "an over-target append must append, not rewrite: rewriting on every \
             admission is the failure the low-water mark exists to prevent"
        );

        // ...and once the work completes, the very next admission compacts.
        let mut after = record("run-5", "task-fresh", MembershipDisposition::Created);
        after.recorded_at = "2026-08-06T00:00:00.000Z".to_owned();
        let (write, held) = record_membership_bounded(&path, &after, held, &no_live(), 4).unwrap();
        assert_eq!(write, MembershipWrite::Appended);
        assert!(
            held.record_count() <= flow_membership_compact_to(4) + 1,
            "the ledger must self-heal on the next admission after the work finishes: {}",
            held.record_count()
        );
        assert!(held.contains("run-5", "task-fresh"));
    }

    /// Reading a newer daemon's record is not enough: it has to survive being
    /// written back. Compaction re-serializes everything it retains, so an older
    /// daemon that merely ignored unknown fields would delete them on the first
    /// compaction — and leave the higher `schemaVersion` in place, so the loss
    /// would be undetectable after rolling the pin forward again.
    #[test]
    fn compaction_re_emits_a_newer_daemons_fields_and_disposition_verbatim() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);
        let future = concat!(
            r#"{"schemaVersion":2,"flowRunId":"run-future","taskUuid":"task-f","#,
            r#""disposition":"teleported","nodeOrdinal":4,"retractedBy":"run-newer","#,
            r#""recordedAt":"2026-08-09T00:00:00.000Z"}"#,
            "\n",
        );
        std::fs::write(&path, future).unwrap();
        let mut held = FlowMembership::read(&path).unwrap();
        assert_eq!(held.record_count(), 1);

        // Force a compaction with the future record retained (it is the most
        // recently touched run, so it survives on merit).
        for index in 0..5 {
            let mut entry = record(
                &format!("run-{index}"),
                "task-0",
                MembershipDisposition::Created,
            );
            entry.recorded_at = format!("2026-08-0{}T00:00:00.000Z", index + 1);
            held = record_membership_bounded(&path, &entry, held, &no_live(), 4)
                .unwrap()
                .1;
        }
        let rewritten = std::fs::read_to_string(&path).unwrap();
        assert!(
            rewritten.lines().count() < 6,
            "this test is only meaningful if a compaction happened: {rewritten}"
        );

        let survivor = FlowMembership::read(&path)
            .unwrap()
            .record("run-future", "task-f")
            .cloned()
            .expect("the newest run must survive compaction");
        assert_eq!(
            survivor.disposition, "teleported",
            "a disposition this binary does not know must be written back verbatim, \
             not flattened to \"unknown\""
        );
        assert_eq!(
            survivor.extra.get("retractedBy"),
            Some(&Value::String("run-newer".to_owned())),
            "an unknown field must be re-emitted, not deleted: {survivor:?}"
        );
        assert_eq!(survivor.schema_version, 2);
        assert_eq!(survivor.node_ordinal, Some(4));
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
            FlowMembership::default(),
            &no_live(),
        )
        .unwrap_err();
        assert!(matches!(error, FlowMembershipError::Io { .. }), "{error}");
        // And the same fault is visible to `preflight`, which is what lets the
        // admission path refuse before the kernel commits anything.
        assert!(matches!(
            preflight(&path).unwrap_err(),
            FlowMembershipError::Io { .. }
        ));
    }

    #[test]
    fn preflight_sees_the_faults_that_would_otherwise_surface_after_the_commit() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(FLOW_MEMBERSHIP_FILE);

        // A ledger that does not exist yet is usable, and preflight returns the
        // index the write path then dedupes against.
        assert!(preflight(&path).unwrap().is_empty());

        let first = record("run-a", "task-1", MembershipDisposition::Created);
        record_membership(&path, &first, FlowMembership::default(), &no_live()).unwrap();
        assert_eq!(preflight(&path).unwrap().record_count(), 1);

        // One malformed complete line -- the state the runbook exists for, and
        // the realistic trigger -- is caught before any admission commits.
        let mut raw = std::fs::read_to_string(&path).unwrap();
        raw.push_str(
            "{\"schemaVersion\":1,\"flowRunId\":\"run-a\",\"taskUuid\":\"task-2\",\
             \"disposition\":\"attached\",\"recordedAt\":\"nope\"}\n",
        );
        std::fs::write(&path, raw).unwrap();
        assert!(matches!(
            preflight(&path).unwrap_err(),
            FlowMembershipError::Malformed { line: 2, .. }
        ));
    }
}
