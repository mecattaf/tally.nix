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
//! The legacy `query run --durable` projection remains a deliberately weaker
//! view and says so. [`rebuild_run_view`] is the promoted primary replay: it
//! reads the same canonical stores and also samples executor unit facts, the
//! one live corroboration leg a ledger cannot contain.
//!
//! Three limits remain explicit:
//!
//! - **In-flight state.** The legacy durable projection passes no unit facts
//!   and therefore claims none. The rebuild projection samples units and can
//!   report running or awaiting-reconciliation work, but queued and paused
//!   state remains process-local and is not invented.
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

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;
use thiserror::Error;

use crate::adapters::ScrapeResult;
use crate::daemon::DaemonPaths;
use crate::evidence::retry_trigger;
use crate::executor::{
    read_capture_excerpt, ExecutionIdentity, Executor, ExecutorError, LocalUnitFact, LocalUnitState,
};
use crate::flow_lineage::FlowLineage;
use crate::flow_membership::FlowMembership;
use crate::history::{
    LifecycleRecord, LifecycleSnapshot, RetentionMetadata, LIFECYCLE_FILE,
    LIFECYCLE_RETENTION_FILE, LIFECYCLE_RETENTION_POLICY,
};
use crate::query::RowStatus;
use crate::query_v2::{
    apply_campaign_run_supersession, apply_reader_state_to_run, apply_run_lineage, query_run,
    LiveJobFact, ObservabilityError, RowDetailFact, RunView,
};
use crate::reader_state::{reader_state_path, ReaderState};
use crate::recovery::{collect_durable_recovery_facts, DurableRecoveryFacts};
use crate::taskdb::RowSeed;
use crate::usage::{UsageEvidence, UsageObservation};
use crate::usage_rollup::AttestationEvidence;
use crate::witness::{
    read_verified_attestations, AttestationRecord, LaborClass, Verdict, WitnessRecord,
};

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
    #[error("unit liveness for durable task {task_uuid} cannot be read: {source}")]
    UnitLiveness {
        task_uuid: String,
        source: ExecutorError,
    },
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

/// The promoted replay's freshness statement. Unit facts make running work
/// visible, but they are sampled in sequence with the canonical files rather
/// than frozen together with them.
pub const REBUILD_VIEW_CAVEAT: &str =
    "rebuild view: replayed canonical state with sampled unit liveness and no live RPC, so concurrent changes may make it stale";

#[derive(Debug, Clone)]
struct RebuildUnitFact {
    fact: LocalUnitFact,
    labor_class: LaborClass,
}

/// A stable, named disagreement between a rebuilt run and the daemon's live
/// derived projection.
#[derive(Debug, Clone, PartialEq)]
pub struct RebuildDifference {
    pub path: String,
    pub rebuilt: Option<Value>,
    pub live: Option<Value>,
}

impl std::fmt::Display for RebuildDifference {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "derived state differs at {}", self.path)?;
        if let Some(rebuilt) = &self.rebuilt {
            write!(formatter, ": rebuild={rebuilt}")?;
        } else {
            formatter.write_str(": rebuild=<missing>")?;
        }
        if let Some(live) = &self.live {
            write!(formatter, " live={live}")
        } else {
            formatter.write_str(" live=<missing>")
        }
    }
}

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
    // `read`, never `preflight`. `preflight` is this read plus
    // `probe_appendable`, which `create_dir_all`s the parent and opens the
    // ledger `create(true).append(true)` — so a diagnostic pointed at a live
    // daemon's data directory would create the directory tree and the ledger,
    // and would die outright where the operator can read the daemon's data but
    // not write it, which is the deployment this whole surface exists for. The
    // durable view has no reason to care whether the ledger is appendable: it
    // never appends.
    let membership = FlowMembership::read(&paths.flow_membership_path())?;
    project_run_view(
        paths,
        flow_run,
        executor,
        now,
        durable,
        membership,
        &BTreeMap::new(),
        DURABLE_VIEW_CAVEAT,
    )
}

/// Replay one flow run from canonical stores and sample executor unit facts.
///
/// This is the implementation behind `tally rebuild`. Unlike
/// [`durable_run_view`], it reports an in-flight unit when the executor can
/// corroborate one. It remains a read-only operation: executor inspection and
/// every store read used below are non-mutating.
pub async fn rebuild_run_view(
    paths: &DaemonPaths,
    flow_run: &str,
    executor: &Executor,
    now: DateTime<Utc>,
) -> Result<DurableRunView, DurableViewError> {
    let durable = collect_durable_recovery_facts(&paths.events_dir(), &paths.witness_path())?;
    let membership = FlowMembership::read(&paths.flow_membership_path())?;
    let units = collect_rebuild_unit_facts(&durable, &membership, flow_run, executor).await?;
    project_run_view(
        paths,
        flow_run,
        Some(executor),
        now,
        durable,
        membership,
        &units,
        REBUILD_VIEW_CAVEAT,
    )
}

#[allow(clippy::too_many_arguments)]
fn project_run_view(
    paths: &DaemonPaths,
    flow_run: &str,
    executor: Option<&Executor>,
    now: DateTime<Utc>,
    durable: DurableRecoveryFacts,
    membership: FlowMembership,
    units: &BTreeMap<uuid::Uuid, RebuildUnitFact>,
    base_caveat: &str,
) -> Result<DurableRunView, DurableViewError> {
    let history = read_lifecycle_read_only(&paths.data_dir)?;
    let (ledger_verified, attestations) =
        match read_verified_attestations(&paths.attestations_path()) {
            Ok((report, records)) => (report.ok, records),
            // Same degradation the daemon applies: an unreadable advisory
            // ledger must not take down the canonical projection, and a rollup
            // that answered it with a confident zero would be worse than one
            // that says it summed nothing.
            Err(_) => (false, Vec::new()),
        };
    let details = rebuilt_row_details(&durable, units, &attestations);
    let live = rebuilt_live_jobs(&durable, units);
    let mut view = query_run(
        flow_run,
        &details,
        &live,
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

    let mut caveats = vec![base_caveat.to_owned()];
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

async fn collect_rebuild_unit_facts(
    durable: &DurableRecoveryFacts,
    membership: &FlowMembership,
    flow_run: &str,
    executor: &Executor,
) -> Result<BTreeMap<uuid::Uuid, RebuildUnitFact>, DurableViewError> {
    let mut relevant = membership
        .tasks(flow_run)
        .filter_map(|task| uuid::Uuid::parse_str(task).ok())
        .collect::<BTreeSet<_>>();
    for event in durable.events() {
        if event.row.uuid.to_string() == flow_run
            || event
                .row
                .orchestration
                .as_ref()
                .is_some_and(|orchestration| orchestration.flow_run_id() == flow_run)
        {
            relevant.insert(event.row.uuid);
        }
    }

    let mut latest = BTreeMap::<uuid::Uuid, &WitnessRecord>::new();
    for record in durable.witness() {
        let Some(task_uuid) = record
            .task_uuid
            .as_deref()
            .and_then(|task_uuid| uuid::Uuid::parse_str(task_uuid).ok())
        else {
            continue;
        };
        latest.insert(task_uuid, record);
    }

    let mut units = BTreeMap::new();
    for event in durable
        .events()
        .iter()
        .filter(|event| relevant.contains(&event.row.uuid))
    {
        let task_uuid = event.row.uuid;
        let latest_record = latest.get(&task_uuid).copied();
        let pending_explicit_retry = latest_record.is_some_and(|record| {
            event.retries.iter().any(|retry| {
                retry.previous_witness_seq == record.seq
                    && retry.attempt == record.attempt.saturating_add(1)
            })
        });
        // A terminal remote row must not make its historical worker a read
        // dependency forever. This is the same cut startup recovery applies.
        if event.row.executor.is_some()
            && latest_record.is_some_and(|record| retry_trigger(record.verdict).is_none())
            && !pending_explicit_retry
        {
            continue;
        }
        let identity = row_execution_identity(&event.row);
        let fact = executor
            .inspect_identity_on(event.row.executor.as_deref(), &identity)
            .await
            .map_err(|source| DurableViewError::UnitLiveness {
                task_uuid: task_uuid.to_string(),
                source,
            })?;
        let (attempt, lease_epoch) = match fact.state {
            LocalUnitState::Absent => continue,
            LocalUnitState::InactiveWithoutRecord => {
                return Err(DurableViewError::UnitLiveness {
                    task_uuid: task_uuid.to_string(),
                    source: ExecutorError::UnitProbe {
                        unit: fact.unit,
                        detail: "unit is inactive but its durable exit record is absent".to_owned(),
                    },
                });
            }
            LocalUnitState::Running | LocalUnitState::Exited => {
                let attempt = fact.attempt.expect("validated present unit has an attempt");
                let lease_epoch = fact
                    .lease_epoch
                    .expect("validated present unit has a lease epoch");
                (attempt, lease_epoch)
            }
        };
        // A lingering collected unit whose generation is already covered by a
        // witness is not live work. Recovery makes the same distinction.
        if latest_record.is_some_and(|record| attempt <= record.attempt) {
            continue;
        }
        let labor_class = match latest_record {
            None => {
                if attempt != event.row.attempt || lease_epoch < event.row.lease_epoch {
                    return Err(invalid_unit_liveness(
                        task_uuid,
                        &fact.unit,
                        format!(
                            "first execution generation {attempt}:{lease_epoch} does not match durable row generation {}:{}",
                            event.row.attempt, event.row.lease_epoch
                        ),
                    ));
                }
                LaborClass::Fresh
            }
            Some(record) => {
                let eligible_attempt = record.attempt.saturating_add(1);
                if attempt != eligible_attempt
                    || (retry_trigger(record.verdict).is_none() && !pending_explicit_retry)
                    || lease_epoch < record.lease_epoch
                {
                    return Err(invalid_unit_liveness(
                        task_uuid,
                        &fact.unit,
                        format!(
                            "execution generation {attempt}:{lease_epoch} is not an eligible replay after witness {} generation {}:{}",
                            record.seq, record.attempt, record.lease_epoch
                        ),
                    ));
                }
                LaborClass::Recovered
            }
        };
        units.insert(task_uuid, RebuildUnitFact { fact, labor_class });
    }
    if let Ok(job_uuid) = uuid::Uuid::parse_str(flow_run) {
        if !durable
            .events()
            .iter()
            .any(|event| event.row.uuid == job_uuid)
        {
            let identity = ExecutionIdentity {
                job_id: job_uuid,
                task_uuid: None,
                task_ref: None,
            };
            let fact = executor
                .inspect_identity_async(&identity)
                .await
                .map_err(|source| DurableViewError::UnitLiveness {
                    task_uuid: flow_run.to_owned(),
                    source,
                })?;
            match fact.state {
                LocalUnitState::Absent => {}
                LocalUnitState::Running | LocalUnitState::Exited => {
                    units.insert(
                        job_uuid,
                        RebuildUnitFact {
                            fact,
                            labor_class: LaborClass::Fresh,
                        },
                    );
                }
                LocalUnitState::InactiveWithoutRecord => {
                    return Err(invalid_unit_liveness(
                        job_uuid,
                        &fact.unit,
                        "unit is inactive but its durable exit record is absent".to_owned(),
                    ));
                }
            }
        }
    }
    Ok(units)
}

fn invalid_unit_liveness(task_uuid: uuid::Uuid, unit: &str, detail: String) -> DurableViewError {
    DurableViewError::UnitLiveness {
        task_uuid: task_uuid.to_string(),
        source: ExecutorError::UnitProbe {
            unit: unit.to_owned(),
            detail,
        },
    }
}

fn row_execution_identity(row: &RowSeed) -> ExecutionIdentity {
    ExecutionIdentity {
        job_id: row.uuid,
        task_uuid: Some(row.uuid),
        task_ref: row
            .orchestration
            .as_ref()
            .and_then(crate::provenance::Orchestration::task_ref),
    }
}

fn rebuilt_row_details(
    durable: &DurableRecoveryFacts,
    units: &BTreeMap<uuid::Uuid, RebuildUnitFact>,
    attestations: &[AttestationRecord],
) -> Vec<RowDetailFact> {
    let mut terminal_by_task = BTreeMap::<uuid::Uuid, &WitnessRecord>::new();
    for record in durable.witness() {
        let Some(task_uuid) = record
            .task_uuid
            .as_deref()
            .and_then(|task_uuid| uuid::Uuid::parse_str(task_uuid).ok())
        else {
            continue;
        };
        terminal_by_task.insert(task_uuid, record);
    }
    durable
        .events()
        .iter()
        .map(|event| {
            let mut row = event.row.clone();
            let terminal = terminal_by_task.get(&row.uuid).copied();
            let (status, labor_class) = if let Some(unit) = units.get(&row.uuid) {
                row.attempt = unit
                    .fact
                    .attempt
                    .expect("validated present unit has an attempt");
                row.lease_epoch = unit
                    .fact
                    .lease_epoch
                    .expect("validated present unit has a lease epoch");
                (RowStatus::Pending, unit.labor_class)
            } else if let Some(retry) = terminal.and_then(|record| {
                event.retries.iter().find(|retry| {
                    retry.previous_witness_seq == record.seq
                        && retry.attempt == record.attempt.saturating_add(1)
                })
            }) {
                row.attempt = retry.attempt;
                (RowStatus::Pending, LaborClass::Recovered)
            } else if let Some(record) = terminal {
                row.attempt = record.attempt;
                row.lease_epoch = record.lease_epoch;
                let status = if record.verdict == Verdict::Cancelled {
                    RowStatus::Deleted
                } else {
                    RowStatus::Completed
                };
                (status, record.labor_class)
            } else {
                (RowStatus::Pending, LaborClass::Fresh)
            };
            hydrate_row_from_attestations(&mut row, attestations);
            RowDetailFact::from_seed(&row, status, labor_class)
        })
        .collect()
}

fn rebuilt_live_jobs(
    durable: &DurableRecoveryFacts,
    units: &BTreeMap<uuid::Uuid, RebuildUnitFact>,
) -> Vec<LiveJobFact> {
    let mut live = durable
        .events()
        .iter()
        .filter_map(|event| {
            let unit = units.get(&event.row.uuid)?;
            Some(LiveJobFact {
                anchor: event.row.uuid.to_string(),
                job_id: event.row.uuid.to_string(),
                // An exited unit with an unconsumed exit record is still live
                // daemon work: its derived row remains running until the
                // reconciler appends the terminal witness.
                live_state: "running".to_owned(),
                attempt: unit
                    .fact
                    .attempt
                    .expect("validated present unit has an attempt"),
                lease_epoch: unit
                    .fact
                    .lease_epoch
                    .expect("validated present unit has a lease epoch"),
                unit: unit.fact.unit.clone(),
                labor_class: unit.labor_class,
            })
        })
        .collect::<Vec<_>>();
    let rowed = durable
        .events()
        .iter()
        .map(|event| event.row.uuid)
        .collect::<BTreeSet<_>>();
    live.extend(
        units
            .iter()
            .filter(|(job_uuid, _)| !rowed.contains(job_uuid))
            .map(|(job_uuid, unit)| LiveJobFact {
                anchor: job_uuid.to_string(),
                job_id: job_uuid.to_string(),
                live_state: "running".to_owned(),
                attempt: unit
                    .fact
                    .attempt
                    .expect("validated present unit has an attempt"),
                lease_epoch: unit
                    .fact
                    .lease_epoch
                    .expect("validated present unit has a lease epoch"),
                unit: unit.fact.unit.clone(),
                labor_class: unit.labor_class,
            }),
    );
    live.sort_by(|left, right| left.anchor.cmp(&right.anchor));
    live
}

fn hydrate_row_from_attestations(row: &mut RowSeed, attestations: &[AttestationRecord]) {
    let task_uuid = row.uuid.to_string();
    let selected = attestations.iter().rev().find(|record| {
        let payload = &record.payload;
        payload.get("kind").and_then(Value::as_str) == Some("adapter-scrape")
            && payload.get("taskUuid").and_then(Value::as_str) == Some(task_uuid.as_str())
            && payload.get("adapter").and_then(Value::as_str) == Some(row.adapter.as_str())
            && payload.get("attempt").and_then(Value::as_u64) == Some(u64::from(row.attempt))
            && payload.get("leaseEpoch").and_then(Value::as_u64) == Some(row.lease_epoch)
    });
    let Some(payload) = selected.map(|record| &record.payload) else {
        return;
    };
    if let Some(captures) = payload
        .get("captures")
        .cloned()
        .and_then(|captures| serde_json::from_value(captures).ok())
        .map(|captures| ScrapeResult { captures })
    {
        if let Ok(Some(session_ref)) = captures.session_ref() {
            row.session_ref = Some(session_ref.to_owned());
            row.record_session_launch_cwd();
        }
        if let Ok(Some(model)) = captures.model() {
            row.model = Some(model.to_owned());
        }
        if let Ok(Some(final_message)) = captures.final_message() {
            row.final_message = Some(final_message.to_owned());
        }
    }
    if let Some(evidence) = payload
        .get("usageEvidence")
        .cloned()
        .and_then(|value| serde_json::from_value::<UsageEvidence>(value).ok())
    {
        row.usage = Some(evidence.observed);
        row.usage_accounting = Some(evidence.accounting);
    } else if let Some(observed) = payload
        .get("usage")
        .cloned()
        .and_then(|value| serde_json::from_value::<UsageObservation>(value).ok())
    {
        row.usage = Some(observed);
        row.usage_accounting = None;
    }
}

/// Compare stable JSON bytes for a rebuilt and live run. Snapshot creation
/// time and elapsed/budget counters are sampled independently, so they are
/// removed before byte comparison; every semantic field remains exact.
pub fn verify_rebuild_matches_live(
    rebuilt: &RunView,
    live: &RunView,
) -> Result<(), Box<RebuildDifference>> {
    let mut rebuilt = serde_json::to_value(rebuilt).expect("RunView serializes");
    let mut live = serde_json::to_value(live).expect("RunView serializes");
    normalize_comparison_value(&mut rebuilt);
    normalize_comparison_value(&mut live);
    if serde_json::to_vec(&rebuilt).expect("JSON value serializes")
        == serde_json::to_vec(&live).expect("JSON value serializes")
    {
        return Ok(());
    }
    Err(Box::new(
        first_json_difference("", &rebuilt, &live).unwrap_or(RebuildDifference {
            path: "/".to_owned(),
            rebuilt: Some(rebuilt),
            live: Some(live),
        }),
    ))
}

fn normalize_comparison_value(value: &mut Value) {
    if let Some(snapshot) = value.get_mut("snapshot").and_then(Value::as_object_mut) {
        snapshot.remove("createdAt");
    }
    if let Some(nodes) = value.get_mut("currentNodes").and_then(Value::as_array_mut) {
        for node in nodes {
            if let Some(node) = node.as_object_mut() {
                node.remove("elapsedSeconds");
                node.remove("budgetRemainingSeconds");
            }
        }
    }
}

fn first_json_difference(path: &str, rebuilt: &Value, live: &Value) -> Option<RebuildDifference> {
    match (rebuilt, live) {
        (Value::Object(rebuilt), Value::Object(live)) => {
            let keys = rebuilt.keys().chain(live.keys()).collect::<BTreeSet<_>>();
            for key in keys {
                let child = format!("{}/{}", path, json_pointer_segment(key));
                match (rebuilt.get(key), live.get(key)) {
                    (Some(rebuilt), Some(live)) => {
                        if let Some(difference) = first_json_difference(&child, rebuilt, live) {
                            return Some(difference);
                        }
                    }
                    (rebuilt, live) => {
                        return Some(RebuildDifference {
                            path: child,
                            rebuilt: rebuilt.cloned(),
                            live: live.cloned(),
                        });
                    }
                }
            }
            None
        }
        (Value::Array(rebuilt), Value::Array(live)) => {
            for (index, (rebuilt, live)) in rebuilt.iter().zip(live).enumerate() {
                let child = format!("{path}/{index}");
                if let Some(difference) = first_json_difference(&child, rebuilt, live) {
                    return Some(difference);
                }
            }
            (rebuilt.len() != live.len()).then(|| RebuildDifference {
                path: format!("{path}/length"),
                rebuilt: Some(Value::from(rebuilt.len())),
                live: Some(Value::from(live.len())),
            })
        }
        _ if rebuilt == live => None,
        _ => Some(RebuildDifference {
            path: if path.is_empty() { "/" } else { path }.to_owned(),
            rebuilt: Some(rebuilt.clone()),
            live: Some(live.clone()),
        }),
    }
}

fn json_pointer_segment(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
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
    use crate::executor::{ExecutionPaths, LocalUnitProbe};
    use crate::taskdb::DurableEnqueueEvent;

    use super::*;

    const ESTATE_GH_EVENT: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test/fixtures/ledger/events/legacy-gh-origin.enqueue.json"
    ));

    struct RunningUnitProbe;

    impl LocalUnitProbe for RunningUnitProbe {
        fn inspect(
            &self,
            unit: &str,
            _paths: &ExecutionPaths,
        ) -> Result<LocalUnitFact, ExecutorError> {
            if !unit.contains("00000000-0000-4000-8000-000000000042") {
                return Ok(LocalUnitFact::absent(unit));
            }
            Ok(LocalUnitFact {
                unit: unit.to_owned(),
                loaded: true,
                state: LocalUnitState::Running,
                invocation_id: Some("estate-rebuild-invocation".to_owned()),
                attempt: Some(1),
                lease_epoch: Some(1),
                exit_record: None,
            })
        }
    }

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

    /// Estate seam: the durable bytes are the checked-in historical row
    /// fixture, including the retired non-null `ghOrigin` object shape. The
    /// rebuild must pass those bytes through the decode-only D33 sink, add the
    /// independently sampled unit fact, and produce the same semantic run
    /// bytes as the live query constructor.
    #[tokio::test]
    async fn estate_gh_row_rebuild_matches_the_live_derived_projection() {
        let temp = tempfile::tempdir().unwrap();
        let paths = DaemonPaths {
            socket: temp.path().join("run/tally.sock"),
            state_dir: temp.path().join("state"),
            data_dir: temp.path().join("data"),
        };
        std::fs::create_dir_all(paths.events_dir()).unwrap();
        std::fs::create_dir_all(&paths.data_dir).unwrap();

        let flow_run = "00000000-0000-4000-8000-000000000043";
        let mut estate: Value = serde_json::from_str(ESTATE_GH_EVENT).unwrap();
        estate["row"]["ghOrigin"] = serde_json::json!({
            "producer": "github",
            "source": "notifications",
            "itemId": "estate-item-482",
            "actor": "historical-contributor",
            "selfActor": "tally-bot",
            "actorExclude": "self"
        });
        estate["row"]["orchestration"] = serde_json::json!({
            "flowName": "spec-build",
            "flowRunId": flow_run,
            "nodeOrdinal": 7,
            "nodeLabel": "agent-t07",
            "nodeRole": "agent",
            "subjectTaskId": "t07",
            "taskRef": "estate/t07"
        });
        estate["row"]["runtimeMaxSec"] = Value::from(600);
        let event_id = estate["eventId"].as_str().unwrap();
        std::fs::write(
            paths.events_dir().join(format!("{event_id}.enqueue.json")),
            serde_json::to_vec(&estate).unwrap(),
        )
        .unwrap();

        let decoded: DurableEnqueueEvent = serde_json::from_value(estate).unwrap();
        assert!(serde_json::to_value(&decoded).unwrap()["row"]
            .get("ghOrigin")
            .is_none());
        let executor = Executor::new(&paths.state_dir, "/nix/store/example/bin/tally")
            .with_unit_probe(RunningUnitProbe);
        let now = DateTime::parse_from_rfc3339("2026-08-14T12:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc);
        let rebuilt = rebuild_run_view(&paths, flow_run, &executor, now)
            .await
            .unwrap();
        assert_eq!(rebuilt.view.current_nodes.len(), 1);
        assert_eq!(rebuilt.view.current_nodes[0].state, "running");

        // Construct the live side independently from the row/index seam the
        // daemon uses, rather than cloning the rebuilt result.
        let detail = RowDetailFact::from_seed(&decoded.row, RowStatus::Pending, LaborClass::Fresh);
        let identity = row_execution_identity(&decoded.row);
        let live = vec![LiveJobFact {
            anchor: decoded.row.uuid.to_string(),
            job_id: decoded.row.uuid.to_string(),
            live_state: "running".to_owned(),
            attempt: 1,
            lease_epoch: 1,
            unit: executor.unit_name(&identity),
            labor_class: LaborClass::Fresh,
        }];
        let history = read_lifecycle_read_only(&paths.data_dir).unwrap();
        let membership = FlowMembership::read(&paths.flow_membership_path()).unwrap();
        let (attestation_report, attestations) =
            read_verified_attestations(&paths.attestations_path()).unwrap();
        let mut live_view = query_run(
            flow_run,
            &[detail],
            &live,
            &history,
            &[],
            now,
            &membership,
            &AttestationEvidence::new(attestation_report.ok, &attestations),
        )
        .unwrap();
        apply_run_lineage(&mut live_view, &FlowLineage::default());
        apply_campaign_run_supersession(
            &mut live_view,
            &[RowDetailFact::from_seed(
                &decoded.row,
                RowStatus::Pending,
                LaborClass::Fresh,
            )],
            &history,
            &[],
            &membership,
        );
        apply_reader_state_to_run(&mut live_view, &ReaderState::default());

        verify_rebuild_matches_live(&rebuilt.view, &live_view).unwrap();
        live_view.archived = true;
        let difference = verify_rebuild_matches_live(&rebuilt.view, &live_view).unwrap_err();
        assert_eq!(difference.path, "/archived");
    }
}
