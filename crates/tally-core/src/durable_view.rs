//! What disk knows about a flow run, with no daemon in the loop.
//!
//! Every observability read normally goes through the daemon, which holds the
//! reconciled task table in memory. That is the right default and it has one
//! failure mode: when the daemon stops answering, observability stops with it —
//! exactly when an operator is diagnosing the stall (#431). The durable stores
//! the daemon reconstructs that table from at startup are still on disk and
//! still answer most of the question, so this module reconstructs the same
//! projection from the same inputs, without an RPC.
//!
//! **This is a strictly weaker view and it says so.** Three things are knowable
//! only from the live daemon and are absent here by construction:
//!
//! - **In-flight state.** A row the daemon is running right now has no terminal
//!   witness yet, so it reads as pending. Systemd unit facts, which is how
//!   startup recovery tells "running" from "pending", are the daemon's to
//!   collect; this view passes none and therefore claims none.
//! - **Freshness.** Nothing here is a snapshot at one instant. Files are read in
//!   sequence while a daemon may still be writing them, so the view can be stale
//!   the moment it is rendered, and its caller must label it as such.
//! - **Post-ack enrichment not yet flushed.** A capture the daemon scraped but
//!   has not written to the attestation ledger is not on disk to be read.
//!
//! Everything it does say is read-only. It never opens a durable store for
//! write, never creates one, never repairs a torn tail, and never takes a lock
//! a live daemon wants: a diagnostic must not be able to damage the thing it is
//! diagnosing. That is a tested claim, not a habit —
//! `a_durable_read_creates_nothing_anywhere_under_the_state_or_data_dir`
//! asserts the whole tree before and after, so a store this view later learns
//! to read is covered without anyone remembering to extend it.
//!
//! **Durability class: derived rebuild.** The complete typed input declaration
//! is [`crate::durability::DURABLE_RUN_VIEW_INPUTS`]; this module never persists
//! the resulting [`DurableRunView`].

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::daemon::DaemonPaths;
use crate::executor::{read_capture_excerpt, ExecutionIdentity, Executor};
use crate::flow_lineage::FlowLineage;
use crate::flow_membership::FlowMembership;
use crate::history::{
    LifecycleRecord, LifecycleSnapshot, RetentionMetadata, LIFECYCLE_FILE,
    LIFECYCLE_RETENTION_FILE, LIFECYCLE_RETENTION_POLICY,
};
use crate::query::RowStatus;
use crate::query_v2::{
    apply_campaign_run_supersession, apply_reader_state_to_run, apply_run_lineage, query_run,
    ObservabilityError, RowDetailFact, RunView,
};
use crate::reader_state::{reader_state_path, ReaderState};
use crate::recovery::collect_durable_recovery_facts;
use crate::usage_rollup::AttestationEvidence;
use crate::witness::{read_verified_attestations, LaborClass, Verdict, WitnessRecord};

#[derive(Debug, Error)]
pub enum DurableViewError {
    #[error("durable enqueue events and witness ledger cannot be read: {0}")]
    Facts(#[from] crate::recovery::RecoveryError),
    #[error("lifecycle history at {path} cannot be read: {source}")]
    Lifecycle {
        path: String,
        source: std::io::Error,
    },
    #[error("durable flow membership cannot be read: {0}")]
    Membership(#[from] crate::flow_membership::FlowMembershipError),
    #[error(transparent)]
    Projection(#[from] ObservabilityError),
}

/// A run view rendered from durable state, with the caveats that make it
/// readable as one.
#[derive(Debug, Clone, PartialEq)]
pub struct DurableRunView {
    pub view: RunView,
    /// Why this view may disagree with a live one. Rendered beside the view so
    /// no caller can print it as if it were live.
    pub caveats: Vec<String>,
}

/// The advertised staleness caveat, spelled once so the CLI and this module
/// cannot drift apart.
pub const DURABLE_VIEW_CAVEAT: &str =
    "durable-state view: read from disk with no live RPC, so it may be stale and shows no in-flight execution state";

/// Project one flow run from the durable stores under `paths`.
///
/// `executor` is optional and buys exactly one thing: the retained capture
/// paths a failing task's stderr was written to, which is what the live
/// `query.run` attaches and is the pointer an operator follows next. Pass
/// `None` and the failures come back without one rather than with a guess.
pub fn durable_run_view(
    paths: &DaemonPaths,
    flow_run: &str,
    executor: Option<&Executor>,
    now: DateTime<Utc>,
) -> Result<DurableRunView, DurableViewError> {
    let durable = collect_durable_recovery_facts(&paths.events_dir(), &paths.witness_path())?;
    // Row state is derived here rather than by running the recovery planner.
    // The planner answers a different question — what the daemon should *do*
    // about each row — and it needs the machine's unit facts to answer it,
    // which is precisely the input a client does not have. What a durable
    // reader can state is what the ledger recorded, so that is all it states:
    // a task with a terminal witness is terminal, and its verdict decides
    // whether that is completion or cancellation, exactly as recovery reads
    // the same record. Everything else is pending as far as disk knows, which
    // includes every row the daemon is running right now.
    let mut terminal_by_task: BTreeMap<&str, &WitnessRecord> = BTreeMap::new();
    for record in durable.witness() {
        if let Some(task_uuid) = record.task_uuid.as_deref() {
            terminal_by_task
                .entry(task_uuid)
                .and_modify(|existing| {
                    if record.seq > existing.seq {
                        *existing = record;
                    }
                })
                .or_insert(record);
        }
    }
    let details = durable
        .events()
        .iter()
        .map(|event| {
            let mut row = event.row.clone();
            let terminal = terminal_by_task.get(row.uuid.to_string().as_str()).copied();
            let (status, labor_class) = match terminal {
                Some(record) => {
                    // The attempt the ledger recorded, not the one the row was
                    // admitted with: a retried row's durable seed still carries
                    // the first attempt until the daemon rewrites it.
                    row.attempt = record.attempt;
                    row.lease_epoch = record.lease_epoch;
                    let status = if record.verdict == Verdict::Cancelled {
                        RowStatus::Deleted
                    } else {
                        RowStatus::Completed
                    };
                    (status, record.labor_class)
                }
                None => (RowStatus::Pending, LaborClass::Fresh),
            };
            RowDetailFact::from_seed(&row, status, labor_class)
        })
        .collect::<Vec<_>>();
    let history = read_lifecycle_read_only(&paths.data_dir)?;
    // `read`, never `preflight`. `preflight` is this read plus
    // `probe_appendable`, which `create_dir_all`s the parent and opens the
    // ledger `create(true).append(true)` — so a diagnostic pointed at a live
    // daemon's data directory would create the directory tree and the ledger,
    // and would die outright where the operator can read the daemon's data but
    // not write it, which is the deployment this whole surface exists for. The
    // durable view has no reason to care whether the ledger is appendable: it
    // never appends.
    let membership = FlowMembership::read(&paths.flow_membership_path())?;
    let (ledger_verified, attestations) =
        match read_verified_attestations(&paths.attestations_path()) {
            Ok((report, records)) => (report.ok, records),
            // Same degradation the daemon applies: an unreadable advisory
            // ledger must not take down the canonical projection, and a rollup
            // that answered it with a confident zero would be worse than one
            // that says it summed nothing.
            Err(_) => (false, Vec::new()),
        };
    let mut view = query_run(
        flow_run,
        &details,
        // No live rows. The daemon owns that map and it is not on disk.
        &[],
        &history,
        durable.witness(),
        now,
        &membership,
        &AttestationEvidence::new(ledger_verified, &attestations),
    )?;
    if let Some(executor) = executor {
        attach_capture_pointers(&mut view, executor);
    }
    apply_run_lineage(
        &mut view,
        &FlowLineage::read(&paths.flow_lineage_path()).unwrap_or_default(),
    );
    apply_campaign_run_supersession(
        &mut view,
        &details,
        &history,
        durable.witness(),
        &membership,
    );
    apply_reader_state_to_run(
        &mut view,
        &ReaderState::read_advisory(&reader_state_path(&paths.data_dir)),
    );

    let mut caveats = vec![DURABLE_VIEW_CAVEAT.to_owned()];
    if !history.retention.complete {
        caveats.push(
            "lifecycle history is incomplete on disk, so event-derived state may be missing"
                .to_owned(),
        );
    }
    if !ledger_verified {
        caveats.push(
            "the advisory attestation ledger did not verify, so the usage rollup summed nothing"
                .to_owned(),
        );
    }
    Ok(DurableRunView { view, caveats })
}

/// Attach the retained stderr capture path, and its excerpt, to each failure —
/// the same pointer the live `query.run` attaches, read from the same retained
/// capture tree. A failure whose captures have been reaped keeps no pointer
/// rather than gaining a path that resolves to nothing.
fn attach_capture_pointers(view: &mut RunView, executor: &Executor) {
    for failure in &mut view.failures {
        let (Some(attempt), Some(lease_epoch), Ok(uuid)) = (
            failure.attempt,
            failure.lease_epoch,
            uuid::Uuid::parse_str(&failure.task_uuid),
        ) else {
            continue;
        };
        let identity = ExecutionIdentity {
            job_id: uuid,
            task_uuid: Some(uuid),
            task_ref: failure.task_ref.clone(),
        };
        let Ok(Some(paths)) = executor.retained_capture_paths(&identity, attempt, lease_epoch)
        else {
            continue;
        };
        let Some(path) = paths.failure_stderr.as_ref() else {
            continue;
        };
        failure.capture_path = Some(path.display().to_string());
        if failure.stderr_tail.is_none() {
            if let Ok(excerpt) = read_capture_excerpt(path) {
                failure.stderr_tail = Some(excerpt.text);
                failure.stderr_truncated = Some(excerpt.truncated);
            }
        }
    }
}

/// Parse the lifecycle log without opening it for write.
///
/// [`crate::history::LifecycleStore::open`] is the daemon's path: it creates
/// the file, takes an exclusive lock, and repairs a torn tail. Every one of
/// those is correct for the writer and wrong for a diagnostic that may be run
/// against a live daemon's data directory — a reader must not create, lock, or
/// rewrite the file it is reading. So this parses lines and stops at the first
/// one that does not decode, reporting the history as incomplete rather than
/// repairing it. A tail being written concurrently is exactly that case.
fn read_lifecycle_read_only(data_dir: &Path) -> Result<LifecycleSnapshot, DurableViewError> {
    let path = data_dir.join(LIFECYCLE_FILE);
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(DurableViewError::Lifecycle {
                path: path.display().to_string(),
                source,
            })
        }
    };
    let mut records = Vec::new();
    let mut truncated = false;
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<LifecycleRecord>(line) {
            Ok(record) => records.push(record),
            Err(_) => {
                truncated = true;
                break;
            }
        }
    }

    let retention = std::fs::read_to_string(data_dir.join(LIFECYCLE_RETENTION_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
    let boundary = retention
        .as_ref()
        .and_then(|state| state.get("truncationBoundary"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let recorded_complete = retention
        .as_ref()
        .and_then(|state| state.get("complete"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let reason = retention
        .as_ref()
        .and_then(|state| state.get("reason"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);

    Ok(LifecycleSnapshot {
        retention: RetentionMetadata {
            complete: recorded_complete && !truncated,
            policy: LIFECYCLE_RETENTION_POLICY.to_owned(),
            earliest_cursor: records.first().map(|record| record.cursor.clone()),
            latest_cursor: records.last().map(|record| record.cursor.clone()),
            truncation_boundary: boundary,
            reason: reason.or_else(|| {
                truncated
                    .then(|| "unparsable lifecycle tail skipped by a read-only reader".to_owned())
            }),
        },
        records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_only_lifecycle_read_creates_nothing_and_reports_a_torn_tail() {
        let temp = tempfile::tempdir().unwrap();

        // An absent log is an empty history, not an error, and reading it must
        // not bring the file into existence: the daemon owns creation.
        let snapshot = read_lifecycle_read_only(temp.path()).unwrap();
        assert!(snapshot.records.is_empty());
        assert!(snapshot.retention.complete);
        assert!(!temp.path().join(LIFECYCLE_FILE).exists());

        // A tail that does not decode is reported as incomplete rather than
        // repaired. `LifecycleStore::open` would truncate it; a reader must
        // not, because the daemon may be mid-write.
        std::fs::write(
            temp.path().join(LIFECYCLE_FILE),
            "{\"not\":\"a lifecycle record\"}\n",
        )
        .unwrap();
        let snapshot = read_lifecycle_read_only(temp.path()).unwrap();
        assert!(snapshot.records.is_empty());
        assert!(!snapshot.retention.complete);
        assert_eq!(
            std::fs::read_to_string(temp.path().join(LIFECYCLE_FILE)).unwrap(),
            "{\"not\":\"a lifecycle record\"}\n",
            "a read-only reader must leave the bytes it read exactly as it found them"
        );
    }

    /// Every path below `root`, sorted, so a test can state what a read left
    /// behind rather than checking the one file it happened to think of.
    fn tree(root: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(directory) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path.clone());
                }
                found.push(
                    path.strip_prefix(root)
                        .unwrap_or(&path)
                        .display()
                        .to_string(),
                );
            }
        }
        found.sort();
        found
    }

    /// #434 (eval F1). The module doc, the operator docs, the CHANGELOG and the
    /// PR body all give "it never creates, locks, or repairs a durable store"
    /// as the *reason* an automatic fallback into a live daemon's data
    /// directory is safe. That claim was false: the view called
    /// `flow_membership::preflight`, whose `probe_appendable` half
    /// `create_dir_all`s the parent and opens the ledger
    /// `create(true).append(true)`, so the diagnostic materialised a `0600`
    /// membership ledger inside the store it was diagnosing.
    ///
    /// Asserted over the whole tree rather than over one filename, so the next
    /// store this view learns to read is covered without anyone remembering to
    /// extend it.
    #[test]
    fn a_durable_read_creates_nothing_anywhere_under_the_state_or_data_dir() {
        let temp = tempfile::tempdir().unwrap();
        let paths = DaemonPaths {
            socket: temp.path().join("run/tally.sock"),
            state_dir: temp.path().join("state"),
            data_dir: temp.path().join("data"),
        };
        std::fs::create_dir_all(paths.events_dir()).unwrap();
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        let before = (tree(&paths.state_dir), tree(&paths.data_dir));

        let error = durable_run_view(
            &paths,
            "00000000-0000-4000-8000-000000000001",
            None,
            Utc::now(),
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                DurableViewError::Projection(ObservabilityError::UnknownJob(_))
            ),
            "{error}"
        );

        assert_eq!(
            (tree(&paths.state_dir), tree(&paths.data_dir)),
            before,
            "the durable view must leave the stores it read exactly as it found them"
        );
    }

    /// #434 (eval F1). The deployment this surface exists for: the operator can
    /// read the daemon's data directory and cannot write it. An appendability
    /// probe dies here with an I/O error that is itself false — the file is
    /// readable and the read would have succeeded — and it dies on the
    /// *automatic* fallback path too, so the operator's only honest window into
    /// a stalled daemon closes exactly when it is needed.
    #[test]
    fn an_unwritable_membership_ledger_is_read_rather_than_probed() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let paths = DaemonPaths {
            socket: temp.path().join("run/tally.sock"),
            state_dir: temp.path().join("state"),
            data_dir: temp.path().join("data"),
        };
        std::fs::create_dir_all(paths.events_dir()).unwrap();
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        let membership = paths.flow_membership_path();
        std::fs::write(&membership, "").unwrap();
        std::fs::set_permissions(&membership, std::fs::Permissions::from_mode(0o444)).unwrap();

        let error = durable_run_view(
            &paths,
            "00000000-0000-4000-8000-000000000001",
            None,
            Utc::now(),
        )
        .unwrap_err();
        // The projection is reached and answers about the run. A membership
        // error here would mean the read never got that far.
        assert!(
            matches!(
                error,
                DurableViewError::Projection(ObservabilityError::UnknownJob(_))
            ),
            "{error}"
        );
    }
}
