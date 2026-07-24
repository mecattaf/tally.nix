use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use taskchampion::Status;
use thiserror::Error;

use crate::completion::{GateSummaryStatus, SemanticCompletion};
use crate::journal::{JournalEntry, TallyEvent};
use crate::taskdb::{GhOrigin, RelatedTrigger, TaskRow, WorkspaceMetadata};
use crate::witness::{counts_toward_canonical_gpu_seconds, LaborClass, Verdict, WitnessRecord};

pub const QUERY_SCHEMA_VERSION: u32 = 1;
pub const QUERY_PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RowStatus {
    Pending,
    Completed,
    Deleted,
    Recurring,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhOriginProjection {
    pub repo: String,
    pub number: u64,
    pub url: String,
}

impl GhOriginProjection {
    pub fn from_origin(origin: &GhOrigin) -> Option<Self> {
        origin.is_current().then(|| Self {
            repo: origin.repo.clone(),
            number: origin.number,
            url: origin.html_url.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RowFact {
    pub task_uuid: String,
    pub description: String,
    pub status: RowStatus,
    pub priority: String,
    #[serde(
        rename = "pool",
        serialize_with = "crate::poolset::serialize_optional",
        deserialize_with = "crate::poolset::deserialize_optional"
    )]
    pub pools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    pub source: Option<String>,
    pub session_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_from: Option<String>,
    #[serde(default = "default_query_attempt")]
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_origin: Option<GhOriginProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_trigger: Option<RelatedTrigger>,
}

const fn default_query_attempt() -> u32 {
    1
}

impl From<&TaskRow> for RowFact {
    fn from(row: &TaskRow) -> Self {
        Self {
            task_uuid: row.uuid.to_string(),
            description: row.description.clone(),
            status: match row.status {
                Status::Pending => RowStatus::Pending,
                Status::Completed => RowStatus::Completed,
                Status::Deleted => RowStatus::Deleted,
                Status::Recurring => RowStatus::Recurring,
                Status::Unknown(_) => RowStatus::Unknown,
            },
            priority: row.priority.clone(),
            pools: row.value("pool").map(|value| {
                crate::poolset::decoded(value).unwrap_or_else(|_| vec![value.to_owned()])
            }),
            executor: row.value("executor").map(ToOwned::to_owned),
            source: row.value("source").map(ToOwned::to_owned),
            session_ref: row.value("session_ref").map(ToOwned::to_owned),
            cwd: row.value("cwd").map(ToOwned::to_owned),
            workspace: row
                .value("workspace_json")
                .and_then(|workspace| serde_json::from_str(workspace).ok()),
            resumed_from: row.value("resumed_from").map(ToOwned::to_owned),
            attempt: row
                .value("attempt")
                .and_then(|attempt| attempt.parse().ok())
                .unwrap_or(1),
            model: row.value("model").map(ToOwned::to_owned),
            gh_origin: row
                .value("gh_origin_json")
                .and_then(|origin| serde_json::from_str::<GhOrigin>(origin).ok())
                .as_ref()
                .and_then(GhOriginProjection::from_origin),
            related_trigger: row
                .value("related_trigger_json")
                .and_then(|related| serde_json::from_str(related).ok()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WindowConsumptionFact {
    pub used: u64,
    pub cap: u64,
    pub reset_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PoolHeadroomFact {
    pub pool: String,
    pub capacity: u64,
    pub held: u64,
    pub queued: usize,
    pub consumption: Option<WindowConsumptionFact>,
    pub meter_utilization_pct: Option<f64>,
    pub weekly_utilization_pct: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeadroomSignal {
    #[serde(rename = "GO")]
    Go,
    #[serde(rename = "SLOW")]
    Slow,
    #[serde(rename = "STOP")]
    Stop,
}

impl HeadroomSignal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Go => "GO",
            Self::Slow => "SLOW",
            Self::Stop => "STOP",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PoolHeadroom {
    pub pool: String,
    pub capacity: u64,
    pub held: u64,
    pub queued: usize,
    pub remaining_capacity: u64,
    pub consumption_used: Option<u64>,
    pub consumption_cap: Option<u64>,
    pub remaining_budget: Option<u64>,
    pub reset_at: Option<String>,
    pub self_utilization_pct: f64,
    pub effective_utilization_pct: f64,
    pub weekly_utilization_pct: Option<f64>,
    pub signal: HeadroomSignal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PoolsView {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub pools: Vec<PoolHeadroom>,
}

#[derive(Debug, Error)]
pub enum QueryError {
    #[error("invalid pool headroom fact: {0}")]
    InvalidPool(String),
    #[error("unknown pool {0:?}")]
    UnknownPool(String),
    #[error("invalid query timestamp {0:?}")]
    InvalidTimestamp(String),
}

pub fn project_pool_headroom(fact: &PoolHeadroomFact) -> Result<PoolHeadroom, QueryError> {
    if fact.pool.trim().is_empty() || fact.pool.chars().any(char::is_control) {
        return Err(QueryError::InvalidPool(
            "pool name must be non-empty and contain no control characters".to_owned(),
        ));
    }
    if fact.capacity == 0 {
        return Err(QueryError::InvalidPool(
            "capacity must be positive".to_owned(),
        ));
    }
    validate_pct("meterUtilizationPct", fact.meter_utilization_pct)?;
    validate_pct("weeklyUtilizationPct", fact.weekly_utilization_pct)?;
    if fact.consumption.is_none()
        && (fact.meter_utilization_pct.is_some() || fact.weekly_utilization_pct.is_some())
    {
        return Err(QueryError::InvalidPool(
            "meter utilization requires a windowed-consumption budget".to_owned(),
        ));
    }

    let remaining_capacity = fact.capacity.saturating_sub(fact.held);
    let capacity_pct = percent(fact.held.min(fact.capacity), fact.capacity);
    let (consumption_used, consumption_cap, self_remaining_budget, reset_at, self_pct) =
        if let Some(window) = &fact.consumption {
            if window.cap == 0 {
                return Err(QueryError::InvalidPool(
                    "consumption cap must be positive".to_owned(),
                ));
            }
            let used = window.used.min(window.cap);
            (
                Some(window.used),
                Some(window.cap),
                Some(window.cap.saturating_sub(window.used)),
                window.reset_at.clone(),
                percent(used, window.cap),
            )
        } else {
            (None, None, None, None, capacity_pct)
        };
    let effective_pct = self_pct.max(fact.meter_utilization_pct.unwrap_or(0.0));
    let remaining_budget = match (self_remaining_budget, consumption_cap) {
        (Some(self_remaining), Some(cap)) => {
            let meter_remaining = fact
                .meter_utilization_pct
                .map(|pct| remaining_from_percent(cap, pct))
                .unwrap_or(cap);
            Some(self_remaining.min(meter_remaining))
        }
        _ => None,
    };
    let mut signal = if effective_pct >= 90.0 {
        HeadroomSignal::Stop
    } else if effective_pct >= 70.0 {
        HeadroomSignal::Slow
    } else {
        HeadroomSignal::Go
    };
    if signal == HeadroomSignal::Go && fact.weekly_utilization_pct.is_some_and(|pct| pct >= 80.0) {
        signal = HeadroomSignal::Slow;
    }
    if remaining_capacity == 0 {
        signal = HeadroomSignal::Stop;
    }
    Ok(PoolHeadroom {
        pool: fact.pool.clone(),
        capacity: fact.capacity,
        held: fact.held,
        queued: fact.queued,
        remaining_capacity,
        consumption_used,
        consumption_cap,
        remaining_budget,
        reset_at,
        self_utilization_pct: self_pct,
        effective_utilization_pct: effective_pct,
        weekly_utilization_pct: fact.weekly_utilization_pct,
        signal,
    })
}

fn validate_pct(name: &str, value: Option<f64>) -> Result<(), QueryError> {
    if value.is_some_and(|value| !value.is_finite() || !(0.0..=100.0).contains(&value)) {
        return Err(QueryError::InvalidPool(format!(
            "{name} must be finite and between 0 and 100"
        )));
    }
    Ok(())
}

fn percent(used: u64, cap: u64) -> f64 {
    (used as f64 / cap as f64) * 100.0
}

fn remaining_from_percent(cap: u64, utilization_pct: f64) -> u64 {
    ((cap as f64 * (100.0 - utilization_pct) / 100.0).floor() as u64).min(cap)
}

pub fn query_pools(pool_facts: &[PoolHeadroomFact]) -> Result<PoolsView, QueryError> {
    let mut pools = pool_facts
        .iter()
        .map(project_pool_headroom)
        .collect::<Result<Vec<_>, _>>()?;
    pools.sort_by(|left, right| left.pool.cmp(&right.pool));
    Ok(PoolsView {
        schema_version: QUERY_SCHEMA_VERSION,
        protocol_version: QUERY_PROTOCOL_VERSION,
        pools,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct JobProjection {
    pub anchor: String,
    pub task_uuid: Option<String>,
    pub description: Option<String>,
    #[serde(
        rename = "pool",
        serialize_with = "crate::poolset::serialize_optional",
        deserialize_with = "crate::poolset::deserialize_optional"
    )]
    pub pools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    pub source: Option<String>,
    pub session_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_origin: Option<GhOriginProjection>,
    pub state: String,
    pub verdict: Option<Verdict>,
    pub gpu_seconds: Option<f64>,
    pub canonical_gpu_seconds: Option<f64>,
    pub last_event_at: Option<String>,
    pub witness_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<SemanticCompletion>,
}

#[derive(Debug, Clone)]
struct ProjectedJob {
    output: JobProjection,
    row_status: Option<RowStatus>,
    last_realtime_us: Option<u64>,
    saw_journal: bool,
    evidence_fail_attempts: BTreeSet<u32>,
    last_attempt: Option<u32>,
    witness_attempt: Option<u32>,
    labor_class: Option<LaborClass>,
}

pub fn project_jobs(
    rows: &[RowFact],
    journal: &[JournalEntry],
    witness: &[WitnessRecord],
) -> Vec<JobProjection> {
    project_job_details(rows, journal, witness)
        .into_values()
        .map(|job| job.output)
        .collect()
}

fn project_job_details(
    rows: &[RowFact],
    journal: &[JournalEntry],
    witness: &[WitnessRecord],
) -> BTreeMap<String, ProjectedJob> {
    let mut jobs = BTreeMap::new();
    for row in rows {
        jobs.insert(
            row.task_uuid.clone(),
            ProjectedJob {
                output: JobProjection {
                    anchor: row.task_uuid.clone(),
                    task_uuid: Some(row.task_uuid.clone()),
                    description: Some(row.description.clone()),
                    pools: row.pools.clone(),
                    executor: row.executor.clone(),
                    source: row.source.clone(),
                    session_ref: row.session_ref.clone(),
                    cwd: row.cwd.clone(),
                    workspace: row.workspace.clone(),
                    resumed_from: row.resumed_from.clone(),
                    model: row.model.clone(),
                    gh_origin: row.gh_origin.clone(),
                    state: row_status_name(row.status).to_owned(),
                    verdict: None,
                    gpu_seconds: None,
                    canonical_gpu_seconds: None,
                    last_event_at: None,
                    witness_seq: None,
                    completion: None,
                },
                row_status: Some(row.status),
                last_realtime_us: None,
                saw_journal: false,
                evidence_fail_attempts: BTreeSet::new(),
                last_attempt: Some(row.attempt),
                witness_attempt: None,
                labor_class: None,
            },
        );
    }
    for entry in journal {
        let anchor = entry.fields.task_uuid.clone();
        let job = jobs.entry(anchor.clone()).or_insert_with(|| ProjectedJob {
            output: JobProjection {
                anchor: anchor.clone(),
                task_uuid: Some(anchor),
                description: None,
                pools: None,
                executor: None,
                source: None,
                session_ref: None,
                cwd: None,
                workspace: None,
                resumed_from: None,
                model: None,
                gh_origin: None,
                state: "observed".to_owned(),
                verdict: None,
                gpu_seconds: None,
                canonical_gpu_seconds: None,
                last_event_at: None,
                witness_seq: None,
                completion: None,
            },
            row_status: None,
            last_realtime_us: None,
            saw_journal: false,
            evidence_fail_attempts: BTreeSet::new(),
            last_attempt: None,
            witness_attempt: None,
            labor_class: None,
        });
        job.saw_journal = true;
        if entry.fields.event == TallyEvent::EvidenceFail {
            if let Some(attempt) = entry.fields.attempt {
                job.evidence_fail_attempts.insert(attempt);
            }
        }
        if let Some(attempt) = entry.fields.attempt {
            job.last_attempt = Some(job.last_attempt.map_or(attempt, |seen| seen.max(attempt)));
        }
        if job.last_realtime_us <= entry.realtime_us {
            job.output.state = entry.fields.event.to_string();
            job.output.pools = entry.fields.pools.clone().or(job.output.pools.take());
            job.output.executor = entry.fields.executor.clone().or(job.output.executor.take());
            if job.output.source.is_none() {
                job.output.source = Some(source_name(entry.fields.source).to_owned());
            }
            job.output.session_ref = entry
                .fields
                .session_ref
                .clone()
                .or(job.output.session_ref.take());
            job.output.last_event_at = entry.realtime_us.and_then(timestamp_from_micros);
            job.last_realtime_us = entry.realtime_us;
        }
    }
    for record in witness {
        let anchor = record
            .task_uuid
            .clone()
            .unwrap_or_else(|| format!("witness:{}", record.seq));
        let job = jobs.entry(anchor.clone()).or_insert_with(|| ProjectedJob {
            output: JobProjection {
                anchor: anchor.clone(),
                task_uuid: record.task_uuid.clone(),
                description: None,
                pools: None,
                executor: record.executor.clone(),
                source: None,
                session_ref: None,
                cwd: None,
                workspace: None,
                resumed_from: None,
                model: record.model.clone(),
                gh_origin: None,
                state: "witnessed".to_owned(),
                verdict: None,
                gpu_seconds: None,
                canonical_gpu_seconds: None,
                last_event_at: None,
                witness_seq: None,
                completion: None,
            },
            row_status: None,
            last_realtime_us: None,
            saw_journal: false,
            evidence_fail_attempts: BTreeSet::new(),
            last_attempt: None,
            witness_attempt: None,
            labor_class: None,
        });
        if job
            .last_attempt
            .is_some_and(|attempt| attempt > record.attempt)
        {
            continue;
        }
        if job.output.witness_seq.is_some_and(|seq| seq > record.seq) {
            continue;
        }
        job.output.task_uuid = record.task_uuid.clone();
        job.output.pools = record.pools.clone().or(job.output.pools.take());
        job.output.executor = record.executor.clone().or(job.output.executor.take());
        job.output.session_ref = record.trace_ref.clone().or(job.output.session_ref.take());
        job.output.model = record.model.clone().or(job.output.model.take());
        job.output.state = "terminal".to_owned();
        job.output.verdict = Some(record.verdict);
        job.output.gpu_seconds = record.gpu_seconds;
        job.output.canonical_gpu_seconds = counts_toward_canonical_gpu_seconds(record)
            .then_some(record.gpu_seconds)
            .flatten();
        job.output.last_event_at = Some(record.transition_timestamp.clone());
        job.output.witness_seq = Some(record.seq);
        job.output.completion.clone_from(&record.completion);
        job.witness_attempt = Some(record.attempt);
        job.labor_class = Some(record.labor_class);
    }
    jobs
}

fn row_status_name(status: RowStatus) -> &'static str {
    match status {
        RowStatus::Pending => "pending",
        RowStatus::Completed => "completed-cache",
        RowStatus::Deleted => "deleted-cache",
        RowStatus::Recurring => "recurring",
        RowStatus::Unknown => "unknown-cache",
    }
}

fn source_name(source: crate::taskdb::EnqueueSource) -> &'static str {
    use crate::taskdb::EnqueueSource;
    match source {
        EnqueueSource::Manual => "manual",
        EnqueueSource::Orchestrator => "orchestrator",
        EnqueueSource::Calendar => "calendar",
        EnqueueSource::EventsDir => "events-dir",
        EnqueueSource::Gh => "gh",
        EnqueueSource::BuildEffect => "build-effect",
        EnqueueSource::PoolReachability => "pool-reachability",
    }
}

fn timestamp_from_micros(micros: u64) -> Option<String> {
    i64::try_from(micros)
        .ok()
        .and_then(DateTime::<Utc>::from_timestamp_micros)
        .map(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Micros, true))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StatusView {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub pools: Vec<PoolHeadroom>,
    pub jobs: Vec<JobProjection>,
}

pub fn query_status(
    pool_facts: &[PoolHeadroomFact],
    pool_filter: Option<&str>,
    rows: &[RowFact],
    journal: &[JournalEntry],
    witness: &[WitnessRecord],
) -> Result<StatusView, QueryError> {
    let mut pools = pool_facts
        .iter()
        .filter(|fact| pool_filter.is_none_or(|pool| fact.pool == pool))
        .map(project_pool_headroom)
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(pool) = pool_filter {
        if pools.is_empty() {
            return Err(QueryError::UnknownPool(pool.to_owned()));
        }
    }
    pools.sort_by(|left, right| left.pool.cmp(&right.pool));
    Ok(StatusView {
        schema_version: QUERY_SCHEMA_VERSION,
        protocol_version: QUERY_PROTOCOL_VERSION,
        pools,
        jobs: project_jobs(rows, journal, witness),
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RenderScope {
    #[default]
    All,
    Queue,
    Witness,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RenderView {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub scope: RenderScope,
    pub jobs: Vec<JobProjection>,
}

pub fn query_render(
    scope: RenderScope,
    rows: &[RowFact],
    journal: &[JournalEntry],
    witness: &[WitnessRecord],
) -> RenderView {
    let mut jobs = project_jobs(rows, journal, witness);
    match scope {
        RenderScope::All => {}
        RenderScope::Queue => jobs.retain(|job| job.witness_seq.is_none()),
        RenderScope::Witness => jobs.retain(|job| job.witness_seq.is_some()),
    }
    RenderView {
        schema_version: QUERY_SCHEMA_VERSION,
        protocol_version: QUERY_PROTOCOL_VERSION,
        scope,
        jobs,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogOrigin {
    Journal,
    Witness,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LogRecord {
    pub origin: LogOrigin,
    pub timestamp: Option<String>,
    pub event: TallyEvent,
    pub task_uuid: Option<String>,
    pub session_ref: Option<String>,
    #[serde(
        rename = "pool",
        serialize_with = "crate::poolset::serialize_optional",
        deserialize_with = "crate::poolset::deserialize_optional"
    )]
    pub pools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    pub source: Option<String>,
    pub gpu_seconds: Option<f64>,
    pub artifact_hash: Option<String>,
    pub verdict: Option<Verdict>,
    pub message: Option<String>,
    pub witness_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_class: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogFilter {
    pub task: Option<String>,
    pub session: Option<String>,
    pub event: Option<TallyEvent>,
    pub source: Option<String>,
    pub since: Option<String>,
}

pub fn query_log(
    rows: &[RowFact],
    journal: &[JournalEntry],
    witness: &[WitnessRecord],
    filter: &LogFilter,
) -> Result<Vec<LogRecord>, QueryError> {
    let since = filter.since.as_deref().map(parse_timestamp).transpose()?;
    let row_by_task = rows
        .iter()
        .map(|row| (row.task_uuid.as_str(), row))
        .collect::<HashMap<_, _>>();
    let mut records = Vec::<(i128, u8, u64, LogRecord)>::new();
    for (index, entry) in journal.iter().enumerate() {
        let row = row_by_task.get(entry.fields.task_uuid.as_str()).copied();
        let timestamp = entry.realtime_us.and_then(timestamp_from_micros);
        let record = LogRecord {
            origin: LogOrigin::Journal,
            timestamp,
            event: entry.fields.event,
            task_uuid: Some(entry.fields.task_uuid.clone()),
            session_ref: entry
                .fields
                .session_ref
                .clone()
                .or_else(|| row.and_then(|row| row.session_ref.clone())),
            pools: entry
                .fields
                .pools
                .clone()
                .or_else(|| row.and_then(|row| row.pools.clone())),
            executor: entry
                .fields
                .executor
                .clone()
                .or_else(|| row.and_then(|row| row.executor.clone())),
            source: Some(source_name(entry.fields.source).to_owned()),
            gpu_seconds: entry.fields.gpu_seconds,
            artifact_hash: entry.fields.artifact_hash.clone(),
            verdict: None,
            message: Some(entry.fields.message.clone()),
            witness_seq: None,
            evidence_class: None,
            manifest_hash: None,
        };
        if log_matches(&record, filter, since) {
            records.push((
                entry.realtime_us.map_or(i128::MIN, i128::from),
                0,
                index as u64,
                record,
            ));
        }
    }
    for record in witness {
        let row = record
            .task_uuid
            .as_deref()
            .and_then(|task| row_by_task.get(task).copied());
        let parsed = parse_timestamp(&record.transition_timestamp)?;
        let output = LogRecord {
            origin: LogOrigin::Witness,
            timestamp: Some(record.transition_timestamp.clone()),
            event: TallyEvent::WitnessEmitted,
            task_uuid: record.task_uuid.clone(),
            session_ref: record
                .trace_ref
                .clone()
                .or_else(|| row.and_then(|row| row.session_ref.clone())),
            pools: record
                .pools
                .clone()
                .or_else(|| row.and_then(|row| row.pools.clone())),
            executor: record
                .executor
                .clone()
                .or_else(|| row.and_then(|row| row.executor.clone())),
            source: row.and_then(|row| row.source.clone()),
            gpu_seconds: record.gpu_seconds,
            artifact_hash: record.artifact_content_hash.clone(),
            verdict: Some(record.verdict),
            message: None,
            witness_seq: Some(record.seq),
            evidence_class: record.evidence_class.clone(),
            manifest_hash: record.manifest_hash.clone(),
        };
        if log_matches(&output, filter, since) {
            records.push((parsed.timestamp_micros() as i128, 1, record.seq, output));
        }
    }
    records.sort_by_key(|record| (record.0, record.1, record.2));
    Ok(records
        .into_iter()
        .map(|(_, _, _, record)| record)
        .collect())
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, QueryError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|_| QueryError::InvalidTimestamp(value.to_owned()))
}

fn log_matches(record: &LogRecord, filter: &LogFilter, since: Option<DateTime<Utc>>) -> bool {
    if filter
        .task
        .as_deref()
        .is_some_and(|task| record.task_uuid.as_deref() != Some(task))
    {
        return false;
    }
    if filter
        .session
        .as_deref()
        .is_some_and(|session| record.session_ref.as_deref() != Some(session))
    {
        return false;
    }
    if filter.event.is_some_and(|event| record.event != event) {
        return false;
    }
    if filter
        .source
        .as_deref()
        .is_some_and(|source| record.source.as_deref() != Some(source))
    {
        return false;
    }
    if let Some(since) = since {
        let Some(timestamp) = record
            .timestamp
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        else {
            return false;
        };
        if timestamp < since {
            return false;
        }
    }
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CompletedEntry {
    pub task_uuid: Option<String>,
    pub gpu_seconds: Option<f64>,
    pub verdict: Verdict,
    pub session_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_origin: Option<GhOriginProjection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct InFlightEntry {
    pub task_uuid: Option<String>,
    pub session_ref: Option<String>,
    pub state: String,
    pub last_event_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_origin: Option<GhOriginProjection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StandupDigest {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub window: StandupWindow,
    pub completed: Vec<CompletedEntry>,
    pub in_flight: Vec<InFlightEntry>,
    pub reused: usize,
    pub gate_fails: Vec<CompletedEntry>,
    pub cancelled: Vec<CompletedEntry>,
    pub canonical_gpu_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StandupWindow {
    pub since: Option<String>,
    pub until: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandupOptions {
    pub since: Option<String>,
    pub since_realtime_us: Option<u64>,
    pub until: String,
    pub source: Option<String>,
}

pub fn query_standup(
    rows: &[RowFact],
    journal: &[JournalEntry],
    witness: &[WitnessRecord],
    options: &StandupOptions,
) -> StandupDigest {
    let filtered_journal = journal
        .iter()
        .filter(|entry| {
            options
                .since_realtime_us
                .is_none_or(|since| entry.realtime_us.is_none_or(|seen| seen >= since))
        })
        .cloned()
        .collect::<Vec<_>>();
    let details = project_job_details(rows, &filtered_journal, witness);
    let source_by_task = rows
        .iter()
        .filter_map(|row| {
            row.source
                .as_deref()
                .map(|source| (row.task_uuid.as_str(), source))
        })
        .collect::<HashMap<_, _>>();
    let gh_origin_by_task = rows
        .iter()
        .filter_map(|row| {
            row.gh_origin
                .as_ref()
                .map(|origin| (row.task_uuid.as_str(), origin))
        })
        .collect::<HashMap<_, _>>();
    let mut completed = Vec::new();
    let mut gate_fails = Vec::new();
    let mut cancelled = Vec::new();
    let mut in_flight = Vec::new();
    let mut reused = 0;
    let canonical_gpu_seconds = witness
        .iter()
        .filter(|record| counts_toward_canonical_gpu_seconds(record))
        .filter(|record| {
            options.source.as_deref().is_none_or(|source| {
                record
                    .task_uuid
                    .as_deref()
                    .and_then(|task| source_by_task.get(task).copied())
                    == Some(source)
            })
        })
        .filter_map(|record| record.gpu_seconds)
        .sum();
    for job in details.into_values() {
        if options
            .source
            .as_deref()
            .is_some_and(|source| job.output.source.as_deref() != Some(source))
        {
            continue;
        }
        let saw_evidence_fail = job
            .witness_attempt
            .or(job.last_attempt)
            .is_some_and(|attempt| job.evidence_fail_attempts.contains(&attempt));
        let semantic_gate_failed = job
            .output
            .completion
            .as_ref()
            .is_some_and(|completion| completion.gates.status == GateSummaryStatus::Fail);
        let gh_origin = job
            .output
            .task_uuid
            .as_deref()
            .and_then(|task| gh_origin_by_task.get(task).copied())
            .cloned();
        if let Some(verdict) = job.output.verdict {
            if job.labor_class == Some(LaborClass::Reused) {
                reused += 1;
            }
            let entry = CompletedEntry {
                task_uuid: job.output.task_uuid,
                gpu_seconds: job.output.gpu_seconds,
                verdict,
                session_ref: job.output.session_ref,
                gh_origin,
            };
            if verdict == Verdict::Cancelled {
                cancelled.push(entry);
            } else if verdict == Verdict::CleanExitNoArtifact
                || saw_evidence_fail
                || semantic_gate_failed
            {
                gate_fails.push(entry);
            } else {
                completed.push(entry);
            }
            continue;
        }
        if job.saw_journal && journal_terminal(&job.output.state) {
            let verdict = if job.output.state == TallyEvent::Failed.as_str() || saw_evidence_fail {
                Verdict::Failed
            } else {
                Verdict::Pass
            };
            let entry = CompletedEntry {
                task_uuid: job.output.task_uuid,
                gpu_seconds: job.output.gpu_seconds,
                verdict,
                session_ref: job.output.session_ref,
                gh_origin,
            };
            if saw_evidence_fail {
                gate_fails.push(entry);
            } else {
                completed.push(entry);
            }
        } else if job.saw_journal
            || matches!(
                job.row_status,
                Some(RowStatus::Pending | RowStatus::Recurring)
            )
        {
            in_flight.push(InFlightEntry {
                task_uuid: job.output.task_uuid,
                session_ref: job.output.session_ref,
                state: job.output.state,
                last_event_at: job.output.last_event_at,
                gh_origin,
            });
        }
    }
    sort_completed(&mut completed);
    sort_completed(&mut gate_fails);
    sort_completed(&mut cancelled);
    in_flight.sort_by(|left, right| left.task_uuid.cmp(&right.task_uuid));
    StandupDigest {
        schema_version: QUERY_SCHEMA_VERSION,
        protocol_version: QUERY_PROTOCOL_VERSION,
        window: StandupWindow {
            since: options.since.clone(),
            until: options.until.clone(),
        },
        completed,
        in_flight,
        reused,
        gate_fails,
        cancelled,
        canonical_gpu_seconds,
    }
}

fn journal_terminal(state: &str) -> bool {
    [
        TallyEvent::Completed,
        TallyEvent::Failed,
        TallyEvent::EvidencePass,
        TallyEvent::EvidenceFail,
        TallyEvent::WitnessEmitted,
    ]
    .iter()
    .any(|event| event.as_str() == state)
}

fn sort_completed(entries: &mut [CompletedEntry]) {
    entries.sort_by(|left, right| left.task_uuid.cmp(&right.task_uuid));
}

pub fn standup_session_refs(digest: &StandupDigest) -> BTreeSet<&str> {
    digest
        .completed
        .iter()
        .chain(&digest.gate_fails)
        .chain(&digest.cancelled)
        .filter_map(|entry| entry.session_ref.as_deref())
        .chain(
            digest
                .in_flight
                .iter()
                .filter_map(|entry| entry.session_ref.as_deref()),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::config::Priority;
    use crate::journal::{EmitEvent, TallyFields};
    use crate::taskdb::EnqueueSource;
    use crate::witness::{Charge, GENESIS_PREV_HASH};

    use super::*;

    fn pool(name: &str, used: u64, cap: u64) -> PoolHeadroomFact {
        PoolHeadroomFact {
            pool: name.to_owned(),
            capacity: 2,
            held: 1,
            queued: 3,
            consumption: Some(WindowConsumptionFact {
                used,
                cap,
                reset_at: Some("2026-07-20T00:00:00Z".to_owned()),
            }),
            meter_utilization_pct: None,
            weekly_utilization_pct: None,
        }
    }

    fn row(task: &str, session: &str) -> RowFact {
        RowFact {
            task_uuid: task.to_owned(),
            description: format!("job {task}"),
            status: RowStatus::Pending,
            priority: "H".to_owned(),
            pools: Some(vec!["gpu".to_owned()]),
            executor: None,
            source: Some("manual".to_owned()),
            session_ref: Some(session.to_owned()),
            cwd: None,
            workspace: None,
            resumed_from: None,
            attempt: 1,
            model: None,
            gh_origin: None,
            related_trigger: None,
        }
    }

    fn journal(task: &str, event: TallyEvent, at: u64) -> JournalEntry {
        let emit = if event == TallyEvent::Enqueued {
            EmitEvent::enqueued(task, Priority::High, EnqueueSource::Manual)
        } else {
            EmitEvent {
                event,
                task_uuid: task.to_owned(),
                class: Priority::High,
                source: EnqueueSource::Manual,
                message: None,
                agent: Some("shell".to_owned()),
                session_ref: None,
                unit: Some(format!("tally-job-{task}.service")),
                exit_code: Some(if event == TallyEvent::Failed { 1 } else { 0 }),
                gpu_seconds: Some(4.0),
                artifact_hash: Some("sha256:artifact".to_owned()),
                evidence: Some("exit:0".to_owned()),
                attempt: Some(1),
                lease_epoch: Some(7),
                labor_class: Some(LaborClass::Fresh),
                job_id: Some(format!("job-{task}")),
                parent: None,
                pools: Some(vec!["gpu".to_owned()]),
                executor: None,
            }
        };
        JournalEntry {
            fields: emit.into_fields().unwrap(),
            realtime_us: Some(at),
        }
    }

    fn witness(task: Option<&str>, verdict: Verdict, labor: LaborClass, seq: u64) -> WitnessRecord {
        WitnessRecord {
            task_uuid: task.map(ToOwned::to_owned),
            transition_timestamp: format!("2026-07-19T12:00:{seq:02}Z"),
            verdict,
            exit_code: if verdict == Verdict::Failed { 1 } else { 0 },
            artifact_content_hash: (verdict == Verdict::Pass).then(|| "sha256:artifact".to_owned()),
            gpu_seconds: Some(10.0),
            wall_clock: 10.0,
            attempt: 1,
            lease_epoch: 7,
            dedup_key: None,
            labor_class: labor,
            trace_ref: None,
            pools: Some(vec!["gpu".to_owned()]),
            executor: None,
            charge: Some(Charge {
                unit: "gpu-second".to_owned(),
                amount: 10.0,
                class_name: "canonical".to_owned(),
            }),
            model: None,
            evidence_class: None,
            manifest_hash: None,
            completion: None,
            seq,
            prev_hash: GENESIS_PREV_HASH.to_owned(),
            hash: format!("sha256:{seq:064x}"),
        }
    }

    #[test]
    fn pool_threshold_boundaries_and_weekly_downgrade_are_exact() {
        for (used, expected) in [
            (69, HeadroomSignal::Go),
            (70, HeadroomSignal::Slow),
            (89, HeadroomSignal::Slow),
            (90, HeadroomSignal::Stop),
        ] {
            assert_eq!(
                project_pool_headroom(&pool("api", used, 100))
                    .unwrap()
                    .signal,
                expected
            );
        }
        let mut fact = pool("api", 20, 100);
        fact.weekly_utilization_pct = Some(80.0);
        assert_eq!(
            project_pool_headroom(&fact).unwrap().signal,
            HeadroomSignal::Slow
        );
    }

    #[test]
    fn meter_clamps_downward_and_never_grants_budget() {
        let mut fact = pool("api", 40, 100);
        fact.meter_utilization_pct = Some(80.5);
        let clamped = project_pool_headroom(&fact).unwrap();
        assert_eq!(clamped.self_utilization_pct, 40.0);
        assert_eq!(clamped.effective_utilization_pct, 80.5);
        assert_eq!(clamped.remaining_budget, Some(19));

        fact.meter_utilization_pct = Some(10.0);
        let lower_meter = project_pool_headroom(&fact).unwrap();
        assert_eq!(lower_meter.effective_utilization_pct, 40.0);
        assert_eq!(lower_meter.remaining_budget, Some(60));
    }

    #[test]
    fn malformed_headroom_facts_fail_closed() {
        let mut fact = pool("api", 1, 0);
        assert!(project_pool_headroom(&fact).is_err());
        fact = pool("api", 1, 10);
        fact.meter_utilization_pct = Some(f64::NAN);
        assert!(project_pool_headroom(&fact).is_err());
        fact.meter_utilization_pct = Some(101.0);
        assert!(project_pool_headroom(&fact).is_err());
        fact.meter_utilization_pct = Some(50.0);
        fact.consumption = None;
        assert!(project_pool_headroom(&fact).is_err());
    }

    #[test]
    fn witness_overrides_cache_and_journal_terminal_state() {
        let mut hostile = row("A", "session-row");
        hostile.status = RowStatus::Deleted;
        let journal = vec![journal("A", TallyEvent::Failed, 20)];
        let witness = vec![witness(Some("A"), Verdict::Pass, LaborClass::Fresh, 1)];
        let jobs = project_jobs(&[hostile], &journal, &witness);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].state, "terminal");
        assert_eq!(jobs[0].verdict, Some(Verdict::Pass));
        assert_eq!(jobs[0].gpu_seconds, Some(10.0));
        assert_eq!(jobs[0].canonical_gpu_seconds, Some(10.0));
        assert_eq!(jobs[0].session_ref.as_deref(), Some("session-row"));
    }

    #[test]
    fn read_time_status_filters_pools_and_reports_jobs() {
        let status = query_status(
            &[pool("zeta", 20, 100), pool("alpha", 90, 100)],
            Some("alpha"),
            &[row("A", "session")],
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(status.protocol_version, QUERY_PROTOCOL_VERSION);
        assert_eq!(status.pools.len(), 1);
        assert_eq!(status.pools[0].pool, "alpha");
        assert_eq!(status.jobs.len(), 1);
        assert!(matches!(
            query_status(&[pool("alpha", 1, 10)], Some("missing"), &[], &[], &[]),
            Err(QueryError::UnknownPool(_))
        ));
    }

    #[test]
    fn status_projects_github_origin_for_a_witnessed_job() {
        let origin = GhOriginProjection {
            repo: "acme/widgets".to_owned(),
            number: 42,
            url: "https://github.com/acme/widgets/issues/42".to_owned(),
        };
        let mut github = row("github", "drive");
        github.source = Some("gh".to_owned());
        github.gh_origin = Some(origin.clone());
        let status = query_status(
            &[pool("slot", 1, 10)],
            None,
            &[github],
            &[],
            &[witness(Some("github"), Verdict::Pass, LaborClass::Fresh, 1)],
        )
        .unwrap();
        assert_eq!(status.jobs.len(), 1);
        assert_eq!(status.jobs[0].gh_origin, Some(origin.clone()));
        assert_eq!(
            serde_json::to_value(status).unwrap()["jobs"][0]["ghOrigin"],
            serde_json::to_value(origin).unwrap()
        );
    }

    #[test]
    fn query_pools_projects_every_pool_in_stable_order() {
        let pools = query_pools(&[pool("zeta", 20, 100), pool("alpha", 90, 100)]).unwrap();
        assert_eq!(pools.protocol_version, QUERY_PROTOCOL_VERSION);
        assert_eq!(
            pools
                .pools
                .iter()
                .map(|pool| pool.pool.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
        assert_eq!(pools.pools[0].remaining_budget, Some(10));
        assert_eq!(pools.pools[0].signal, HeadroomSignal::Stop);
    }

    #[test]
    fn render_scopes_do_not_restore_the_cut_session_tree() {
        let rows = vec![row("queued", "session")];
        let witness = vec![witness(Some("done"), Verdict::Pass, LaborClass::Fresh, 1)];
        let all = query_render(RenderScope::All, &rows, &[], &witness);
        assert_eq!(all.jobs.len(), 2);
        assert_eq!(
            query_render(RenderScope::Queue, &rows, &[], &witness)
                .jobs
                .len(),
            1
        );
        assert_eq!(
            query_render(RenderScope::Witness, &rows, &[], &witness)
                .jobs
                .len(),
            1
        );
        let encoded = serde_json::to_value(all).unwrap();
        assert!(encoded.get("workspaces").is_none());
        assert!(encoded.get("jobs").is_some());
    }

    #[test]
    fn log_is_a_chronological_journal_and_witness_join() {
        let rows = vec![row("A", "session-A")];
        let journal = vec![journal("A", TallyEvent::Started, 1_752_923_200_000_000)];
        let witness = vec![witness(Some("A"), Verdict::Pass, LaborClass::Fresh, 1)];
        let records = query_log(
            &rows,
            &journal,
            &witness,
            &LogFilter {
                task: Some("A".to_owned()),
                session: Some("session-A".to_owned()),
                ..LogFilter::default()
            },
        )
        .unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].origin, LogOrigin::Journal);
        assert_eq!(records[1].origin, LogOrigin::Witness);
        assert_eq!(records[1].verdict, Some(Verdict::Pass));

        let witness_only = query_log(
            &rows,
            &journal,
            &witness,
            &LogFilter {
                event: Some(TallyEvent::WitnessEmitted),
                ..LogFilter::default()
            },
        )
        .unwrap();
        assert_eq!(witness_only.len(), 1);
    }

    #[test]
    fn standup_buckets_match_expected_output_and_witness_survives_pruned_journal() {
        let rows = vec![
            row("pass", "drive"),
            row("gate", "drive"),
            row("failed-gate", "drive"),
            row("cancelled", "drive"),
            row("reused", "drive"),
            row("running", "drive"),
        ];
        let journal = vec![
            journal("failed-gate", TallyEvent::EvidenceFail, 10),
            journal("failed-gate", TallyEvent::Failed, 11),
            journal("running", TallyEvent::Started, 12),
        ];
        let witness = vec![
            witness(Some("pass"), Verdict::Pass, LaborClass::Fresh, 1),
            witness(
                Some("gate"),
                Verdict::CleanExitNoArtifact,
                LaborClass::Fresh,
                2,
            ),
            witness(Some("failed-gate"), Verdict::Failed, LaborClass::Fresh, 3),
            witness(Some("cancelled"), Verdict::Cancelled, LaborClass::Fresh, 4),
            witness(Some("reused"), Verdict::Reused, LaborClass::Reused, 5),
        ];
        let digest = query_standup(
            &rows,
            &journal,
            &witness,
            &StandupOptions {
                since: None,
                since_realtime_us: None,
                until: "2026-07-19T13:00:00Z".to_owned(),
                source: None,
            },
        );
        assert_eq!(digest.completed.len(), 2);
        assert_eq!(digest.gate_fails.len(), 2);
        assert_eq!(digest.cancelled.len(), 1);
        assert_eq!(digest.in_flight.len(), 1);
        assert_eq!(digest.reused, 1);
        assert_eq!(digest.canonical_gpu_seconds, 20.0);
        assert_eq!(standup_session_refs(&digest), BTreeSet::from(["drive"]));
        assert!(digest
            .completed
            .iter()
            .any(|entry| entry.task_uuid.as_deref() == Some("pass")));
    }

    #[test]
    fn standup_projects_github_origin_for_completed_and_in_flight_rows() {
        let origin = GhOriginProjection {
            repo: "acme/widgets".to_owned(),
            number: 42,
            url: "https://github.com/acme/widgets/issues/42".to_owned(),
        };
        let mut completed = row("completed", "drive");
        completed.source = Some("gh".to_owned());
        completed.gh_origin = Some(origin.clone());
        let mut running = row("running", "drive");
        running.source = Some("gh".to_owned());
        running.gh_origin = Some(origin.clone());

        let digest = query_standup(
            &[completed, running],
            &[],
            &[witness(
                Some("completed"),
                Verdict::Pass,
                LaborClass::Fresh,
                1,
            )],
            &StandupOptions {
                since: None,
                since_realtime_us: None,
                until: "2026-07-19T13:00:00Z".to_owned(),
                source: Some("gh".to_owned()),
            },
        );
        assert_eq!(digest.completed.len(), 1);
        assert_eq!(digest.completed[0].gh_origin, Some(origin.clone()));
        assert_eq!(digest.in_flight.len(), 1);
        assert_eq!(digest.in_flight[0].gh_origin, Some(origin));
    }

    #[test]
    fn witnessed_semantic_gate_failure_is_a_gate_fail_without_journal() {
        let mut failed = witness(Some("semantic-gate"), Verdict::Failed, LaborClass::Fresh, 1);
        failed.exit_code = 0;
        failed.completion = Some(
            serde_json::from_value(serde_json::json!({
                "schemaVersion": 1,
                "execution": {
                    "status": "success",
                    "exitCode": 0,
                    "reason": "process exited with code 0"
                },
                "gates": {
                    "status": "fail",
                    "artifact": {"commit": "abc"},
                    "gates": [{
                        "id": "tests",
                        "status": "fail",
                        "reason": "one test failed"
                    }],
                    "missingRequiredGateIds": ["live"]
                },
                "acceptance": {
                    "status": "rejected",
                    "policy": "execution-and-gates",
                    "reason": "execution or a declared gate failed"
                }
            }))
            .unwrap(),
        );
        let digest = query_standup(
            &[row("semantic-gate", "drive")],
            &[],
            &[failed],
            &StandupOptions {
                since: None,
                since_realtime_us: None,
                until: "2026-07-19T13:00:00Z".to_owned(),
                source: None,
            },
        );
        assert!(digest.completed.is_empty());
        assert_eq!(digest.gate_fails.len(), 1);
        assert_eq!(digest.gate_fails[0].verdict, Verdict::Failed);
        assert_eq!(
            digest.gate_fails[0].task_uuid.as_deref(),
            Some("semantic-gate")
        );
    }

    #[test]
    fn journald_only_terminal_is_observational_and_unmetered() {
        let fields: TallyFields = journal("rowless", TallyEvent::Completed, 10).fields;
        let digest = query_standup(
            &[],
            &[JournalEntry {
                fields,
                realtime_us: Some(10),
            }],
            &[],
            &StandupOptions {
                since: None,
                since_realtime_us: None,
                until: "2026-07-19T13:00:00Z".to_owned(),
                source: None,
            },
        );
        assert_eq!(digest.completed.len(), 1);
        assert_eq!(digest.completed[0].verdict, Verdict::Pass);
        assert_eq!(digest.canonical_gpu_seconds, 0.0);
    }

    #[test]
    fn stale_evidence_failure_does_not_poison_a_later_attempt() {
        let mut failed_attempt = journal("retry", TallyEvent::EvidenceFail, 10);
        failed_attempt.fields.attempt = Some(1);
        let mut completed_attempt = journal("retry", TallyEvent::Completed, 20);
        completed_attempt.fields.attempt = Some(2);
        let mut passed = witness(Some("retry"), Verdict::Pass, LaborClass::Recovered, 2);
        passed.attempt = 2;
        let digest = query_standup(
            &[row("retry", "drive")],
            &[failed_attempt, completed_attempt],
            &[passed],
            &StandupOptions {
                since: None,
                since_realtime_us: None,
                until: "2026-07-19T13:00:00Z".to_owned(),
                source: None,
            },
        );
        assert_eq!(digest.completed.len(), 1);
        assert!(digest.gate_fails.is_empty());
    }

    #[test]
    fn a_later_live_attempt_is_not_hidden_by_an_older_witness() {
        let mut started = journal("retry", TallyEvent::Started, 20);
        started.fields.attempt = Some(2);
        let prior = witness(Some("retry"), Verdict::Pass, LaborClass::Fresh, 1);
        let jobs = project_jobs(
            &[row("retry", "drive")],
            std::slice::from_ref(&started),
            std::slice::from_ref(&prior),
        );
        assert_eq!(jobs[0].state, TallyEvent::Started.as_str());
        assert_eq!(jobs[0].verdict, None);
        assert_eq!(jobs[0].witness_seq, None);

        let mut restarted = row("retry", "drive");
        restarted.attempt = 2;
        let without_restart_journal = project_jobs(&[restarted], &[], std::slice::from_ref(&prior));
        assert_eq!(without_restart_journal[0].state, "pending");
        assert_eq!(without_restart_journal[0].verdict, None);
        assert_eq!(without_restart_journal[0].witness_seq, None);

        let digest = query_standup(
            &[row("retry", "drive")],
            &[started],
            &[prior],
            &StandupOptions {
                since: None,
                since_realtime_us: None,
                until: "2026-07-19T13:00:00Z".to_owned(),
                source: None,
            },
        );
        assert!(digest.completed.is_empty());
        assert_eq!(digest.in_flight.len(), 1);
        assert_eq!(digest.canonical_gpu_seconds, 10.0);
    }

    #[test]
    fn journal_source_cannot_reclassify_witnessed_usage() {
        let mut observed = journal("A", TallyEvent::Completed, 20);
        observed.fields.source = EnqueueSource::Gh;
        let witnessed = witness(Some("A"), Verdict::Pass, LaborClass::Fresh, 1);
        let rows = [row("A", "drive")];
        let digest = query_standup(
            &rows,
            &[observed],
            &[witnessed],
            &StandupOptions {
                since: None,
                since_realtime_us: None,
                until: "2026-07-19T13:00:00Z".to_owned(),
                source: Some("manual".to_owned()),
            },
        );
        assert_eq!(digest.completed.len(), 1);
        assert_eq!(digest.canonical_gpu_seconds, 10.0);
    }

    #[test]
    fn canonical_meter_sums_all_fresh_attempts_not_only_the_latest_projection() {
        let mut expired = witness(
            Some("retry"),
            Verdict::RuntimeExceeded,
            LaborClass::Fresh,
            1,
        );
        expired.attempt = 1;
        let mut passed = witness(Some("retry"), Verdict::Pass, LaborClass::Fresh, 2);
        passed.attempt = 2;
        let digest = query_standup(
            &[row("retry", "drive")],
            &[],
            &[expired, passed],
            &StandupOptions {
                since: None,
                since_realtime_us: None,
                until: "2026-07-19T13:00:00Z".to_owned(),
                source: None,
            },
        );
        assert_eq!(digest.completed.len(), 1);
        assert_eq!(digest.completed[0].verdict, Verdict::Pass);
        assert_eq!(digest.canonical_gpu_seconds, 20.0);
    }
}
