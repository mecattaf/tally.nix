use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;
use thiserror::Error;

use crate::evidence::{
    retry_disposition, retry_trigger, RetryDisposition, RetryPolicy, RetryTrigger,
};
use crate::executor::{
    ExecutionIdentity, Executor, ExecutorError, LocalUnitFact, LocalUnitState, UnitExitRecord, Uuid,
};
use crate::taskdb::{read_acknowledged_events, DurableEnqueueEvent, RowSeed, TaskDbError};
use crate::witness::{read_verified_records, LaborClass, Verdict, WitnessError, WitnessRecord};

#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("task database source error: {0}")]
    TaskDb(#[from] TaskDbError),
    #[error("witness source error: {0}")]
    Witness(#[from] WitnessError),
    #[error("executor fact collection failed: {0}")]
    Executor(#[from] ExecutorError),
    #[error("witness chain is not verified: {0}")]
    InvalidWitness(String),
    #[error("invalid recovery facts: {0}")]
    InvalidFacts(String),
    #[error("invalid recovery policy: {0}")]
    InvalidPolicy(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct DurableRecoveryFacts {
    witness_lsn: u64,
    events: Vec<DurableEnqueueEvent>,
    witness: Vec<WitnessRecord>,
}

impl DurableRecoveryFacts {
    fn from_verified(
        mut events: Vec<DurableEnqueueEvent>,
        witness: Vec<WitnessRecord>,
    ) -> Result<Self, RecoveryError> {
        let mut durable_uuids = BTreeSet::new();
        for event in &events {
            event.validate()?;
            if !event.acknowledged {
                return Err(RecoveryError::InvalidFacts(format!(
                    "event {} is not acknowledged",
                    event.event_id
                )));
            }
            event.row.validate()?;
            if !durable_uuids.insert(event.row.uuid) {
                return Err(RecoveryError::InvalidFacts(format!(
                    "durable row {} appears in more than one acknowledged event",
                    event.row.uuid
                )));
            }
        }
        events.sort_by_key(|event| event.row.uuid);

        for (index, record) in witness.iter().enumerate() {
            let expected = index as u64 + 1;
            if record.seq != expected {
                return Err(RecoveryError::InvalidFacts(format!(
                    "witness_lsn reconciliation expected seq {expected}, found {}",
                    record.seq
                )));
            }
        }
        let witness_lsn = witness.last().map_or(0, |record| record.seq);
        Ok(Self {
            witness_lsn,
            events,
            witness,
        })
    }

    pub const fn witness_lsn(&self) -> u64 {
        self.witness_lsn
    }

    pub fn events(&self) -> &[DurableEnqueueEvent] {
        &self.events
    }

    pub fn witness(&self) -> &[WitnessRecord] {
        &self.witness
    }
}

pub fn collect_durable_recovery_facts(
    events_dir: &Path,
    witness_path: &Path,
) -> Result<DurableRecoveryFacts, RecoveryError> {
    let (report, witness) = read_verified_records(witness_path)?;
    if !report.ok {
        let detail = report.problems.first().map_or_else(
            || "unknown verification failure".to_owned(),
            |problem| {
                format!(
                    "line {} seq {:?} {:?}: {}",
                    problem.line, problem.seq, problem.kind, problem.reason
                )
            },
        );
        return Err(RecoveryError::InvalidWitness(detail));
    }
    let events = read_acknowledged_events(events_dir)?;
    let facts = DurableRecoveryFacts::from_verified(events, witness)?;
    if facts.witness_lsn != report.last_seq.unwrap_or(0) {
        return Err(RecoveryError::InvalidWitness(
            "verified last seq does not match the loaded witness records".to_owned(),
        ));
    }
    Ok(facts)
}

pub async fn collect_local_unit_facts(
    executor: &Executor,
    durable: &DurableRecoveryFacts,
) -> Result<BTreeMap<Uuid, LocalUnitFact>, RecoveryError> {
    let mut latest_records = BTreeMap::new();
    for record in &durable.witness {
        let Some(task_uuid) = record
            .task_uuid
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok())
        else {
            continue;
        };
        latest_records.insert(task_uuid, (record.verdict, record.attempt, record.seq));
    }

    let mut facts = BTreeMap::new();
    for event in &durable.events {
        let uuid = event.row.uuid;
        let identity = ExecutionIdentity {
            job_id: uuid,
            task_uuid: Some(uuid),
        };
        // A non-retryable witness is already the canonical proof that this
        // remote generation terminated. Probing it again cannot affect the
        // recovery plan, but it would make every historical worker a startup
        // dependency forever. Rows with no witness or a retryable verdict must
        // still be probed because a later presentation may be in flight.
        let pending_explicit_retry = latest_records.get(&uuid).is_some_and(|(_, attempt, seq)| {
            event.retries.iter().any(|retry| {
                retry.previous_witness_seq == *seq && retry.attempt == attempt.saturating_add(1)
            })
        });
        let remote_is_canonically_terminal = event.row.executor.is_some()
            && latest_records
                .get(&uuid)
                .is_some_and(|(verdict, _, _)| retry_trigger(*verdict).is_none())
            && !pending_explicit_retry;
        let fact = if remote_is_canonically_terminal {
            LocalUnitFact::absent(executor.unit_name(&identity))
        } else {
            executor
                .inspect_identity_on(event.row.executor.as_deref(), &identity)
                .await?
        };
        facts.insert(uuid, fact);
    }
    Ok(facts)
}

pub fn collect_rowless_unit_fact(
    executor: &Executor,
    job_uuid: Uuid,
) -> Result<LocalUnitFact, RecoveryError> {
    executor
        .inspect_identity(&ExecutionIdentity {
            job_id: job_uuid,
            task_uuid: None,
        })
        .map_err(RecoveryError::from)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryTriggers {
    pub confirmed_pool_returns: BTreeSet<String>,
    pub resource_returns: BTreeSet<String>,
    pub bounded_requeues: BTreeSet<Uuid>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdvisoryReturnAttestation {
    pub pool: String,
    pub payload: Value,
}

impl AdvisoryReturnAttestation {
    pub const fn no_enqueue(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryFacts {
    pub durable: DurableRecoveryFacts,
    pub current_lease_epoch: u64,
    pub units: BTreeMap<Uuid, LocalUnitFact>,
    pub rowless_units: BTreeMap<Uuid, LocalUnitFact>,
    pub triggers: RecoveryTriggers,
    pub advisory_return_attestations: Vec<AdvisoryReturnAttestation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryPolicy {
    pub retry: RetryPolicy,
    pub max_attempts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryRowState {
    Pending,
    Completed,
    Deleted,
    AdoptedRunning,
    AwaitingReconciliation,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryRow {
    pub row: RowSeed,
    pub state: RecoveryRowState,
    pub labor_class: LaborClass,
    pub guardrail_depth: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RecoveryIdentity {
    Task(Uuid),
    Job(Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LeaseEpochFence {
    pub identity: RecoveryIdentity,
    pub stale_epoch: u64,
    pub current_epoch: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecoveryAction {
    QueueExisting {
        task_uuid: Uuid,
        attempt: u32,
        lease_epoch: u64,
    },
    AdoptRunning {
        identity: RecoveryIdentity,
        unit: String,
        invocation_id: String,
        attempt: u32,
        lease_epoch: u64,
        labor_class: Option<LaborClass>,
    },
    ReconcileExit {
        identity: RecoveryIdentity,
        record: UnitExitRecord,
        labor_class: Option<LaborClass>,
    },
    AwaitUnitCollection {
        identity: RecoveryIdentity,
        unit: String,
    },
    RePresent {
        row: Box<RowSeed>,
        trigger: RetryTrigger,
        previous_witness_seq: u64,
        previous_attempt: u32,
        previous_lease_epoch: u64,
    },
    AwaitRetry {
        task_uuid: Uuid,
        disposition: RetryDisposition,
        trigger_observed: bool,
    },
    RetryExhausted {
        task_uuid: Uuid,
        last_attempt: u32,
        max_attempts: u32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryPlan {
    pub witness_lsn: u64,
    pub rows: Vec<RecoveryRow>,
    pub actions: Vec<RecoveryAction>,
    pub lease_epoch_fences: Vec<LeaseEpochFence>,
    pub advisory_return_attestations: Vec<AdvisoryReturnAttestation>,
}

pub fn recover(
    facts: &RecoveryFacts,
    policy: RecoveryPolicy,
) -> Result<RecoveryPlan, RecoveryError> {
    if facts.current_lease_epoch == 0 {
        return Err(RecoveryError::InvalidFacts(
            "current lease epoch must be positive".to_owned(),
        ));
    }
    if policy.max_attempts == 0 {
        return Err(RecoveryError::InvalidPolicy(
            "maxAttempts must be positive".to_owned(),
        ));
    }

    let durable_uuids = facts
        .durable
        .events
        .iter()
        .map(|event| event.row.uuid)
        .collect::<BTreeSet<_>>();
    if let Some(extra) = facts
        .units
        .keys()
        .find(|uuid| !durable_uuids.contains(uuid))
    {
        return Err(RecoveryError::InvalidFacts(format!(
            "rowed unit fact {extra} has no acknowledged durable row"
        )));
    }
    if let Some(collision) = facts
        .rowless_units
        .keys()
        .find(|uuid| durable_uuids.contains(uuid))
    {
        return Err(RecoveryError::InvalidFacts(format!(
            "rowless job {collision} collides with a durable task identity"
        )));
    }
    let mut histories = BTreeMap::<Uuid, Vec<&WitnessRecord>>::new();
    for record in &facts.durable.witness {
        let Some(task_uuid) = record.task_uuid.as_deref() else {
            continue;
        };
        let uuid = Uuid::parse_str(task_uuid).map_err(|_| {
            RecoveryError::InvalidFacts(format!(
                "witness seq {} has invalid task UUID {task_uuid:?}",
                record.seq
            ))
        })?;
        if durable_uuids.contains(&uuid) {
            histories.entry(uuid).or_default().push(record);
        }
    }

    let mut rows = Vec::with_capacity(facts.durable.events.len());
    let mut actions = Vec::new();
    let mut fences = BTreeSet::new();
    for event in &facts.durable.events {
        let mut row = event.row.clone();
        let identity = RecoveryIdentity::Task(row.uuid);
        if row.lease_epoch > facts.current_lease_epoch {
            return Err(RecoveryError::InvalidFacts(format!(
                "durable row {} carries future lease epoch {} at current epoch {}",
                row.uuid, row.lease_epoch, facts.current_lease_epoch
            )));
        }
        let history = histories.get(&row.uuid).map(Vec::as_slice).unwrap_or(&[]);
        validate_history(&row, history, &event.retries, facts.current_lease_epoch)?;
        let latest = history.last().copied();
        let explicit_retry = latest.is_some_and(|record| {
            event.retries.iter().any(|retry| {
                retry.attempt == record.attempt.saturating_add(1)
                    && retry.previous_witness_seq == record.seq
            })
        });
        let unit = facts.units.get(&row.uuid).ok_or_else(|| {
            RecoveryError::InvalidFacts(format!(
                "local unit fact is missing for durable row {}",
                row.uuid
            ))
        })?;
        validate_unit_fact(&row, unit, facts.current_lease_epoch)?;
        remember_stale_epoch(
            &mut fences,
            identity,
            row.lease_epoch,
            facts.current_lease_epoch,
        );
        if let Some(record) = latest {
            remember_stale_epoch(
                &mut fences,
                identity,
                record.lease_epoch,
                facts.current_lease_epoch,
            );
        }
        if let Some(epoch) = unit.lease_epoch {
            remember_stale_epoch(&mut fences, identity, epoch, facts.current_lease_epoch);
        }

        if handle_present_unit(
            &mut row,
            event.guardrail_depth,
            latest,
            explicit_retry,
            unit,
            &mut rows,
            &mut actions,
        )? {
            continue;
        }

        match latest {
            None => {
                row.lease_epoch = facts.current_lease_epoch;
                actions.push(RecoveryAction::QueueExisting {
                    task_uuid: row.uuid,
                    attempt: row.attempt,
                    lease_epoch: row.lease_epoch,
                });
                rows.push(RecoveryRow {
                    row,
                    state: RecoveryRowState::Pending,
                    labor_class: LaborClass::Fresh,
                    guardrail_depth: event.guardrail_depth,
                });
            }
            Some(record) if explicit_retry => {
                row.attempt = record.attempt.checked_add(1).ok_or_else(|| {
                    RecoveryError::InvalidFacts(format!("attempt overflow for row {}", row.uuid))
                })?;
                row.lease_epoch = facts.current_lease_epoch;
                actions.push(RecoveryAction::QueueExisting {
                    task_uuid: row.uuid,
                    attempt: row.attempt,
                    lease_epoch: row.lease_epoch,
                });
                rows.push(RecoveryRow {
                    row,
                    state: RecoveryRowState::Pending,
                    labor_class: LaborClass::Recovered,
                    guardrail_depth: event.guardrail_depth,
                });
            }
            Some(record) => plan_witnessed_row(
                &mut row,
                record,
                &facts.triggers,
                facts.current_lease_epoch,
                policy,
                event.guardrail_depth,
                &mut rows,
                &mut actions,
            )?,
        }
    }

    plan_rowless_units(
        &facts.rowless_units,
        facts.current_lease_epoch,
        &mut fences,
        &mut actions,
    )?;

    let advisory_return_attestations = facts
        .advisory_return_attestations
        .iter()
        .filter(|advisory| {
            facts
                .triggers
                .confirmed_pool_returns
                .contains(&advisory.pool)
        })
        .cloned()
        .collect();
    Ok(RecoveryPlan {
        witness_lsn: facts.durable.witness_lsn,
        rows,
        actions,
        lease_epoch_fences: fences.into_iter().collect(),
        advisory_return_attestations,
    })
}

fn validate_history(
    row: &RowSeed,
    history: &[&WitnessRecord],
    retries: &[crate::taskdb::DurableRetry],
    current_epoch: u64,
) -> Result<(), RecoveryError> {
    let mut expected_attempt = row.attempt;
    let mut previous_epoch = row.lease_epoch;
    let mut previous_record: Option<&WitnessRecord> = None;
    for record in history {
        let explicit_retry = previous_record.is_some_and(|previous| {
            retries.iter().any(|retry| {
                retry.attempt == record.attempt
                    && retry.previous_witness_seq == previous.seq
                    && record.attempt == previous.attempt.saturating_add(1)
            })
        });
        if previous_record.is_some_and(|previous| retry_trigger(previous.verdict).is_none())
            && !explicit_retry
        {
            return Err(RecoveryError::InvalidFacts(format!(
                "witness seq {} replays row {} after a terminal outcome",
                record.seq, row.uuid
            )));
        }
        if record.attempt != expected_attempt {
            return Err(RecoveryError::InvalidFacts(format!(
                "witness seq {} for row {} has attempt {}, expected {}",
                record.seq, row.uuid, record.attempt, expected_attempt
            )));
        }
        expected_attempt = expected_attempt.checked_add(1).ok_or_else(|| {
            RecoveryError::InvalidFacts(format!("attempt overflow for row {}", row.uuid))
        })?;
        if record.lease_epoch == 0 || record.lease_epoch > current_epoch {
            return Err(RecoveryError::InvalidFacts(format!(
                "witness seq {} for row {} has impossible lease epoch {} at current epoch {current_epoch}",
                record.seq, row.uuid, record.lease_epoch
            )));
        }
        if record.lease_epoch < previous_epoch {
            return Err(RecoveryError::InvalidFacts(format!(
                "witness lease epoch regressed for row {}",
                row.uuid
            )));
        }
        previous_epoch = record.lease_epoch;
        if previous_record.is_some() && record.labor_class != LaborClass::Recovered {
            return Err(RecoveryError::InvalidFacts(format!(
                "witness seq {} for re-presented row {} is not recovered labor",
                record.seq, row.uuid
            )));
        }
        previous_record = Some(record);
        if record
            .pools
            .as_ref()
            .is_some_and(|pools| pools != &row.pools)
        {
            return Err(RecoveryError::InvalidFacts(format!(
                "witness seq {} pool does not match durable row {}",
                record.seq, row.uuid
            )));
        }
        if record.executor != row.executor {
            return Err(RecoveryError::InvalidFacts(format!(
                "witness seq {} executor does not match durable row {}",
                record.seq, row.uuid
            )));
        }
        if record.payload_hash != row.payload_hash {
            return Err(RecoveryError::InvalidFacts(format!(
                "witness seq {} payload hash does not match durable row {}",
                record.seq, row.uuid
            )));
        }
        if record.brief_hash != row.brief_hash {
            return Err(RecoveryError::InvalidFacts(format!(
                "witness seq {} brief hash does not match durable row {}",
                record.seq, row.uuid
            )));
        }
        if record.orchestration != row.orchestration {
            return Err(RecoveryError::InvalidFacts(format!(
                "witness seq {} orchestration does not match durable row {}",
                record.seq, row.uuid
            )));
        }
    }
    for retry in retries {
        let previous = history
            .iter()
            .copied()
            .find(|record| record.seq == retry.previous_witness_seq)
            .ok_or_else(|| {
                RecoveryError::InvalidFacts(format!(
                    "retry attempt {} for row {} references missing witness seq {}",
                    retry.attempt, row.uuid, retry.previous_witness_seq
                ))
            })?;
        if previous.verdict == Verdict::Pass {
            return Err(RecoveryError::InvalidFacts(format!(
                "retry attempt {} for row {} follows a pass verdict",
                retry.attempt, row.uuid
            )));
        }
        if previous
            .attempt
            .checked_add(1)
            .is_none_or(|attempt| attempt != retry.attempt)
        {
            return Err(RecoveryError::InvalidFacts(format!(
                "retry attempt {} for row {} does not follow witness attempt {}",
                retry.attempt, row.uuid, previous.attempt
            )));
        }
        let has_result = history.iter().any(|record| record.attempt == retry.attempt);
        let is_pending_latest = history.last().is_some_and(|latest| {
            latest.seq == previous.seq && latest.attempt.saturating_add(1) == retry.attempt
        });
        if !has_result && !is_pending_latest {
            return Err(RecoveryError::InvalidFacts(format!(
                "retry attempt {} for row {} has no terminal result and is not the live frontier",
                retry.attempt, row.uuid
            )));
        }
    }
    Ok(())
}

fn validate_unit_fact(
    row: &RowSeed,
    fact: &LocalUnitFact,
    current_epoch: u64,
) -> Result<(), RecoveryError> {
    validate_unit_fact_for_uuid(row.uuid, fact, current_epoch)
}

fn validate_unit_fact_for_uuid(
    uuid: Uuid,
    fact: &LocalUnitFact,
    current_epoch: u64,
) -> Result<(), RecoveryError> {
    let expected_unit = format!("tally-job-{uuid}.service");
    if fact.unit != expected_unit {
        return Err(RecoveryError::InvalidFacts(format!(
            "unit fact {:?} does not match durable row {}",
            fact.unit, uuid
        )));
    }
    if fact.lease_epoch.is_some_and(|epoch| epoch > current_epoch) {
        return Err(RecoveryError::InvalidFacts(format!(
            "unit {} carries future lease epoch {:?} at current epoch {current_epoch}",
            fact.unit, fact.lease_epoch
        )));
    }
    match fact.state {
        LocalUnitState::Absent => {
            if fact.loaded
                || fact.invocation_id.is_some()
                || fact.attempt.is_some()
                || fact.lease_epoch.is_some()
                || fact.exit_record.is_some()
            {
                return Err(RecoveryError::InvalidFacts(format!(
                    "absent unit {} carries execution metadata",
                    fact.unit
                )));
            }
        }
        LocalUnitState::Running => {
            if !fact.loaded
                || fact.invocation_id.as_deref().is_none_or(str::is_empty)
                || fact.attempt.is_none()
                || fact.lease_epoch.is_none()
                || fact.exit_record.is_some()
            {
                return Err(RecoveryError::InvalidFacts(format!(
                    "running unit {} has incomplete or contradictory metadata",
                    fact.unit
                )));
            }
        }
        LocalUnitState::Exited => {
            let record = fact.exit_record.as_ref().ok_or_else(|| {
                RecoveryError::InvalidFacts(format!(
                    "exited unit {} has no durable exit record",
                    fact.unit
                ))
            })?;
            record.validate(&fact.unit)?;
            if fact.invocation_id.as_deref() != Some(record.invocation_id.as_str())
                || fact.attempt != Some(record.attempt)
                || fact.lease_epoch != Some(record.lease_epoch)
                || record.unit != fact.unit
            {
                return Err(RecoveryError::InvalidFacts(format!(
                    "exited unit {} metadata does not match its durable record",
                    fact.unit
                )));
            }
        }
        LocalUnitState::InactiveWithoutRecord => {
            if !fact.loaded || fact.exit_record.is_some() {
                return Err(RecoveryError::InvalidFacts(format!(
                    "inactive unit {} has contradictory metadata",
                    fact.unit
                )));
            }
        }
    }
    Ok(())
}

fn plan_rowless_units(
    units: &BTreeMap<Uuid, LocalUnitFact>,
    current_epoch: u64,
    fences: &mut BTreeSet<LeaseEpochFence>,
    actions: &mut Vec<RecoveryAction>,
) -> Result<(), RecoveryError> {
    for (job_uuid, unit) in units {
        validate_unit_fact_for_uuid(*job_uuid, unit, current_epoch)?;
        let identity = RecoveryIdentity::Job(*job_uuid);
        if let Some(epoch) = unit.lease_epoch {
            remember_stale_epoch(fences, identity, epoch, current_epoch);
        }
        match unit.state {
            LocalUnitState::Absent => {}
            LocalUnitState::Running => actions.push(RecoveryAction::AdoptRunning {
                identity,
                unit: unit.unit.clone(),
                invocation_id: unit
                    .invocation_id
                    .clone()
                    .expect("validated running rowless unit has an invocation ID"),
                attempt: unit
                    .attempt
                    .expect("validated running rowless unit has an attempt"),
                lease_epoch: unit
                    .lease_epoch
                    .expect("validated running rowless unit has a lease epoch"),
                labor_class: None,
            }),
            LocalUnitState::Exited => actions.push(RecoveryAction::ReconcileExit {
                identity,
                record: unit
                    .exit_record
                    .clone()
                    .expect("validated exited rowless unit has a record"),
                labor_class: None,
            }),
            LocalUnitState::InactiveWithoutRecord => {
                return Err(RecoveryError::InvalidFacts(format!(
                    "rowless unit {} is inactive but its durable exit record is absent",
                    unit.unit
                )));
            }
        }
    }
    Ok(())
}

fn handle_present_unit(
    row: &mut RowSeed,
    guardrail_depth: u32,
    latest: Option<&WitnessRecord>,
    explicit_retry: bool,
    unit: &LocalUnitFact,
    rows: &mut Vec<RecoveryRow>,
    actions: &mut Vec<RecoveryAction>,
) -> Result<bool, RecoveryError> {
    if unit.state == LocalUnitState::InactiveWithoutRecord {
        return Err(RecoveryError::InvalidFacts(format!(
            "unit {} is inactive but its durable exit record is absent",
            unit.unit
        )));
    }
    if unit.state == LocalUnitState::Absent {
        return Ok(false);
    }

    let attempt = unit.attempt.expect("validated present unit has an attempt");
    let lease_epoch = unit
        .lease_epoch
        .expect("validated present unit has a lease epoch");
    let labor_class = match latest {
        None => {
            if attempt != row.attempt || lease_epoch < row.lease_epoch {
                return Err(RecoveryError::InvalidFacts(format!(
                    "first execution unit {} does not match durable attempt and epoch",
                    unit.unit
                )));
            }
            LaborClass::Fresh
        }
        Some(record) if attempt <= record.attempt => {
            if unit.state == LocalUnitState::Running {
                return Err(RecoveryError::InvalidFacts(format!(
                    "running unit {} is already covered by witness seq {}",
                    unit.unit, record.seq
                )));
            }
            if attempt < record.attempt {
                return Err(RecoveryError::InvalidFacts(format!(
                    "unit {} durable exit attempt {} predates latest witness attempt {}",
                    unit.unit, attempt, record.attempt
                )));
            }
            if unit.loaded {
                row.attempt = record.attempt;
                row.lease_epoch = record.lease_epoch;
                actions.push(RecoveryAction::AwaitUnitCollection {
                    identity: RecoveryIdentity::Task(row.uuid),
                    unit: unit.unit.clone(),
                });
                rows.push(RecoveryRow {
                    row: row.clone(),
                    state: RecoveryRowState::Completed,
                    labor_class: record.labor_class,
                    guardrail_depth,
                });
                return Ok(true);
            }
            return Ok(false);
        }
        Some(record) => {
            let expected_attempt = record.attempt.checked_add(1).ok_or_else(|| {
                RecoveryError::InvalidFacts(format!("attempt overflow for row {}", row.uuid))
            })?;
            if attempt != expected_attempt
                || (retry_trigger(record.verdict).is_none() && !explicit_retry)
            {
                return Err(RecoveryError::InvalidFacts(format!(
                    "unit {} attempt {} is not the next eligible presentation after witness seq {}",
                    unit.unit, attempt, record.seq
                )));
            }
            if lease_epoch < record.lease_epoch {
                return Err(RecoveryError::InvalidFacts(format!(
                    "unit {} regressed lease epoch from {} to {}",
                    unit.unit, record.lease_epoch, lease_epoch
                )));
            }
            LaborClass::Recovered
        }
    };

    row.attempt = attempt;
    row.lease_epoch = lease_epoch;
    match unit.state {
        LocalUnitState::Running => {
            actions.push(RecoveryAction::AdoptRunning {
                identity: RecoveryIdentity::Task(row.uuid),
                unit: unit.unit.clone(),
                invocation_id: unit
                    .invocation_id
                    .clone()
                    .expect("validated running unit has an invocation ID"),
                attempt,
                lease_epoch,
                labor_class: Some(labor_class),
            });
            rows.push(RecoveryRow {
                row: row.clone(),
                state: RecoveryRowState::AdoptedRunning,
                labor_class,
                guardrail_depth,
            });
        }
        LocalUnitState::Exited => {
            actions.push(RecoveryAction::ReconcileExit {
                identity: RecoveryIdentity::Task(row.uuid),
                record: unit
                    .exit_record
                    .clone()
                    .expect("validated exited unit has a record"),
                labor_class: Some(labor_class),
            });
            rows.push(RecoveryRow {
                row: row.clone(),
                state: RecoveryRowState::AwaitingReconciliation,
                labor_class,
                guardrail_depth,
            });
        }
        LocalUnitState::Absent | LocalUnitState::InactiveWithoutRecord => {
            unreachable!("absent and inactive units returned before reconciliation")
        }
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn plan_witnessed_row(
    row: &mut RowSeed,
    record: &WitnessRecord,
    triggers: &RecoveryTriggers,
    current_epoch: u64,
    policy: RecoveryPolicy,
    guardrail_depth: u32,
    rows: &mut Vec<RecoveryRow>,
    actions: &mut Vec<RecoveryAction>,
) -> Result<(), RecoveryError> {
    row.attempt = record.attempt;
    row.lease_epoch = record.lease_epoch;
    let terminal_state = if record.verdict == Verdict::Cancelled {
        RecoveryRowState::Deleted
    } else {
        RecoveryRowState::Completed
    };
    let Some(trigger) = retry_trigger(record.verdict) else {
        rows.push(RecoveryRow {
            row: row.clone(),
            state: terminal_state,
            labor_class: record.labor_class,
            guardrail_depth,
        });
        return Ok(());
    };
    let next_attempt = record.attempt.checked_add(1).ok_or_else(|| {
        RecoveryError::InvalidFacts(format!("attempt overflow for row {}", row.uuid))
    })?;
    if next_attempt > policy.max_attempts {
        actions.push(RecoveryAction::RetryExhausted {
            task_uuid: row.uuid,
            last_attempt: record.attempt,
            max_attempts: policy.max_attempts,
        });
        rows.push(RecoveryRow {
            row: row.clone(),
            state: RecoveryRowState::Completed,
            labor_class: record.labor_class,
            guardrail_depth,
        });
        return Ok(());
    }
    let disposition = retry_disposition(record.verdict, policy.retry);
    let trigger_observed = match trigger {
        RetryTrigger::PoolReturn => row
            .pools
            .iter()
            .any(|pool| triggers.confirmed_pool_returns.contains(pool)),
        RetryTrigger::ResourceReturn => row
            .pools
            .iter()
            .any(|pool| triggers.resource_returns.contains(pool)),
        RetryTrigger::BoundedRequeue => triggers.bounded_requeues.contains(&row.uuid),
    };
    if disposition != RetryDisposition::Automatic(trigger) || !trigger_observed {
        actions.push(RecoveryAction::AwaitRetry {
            task_uuid: row.uuid,
            disposition,
            trigger_observed,
        });
        rows.push(RecoveryRow {
            row: row.clone(),
            state: RecoveryRowState::Completed,
            labor_class: record.labor_class,
            guardrail_depth,
        });
        return Ok(());
    }

    row.attempt = next_attempt;
    row.lease_epoch = current_epoch;
    actions.push(RecoveryAction::RePresent {
        row: Box::new(row.clone()),
        trigger,
        previous_witness_seq: record.seq,
        previous_attempt: record.attempt,
        previous_lease_epoch: record.lease_epoch,
    });
    rows.push(RecoveryRow {
        row: row.clone(),
        state: RecoveryRowState::Pending,
        labor_class: LaborClass::Recovered,
        guardrail_depth,
    });
    Ok(())
}

fn remember_stale_epoch(
    fences: &mut BTreeSet<LeaseEpochFence>,
    identity: RecoveryIdentity,
    epoch: u64,
    current_epoch: u64,
) {
    if epoch < current_epoch {
        fences.insert(LeaseEpochFence {
            identity,
            stale_epoch: epoch,
            current_epoch,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::future::Future;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use crate::config::{ExecutionTargetConfig, Priority, SshExecutorConfig};
    use crate::executor::{
        RemoteExecutorReply, RemoteExecutorRequest, RemoteExecutorResult, RemoteTransport,
        RemoteTransportError, REMOTE_EXECUTOR_PROTOCOL_VERSION,
    };
    use crate::taskdb::{write_enqueue_event_atomic, DurableRetry, EnqueueSource};
    use crate::witness::{build_record, ChainHead, WitnessBody};

    use super::*;

    fn row(uuid: Uuid, pool: &str, lease_epoch: u64) -> RowSeed {
        RowSeed {
            uuid,
            description: "recover this durable leaf".to_owned(),
            priority: Priority::High,
            source: EnqueueSource::EventsDir,
            adapter: "shell".to_owned(),
            pools: vec![pool.to_owned()],
            executor: None,
            model: None,
            cwd: Some(PathBuf::from("/work")),
            workspace: None,
            adapter_options: Default::default(),
            gate_manifest: None,
            resumed_from: None,
            dedup_key: Some(format!("dedup:{uuid}")),
            payload_hash: None,
            brief_hash: None,
            orchestration: None,
            session_ref: None,
            lease_epoch,
            attempt: 1,
            argv: vec!["worker".to_owned(), "leaf".to_owned()],
            evidence: vec!["exit:0".to_owned()],
            parent_uuid: None,
            consumption_estimate: Some(3),
            runtime_max_sec: Some(30),
            no_enqueue: false,
            credentials: BTreeMap::new(),
            origin: None,
            gh_origin: None,
            related_trigger: None,
            evidence_class: None,
            manifest_hash: None,
        }
    }

    fn event(row: RowSeed) -> DurableEnqueueEvent {
        DurableEnqueueEvent::new(row).unwrap()
    }

    fn witness(row: &RowSeed, verdicts: &[(Verdict, u64)]) -> Vec<WitnessRecord> {
        let mut head = ChainHead::default();
        verdicts
            .iter()
            .enumerate()
            .map(|(index, (verdict, lease_epoch))| {
                let record = build_record(
                    WitnessBody {
                        task_uuid: Some(row.uuid.to_string()),
                        transition_timestamp: format!("2026-07-19T10:00:0{index}Z"),
                        verdict: *verdict,
                        exit_code: if *verdict == Verdict::Pass { 0 } else { 1 },
                        artifact_content_hash: None,
                        gpu_seconds: None,
                        wall_clock: 1.0,
                        attempt: row.attempt + index as u32,
                        lease_epoch: *lease_epoch,
                        dedup_key: row.dedup_key.clone(),
                        payload_hash: row.payload_hash.clone(),
                        brief_hash: row.brief_hash.clone(),
                        orchestration: row.orchestration.clone(),
                        labor_class: if index == 0 {
                            LaborClass::Fresh
                        } else {
                            LaborClass::Recovered
                        },
                        trace_ref: None,
                        pools: Some(row.pools.clone()),
                        executor: row.executor.clone(),
                        charge: None,
                        model: None,
                        evidence_class: None,
                        manifest_hash: None,
                        completion: None,
                    },
                    &head,
                )
                .unwrap();
                head = ChainHead {
                    seq: record.seq,
                    hash: record.hash.clone(),
                };
                record
            })
            .collect()
    }

    fn empty_triggers() -> RecoveryTriggers {
        RecoveryTriggers {
            confirmed_pool_returns: BTreeSet::new(),
            resource_returns: BTreeSet::new(),
            bounded_requeues: BTreeSet::new(),
        }
    }

    fn retry_policy(auto: bool, max_attempts: u32) -> RecoveryPolicy {
        RecoveryPolicy {
            retry: RetryPolicy {
                auto_pool_return: auto,
                auto_resource_return: auto,
                auto_bounded_requeue: auto,
            },
            max_attempts,
        }
    }

    fn facts(
        row: RowSeed,
        witness: Vec<WitnessRecord>,
        unit: LocalUnitFact,
        current_lease_epoch: u64,
        triggers: RecoveryTriggers,
    ) -> RecoveryFacts {
        RecoveryFacts {
            durable: DurableRecoveryFacts::from_verified(vec![event(row.clone())], witness)
                .unwrap(),
            current_lease_epoch,
            units: BTreeMap::from([(row.uuid, unit)]),
            rowless_units: BTreeMap::new(),
            triggers,
            advisory_return_attestations: Vec::new(),
        }
    }

    fn unit_name(row: &RowSeed) -> String {
        format!("tally-job-{}.service", row.uuid)
    }

    fn exit_fact(row: &RowSeed, attempt: u32, lease_epoch: u64, loaded: bool) -> LocalUnitFact {
        let record = UnitExitRecord {
            schema_version: crate::executor::UNIT_EXIT_SCHEMA_VERSION,
            unit: unit_name(row),
            invocation_id: format!("invocation-{attempt}"),
            attempt,
            lease_epoch,
            service_result: "success".to_owned(),
            exit_code: Some("exited".to_owned()),
            exit_status: Some("0".to_owned()),
        };
        LocalUnitFact {
            unit: record.unit.clone(),
            loaded,
            state: LocalUnitState::Exited,
            invocation_id: Some(record.invocation_id.clone()),
            attempt: Some(attempt),
            lease_epoch: Some(lease_epoch),
            exit_record: Some(record),
        }
    }

    #[derive(Clone)]
    struct ProbeTransport {
        calls: Arc<AtomicUsize>,
    }

    impl RemoteTransport for ProbeTransport {
        fn call<'a>(
            &'a self,
            _config: &'a SshExecutorConfig,
            request: RemoteExecutorRequest,
        ) -> Pin<
            Box<dyn Future<Output = Result<RemoteExecutorReply, RemoteTransportError>> + Send + 'a>,
        > {
            let calls = self.calls.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let RemoteExecutorRequest::Probe { identity, .. } = request else {
                    return Err(RemoteTransportError {
                        detail: "expected a recovery probe".to_owned(),
                    });
                };
                Ok(RemoteExecutorReply::Ok {
                    protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                    result: Box::new(RemoteExecutorResult::Fact(LocalUnitFact::absent(format!(
                        "tally-job-{}.service",
                        identity.unit_uuid()
                    )))),
                })
            })
        }
    }

    fn executor_with_probe_transport(state_dir: &Path, calls: Arc<AtomicUsize>) -> Executor {
        Executor::new(state_dir, "/bin/tally")
            .with_remote_executors(BTreeMap::from([(
                "worker".to_owned(),
                ExecutionTargetConfig::Ssh(SshExecutorConfig {
                    host: "worker.example".to_owned(),
                    user: "tally-worker".to_owned(),
                    port: 22,
                    ssh_program: PathBuf::from("/bin/ssh"),
                    identity_file: PathBuf::from("/key"),
                    known_hosts_file: PathBuf::from("/known-hosts"),
                    program: PathBuf::from("/bin/tally"),
                    state_dir: PathBuf::from("/remote-state"),
                    connect_timeout_sec: 1,
                    server_alive_interval_sec: 1,
                    server_alive_count_max: 1,
                    retry_interval_ms: 10,
                }),
            )]))
            .with_remote_transport(ProbeTransport { calls })
    }

    #[tokio::test]
    async fn terminal_remote_history_is_not_a_permanent_startup_dependency() {
        let temp = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let executor = executor_with_probe_transport(temp.path(), calls.clone());

        let mut terminal = row(Uuid::new_v4(), "worker", 3);
        terminal.executor = Some("worker".to_owned());
        let terminal_facts = DurableRecoveryFacts::from_verified(
            vec![event(terminal.clone())],
            witness(&terminal, &[(Verdict::Pass, 3)]),
        )
        .unwrap();
        let facts = collect_local_unit_facts(&executor, &terminal_facts)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(facts[&terminal.uuid].state, LocalUnitState::Absent);

        let mut retryable = row(Uuid::new_v4(), "worker", 3);
        retryable.executor = Some("worker".to_owned());
        let retryable_facts = DurableRecoveryFacts::from_verified(
            vec![event(retryable.clone())],
            witness(&retryable, &[(Verdict::PoolVanished, 3)]),
        )
        .unwrap();
        collect_local_unit_facts(&executor, &retryable_facts)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut explicitly_retried = row(Uuid::new_v4(), "worker", 3);
        explicitly_retried.executor = Some("worker".to_owned());
        let records = witness(&explicitly_retried, &[(Verdict::Failed, 3)]);
        let mut retried_event = event(explicitly_retried.clone());
        retried_event.retries.push(DurableRetry {
            attempt: 2,
            previous_witness_seq: records[0].seq,
        });
        let retried_facts =
            DurableRecoveryFacts::from_verified(vec![retried_event], records).unwrap();
        collect_local_unit_facts(&executor, &retried_facts)
            .await
            .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a pending explicit retry must probe its remote execution unit"
        );
    }

    #[test]
    fn confirmed_pool_return_represents_same_row_with_recovered_attempt() {
        let row = row(Uuid::new_v4(), "worker-gpu", 3);
        let witness = witness(&row, &[(Verdict::PoolVanished, 3)]);
        let mut triggers = empty_triggers();
        triggers.confirmed_pool_returns.extend(row.pools.clone());
        let mut facts = facts(
            row.clone(),
            witness,
            LocalUnitFact::absent(unit_name(&row)),
            4,
            triggers,
        );
        facts
            .advisory_return_attestations
            .push(AdvisoryReturnAttestation {
                pool: row.pools[0].clone(),
                payload: serde_json::json!({"assessment": "advisory only"}),
            });

        let plan = recover(&facts, retry_policy(true, 3)).unwrap();
        assert_eq!(plan.witness_lsn, 1);
        assert_eq!(plan.rows.len(), 1);
        assert_eq!(plan.rows[0].row.uuid, row.uuid);
        assert_eq!(plan.rows[0].row.attempt, 2);
        assert_eq!(plan.rows[0].row.lease_epoch, 4);
        assert_eq!(plan.rows[0].labor_class, LaborClass::Recovered);
        assert_eq!(plan.rows[0].state, RecoveryRowState::Pending);
        assert!(matches!(
            &plan.actions[..],
            [RecoveryAction::RePresent {
                row: represented,
                trigger: RetryTrigger::PoolReturn,
                previous_witness_seq: 1,
                previous_attempt: 1,
                previous_lease_epoch: 3,
            }] if represented.uuid == row.uuid && represented.attempt == 2
        ));
        assert_eq!(
            plan.lease_epoch_fences,
            [LeaseEpochFence {
                identity: RecoveryIdentity::Task(row.uuid),
                stale_epoch: 3,
                current_epoch: 4,
            }]
        );
        assert_eq!(plan.advisory_return_attestations.len(), 1);
        assert!(plan.advisory_return_attestations[0].no_enqueue());
        assert_eq!(
            plan.actions.len(),
            1,
            "advisory result must not fan out work"
        );
    }

    #[test]
    fn caller_policy_and_bound_gate_every_retry() {
        let row = row(Uuid::new_v4(), "worker", 5);
        let records = witness(&row, &[(Verdict::PoolVanished, 5)]);
        let mut triggers = empty_triggers();
        triggers.confirmed_pool_returns.extend(row.pools.clone());
        let manual = facts(
            row.clone(),
            records.clone(),
            LocalUnitFact::absent(unit_name(&row)),
            5,
            triggers.clone(),
        );
        let plan = recover(&manual, retry_policy(false, 2)).unwrap();
        assert!(matches!(
            plan.actions.as_slice(),
            [RecoveryAction::AwaitRetry {
                disposition: RetryDisposition::Manual(RetryTrigger::PoolReturn),
                trigger_observed: true,
                ..
            }]
        ));
        assert_eq!(plan.rows[0].state, RecoveryRowState::Completed);

        let bounded = facts(
            row.clone(),
            records,
            LocalUnitFact::absent(unit_name(&row)),
            5,
            triggers,
        );
        let plan = recover(&bounded, retry_policy(true, 1)).unwrap();
        assert!(matches!(
            plan.actions.as_slice(),
            [RecoveryAction::RetryExhausted {
                last_attempt: 1,
                max_attempts: 1,
                ..
            }]
        ));
        assert_eq!(plan.rows[0].state, RecoveryRowState::Completed);
    }

    #[test]
    fn retry_verdicts_require_their_distinct_caller_supplied_trigger() {
        for (verdict, expected_trigger) in [
            (Verdict::PoolVanished, RetryTrigger::PoolReturn),
            (Verdict::Preempted, RetryTrigger::ResourceReturn),
            (Verdict::RuntimeExceeded, RetryTrigger::BoundedRequeue),
        ] {
            let row = row(Uuid::new_v4(), "resource", 7);
            let mut triggers = empty_triggers();
            match expected_trigger {
                RetryTrigger::PoolReturn => {
                    triggers.confirmed_pool_returns.extend(row.pools.clone());
                }
                RetryTrigger::ResourceReturn => {
                    triggers.resource_returns.extend(row.pools.clone());
                }
                RetryTrigger::BoundedRequeue => {
                    triggers.bounded_requeues.insert(row.uuid);
                }
            }
            let facts = facts(
                row.clone(),
                witness(&row, &[(verdict, 7)]),
                LocalUnitFact::absent(unit_name(&row)),
                7,
                triggers,
            );
            let plan = recover(&facts, retry_policy(true, 2)).unwrap();
            assert!(matches!(
                plan.actions.as_slice(),
                [RecoveryAction::RePresent { trigger, .. }] if *trigger == expected_trigger
            ));
        }
    }

    #[test]
    fn terminal_business_outcomes_never_represent() {
        for verdict in [
            Verdict::Pass,
            Verdict::CleanExitNoArtifact,
            Verdict::Failed,
            Verdict::Cancelled,
            Verdict::Reused,
        ] {
            let row = row(Uuid::new_v4(), "resource", 2);
            let mut triggers = empty_triggers();
            triggers.confirmed_pool_returns.extend(row.pools.clone());
            triggers.resource_returns.extend(row.pools.clone());
            triggers.bounded_requeues.insert(row.uuid);
            let facts = facts(
                row.clone(),
                witness(&row, &[(verdict, 2)]),
                LocalUnitFact::absent(unit_name(&row)),
                2,
                triggers,
            );
            let plan = recover(&facts, retry_policy(true, 9)).unwrap();
            assert!(plan.actions.is_empty());
            assert_eq!(
                plan.rows[0].state,
                if verdict == Verdict::Cancelled {
                    RecoveryRowState::Deleted
                } else {
                    RecoveryRowState::Completed
                }
            );
        }
    }

    #[test]
    fn surviving_retry_is_adopted_even_when_current_policy_is_manual() {
        let row = row(Uuid::new_v4(), "worker", 3);
        let records = witness(&row, &[(Verdict::PoolVanished, 3)]);
        let running = LocalUnitFact {
            unit: unit_name(&row),
            loaded: true,
            state: LocalUnitState::Running,
            invocation_id: Some("surviving-invocation".to_owned()),
            attempt: Some(2),
            lease_epoch: Some(4),
            exit_record: None,
        };
        let facts = facts(row.clone(), records, running, 4, empty_triggers());
        let plan = recover(&facts, retry_policy(false, 1)).unwrap();
        assert!(matches!(
            plan.actions.as_slice(),
            [RecoveryAction::AdoptRunning {
                identity: RecoveryIdentity::Task(task_uuid),
                attempt: 2,
                lease_epoch: 4,
                labor_class: Some(LaborClass::Recovered),
                ..
            }] if *task_uuid == row.uuid
        ));
        assert_eq!(plan.rows[0].state, RecoveryRowState::AdoptedRunning);
        assert_eq!(plan.rows[0].labor_class, LaborClass::Recovered);
    }

    #[test]
    fn durable_exit_is_reconciled_and_stale_exit_cannot_replay() {
        let row = row(Uuid::new_v4(), "worker", 3);
        let first = facts(
            row.clone(),
            Vec::new(),
            exit_fact(&row, 1, 4, false),
            4,
            empty_triggers(),
        );
        let first_plan = recover(&first, retry_policy(true, 3)).unwrap();
        assert!(matches!(
            first_plan.actions.as_slice(),
            [RecoveryAction::ReconcileExit { record, .. }] if record.attempt == 1
        ));
        assert_eq!(
            first_plan.rows[0].state,
            RecoveryRowState::AwaitingReconciliation
        );

        let mut triggers = empty_triggers();
        triggers.confirmed_pool_returns.extend(row.pools.clone());
        let stale = facts(
            row.clone(),
            witness(&row, &[(Verdict::PoolVanished, 3)]),
            exit_fact(&row, 1, 3, false),
            4,
            triggers,
        );
        let stale_plan = recover(&stale, retry_policy(true, 3)).unwrap();
        assert!(matches!(
            stale_plan.actions.as_slice(),
            [RecoveryAction::RePresent { row, .. }] if row.attempt == 2
        ));
    }

    #[test]
    fn loaded_finished_unit_waits_for_collection_and_missing_record_fails_closed() {
        let row = row(Uuid::new_v4(), "worker", 3);
        let records = witness(&row, &[(Verdict::PoolVanished, 3)]);
        let loaded = facts(
            row.clone(),
            records.clone(),
            exit_fact(&row, 1, 3, true),
            4,
            empty_triggers(),
        );
        let plan = recover(&loaded, retry_policy(true, 3)).unwrap();
        assert!(matches!(
            plan.actions.as_slice(),
            [RecoveryAction::AwaitUnitCollection { .. }]
        ));

        let incomplete = LocalUnitFact {
            unit: unit_name(&row),
            loaded: true,
            state: LocalUnitState::InactiveWithoutRecord,
            invocation_id: Some("incomplete".to_owned()),
            attempt: None,
            lease_epoch: None,
            exit_record: None,
        };
        let facts = facts(row, records, incomplete, 4, empty_triggers());
        assert!(recover(&facts, retry_policy(true, 3)).is_err());
    }

    #[test]
    fn rowless_survivors_use_job_identity_for_adoption_and_reconciliation() {
        let job_uuid = Uuid::new_v4();
        let unit = format!("tally-job-{job_uuid}.service");
        let running = LocalUnitFact {
            unit: unit.clone(),
            loaded: true,
            state: LocalUnitState::Running,
            invocation_id: Some("rowless-running".to_owned()),
            attempt: Some(1),
            lease_epoch: Some(3),
            exit_record: None,
        };
        let durable = DurableRecoveryFacts::from_verified(Vec::new(), Vec::new()).unwrap();
        let running_facts = RecoveryFacts {
            durable: durable.clone(),
            current_lease_epoch: 4,
            units: BTreeMap::new(),
            rowless_units: BTreeMap::from([(job_uuid, running)]),
            triggers: empty_triggers(),
            advisory_return_attestations: Vec::new(),
        };
        let plan = recover(&running_facts, retry_policy(false, 1)).unwrap();
        assert!(plan.rows.is_empty());
        assert!(matches!(
            plan.actions.as_slice(),
            [RecoveryAction::AdoptRunning {
                identity: RecoveryIdentity::Job(identity),
                unit: observed_unit,
                labor_class: None,
                ..
            }] if *identity == job_uuid && observed_unit == &unit
        ));
        assert_eq!(
            plan.lease_epoch_fences,
            [LeaseEpochFence {
                identity: RecoveryIdentity::Job(job_uuid),
                stale_epoch: 3,
                current_epoch: 4,
            }]
        );

        let record = UnitExitRecord {
            schema_version: crate::executor::UNIT_EXIT_SCHEMA_VERSION,
            unit: unit.clone(),
            invocation_id: "rowless-exited".to_owned(),
            attempt: 1,
            lease_epoch: 4,
            service_result: "success".to_owned(),
            exit_code: Some("exited".to_owned()),
            exit_status: Some("0".to_owned()),
        };
        let exited = LocalUnitFact {
            unit,
            loaded: false,
            state: LocalUnitState::Exited,
            invocation_id: Some(record.invocation_id.clone()),
            attempt: Some(record.attempt),
            lease_epoch: Some(record.lease_epoch),
            exit_record: Some(record),
        };
        let exited_facts = RecoveryFacts {
            durable,
            current_lease_epoch: 4,
            units: BTreeMap::new(),
            rowless_units: BTreeMap::from([(job_uuid, exited)]),
            triggers: empty_triggers(),
            advisory_return_attestations: Vec::new(),
        };
        let plan = recover(&exited_facts, retry_policy(false, 1)).unwrap();
        assert!(matches!(
            plan.actions.as_slice(),
            [RecoveryAction::ReconcileExit {
                identity: RecoveryIdentity::Job(identity),
                labor_class: None,
                ..
            }] if *identity == job_uuid
        ));
    }

    #[test]
    fn planner_rejects_structurally_invalid_durable_exit_fact() {
        let row = row(Uuid::new_v4(), "worker", 3);
        let mut invalid = exit_fact(&row, 1, 3, false);
        invalid.exit_record.as_mut().unwrap().schema_version = 999;
        let facts = facts(row, Vec::new(), invalid, 3, empty_triggers());
        assert!(matches!(
            recover(&facts, retry_policy(true, 2)),
            Err(RecoveryError::Executor(ExecutorError::InvalidExitRecord(_)))
        ));
    }

    #[test]
    fn future_epochs_missing_facts_and_duplicate_rows_fail_closed() {
        let row = row(Uuid::new_v4(), "worker", 5);
        let mut future = facts(
            row.clone(),
            Vec::new(),
            LocalUnitFact::absent(unit_name(&row)),
            4,
            empty_triggers(),
        );
        assert!(recover(&future, retry_policy(true, 3)).is_err());
        future.current_lease_epoch = 5;
        future.units.clear();
        assert!(recover(&future, retry_policy(true, 3)).is_err());

        let first = event(row.clone());
        let mut second = first.clone();
        second.event_id = Uuid::new_v4();
        assert!(DurableRecoveryFacts::from_verified(vec![first, second], Vec::new()).is_err());
    }

    #[test]
    fn witnessed_history_rejects_terminal_replay_and_non_recovered_retry() {
        let row = row(Uuid::new_v4(), "worker", 2);
        let replay = witness(&row, &[(Verdict::Pass, 2), (Verdict::Pass, 3)]);
        let replay = facts(
            row.clone(),
            replay,
            LocalUnitFact::absent(unit_name(&row)),
            3,
            empty_triggers(),
        );
        assert!(recover(&replay, retry_policy(true, 3)).is_err());

        let mut wrong_labor = witness(&row, &[(Verdict::RuntimeExceeded, 2), (Verdict::Pass, 3)]);
        wrong_labor[1].labor_class = LaborClass::Fresh;
        let wrong_labor = facts(
            row.clone(),
            wrong_labor,
            LocalUnitFact::absent(unit_name(&row)),
            3,
            empty_triggers(),
        );
        assert!(recover(&wrong_labor, retry_policy(true, 3)).is_err());
    }

    #[test]
    fn explicit_retry_is_a_durable_pending_lane_and_only_non_pass_can_open_it() {
        let row = row(Uuid::new_v4(), "worker", 2);
        let failed = witness(&row, &[(Verdict::Failed, 2)]);
        let mut retried = event(row.clone());
        retried.retries.push(DurableRetry {
            attempt: 2,
            previous_witness_seq: failed[0].seq,
        });
        let facts = RecoveryFacts {
            durable: DurableRecoveryFacts::from_verified(vec![retried.clone()], failed.clone())
                .unwrap(),
            current_lease_epoch: 3,
            units: BTreeMap::from([(row.uuid, LocalUnitFact::absent(unit_name(&row)))]),
            rowless_units: BTreeMap::new(),
            triggers: empty_triggers(),
            advisory_return_attestations: Vec::new(),
        };
        let plan = recover(&facts, retry_policy(false, 1)).unwrap();
        assert!(matches!(
            plan.actions.as_slice(),
            [RecoveryAction::QueueExisting {
                task_uuid,
                attempt: 2,
                lease_epoch: 3,
            }] if *task_uuid == row.uuid
        ));
        assert_eq!(plan.rows[0].row.attempt, 2);
        assert_eq!(plan.rows[0].state, RecoveryRowState::Pending);
        assert_eq!(plan.rows[0].labor_class, LaborClass::Recovered);

        let settled = witness(
            &row,
            &[(Verdict::Failed, 2), (Verdict::CleanExitNoArtifact, 3)],
        );
        let settled_facts = RecoveryFacts {
            durable: DurableRecoveryFacts::from_verified(vec![retried], settled).unwrap(),
            current_lease_epoch: 3,
            units: BTreeMap::from([(row.uuid, LocalUnitFact::absent(unit_name(&row)))]),
            rowless_units: BTreeMap::new(),
            triggers: empty_triggers(),
            advisory_return_attestations: Vec::new(),
        };
        let settled_plan = recover(&settled_facts, retry_policy(false, 1)).unwrap();
        assert!(settled_plan.actions.is_empty());
        assert_eq!(settled_plan.rows[0].row.attempt, 2);
        assert_eq!(settled_plan.rows[0].state, RecoveryRowState::Completed);

        let passed = witness(&row, &[(Verdict::Pass, 2)]);
        let mut invalid = event(row.clone());
        invalid.retries.push(DurableRetry {
            attempt: 2,
            previous_witness_seq: passed[0].seq,
        });
        let invalid_facts = RecoveryFacts {
            durable: DurableRecoveryFacts::from_verified(vec![invalid], passed).unwrap(),
            current_lease_epoch: 2,
            units: BTreeMap::from([(row.uuid, LocalUnitFact::absent(unit_name(&row)))]),
            rowless_units: BTreeMap::new(),
            triggers: empty_triggers(),
            advisory_return_attestations: Vec::new(),
        };
        assert!(matches!(
            recover(&invalid_facts, retry_policy(false, 9)),
            Err(RecoveryError::InvalidFacts(reason))
                if reason.contains("follows a pass verdict")
        ));
    }

    #[test]
    fn durable_collector_verifies_chain_and_ignores_unacknowledged_events() {
        let temp = tempfile::tempdir().unwrap();
        let events_dir = temp.path().join("events");
        let witness_path = temp.path().join("witness.jsonl");
        let durable_row = row(Uuid::new_v4(), "worker", 1);
        write_enqueue_event_atomic(&events_dir, &event(durable_row.clone())).unwrap();
        let mut unacknowledged = event(row(Uuid::new_v4(), "worker", 1));
        unacknowledged.acknowledged = false;
        write_enqueue_event_atomic(&events_dir, &unacknowledged).unwrap();
        let records = witness(&durable_row, &[(Verdict::PoolVanished, 1)]);
        let encoded = records
            .iter()
            .map(|record| serde_json::to_string(record).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(&witness_path, &encoded).unwrap();

        let durable = collect_durable_recovery_facts(&events_dir, &witness_path).unwrap();
        assert_eq!(durable.events().len(), 1);
        assert_eq!(durable.events()[0].row.uuid, durable_row.uuid);
        assert_eq!(durable.witness_lsn(), records[0].seq);

        fs::write(
            &witness_path,
            encoded.replace("pool-vanished", "runtime-exceeded"),
        )
        .unwrap();
        assert!(matches!(
            collect_durable_recovery_facts(&events_dir, &witness_path),
            Err(RecoveryError::InvalidWitness(_))
        ));
    }
}
