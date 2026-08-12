use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{
    CoResidencyPredicate, PoolConfig, PoolPredicate, Priority, ResourceKind,
    DEFAULT_AGING_THRESHOLD_SEC,
};
use crate::witness::{Verdict, WitnessError, WitnessRecord};

pub const LEASE_EPOCH_FILE: &str = "lease_epoch";
pub const LEASE_EVENTS_FILE: &str = "lease-events.jsonl";
pub const CAMPAIGN_POOL_PREFIX: &str = "campaign/";

/// Whether `name` identifies a repository-scoped campaign runner mutex.
///
/// The name is the complete durable definition of this reserved pool: exactly
/// `campaign/<owner>/<repo>`, using the same bounded alphabet accepted for
/// forge repository locators. No configured pool or extra persistence is
/// needed to reconstruct its capacity and predicate.
pub fn is_campaign_pool_name(name: &str) -> bool {
    let mut parts = name.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("campaign"), Some(owner), Some(repository), None)
            if safe_campaign_pool_component(owner)
                && safe_campaign_pool_component(repository)
    )
}

fn safe_campaign_pool_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn campaign_mutex_config() -> PoolConfig {
    PoolConfig {
        resource: Some(ResourceKind::Mutex),
        capacity: 1,
        predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
        ..PoolConfig::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LeaseRequest {
    pub job_id: String,
    pub unit: String,
    pub pools: Vec<String>,
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_key: Option<String>,
    #[serde(default)]
    pub consumption_estimate: Option<u64>,
    #[serde(skip)]
    pub scheduling_group: LeaseSchedulingGroup,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum LeaseSchedulingGroup {
    Flow(String),
    Parent(String),
    #[default]
    Standalone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LeaseGrant {
    pub lease_id: String,
    pub job_id: String,
    pub unit: String,
    pub pools: Vec<String>,
    pub priority: Priority,
    pub epoch: u64,
    pub granted_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumption_estimate: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdmitOutcome {
    Granted(LeaseGrant),
    Queued { ticket_id: String, position: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HeartbeatOutcome {
    Alive,
    HolderExited,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseOutcome {
    pub released: LeaseGrant,
    pub verdict: Option<Verdict>,
    pub promoted: Vec<LeaseGrant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickOutcome {
    pub preempted: Vec<LeaseGrant>,
    pub promoted: Vec<LeaseGrant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LeaseStatus {
    pub lease_id: String,
    pub epoch: u64,
    pub held: bool,
    pub yield_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yield_deadline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BudgetDebit {
    pub pool: String,
    pub amount: u64,
    pub admitted_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum LeaseEventKind {
    Granted {
        grant: LeaseGrant,
        budget_debits: Vec<BudgetDebit>,
    },
    Released {
        lease_id: String,
    },
    YieldRequested {
        lease_id: String,
        by_ticket: String,
        deadline: String,
    },
    Preempted {
        lease_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LeaseEvent {
    pub schema_version: u32,
    pub observed_at: String,
    pub epoch: u64,
    pub event: LeaseEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowUsageSnapshot {
    pub usage: BTreeMap<String, u64>,
    pub debits: BTreeMap<String, Vec<BudgetDebit>>,
    pub debited_admissions: HashSet<String>,
    pub witness_records: usize,
}

#[derive(Debug, Error)]
pub enum LeaseError {
    #[error("lease request is invalid: {0}")]
    InvalidRequest(String),
    #[error("unknown pool {0:?}")]
    UnknownPool(String),
    #[error("lease {0:?} was not found")]
    NotFound(String),
    #[error("lease epoch {presented} is stale; current epoch is {current}")]
    StaleEpoch { presented: u64, current: u64 },
    #[error("lease epoch counter is corrupt: {0}")]
    CorruptEpoch(String),
    #[error("lease epoch counter overflowed")]
    EpochOverflow,
    #[error("lease event {path} has unsupported schema version {version}")]
    EventVersion { path: PathBuf, version: u32 },
    #[error("lease I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("lease JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("witness error: {0}")]
    Witness(#[from] WitnessError),
    #[error("witness ledger is not valid")]
    InvalidWitness,
    #[error("systemd liveness probe failed: {0}")]
    Liveness(String),
}

fn io_error(path: &Path, source: std::io::Error) -> LeaseError {
    LeaseError::Io {
        path: path.to_owned(),
        source,
    }
}

fn timestamp(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn bump_epoch(state_dir: &Path) -> Result<u64, LeaseError> {
    std::fs::create_dir_all(state_dir).map_err(|source| io_error(state_dir, source))?;
    let counter_path = state_dir.join(LEASE_EPOCH_FILE);
    let lock_path = state_dir.join(format!("{LEASE_EPOCH_FILE}.lock"));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|source| io_error(&lock_path, source))?;
    lock.lock_exclusive()
        .map_err(|source| io_error(&lock_path, source))?;

    let previous = match std::fs::read_to_string(&counter_path) {
        Ok(value) => value
            .trim()
            .parse::<u64>()
            .map_err(|_| LeaseError::CorruptEpoch(value))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(source) => return Err(io_error(&counter_path, source)),
    };
    let next = previous.checked_add(1).ok_or(LeaseError::EpochOverflow)?;
    let temporary = state_dir.join(format!(".{LEASE_EPOCH_FILE}.{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|source| io_error(&temporary, source))?;
    writeln!(file, "{next}").map_err(|source| io_error(&temporary, source))?;
    file.sync_all()
        .map_err(|source| io_error(&temporary, source))?;
    std::fs::rename(&temporary, &counter_path).map_err(|source| io_error(&counter_path, source))?;
    File::open(state_dir)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(state_dir, source))?;
    Ok(next)
}

#[derive(Debug, Clone)]
pub struct LeaseEventLog {
    path: PathBuf,
}

impl LeaseEventLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn in_state_dir(state_dir: &Path) -> Self {
        Self::new(state_dir.join(LEASE_EVENTS_FILE))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, event: &LeaseEvent) -> Result<(), LeaseError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        }
        let created = !self.path.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)
            .map_err(|source| io_error(&self.path, source))?;
        file.lock_exclusive()
            .map_err(|source| io_error(&self.path, source))?;
        let mut encoded = serde_json::to_vec(event)?;
        encoded.push(b'\n');
        file.write_all(&encoded)
            .map_err(|source| io_error(&self.path, source))?;
        file.sync_all()
            .map_err(|source| io_error(&self.path, source))?;
        if created {
            if let Some(parent) = self.path.parent() {
                File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|source| io_error(parent, source))?;
            }
        }
        Ok(())
    }

    pub fn read(&self) -> Result<Vec<LeaseEvent>, LeaseError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&self.path).map_err(|source| io_error(&self.path, source))?;
        BufReader::new(file)
            .lines()
            .filter_map(|line| match line {
                Ok(line) if line.trim().is_empty() => None,
                other => Some(other),
            })
            .map(|line| {
                let line = line.map_err(|source| io_error(&self.path, source))?;
                let event: LeaseEvent = serde_json::from_str(&line)?;
                if event.schema_version != 1 {
                    return Err(LeaseError::EventVersion {
                        path: self.path.clone(),
                        version: event.schema_version,
                    });
                }
                Ok(event)
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
struct HeldLease {
    grant: LeaseGrant,
    yield_demands: BTreeMap<String, YieldDemand>,
}

#[derive(Debug, Clone)]
struct YieldDemand {
    deadline: DateTime<Utc>,
    hard_reclaim: bool,
    pools: HashSet<String>,
}

#[derive(Debug, Clone)]
struct YieldIntent {
    hard_reclaim: bool,
    pools: HashSet<String>,
}

#[derive(Debug, Clone)]
struct PendingRequest {
    ticket_id: String,
    sequence: u64,
    admitted_at: DateTime<Utc>,
    effective_rank: u16,
    request: LeaseRequest,
}

#[derive(Debug, Clone)]
struct RuntimePool {
    config: PoolConfig,
    holders: HashSet<String>,
    debits: VecDeque<BudgetDebit>,
}

impl RuntimePool {
    fn new(config: PoolConfig) -> Self {
        Self {
            config,
            holders: HashSet::new(),
            debits: VecDeque::new(),
        }
    }
}

pub struct LeaseEngine {
    epoch: u64,
    yield_grace: Duration,
    aging_threshold: Duration,
    pools: BTreeMap<String, RuntimePool>,
    held: HashMap<String, HeldLease>,
    pending: Vec<PendingRequest>,
    debited_admissions: HashSet<String>,
    next_sequence: u64,
    events: Option<LeaseEventLog>,
}

fn effective_rank_at(
    pending: &PendingRequest,
    now: DateTime<Utc>,
    aging_threshold: Duration,
) -> u16 {
    let aged = now
        .signed_duration_since(pending.admitted_at)
        .to_std()
        .is_ok_and(|waited| waited > aging_threshold);
    if !aged {
        return pending.request.priority.rank();
    }
    match pending.request.priority {
        Priority::Low => Priority::Medium.rank(),
        Priority::Medium => Priority::High.rank(),
        Priority::High => Priority::Interrupt.rank(),
        Priority::Interrupt => Priority::Interrupt.rank(),
    }
}

impl LeaseEngine {
    pub fn new(
        epoch: u64,
        yield_grace: Duration,
        pools: BTreeMap<String, PoolConfig>,
        events: Option<LeaseEventLog>,
    ) -> Result<Self, LeaseError> {
        Self::new_with_aging_threshold(
            epoch,
            yield_grace,
            Duration::from_secs(DEFAULT_AGING_THRESHOLD_SEC),
            pools,
            events,
        )
    }

    pub fn new_with_aging_threshold(
        epoch: u64,
        yield_grace: Duration,
        aging_threshold: Duration,
        mut pools: BTreeMap<String, PoolConfig>,
        events: Option<LeaseEventLog>,
    ) -> Result<Self, LeaseError> {
        if epoch == 0 {
            return Err(LeaseError::InvalidRequest(
                "lease epoch must be positive".to_owned(),
            ));
        }
        if yield_grace.is_zero() {
            return Err(LeaseError::InvalidRequest(
                "yieldGraceSec must be positive".to_owned(),
            ));
        }
        if aging_threshold.is_zero() {
            return Err(LeaseError::InvalidRequest(
                "agingThresholdSec must be positive".to_owned(),
            ));
        }
        for (name, config) in &mut pools {
            if name.starts_with(CAMPAIGN_POOL_PREFIX) {
                if !is_campaign_pool_name(name) {
                    return Err(LeaseError::InvalidRequest(format!(
                        "configured pool {name:?} uses the reserved campaign/ namespace"
                    )));
                }
                // Durable replay feeds derived pools through this constructor
                // too. Normalize any exact namespace name here so neither a
                // stale nor hand-written config can redefine its shape.
                *config = campaign_mutex_config();
            }
            if config.capacity == 0 {
                return Err(LeaseError::InvalidRequest(format!(
                    "pool {name:?} has zero capacity"
                )));
            }
            if config.resource() == ResourceKind::Mutex
                && (config.capacity != 1
                    || !matches!(config.predicate, PoolPredicate::CoResidency(_)))
            {
                return Err(LeaseError::InvalidRequest(format!(
                    "mutex pool {name:?} must use co-residency with capacity 1"
                )));
            }
        }
        Ok(Self {
            epoch,
            yield_grace,
            aging_threshold,
            pools: pools
                .into_iter()
                .map(|(name, config)| (name, RuntimePool::new(config)))
                .collect(),
            held: HashMap::new(),
            pending: Vec::new(),
            debited_admissions: HashSet::new(),
            next_sequence: 1,
            events,
        })
    }

    pub fn from_durable(
        epoch: u64,
        yield_grace: Duration,
        pools: BTreeMap<String, PoolConfig>,
        events: LeaseEventLog,
        witness: &[WitnessRecord],
        now: DateTime<Utc>,
    ) -> Result<Self, LeaseError> {
        Self::from_durable_with_aging_threshold(
            epoch,
            yield_grace,
            Duration::from_secs(DEFAULT_AGING_THRESHOLD_SEC),
            pools,
            events,
            witness,
            now,
        )
    }

    pub fn from_durable_with_aging_threshold(
        epoch: u64,
        yield_grace: Duration,
        aging_threshold: Duration,
        mut pools: BTreeMap<String, PoolConfig>,
        events: LeaseEventLog,
        witness: &[WitnessRecord],
        now: DateTime<Utc>,
    ) -> Result<Self, LeaseError> {
        // Grants are already the durable admission authority. Their pool names
        // therefore contain everything needed to close restart replay over the
        // reserved campaign namespace, without a registry or another event.
        for event in events.read()? {
            let LeaseEventKind::Granted { grant, .. } = event.event else {
                continue;
            };
            for pool in grant.pools {
                if is_campaign_pool_name(&pool) {
                    pools.entry(pool).or_insert_with(campaign_mutex_config);
                }
            }
        }
        let rebuilt = rebuild_window_usage(&pools, &events, witness, now)?;
        let mut engine = Self::new_with_aging_threshold(
            epoch,
            yield_grace,
            aging_threshold,
            pools,
            Some(events),
        )?;
        for (pool, debits) in rebuilt.debits {
            if let Some(state) = engine.pools.get_mut(&pool) {
                state.debits = debits.into();
            }
        }
        engine.debited_admissions = rebuilt.debited_admissions;
        Ok(engine)
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn queue_len(&self) -> usize {
        self.pending.len()
    }

    pub fn held_len(&self) -> usize {
        self.held.len()
    }

    /// Whether a pool was **explicitly** configured `resource = "vram"` —
    /// `None` both when the name names no pool and when the operator
    /// declared no `resource` at all. `vram` is the codebase's one
    /// definition of "GPU pool" (see `doc/src/configuration/mechanisms.md`),
    /// but `vram` is also `ResourceKind`'s default: reading the *effective*
    /// resource (`PoolConfig::resource()`) would make every pool that
    /// declared nothing register as a GPU pool. This is the narrower
    /// question #382's `gpuSeconds` gate needs — "did the operator say GPU
    /// pool", not "assume GPU pool because nothing was said" — so it reads
    /// the raw declared field, not the defaulted one.
    pub fn declared_resource_kind(&self, pool: &str) -> Option<ResourceKind> {
        self.pools.get(pool)?.config.resource
    }

    pub fn held_in_pool(&self, pool: &str) -> Result<usize, LeaseError> {
        self.pools
            .get(pool)
            .map(|state| state.holders.len())
            .or_else(|| is_campaign_pool_name(pool).then_some(0))
            .ok_or_else(|| LeaseError::UnknownPool(pool.to_owned()))
    }

    pub fn queued_in_pool(&self, pool: &str) -> Result<usize, LeaseError> {
        if !self.pools.contains_key(pool) && !is_campaign_pool_name(pool) {
            return Err(LeaseError::UnknownPool(pool.to_owned()));
        }
        Ok(self
            .pending
            .iter()
            .filter(|pending| pending.request.pools.iter().any(|name| name == pool))
            .count())
    }

    pub fn validate_admission(&self, request: &LeaseRequest) -> Result<(), LeaseError> {
        let mut request = request.clone();
        self.canonicalize_request(&mut request)
    }

    pub fn budget_used_at(&mut self, pool: &str, now: DateTime<Utc>) -> Result<u64, LeaseError> {
        let Some(state) = self.pools.get_mut(pool) else {
            if is_campaign_pool_name(pool) {
                return Ok(0);
            }
            return Err(LeaseError::UnknownPool(pool.to_owned()));
        };
        prune_debits(state, now)?;
        Ok(state.debits.iter().map(|debit| debit.amount).sum())
    }

    pub fn window_reset_at(
        &mut self,
        pool: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<String>, LeaseError> {
        let Some(state) = self.pools.get_mut(pool) else {
            if is_campaign_pool_name(pool) {
                return Ok(None);
            }
            return Err(LeaseError::UnknownPool(pool.to_owned()));
        };
        prune_debits(state, now)?;
        let PoolPredicate::WindowedConsumption(window) = &state.config.predicate else {
            return Ok(None);
        };
        let Some(debit) = state.debits.front() else {
            return Ok(None);
        };
        let admitted_at = DateTime::parse_from_rfc3339(&debit.admitted_at)
            .map_err(|error| LeaseError::InvalidRequest(format!("invalid debit time: {error}")))?
            .with_timezone(&Utc);
        let window = chrono::Duration::seconds(
            i64::try_from(window.window_sec)
                .map_err(|_| LeaseError::InvalidRequest("windowSec is too large".to_owned()))?,
        );
        Ok(Some(timestamp(admitted_at + window)))
    }

    pub fn admit_at(
        &mut self,
        mut request: LeaseRequest,
        now: DateTime<Utc>,
    ) -> Result<AdmitOutcome, LeaseError> {
        self.canonicalize_request(&mut request)?;
        for pool in &request.pools {
            if is_campaign_pool_name(pool) {
                self.pools
                    .entry(pool.clone())
                    .or_insert_with(|| RuntimePool::new(campaign_mutex_config()));
            }
        }
        let sequence = self.next_sequence;
        self.next_sequence =
            self.next_sequence
                .checked_add(1)
                .ok_or(LeaseError::InvalidRequest(
                    "request sequence overflow".to_owned(),
                ))?;
        let ticket_id = format!("lease-{}-{sequence}", self.epoch);
        if self.can_grant(&request, now)? {
            return self
                .grant(ticket_id, request, now)
                .map(AdmitOutcome::Granted);
        }

        let pending = PendingRequest {
            ticket_id: ticket_id.clone(),
            sequence,
            admitted_at: now,
            effective_rank: request.priority.rank(),
            request,
        };
        self.pending.push(pending);
        self.sort_pending(now);
        if let Err(error) = self.reconcile_yield_demands(now) {
            self.pending
                .retain(|pending| pending.ticket_id != ticket_id);
            return Err(error);
        }
        let position = self
            .pending
            .iter()
            .position(|queued| queued.ticket_id == ticket_id)
            .expect("newly inserted request is present")
            + 1;
        Ok(AdmitOutcome::Queued {
            ticket_id,
            position,
        })
    }

    pub fn status(&self, lease_id: &str, epoch: u64) -> Result<LeaseStatus, LeaseError> {
        self.check_epoch(epoch)?;
        if let Some(held) = self.held.get(lease_id) {
            return Ok(LeaseStatus {
                lease_id: lease_id.to_owned(),
                epoch,
                held: true,
                yield_requested: !held.yield_demands.is_empty(),
                yield_deadline: held
                    .yield_demands
                    .values()
                    .map(|demand| demand.deadline)
                    .min()
                    .map(timestamp),
            });
        }
        if self
            .pending
            .iter()
            .any(|pending| pending.ticket_id == lease_id)
        {
            return Ok(LeaseStatus {
                lease_id: lease_id.to_owned(),
                epoch,
                held: false,
                yield_requested: false,
                yield_deadline: None,
            });
        }
        Err(LeaseError::NotFound(lease_id.to_owned()))
    }

    pub fn release_at(
        &mut self,
        lease_id: &str,
        epoch: u64,
        now: DateTime<Utc>,
    ) -> Result<ReleaseOutcome, LeaseError> {
        self.check_epoch(epoch)?;
        let held = self
            .held
            .get(lease_id)
            .cloned()
            .ok_or_else(|| LeaseError::NotFound(lease_id.to_owned()))?;
        self.append_event(
            now,
            LeaseEventKind::Released {
                lease_id: lease_id.to_owned(),
            },
        )?;
        self.remove_held(&held.grant);
        let promoted = self.promote(now)?;
        self.reconcile_yield_demands(now)?;
        Ok(ReleaseOutcome {
            released: held.grant,
            verdict: None,
            promoted,
        })
    }

    pub fn cancel_pending_at(
        &mut self,
        ticket_id: &str,
        epoch: u64,
        _now: DateTime<Utc>,
    ) -> Result<(), LeaseError> {
        self.check_epoch(epoch)?;
        let position = self
            .pending
            .iter()
            .position(|pending| pending.ticket_id == ticket_id)
            .ok_or_else(|| LeaseError::NotFound(ticket_id.to_owned()))?;
        self.pending.remove(position);
        for held in self.held.values_mut() {
            held.yield_demands.remove(ticket_id);
        }
        Ok(())
    }

    pub fn plan_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<LeaseGrant>, LeaseError> {
        self.reconcile_yield_demands(now)?;
        Ok(self
            .held
            .values()
            .filter(|held| {
                held.yield_demands
                    .values()
                    .any(|demand| demand.hard_reclaim && now >= demand.deadline)
            })
            .map(|held| held.grant.clone())
            .collect())
    }

    pub fn commit_preemptions(
        &mut self,
        reclaimed: &[String],
        now: DateTime<Utc>,
    ) -> Result<TickOutcome, LeaseError> {
        self.reconcile_yield_demands(now)?;
        let mut preempted = Vec::with_capacity(reclaimed.len());
        for lease_id in reclaimed {
            let held = self
                .held
                .get(lease_id)
                .cloned()
                .ok_or_else(|| LeaseError::NotFound(lease_id.clone()))?;
            if !held
                .yield_demands
                .values()
                .any(|demand| demand.hard_reclaim && now >= demand.deadline)
            {
                return Err(LeaseError::InvalidRequest(format!(
                    "lease {lease_id:?} is no longer eligible for hard preemption"
                )));
            }
            self.append_event(
                now,
                LeaseEventKind::Preempted {
                    lease_id: lease_id.clone(),
                },
            )?;
            self.remove_held(&held.grant);
            preempted.push(held.grant);
        }
        let promoted = self.promote(now)?;
        self.reconcile_yield_demands(now)?;
        Ok(TickOutcome {
            preempted,
            promoted,
        })
    }

    pub fn tick(&mut self, now: DateTime<Utc>) -> Result<TickOutcome, LeaseError> {
        let planned = self.plan_tick(now)?;
        let reclaimed = planned
            .into_iter()
            .map(|grant| grant.lease_id)
            .collect::<Vec<_>>();
        self.commit_preemptions(&reclaimed, now)
    }

    fn validate_request(&self, request: &LeaseRequest) -> Result<(), LeaseError> {
        if request.job_id.trim().is_empty() || request.unit.trim().is_empty() {
            return Err(LeaseError::InvalidRequest(
                "jobId and unit must not be empty".to_owned(),
            ));
        }
        for pool in &request.pools {
            let Some(state) = self.pools.get(pool) else {
                if is_campaign_pool_name(pool) {
                    continue;
                }
                return Err(LeaseError::UnknownPool(pool.clone()));
            };
            if matches!(
                state.config.predicate,
                PoolPredicate::WindowedConsumption(_)
            ) && request.consumption_estimate.is_none()
            {
                return Err(LeaseError::InvalidRequest(format!(
                    "windowed-consumption pool {pool:?} requires consumptionEstimate"
                )));
            }
        }
        Ok(())
    }

    fn canonicalize_request(&self, request: &mut LeaseRequest) -> Result<(), LeaseError> {
        crate::poolset::canonicalize(&mut request.pools)
            .map_err(|error| LeaseError::InvalidRequest(error.to_string()))?;
        self.validate_request(request)
    }

    fn can_grant(
        &mut self,
        request: &LeaseRequest,
        now: DateTime<Utc>,
    ) -> Result<bool, LeaseError> {
        let mut effective = request.clone();
        if effective
            .admission_key
            .as_ref()
            .is_some_and(|key| self.debited_admissions.contains(key))
        {
            effective.consumption_estimate = Some(0);
        }
        for pool in &request.pools {
            let state = self
                .pools
                .get_mut(pool)
                .ok_or_else(|| LeaseError::UnknownPool(pool.clone()))?;
            if !pool_allows(pool, state, &effective, now)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn grant(
        &mut self,
        lease_id: String,
        request: LeaseRequest,
        now: DateTime<Utc>,
    ) -> Result<LeaseGrant, LeaseError> {
        let grant = LeaseGrant {
            lease_id: lease_id.clone(),
            job_id: request.job_id,
            unit: request.unit,
            pools: request.pools,
            priority: request.priority,
            epoch: self.epoch,
            granted_at: timestamp(now),
            admission_key: request.admission_key,
            consumption_estimate: request.consumption_estimate,
        };
        let mut budget_debits = Vec::new();
        let already_debited = grant
            .admission_key
            .as_ref()
            .is_some_and(|key| self.debited_admissions.contains(key));
        for pool in &grant.pools {
            let state = self
                .pools
                .get(pool)
                .ok_or_else(|| LeaseError::UnknownPool(pool.clone()))?;
            if !already_debited
                && matches!(
                    state.config.predicate,
                    PoolPredicate::WindowedConsumption(_)
                )
            {
                budget_debits.push(BudgetDebit {
                    pool: pool.clone(),
                    amount: grant.consumption_estimate.unwrap_or(0),
                    admitted_at: timestamp(now),
                });
            }
        }
        self.append_event(
            now,
            LeaseEventKind::Granted {
                grant: grant.clone(),
                budget_debits: budget_debits.clone(),
            },
        )?;
        if !budget_debits.is_empty() {
            if let Some(key) = &grant.admission_key {
                self.debited_admissions.insert(key.clone());
            }
        }
        for pool in &grant.pools {
            self.pools
                .get_mut(pool)
                .expect("validated pool exists")
                .holders
                .insert(lease_id.clone());
        }
        for debit in budget_debits {
            self.pools
                .get_mut(&debit.pool)
                .expect("validated pool exists")
                .debits
                .push_back(debit);
        }
        self.held.insert(
            lease_id,
            HeldLease {
                grant: grant.clone(),
                yield_demands: BTreeMap::new(),
            },
        );
        Ok(grant)
    }

    fn reconcile_yield_demands(&mut self, now: DateTime<Utc>) -> Result<(), LeaseError> {
        self.refresh_aged_order(now);
        let interrupts = self
            .pending
            .iter()
            .filter(|pending| pending.request.priority == Priority::Interrupt)
            .cloned()
            .collect::<Vec<_>>();
        let mut desired = HashMap::<String, HashMap<String, YieldIntent>>::new();
        let mut reserved_by_pool = HashMap::<String, HashSet<String>>::new();

        for pending in interrupts {
            let mut windows_allow = true;
            for pool in &pending.request.pools {
                let Some(state) = self.pools.get_mut(pool) else {
                    continue;
                };
                if matches!(
                    state.config.predicate,
                    PoolPredicate::WindowedConsumption(_)
                ) && !pool_allows(pool, state, &pending.request, now)?
                {
                    windows_allow = false;
                    break;
                }
            }
            if !windows_allow {
                continue;
            }

            let mut assignments = Vec::new();
            let mut all_blockers_yieldable = true;
            for pool in &pending.request.pools {
                let Some(state) = self.pools.get(pool) else {
                    continue;
                };
                if !matches!(state.config.predicate, PoolPredicate::CoResidency(_))
                    || state.holders.len() < state.config.capacity as usize
                {
                    continue;
                }
                let reserved = reserved_by_pool.get(pool);
                let eligible = || {
                    state
                        .holders
                        .iter()
                        .filter_map(|lease_id| self.held.get(lease_id))
                        .filter(|held| held.grant.priority.rank() < Priority::Interrupt.rank())
                        .filter(|held| {
                            reserved.is_none_or(|leases| !leases.contains(&held.grant.lease_id))
                        })
                };
                let candidate = eligible()
                    .filter(|held| {
                        held.yield_demands
                            .get(&pending.ticket_id)
                            .is_some_and(|demand| demand.pools.contains(pool))
                    })
                    .min_by(|left, right| left.grant.lease_id.cmp(&right.grant.lease_id))
                    .or_else(|| {
                        eligible().min_by(|left, right| {
                            left.grant
                                .priority
                                .rank()
                                .cmp(&right.grant.priority.rank())
                                .then_with(|| left.grant.lease_id.cmp(&right.grant.lease_id))
                        })
                    });
                let Some(candidate) = candidate else {
                    all_blockers_yieldable = false;
                    break;
                };
                assignments.push((
                    pool.clone(),
                    candidate.grant.lease_id.clone(),
                    state.config.hard_preempt,
                ));
            }
            if !all_blockers_yieldable {
                continue;
            }
            for (pool, lease_id, hard_preempt) in assignments {
                reserved_by_pool
                    .entry(pool.clone())
                    .or_default()
                    .insert(lease_id.clone());
                desired
                    .entry(lease_id)
                    .or_default()
                    .entry(pending.ticket_id.clone())
                    .and_modify(|intent| {
                        // Conjunctive, deliberately: a victim co-allocated on
                        // several pools becomes hard-reclaim eligible only when
                        // every pool this request asks it to yield in carries
                        // `hardPreempt`. A pool's `false` is a promise to its
                        // own workloads, and OR let a neighboring pool's opt-in
                        // override it.
                        intent.hard_reclaim &= hard_preempt;
                        intent.pools.insert(pool.clone());
                    })
                    .or_insert_with(|| YieldIntent {
                        hard_reclaim: hard_preempt,
                        pools: HashSet::from([pool]),
                    });
            }
        }

        let deadline = now
            + chrono::Duration::from_std(self.yield_grace)
                .map_err(|_| LeaseError::InvalidRequest("yield grace is too large".to_owned()))?;
        let mut newly_requested = Vec::new();
        for (lease_id, tickets) in &desired {
            let held = self.held.get(lease_id).expect("yield candidate is held");
            for (ticket_id, intent) in tickets {
                match held.yield_demands.get(ticket_id) {
                    None => newly_requested.push((lease_id.clone(), ticket_id.clone())),
                    Some(demand) if intent.hard_reclaim && !demand.hard_reclaim => {
                        newly_requested.push((lease_id.clone(), ticket_id.clone()));
                    }
                    Some(_) => {}
                }
            }
        }
        for (lease_id, ticket_id) in &newly_requested {
            self.append_event(
                now,
                LeaseEventKind::YieldRequested {
                    lease_id: lease_id.clone(),
                    by_ticket: ticket_id.clone(),
                    deadline: timestamp(deadline),
                },
            )?;
        }

        for (lease_id, held) in &mut self.held {
            let wanted = desired.get(lease_id);
            held.yield_demands.retain(|ticket_id, _| {
                wanted.is_some_and(|tickets| tickets.contains_key(ticket_id))
            });
            let Some(wanted) = wanted else {
                continue;
            };
            for (ticket_id, intent) in wanted {
                match held.yield_demands.entry(ticket_id.clone()) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(YieldDemand {
                            deadline,
                            hard_reclaim: intent.hard_reclaim,
                            pools: intent.pools.clone(),
                        });
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        let demand = entry.get_mut();
                        if intent.hard_reclaim && !demand.hard_reclaim {
                            demand.deadline = deadline;
                        }
                        demand.hard_reclaim = intent.hard_reclaim;
                        demand.pools.clone_from(&intent.pools);
                    }
                }
            }
        }
        Ok(())
    }

    fn sort_pending(&mut self, now: DateTime<Utc>) {
        let aging_threshold = self.aging_threshold;
        for pending in &mut self.pending {
            pending.effective_rank = effective_rank_at(pending, now, aging_threshold);
        }
        let mut groups = HashMap::<(u16, LeaseSchedulingGroup), Vec<u64>>::new();
        for pending in &self.pending {
            groups
                .entry((
                    pending.effective_rank,
                    pending.request.scheduling_group.clone(),
                ))
                .or_default()
                .push(pending.sequence);
        }
        let mut braid = HashMap::<u64, (usize, u64)>::new();
        for sequences in groups.values_mut() {
            sequences.sort_unstable();
            let oldest = sequences[0];
            for (index, sequence) in sequences.iter().copied().enumerate() {
                braid.insert(sequence, (index, oldest));
            }
        }
        self.pending.sort_by(|left, right| {
            right
                .effective_rank
                .cmp(&left.effective_rank)
                .then_with(|| braid[&left.sequence].0.cmp(&braid[&right.sequence].0))
                .then_with(|| braid[&left.sequence].1.cmp(&braid[&right.sequence].1))
                .then_with(|| left.sequence.cmp(&right.sequence))
        });
    }

    fn refresh_aged_order(&mut self, now: DateTime<Utc>) {
        let aging_threshold = self.aging_threshold;
        if self.pending.iter().any(|pending| {
            pending.effective_rank != effective_rank_at(pending, now, aging_threshold)
        }) {
            self.sort_pending(now);
        }
    }

    fn promote(&mut self, now: DateTime<Utc>) -> Result<Vec<LeaseGrant>, LeaseError> {
        self.refresh_aged_order(now);
        let mut promoted = Vec::new();
        loop {
            let mut selected = None;
            for index in 0..self.pending.len() {
                let request = self.pending[index].request.clone();
                if self.can_grant(&request, now)? {
                    selected = Some(index);
                    break;
                }
            }
            let Some(index) = selected else {
                break;
            };
            // Keep the accepted request reachable until the grant event has
            // crossed its fsync boundary. A failed append can then be retried
            // instead of silently losing the pending ticket.
            let pending = self.pending[index].clone();
            let grant = self.grant(pending.ticket_id, pending.request, now)?;
            self.pending.remove(index);
            promoted.push(grant);
        }
        Ok(promoted)
    }

    fn remove_held(&mut self, grant: &LeaseGrant) {
        self.held.remove(&grant.lease_id);
        for pool in &grant.pools {
            if let Some(state) = self.pools.get_mut(pool) {
                state.holders.remove(&grant.lease_id);
            }
        }
    }

    fn append_event(&self, now: DateTime<Utc>, event: LeaseEventKind) -> Result<(), LeaseError> {
        // Only grants are authoritative durable lease facts: they fence launch
        // and carry window-consumption debits. Yield requests and release /
        // preemption transitions are in-memory coordination; terminal
        // preemption is authoritative in the canonical witness chain. Keeping
        // those non-grant events out of this fsyncing log preserves BS-9's
        // exact admission + grant + verdict-witness pre-ack boundary.
        if !matches!(&event, LeaseEventKind::Granted { .. }) {
            return Ok(());
        }
        if let Some(log) = &self.events {
            log.append(&LeaseEvent {
                schema_version: 1,
                observed_at: timestamp(now),
                epoch: self.epoch,
                event,
            })?;
        }
        Ok(())
    }

    fn check_epoch(&self, presented: u64) -> Result<(), LeaseError> {
        if presented == self.epoch {
            Ok(())
        } else {
            Err(LeaseError::StaleEpoch {
                presented,
                current: self.epoch,
            })
        }
    }
}

fn pool_allows(
    pool: &str,
    state: &mut RuntimePool,
    request: &LeaseRequest,
    now: DateTime<Utc>,
) -> Result<bool, LeaseError> {
    match state.config.predicate.clone() {
        PoolPredicate::CoResidency(_) => Ok(state.holders.len() < state.config.capacity as usize),
        PoolPredicate::WindowedConsumption(window) => {
            prune_debits(state, now)?;
            let used = state.debits.iter().try_fold(0_u64, |sum, debit| {
                sum.checked_add(debit.amount).ok_or_else(|| {
                    LeaseError::InvalidRequest(format!("window usage overflow in pool {pool:?}"))
                })
            })?;
            let estimate = request.consumption_estimate.unwrap_or(0);
            Ok(used
                .checked_add(estimate)
                .is_some_and(|total| total <= window.consumption_cap))
        }
    }
}

fn prune_debits(state: &mut RuntimePool, now: DateTime<Utc>) -> Result<(), LeaseError> {
    let PoolPredicate::WindowedConsumption(window) = &state.config.predicate else {
        return Ok(());
    };
    let window = chrono::Duration::seconds(
        i64::try_from(window.window_sec)
            .map_err(|_| LeaseError::InvalidRequest("windowSec is too large".to_owned()))?,
    );
    while let Some(front) = state.debits.front() {
        let admitted_at = DateTime::parse_from_rfc3339(&front.admitted_at)
            .map_err(|error| LeaseError::InvalidRequest(format!("invalid debit time: {error}")))?
            .with_timezone(&Utc);
        if admitted_at + window <= now {
            state.debits.pop_front();
        } else {
            break;
        }
    }
    Ok(())
}

pub fn rebuild_window_usage(
    pools: &BTreeMap<String, PoolConfig>,
    event_log: &LeaseEventLog,
    witness: &[WitnessRecord],
    now: DateTime<Utc>,
) -> Result<WindowUsageSnapshot, LeaseError> {
    let witness_records = witness.len();
    let mut unique_grants = HashSet::new();
    let mut usage = BTreeMap::new();
    let mut debits = BTreeMap::<String, Vec<BudgetDebit>>::new();
    let mut debited_admissions = HashSet::new();
    for event in event_log.read()? {
        let LeaseEventKind::Granted {
            grant,
            budget_debits,
        } = event.event
        else {
            continue;
        };
        if !unique_grants.insert(grant.lease_id) {
            continue;
        }
        if !budget_debits.is_empty()
            && grant
                .admission_key
                .is_some_and(|key| !debited_admissions.insert(key))
        {
            // An append may have reached the log even when sync_all reported
            // failure. Admission keys make the authoritative debit idempotent
            // across a retry with a different lease ID.
            continue;
        }
        for debit in budget_debits {
            let Some(pool) = pools.get(&debit.pool) else {
                continue;
            };
            let PoolPredicate::WindowedConsumption(window) = &pool.predicate else {
                continue;
            };
            let admitted_at = DateTime::parse_from_rfc3339(&debit.admitted_at)
                .map_err(|error| {
                    LeaseError::InvalidRequest(format!("invalid debit time: {error}"))
                })?
                .with_timezone(&Utc);
            let window =
                chrono::Duration::seconds(i64::try_from(window.window_sec).map_err(|_| {
                    LeaseError::InvalidRequest("windowSec is too large".to_owned())
                })?);
            if admitted_at + window > now {
                let total = usage.entry(debit.pool.clone()).or_insert(0_u64);
                *total = total.checked_add(debit.amount).ok_or_else(|| {
                    LeaseError::InvalidRequest("rebuilt window usage overflow".to_owned())
                })?;
                debits.entry(debit.pool.clone()).or_default().push(debit);
            }
        }
    }
    Ok(WindowUsageSnapshot {
        usage,
        debits,
        debited_admissions,
        witness_records,
    })
}

pub trait UnitLiveness {
    fn is_active(&self, unit: &str) -> Result<bool, LeaseError>;
}

#[derive(Debug, Clone)]
pub struct SystemdUnitLiveness {
    systemctl: PathBuf,
}

impl Default for SystemdUnitLiveness {
    fn default() -> Self {
        Self {
            systemctl: PathBuf::from("systemctl"),
        }
    }
}

impl SystemdUnitLiveness {
    pub fn with_program(path: impl Into<PathBuf>) -> Self {
        Self {
            systemctl: path.into(),
        }
    }
}

impl UnitLiveness for SystemdUnitLiveness {
    fn is_active(&self, unit: &str) -> Result<bool, LeaseError> {
        let output = Command::new(&self.systemctl)
            .args([
                "--user",
                "show",
                "--property=ActiveState",
                "--value",
                "--",
                unit,
            ])
            .output()
            .map_err(|error| LeaseError::Liveness(error.to_string()))?;
        interpret_systemctl_show(
            unit,
            output.status.success(),
            &output.stdout,
            &output.stderr,
        )
    }
}

fn interpret_systemctl_show(
    unit: &str,
    success: bool,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<bool, LeaseError> {
    if !success {
        let detail = String::from_utf8_lossy(stderr);
        return Err(LeaseError::Liveness(format!(
            "systemctl show failed for {unit:?}: {}",
            detail.trim()
        )));
    }
    let state = String::from_utf8_lossy(stdout);
    match state.trim() {
        "active" | "activating" | "reloading" => Ok(true),
        "inactive" | "failed" | "deactivating" => Ok(false),
        other => Err(LeaseError::Liveness(format!(
            "systemctl show returned unknown ActiveState {other:?} for {unit:?}"
        ))),
    }
}

pub trait LeaseBackend {
    fn admit(
        &mut self,
        request: LeaseRequest,
        now: DateTime<Utc>,
    ) -> Result<AdmitOutcome, LeaseError>;
    fn release(
        &mut self,
        lease_id: &str,
        epoch: u64,
        now: DateTime<Utc>,
    ) -> Result<ReleaseOutcome, LeaseError>;
    fn heartbeat(
        &mut self,
        lease_id: &str,
        epoch: u64,
        now: DateTime<Utc>,
    ) -> Result<HeartbeatOutcome, LeaseError>;
}

pub struct LocalLease<L> {
    engine: LeaseEngine,
    liveness: L,
}

impl<L> LocalLease<L> {
    pub fn new(engine: LeaseEngine, liveness: L) -> Self {
        Self { engine, liveness }
    }

    pub fn engine(&self) -> &LeaseEngine {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut LeaseEngine {
        &mut self.engine
    }
}

impl<L: UnitLiveness> LeaseBackend for LocalLease<L> {
    fn admit(
        &mut self,
        request: LeaseRequest,
        now: DateTime<Utc>,
    ) -> Result<AdmitOutcome, LeaseError> {
        self.engine.admit_at(request, now)
    }

    fn release(
        &mut self,
        lease_id: &str,
        epoch: u64,
        now: DateTime<Utc>,
    ) -> Result<ReleaseOutcome, LeaseError> {
        self.engine.release_at(lease_id, epoch, now)
    }

    fn heartbeat(
        &mut self,
        lease_id: &str,
        epoch: u64,
        now: DateTime<Utc>,
    ) -> Result<HeartbeatOutcome, LeaseError> {
        let status = self.engine.status(lease_id, epoch)?;
        if !status.held {
            return Err(LeaseError::NotFound(lease_id.to_owned()));
        }
        let unit = self
            .engine
            .held
            .get(lease_id)
            .expect("status reported a held lease")
            .grant
            .unit
            .clone();
        if self.liveness.is_active(&unit)? {
            Ok(HeartbeatOutcome::Alive)
        } else {
            self.engine.release_at(lease_id, epoch, now)?;
            Ok(HeartbeatOutcome::HolderExited)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::thread;

    use crate::config::{CoResidencyPredicate, WindowedConsumptionPredicate};
    use crate::taskdb::{AdmissionOrigin, EnqueueSource};
    use crate::witness::{read_verified_records, LaborClass, WitnessBody, WitnessLedger};

    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-19T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn pool(capacity: u32) -> PoolConfig {
        PoolConfig {
            resource: Some(ResourceKind::BuildSlot),
            capacity,
            predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
            ..PoolConfig::default()
        }
    }

    fn window_pool(cap: u64) -> PoolConfig {
        PoolConfig {
            resource: Some(ResourceKind::Budget),
            predicate: PoolPredicate::WindowedConsumption(WindowedConsumptionPredicate {
                window_sec: 60,
                consumption_cap: cap,
            }),
            ..PoolConfig::default()
        }
    }

    fn request(job: &str, pools: &[&str], priority: Priority) -> LeaseRequest {
        LeaseRequest {
            job_id: job.to_owned(),
            unit: format!("tally-job-{job}.service"),
            pools: pools.iter().map(|pool| (*pool).to_owned()).collect(),
            priority,
            admission_key: None,
            consumption_estimate: None,
            scheduling_group: LeaseSchedulingGroup::Standalone,
        }
    }

    fn grant(outcome: AdmitOutcome) -> LeaseGrant {
        let AdmitOutcome::Granted(grant) = outcome else {
            panic!("expected a grant")
        };
        grant
    }

    #[test]
    fn fifty_contenders_form_a_stable_fifty_deep_queue() {
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("cpu".to_owned(), pool(1))]),
            None,
        )
        .unwrap();
        let holder = grant(
            engine
                .admit_at(request("holder", &["cpu"], Priority::Low), now())
                .unwrap(),
        );
        for index in 0..50 {
            let outcome = engine
                .admit_at(
                    request(&format!("child-{index:02}"), &["cpu"], Priority::Low),
                    now(),
                )
                .unwrap();
            assert!(matches!(outcome, AdmitOutcome::Queued { .. }));
        }
        assert_eq!(engine.queue_len(), 50);
        let first = engine
            .release_at(&holder.lease_id, holder.epoch, now())
            .unwrap()
            .promoted;
        assert_eq!(first[0].job_id, "child-00");
    }

    #[test]
    fn counted_external_slot_admits_up_to_its_capacity() {
        let slot = PoolConfig {
            resource: Some(ResourceKind::Slot),
            capacity: 3,
            predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
            ..PoolConfig::default()
        };
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("codex-window".to_owned(), slot)]),
            None,
        )
        .unwrap();

        for index in 0..3 {
            let outcome = engine
                .admit_at(
                    request(&format!("agent-{index}"), &["codex-window"], Priority::Low),
                    now(),
                )
                .unwrap();
            assert!(matches!(outcome, AdmitOutcome::Granted(_)));
        }
        assert!(matches!(
            engine
                .admit_at(request("agent-3", &["codex-window"], Priority::Low), now(),)
                .unwrap(),
            AdmitOutcome::Queued { .. }
        ));
    }

    #[test]
    fn campaign_pool_namespace_shape_is_exact() {
        for valid in ["campaign/acme/widgets", "campaign/Acme-Inc/widget_repo.rs"] {
            assert!(is_campaign_pool_name(valid), "rejected {valid:?}");
        }
        for invalid in [
            "campaign",
            "campaign/",
            "campaign/acme",
            "campaign//widgets",
            "campaign/acme/..",
            "campaign/acme/widgets/extra",
            "campaign/acme/widget space",
        ] {
            assert!(!is_campaign_pool_name(invalid), "accepted {invalid:?}");
        }
    }

    #[test]
    fn untouched_campaign_namespace_is_known_to_pool_queries() {
        let campaign_pool = "campaign/acme/widgets";
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("flow".to_owned(), pool(2))]),
            None,
        )
        .unwrap();

        assert_eq!(engine.held_in_pool(campaign_pool).unwrap(), 0);
        assert_eq!(engine.queued_in_pool(campaign_pool).unwrap(), 0);
        assert_eq!(engine.budget_used_at(campaign_pool, now()).unwrap(), 0);
        assert_eq!(engine.window_reset_at(campaign_pool, now()).unwrap(), None);
    }

    #[test]
    fn configured_campaign_namespace_cannot_override_the_derived_mutex() {
        let campaign_pool = "campaign/acme/widgets";
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([(campaign_pool.to_owned(), pool(2))]),
            None,
        )
        .unwrap();

        grant(
            engine
                .admit_at(request("first", &[campaign_pool], Priority::Low), now())
                .unwrap(),
        );
        assert_eq!(
            engine.declared_resource_kind(campaign_pool),
            Some(ResourceKind::Mutex)
        );
        assert!(matches!(
            engine
                .admit_at(request("second", &[campaign_pool], Priority::Low), now())
                .unwrap(),
            AdmitOutcome::Queued { .. }
        ));

        let Err(error) = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("campaign/acme".to_owned(), pool(1))]),
            None,
        ) else {
            panic!("malformed reserved pool config must be rejected");
        };
        assert!(error.to_string().contains("reserved campaign/ namespace"));
    }

    #[test]
    fn repository_campaign_mutexes_hold_concurrently_across_repositories() {
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("flow".to_owned(), pool(2))]),
            None,
        )
        .unwrap();

        for (job, campaign_pool) in [
            ("widgets", "campaign/acme/widgets"),
            ("gadgets", "campaign/acme/gadgets"),
        ] {
            assert!(matches!(
                engine
                    .admit_at(request(job, &["flow", campaign_pool], Priority::Low), now())
                    .unwrap(),
                AdmitOutcome::Granted(_)
            ));
            assert_eq!(
                engine.declared_resource_kind(campaign_pool),
                Some(ResourceKind::Mutex)
            );
        }
        assert_eq!(engine.held_len(), 2);
    }

    #[test]
    fn repository_campaign_mutex_serializes_the_same_repository() {
        let campaign_pool = "campaign/acme/widgets";
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("flow".to_owned(), pool(2))]),
            None,
        )
        .unwrap();
        let holder = grant(
            engine
                .admit_at(
                    request("first", &["flow", campaign_pool], Priority::Low),
                    now(),
                )
                .unwrap(),
        );
        assert!(matches!(
            engine
                .admit_at(
                    request("second", &["flow", campaign_pool], Priority::Low),
                    now(),
                )
                .unwrap(),
            AdmitOutcome::Queued { .. }
        ));
        assert_eq!(engine.held_in_pool(campaign_pool).unwrap(), 1);
        assert_eq!(engine.queued_in_pool(campaign_pool).unwrap(), 1);

        let promoted = engine
            .release_at(&holder.lease_id, holder.epoch, now())
            .unwrap()
            .promoted;
        assert_eq!(promoted.len(), 1);
        assert_eq!(promoted[0].job_id, "second");
    }

    #[test]
    fn namespace_mutex_restart_recovery_preserves_held_and_queued_order() {
        let temp = tempfile::tempdir().unwrap();
        let log = LeaseEventLog::in_state_dir(temp.path());
        let campaign_pool = "campaign/acme/widgets";
        let pools = BTreeMap::from([("flow".to_owned(), pool(2))]);
        let mut first =
            LeaseEngine::new(3, Duration::from_secs(20), pools.clone(), Some(log.clone())).unwrap();
        let old_holder = grant(
            first
                .admit_at(
                    request("held", &["flow", campaign_pool], Priority::Low),
                    now(),
                )
                .unwrap(),
        );
        assert_eq!(
            first
                .admit_at(
                    request("queued", &["flow", campaign_pool], Priority::Low),
                    now(),
                )
                .unwrap(),
            AdmitOutcome::Queued {
                ticket_id: "lease-3-2".to_owned(),
                position: 1,
            }
        );
        assert_eq!(first.held_in_pool(campaign_pool).unwrap(), 1);
        assert_eq!(first.queued_in_pool(campaign_pool).unwrap(), 1);
        assert_eq!(log.read().unwrap().len(), 1, "only grants are durable");
        drop(first);

        let mut restarted = LeaseEngine::from_durable_with_aging_threshold(
            4,
            Duration::from_secs(20),
            Duration::from_secs(60),
            pools,
            log,
            &[],
            now(),
        )
        .unwrap();
        assert_eq!(
            restarted.declared_resource_kind(campaign_pool),
            Some(ResourceKind::Mutex),
            "the old grant event alone must reconstruct the namespace pool"
        );
        let recovered_holder = grant(
            restarted
                .admit_at(
                    request("held", &["flow", campaign_pool], Priority::Low),
                    now(),
                )
                .unwrap(),
        );
        assert_ne!(recovered_holder.lease_id, old_holder.lease_id);
        assert!(
            restarted
                .status(&recovered_holder.lease_id, 4)
                .unwrap()
                .held
        );
        assert_eq!(
            restarted
                .admit_at(
                    request("queued", &["flow", campaign_pool], Priority::Low),
                    now(),
                )
                .unwrap(),
            AdmitOutcome::Queued {
                ticket_id: "lease-4-2".to_owned(),
                position: 1,
            },
            "the recovered holder must fence duplicate dispatch of the queued runner"
        );
        assert_eq!(restarted.held_in_pool(campaign_pool).unwrap(), 1);
        assert_eq!(restarted.queued_in_pool(campaign_pool).unwrap(), 1);
        assert_eq!(
            restarted
                .pending
                .iter()
                .map(|pending| pending.request.job_id.as_str())
                .collect::<Vec<_>>(),
            ["queued"],
            "durable registration replay must retain queue order"
        );

        let promoted = restarted
            .release_at(&recovered_holder.lease_id, 4, now())
            .unwrap()
            .promoted;
        assert_eq!(
            promoted
                .iter()
                .map(|grant| grant.job_id.as_str())
                .collect::<Vec<_>>(),
            ["queued"]
        );
        assert_eq!(restarted.held_in_pool(campaign_pool).unwrap(), 1);
        assert_eq!(restarted.queued_in_pool(campaign_pool).unwrap(), 0);
    }

    #[test]
    fn workload_mutex_replay_waits_behind_the_next_process_holder() {
        let mutex = PoolConfig {
            resource: Some(ResourceKind::Mutex),
            capacity: 1,
            predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
            ..PoolConfig::default()
        };
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([
                ("flow".to_owned(), pool(8)),
                ("monthly-review".to_owned(), mutex),
            ]),
            None,
        )
        .unwrap();

        let first_process = grant(
            engine
                .admit_at(
                    request(
                        "run-a-attempt-1",
                        &["flow", "monthly-review"],
                        Priority::Low,
                    ),
                    now(),
                )
                .unwrap(),
        );
        assert!(matches!(
            engine
                .admit_at(
                    request(
                        "run-b-attempt-1",
                        &["flow", "monthly-review"],
                        Priority::Low,
                    ),
                    now(),
                )
                .unwrap(),
            AdmitOutcome::Queued { .. }
        ));

        // Releasing the first runner process models death or preemption: the next run takes the
        // mutex even though the interrupted flow's durable children may still be completing.
        let second_process = engine
            .release_at(&first_process.lease_id, first_process.epoch, now())
            .unwrap()
            .promoted
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(second_process.job_id, "run-b-attempt-1");

        assert!(matches!(
            engine
                .admit_at(
                    request("run-a-replay", &["flow", "monthly-review"], Priority::Low,),
                    now(),
                )
                .unwrap(),
            AdmitOutcome::Queued { .. }
        ));
        let replay = engine
            .release_at(&second_process.lease_id, second_process.epoch, now())
            .unwrap()
            .promoted
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(replay.job_id, "run-a-replay");
    }

    fn grouped_request(job: &str, group: LeaseSchedulingGroup, priority: Priority) -> LeaseRequest {
        let mut request = request(job, &["cpu"], priority);
        request.scheduling_group = group;
        request
    }

    fn fairness_fixture() -> (LeaseEngine, LeaseGrant) {
        let mut engine = LeaseEngine::new_with_aging_threshold(
            1,
            Duration::from_secs(20),
            Duration::from_secs(3_600),
            BTreeMap::from([("cpu".to_owned(), pool(1))]),
            None,
        )
        .unwrap();
        let holder = grant(
            engine
                .admit_at(request("holder", &["cpu"], Priority::Medium), now())
                .unwrap(),
        );
        for index in 0..400 {
            engine
                .admit_at(
                    grouped_request(
                        &format!("large-{index:03}"),
                        LeaseSchedulingGroup::Flow("large-flow".to_owned()),
                        Priority::Medium,
                    ),
                    now(),
                )
                .unwrap();
        }
        for index in 0..6 {
            engine
                .admit_at(
                    grouped_request(
                        &format!("small-{index:03}"),
                        LeaseSchedulingGroup::Flow("small-flow".to_owned()),
                        Priority::Medium,
                    ),
                    now(),
                )
                .unwrap();
        }
        for index in 0..6 {
            engine
                .admit_at(
                    grouped_request(
                        &format!("ordinary-{index:03}"),
                        LeaseSchedulingGroup::Standalone,
                        Priority::Medium,
                    ),
                    now(),
                )
                .unwrap();
        }
        (engine, holder)
    }

    fn drain_prefix(engine: &mut LeaseEngine, mut held: LeaseGrant, count: usize) -> Vec<String> {
        let mut jobs = Vec::with_capacity(count);
        for _ in 0..count {
            let promoted = engine
                .release_at(&held.lease_id, held.epoch, now())
                .unwrap()
                .promoted;
            assert_eq!(promoted.len(), 1);
            held = promoted.into_iter().next().unwrap();
            jobs.push(held.job_id.clone());
        }
        jobs
    }

    #[test]
    fn fleet_conformance_fairness_braids_400_node_flow_siblings_and_standalone_work() {
        let (mut first, first_holder) = fairness_fixture();
        let (mut second, second_holder) = fairness_fixture();
        let first_order = drain_prefix(&mut first, first_holder, 18);
        let second_order = drain_prefix(&mut second, second_holder, 18);
        let expected = (0..6)
            .flat_map(|index| {
                [
                    format!("large-{index:03}"),
                    format!("small-{index:03}"),
                    format!("ordinary-{index:03}"),
                ]
            })
            .collect::<Vec<_>>();
        assert_eq!(first_order, expected);
        assert_eq!(second_order, first_order);
    }

    #[test]
    fn no_provenance_rows_group_per_parent_and_parentless_rows_share_one_group() {
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("cpu".to_owned(), pool(1))]),
            None,
        )
        .unwrap();
        grant(
            engine
                .admit_at(request("holder", &["cpu"], Priority::Medium), now())
                .unwrap(),
        );
        for job in ["parent-a-0", "parent-a-1"] {
            engine
                .admit_at(
                    grouped_request(
                        job,
                        LeaseSchedulingGroup::Parent("parent-a".to_owned()),
                        Priority::Medium,
                    ),
                    now(),
                )
                .unwrap();
        }
        for job in ["parent-b-0", "parent-b-1"] {
            engine
                .admit_at(
                    grouped_request(
                        job,
                        LeaseSchedulingGroup::Parent("parent-b".to_owned()),
                        Priority::Medium,
                    ),
                    now(),
                )
                .unwrap();
        }
        for job in ["standalone-0", "standalone-1"] {
            engine
                .admit_at(
                    grouped_request(job, LeaseSchedulingGroup::Standalone, Priority::Medium),
                    now(),
                )
                .unwrap();
        }
        assert_eq!(
            engine
                .pending
                .iter()
                .map(|pending| pending.request.job_id.as_str())
                .collect::<Vec<_>>(),
            [
                "parent-a-0",
                "parent-b-0",
                "standalone-0",
                "parent-a-1",
                "parent-b-1",
                "standalone-1",
            ]
        );
    }

    #[test]
    fn fleet_conformance_fairness_ages_once_strictly_after_the_threshold() {
        let threshold = Duration::from_secs(3_600);
        let mut engine = LeaseEngine::new_with_aging_threshold(
            1,
            Duration::from_secs(20),
            threshold,
            BTreeMap::from([("cpu".to_owned(), pool(1))]),
            None,
        )
        .unwrap();
        grant(
            engine
                .admit_at(request("holder", &["cpu"], Priority::Interrupt), now())
                .unwrap(),
        );
        engine
            .admit_at(
                grouped_request(
                    "old-low",
                    LeaseSchedulingGroup::Flow("old".to_owned()),
                    Priority::Low,
                ),
                now(),
            )
            .unwrap();
        let boundary = now() + chrono::Duration::seconds(3_600);
        engine
            .admit_at(
                grouped_request(
                    "new-medium",
                    LeaseSchedulingGroup::Flow("new".to_owned()),
                    Priority::Medium,
                ),
                boundary,
            )
            .unwrap();
        assert_eq!(engine.pending[0].request.job_id, "new-medium");

        engine.sort_pending(boundary + chrono::Duration::milliseconds(1));
        assert_eq!(engine.pending[0].request.job_id, "old-low");
        assert_eq!(engine.pending[0].effective_rank, Priority::Medium.rank());
        assert_eq!(engine.pending[0].request.priority, Priority::Low);

        engine.sort_pending(boundary + chrono::Duration::seconds(3_601));
        assert_eq!(engine.pending[0].request.job_id, "new-medium");
        let old = engine
            .pending
            .iter()
            .find(|pending| pending.request.job_id == "old-low")
            .unwrap();
        assert_eq!(old.effective_rank, Priority::Medium.rank());
        assert_eq!(old.request.priority, Priority::Low);
    }

    #[test]
    fn every_priority_uses_the_normative_single_step_aging_map() {
        for (priority, expected) in [
            (Priority::Low, Priority::Medium.rank()),
            (Priority::Medium, Priority::High.rank()),
            (Priority::High, Priority::Interrupt.rank()),
            (Priority::Interrupt, Priority::Interrupt.rank()),
        ] {
            let pending = PendingRequest {
                ticket_id: "ticket".to_owned(),
                sequence: 1,
                admitted_at: now(),
                effective_rank: priority.rank(),
                request: request("job", &["cpu"], priority),
            };
            assert_eq!(
                effective_rank_at(
                    &pending,
                    now() + chrono::Duration::seconds(3_601),
                    Duration::from_secs(3_600),
                ),
                expected
            );
        }
    }

    #[test]
    fn durable_lease_log_contains_grants_only() {
        let temp = tempfile::tempdir().unwrap();
        let log = LeaseEventLog::in_state_dir(temp.path());
        let mut config = pool(1);
        config.hard_preempt = true;
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_millis(1),
            BTreeMap::from([("cpu".to_owned(), config)]),
            Some(log.clone()),
        )
        .unwrap();
        let holder = grant(
            engine
                .admit_at(request("holder", &["cpu"], Priority::Low), now())
                .unwrap(),
        );
        assert!(matches!(
            engine
                .admit_at(request("urgent", &["cpu"], Priority::Interrupt), now())
                .unwrap(),
            AdmitOutcome::Queued { .. }
        ));
        assert_eq!(log.read().unwrap().len(), 1);

        let outcome = engine
            .tick(now() + chrono::Duration::milliseconds(2))
            .unwrap();
        assert_eq!(outcome.preempted, [holder]);
        assert_eq!(outcome.promoted.len(), 1);
        engine
            .release_at(
                &outcome.promoted[0].lease_id,
                outcome.promoted[0].epoch,
                now(),
            )
            .unwrap();
        let events = log.read().unwrap();
        assert_eq!(events.len(), 2);
        assert!(events
            .iter()
            .all(|event| matches!(event.event, LeaseEventKind::Granted { .. })));
    }

    #[test]
    fn coallocation_is_all_or_queue_with_no_partial_debit() {
        let mut engine = LeaseEngine::new(
            7,
            Duration::from_secs(20),
            BTreeMap::from([
                ("slot".to_owned(), pool(1)),
                ("api".to_owned(), window_pool(100)),
            ]),
            None,
        )
        .unwrap();
        grant(
            engine
                .admit_at(request("holder", &["slot"], Priority::Low), now())
                .unwrap(),
        );
        let mut coalloc = request("coalloc", &["slot", "api"], Priority::High);
        coalloc.consumption_estimate = Some(40);
        assert!(matches!(
            engine.admit_at(coalloc, now()).unwrap(),
            AdmitOutcome::Queued { .. }
        ));
        assert_eq!(engine.held_in_pool("api").unwrap(), 0);
        assert_eq!(engine.budget_used_at("api", now()).unwrap(), 0);
    }

    #[test]
    fn declared_resource_kind_reports_a_configured_pools_resource_and_none_for_an_unknown_name() {
        let gpu = PoolConfig {
            resource: Some(ResourceKind::Vram),
            capacity: 1,
            ..PoolConfig::default()
        };
        let engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("gpu".to_owned(), gpu), ("cpu".to_owned(), pool(1))]),
            None,
        )
        .unwrap();
        assert_eq!(
            engine.declared_resource_kind("gpu"),
            Some(ResourceKind::Vram)
        );
        assert_eq!(
            engine.declared_resource_kind("cpu"),
            Some(ResourceKind::BuildSlot)
        );
        assert_eq!(engine.declared_resource_kind("unknown"), None);
    }

    /// #382 HIGH-1: `vram` is `ResourceKind`'s default, so a pool that
    /// declares no `resource` at all must read as "undeclared" here, not
    /// silently as "GPU pool". `PoolConfig::resource()` (the *effective*,
    /// defaulted reading every admission decision already used) still
    /// resolves the same undeclared pool to `Vram` — that behavior is
    /// unchanged by #382 and is asserted here precisely so the two readings
    /// cannot drift back together by accident.
    #[test]
    fn declared_resource_kind_is_none_for_a_pool_that_declares_no_resource_at_all() {
        let undeclared = PoolConfig {
            capacity: 1,
            ..PoolConfig::default()
        };
        assert_eq!(undeclared.resource, None);
        assert_eq!(
            undeclared.resource(),
            ResourceKind::Vram,
            "the effective reading used by admission is unchanged"
        );
        let engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("worker".to_owned(), undeclared)]),
            None,
        )
        .unwrap();
        assert_eq!(
            engine.declared_resource_kind("worker"),
            None,
            "a pool that said nothing must not read as a declared GPU pool"
        );
    }

    /// The same distinction, from the JSON an operator actually writes:
    /// `{"capacity": 4}` with no `resource` key must deserialize to
    /// `resource: None`, not to `Some(Vram)` by way of `#[serde(default)]`
    /// reaching for `ResourceKind::default()`.
    #[test]
    fn pool_config_without_a_resource_key_deserializes_to_a_declared_none() {
        let pool: PoolConfig = serde_json::from_str(r#"{"capacity": 4}"#).unwrap();
        assert_eq!(pool.resource, None);
        assert_eq!(pool.resource(), ResourceKind::Vram);

        let declared: PoolConfig = serde_json::from_str(r#"{"resource": "vram"}"#).unwrap();
        assert_eq!(declared.resource, Some(ResourceKind::Vram));
    }

    #[test]
    fn lease_admission_canonicalizes_pool_order_before_grant_and_event_persistence() {
        let temp = tempfile::tempdir().unwrap();
        let log = LeaseEventLog::in_state_dir(temp.path());
        let mut engine = LeaseEngine::new(
            3,
            Duration::from_secs(20),
            BTreeMap::from([("alpha".to_owned(), pool(1)), ("zeta".to_owned(), pool(1))]),
            Some(log.clone()),
        )
        .unwrap();
        let granted = grant(
            engine
                .admit_at(
                    request("ordered", &["zeta", "alpha"], Priority::High),
                    now(),
                )
                .unwrap(),
        );
        assert_eq!(granted.pools, ["alpha", "zeta"]);
        let events = log.read().unwrap();
        let LeaseEventKind::Granted { grant, .. } = &events[0].event else {
            panic!("expected durable grant")
        };
        assert_eq!(grant.pools, ["alpha", "zeta"]);
    }

    #[test]
    fn window_estimate_is_authoritative_and_expires() {
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("api".to_owned(), window_pool(100))]),
            None,
        )
        .unwrap();
        let mut first = request("first", &["api"], Priority::Low);
        first.consumption_estimate = Some(70);
        grant(engine.admit_at(first, now()).unwrap());
        let mut second = request("second", &["api"], Priority::High);
        second.consumption_estimate = Some(31);
        assert!(matches!(
            engine.admit_at(second, now()).unwrap(),
            AdmitOutcome::Queued { .. }
        ));
        let tick = engine.tick(now() + chrono::Duration::seconds(61)).unwrap();
        assert_eq!(tick.promoted[0].job_id, "second");
        assert_eq!(engine.queue_len(), 0);
    }

    #[test]
    fn budget_blocked_interrupt_does_not_yield_or_preempt_a_holder() {
        let mut api = window_pool(100);
        api.hard_preempt = true;
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("api".to_owned(), api)]),
            None,
        )
        .unwrap();
        let mut first = request("first", &["api"], Priority::Low);
        first.consumption_estimate = Some(90);
        let holder = grant(engine.admit_at(first, now()).unwrap());

        let mut interrupt = request("interrupt", &["api"], Priority::Interrupt);
        interrupt.consumption_estimate = Some(20);
        assert!(matches!(
            engine.admit_at(interrupt, now()).unwrap(),
            AdmitOutcome::Queued { .. }
        ));
        assert!(
            !engine
                .status(&holder.lease_id, holder.epoch)
                .unwrap()
                .yield_requested
        );

        let tick = engine.tick(now() + chrono::Duration::seconds(20)).unwrap();
        assert!(tick.preempted.is_empty());
        assert_eq!(engine.held_len(), 1);
    }

    #[test]
    fn tick_requests_yield_after_a_coallocated_budget_window_reopens() {
        let mut slot = pool(1);
        slot.hard_preempt = true;
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([
                ("api".to_owned(), window_pool(100)),
                ("slot".to_owned(), slot),
            ]),
            None,
        )
        .unwrap();
        let mut budget_user = request("budget-user", &["api"], Priority::Low);
        budget_user.consumption_estimate = Some(100);
        grant(engine.admit_at(budget_user, now()).unwrap());
        let slot_holder = grant(
            engine
                .admit_at(request("slot-holder", &["slot"], Priority::Low), now())
                .unwrap(),
        );

        let mut interrupt = request("interrupt", &["api", "slot"], Priority::Interrupt);
        interrupt.consumption_estimate = Some(1);
        assert!(matches!(
            engine.admit_at(interrupt, now()).unwrap(),
            AdmitOutcome::Queued { .. }
        ));
        assert!(
            !engine
                .status(&slot_holder.lease_id, slot_holder.epoch)
                .unwrap()
                .yield_requested
        );

        let window_reopened = now() + chrono::Duration::seconds(61);
        let tick = engine.tick(window_reopened).unwrap();
        assert!(tick.preempted.is_empty());
        assert!(
            engine
                .status(&slot_holder.lease_id, slot_holder.epoch)
                .unwrap()
                .yield_requested
        );

        let tick = engine
            .tick(window_reopened + chrono::Duration::seconds(20))
            .unwrap();
        assert_eq!(tick.preempted[0].lease_id, slot_holder.lease_id);
        assert_eq!(tick.promoted[0].job_id, "interrupt");
    }

    #[test]
    fn later_hard_preempt_request_upgrades_an_existing_soft_yield() {
        let soft = pool(1);
        let mut hard = pool(1);
        hard.hard_preempt = true;
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("hard".to_owned(), hard), ("soft".to_owned(), soft)]),
            None,
        )
        .unwrap();
        let holder = grant(
            engine
                .admit_at(request("holder", &["soft", "hard"], Priority::Low), now())
                .unwrap(),
        );

        assert!(matches!(
            engine
                .admit_at(request("soft-first", &["soft"], Priority::Interrupt), now())
                .unwrap(),
            AdmitOutcome::Queued { .. }
        ));
        assert!(engine.held[&holder.lease_id]
            .yield_demands
            .values()
            .all(|demand| !demand.hard_reclaim));

        assert!(matches!(
            engine
                .admit_at(
                    request("hard-second", &["hard"], Priority::Interrupt),
                    now() + chrono::Duration::seconds(1)
                )
                .unwrap(),
            AdmitOutcome::Queued { .. }
        ));
        assert!(engine.held[&holder.lease_id]
            .yield_demands
            .values()
            .any(|demand| demand.hard_reclaim));

        let tick = engine.tick(now() + chrono::Duration::seconds(20)).unwrap();
        assert!(tick.preempted.is_empty());
        let tick = engine.tick(now() + chrono::Duration::seconds(21)).unwrap();
        assert_eq!(tick.preempted[0].lease_id, holder.lease_id);
    }

    #[test]
    fn promoted_interrupt_cancels_its_stale_hard_yield_demand() {
        let mut shared = pool(2);
        shared.hard_preempt = true;
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("shared".to_owned(), shared)]),
            None,
        )
        .unwrap();
        let first = grant(
            engine
                .admit_at(request("first", &["shared"], Priority::Low), now())
                .unwrap(),
        );
        let second = grant(
            engine
                .admit_at(request("second", &["shared"], Priority::Low), now())
                .unwrap(),
        );
        assert!(matches!(
            engine
                .admit_at(
                    request("interrupt", &["shared"], Priority::Interrupt),
                    now()
                )
                .unwrap(),
            AdmitOutcome::Queued { .. }
        ));

        let first_flagged = engine
            .status(&first.lease_id, first.epoch)
            .unwrap()
            .yield_requested;
        let (flagged, releasable) = if first_flagged {
            (&first, &second)
        } else {
            (&second, &first)
        };
        assert!(
            engine
                .status(&flagged.lease_id, flagged.epoch)
                .unwrap()
                .yield_requested
        );

        let released = engine
            .release_at(&releasable.lease_id, releasable.epoch, now())
            .unwrap();
        assert_eq!(released.promoted[0].job_id, "interrupt");
        assert!(
            !engine
                .status(&flagged.lease_id, flagged.epoch)
                .unwrap()
                .yield_requested
        );

        let tick = engine.tick(now() + chrono::Duration::seconds(20)).unwrap();
        assert!(tick.preempted.is_empty());
        assert!(engine.held.contains_key(&flagged.lease_id));
    }

    #[test]
    fn simultaneous_interrupts_reserve_distinct_victims_and_keep_their_grace() {
        let mut shared = pool(2);
        shared.hard_preempt = true;
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("shared".to_owned(), shared)]),
            None,
        )
        .unwrap();
        let first = grant(
            engine
                .admit_at(request("first", &["shared"], Priority::Low), now())
                .unwrap(),
        );
        let second = grant(
            engine
                .admit_at(request("second", &["shared"], Priority::Low), now())
                .unwrap(),
        );
        for job in ["interrupt-a", "interrupt-b"] {
            assert!(matches!(
                engine
                    .admit_at(request(job, &["shared"], Priority::Interrupt), now())
                    .unwrap(),
                AdmitOutcome::Queued { .. }
            ));
        }

        assert!(
            engine
                .status(&first.lease_id, first.epoch)
                .unwrap()
                .yield_requested
        );
        assert!(
            engine
                .status(&second.lease_id, second.epoch)
                .unwrap()
                .yield_requested
        );
        let tick = engine.tick(now() + chrono::Duration::seconds(19)).unwrap();
        assert!(tick.preempted.is_empty());

        let tick = engine.tick(now() + chrono::Duration::seconds(20)).unwrap();
        assert_eq!(tick.preempted.len(), 2);
        assert_eq!(
            tick.promoted
                .iter()
                .map(|grant| grant.job_id.as_str())
                .collect::<Vec<_>>(),
            vec!["interrupt-a", "interrupt-b"]
        );
    }

    #[test]
    fn surviving_interrupt_keeps_its_assigned_victim_and_original_deadline() {
        let mut shared = pool(3);
        shared.hard_preempt = true;
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("shared".to_owned(), shared)]),
            None,
        )
        .unwrap();
        let holders = ["first", "second", "third"].map(|job| {
            grant(
                engine
                    .admit_at(request(job, &["shared"], Priority::Low), now())
                    .unwrap(),
            )
        });
        for job in ["interrupt-a", "interrupt-b"] {
            assert!(matches!(
                engine
                    .admit_at(request(job, &["shared"], Priority::Interrupt), now())
                    .unwrap(),
                AdmitOutcome::Queued { .. }
            ));
        }
        assert!(
            engine
                .status(&holders[0].lease_id, holders[0].epoch)
                .unwrap()
                .yield_requested
        );
        assert!(
            engine
                .status(&holders[1].lease_id, holders[1].epoch)
                .unwrap()
                .yield_requested
        );
        assert!(
            !engine
                .status(&holders[2].lease_id, holders[2].epoch)
                .unwrap()
                .yield_requested
        );

        let released = engine
            .release_at(
                &holders[2].lease_id,
                holders[2].epoch,
                now() + chrono::Duration::seconds(10),
            )
            .unwrap();
        assert_eq!(released.promoted[0].job_id, "interrupt-a");
        assert!(
            !engine
                .status(&holders[0].lease_id, holders[0].epoch)
                .unwrap()
                .yield_requested
        );
        let surviving = engine
            .status(&holders[1].lease_id, holders[1].epoch)
            .unwrap();
        assert!(surviving.yield_requested);
        assert_eq!(
            surviving.yield_deadline.as_deref(),
            Some(timestamp(now() + chrono::Duration::seconds(20)).as_str())
        );

        assert!(engine
            .tick(now() + chrono::Duration::seconds(19))
            .unwrap()
            .preempted
            .is_empty());
        let tick = engine.tick(now() + chrono::Duration::seconds(20)).unwrap();
        assert_eq!(tick.preempted[0].lease_id, holders[1].lease_id);
        assert_eq!(tick.promoted[0].job_id, "interrupt-b");
    }

    #[test]
    fn coallocation_does_not_yield_when_another_blocker_cannot_be_preempted() {
        let mut first_pool = pool(1);
        first_pool.hard_preempt = true;
        let mut second_pool = pool(1);
        second_pool.hard_preempt = true;
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([
                ("first".to_owned(), first_pool),
                ("second".to_owned(), second_pool),
            ]),
            None,
        )
        .unwrap();
        let low = grant(
            engine
                .admit_at(request("low", &["first"], Priority::Low), now())
                .unwrap(),
        );
        grant(
            engine
                .admit_at(
                    request("interrupt-holder", &["second"], Priority::Interrupt),
                    now(),
                )
                .unwrap(),
        );
        assert!(matches!(
            engine
                .admit_at(
                    request(
                        "blocked-interrupt",
                        &["first", "second"],
                        Priority::Interrupt,
                    ),
                    now(),
                )
                .unwrap(),
            AdmitOutcome::Queued { .. }
        ));
        assert!(
            !engine
                .status(&low.lease_id, low.epoch)
                .unwrap()
                .yield_requested
        );
        assert!(engine
            .tick(now() + chrono::Duration::seconds(20))
            .unwrap()
            .preempted
            .is_empty());
    }

    /// Two capacity-1 pools, one victim holding both, one interrupt asking for
    /// both. The pools disagree about `hardPreempt`, and the ruling on the
    /// record is conjunctive: `beta`'s disabled flag is a promise to its own
    /// workloads, so `alpha`'s opt-in must not reach across and kill a holder
    /// of `beta`. The yield is still requested — cooperation is always asked
    /// for — but the deadline passes without a reclaim and the interrupt stays
    /// queued.
    #[test]
    fn a_co_allocated_victim_is_never_hard_reclaimed_when_one_pool_opts_out() {
        let mut opted_in = pool(1);
        opted_in.hard_preempt = true;
        let opted_out = pool(1);
        assert!(!opted_out.hard_preempt);
        let mut engine = LeaseEngine::new(
            9,
            Duration::from_secs(20),
            BTreeMap::from([
                ("alpha".to_owned(), opted_in),
                ("beta".to_owned(), opted_out),
            ]),
            None,
        )
        .unwrap();
        let victim = grant(
            engine
                .admit_at(request("victim", &["alpha", "beta"], Priority::Low), now())
                .unwrap(),
        );
        assert!(matches!(
            engine
                .admit_at(
                    request("urgent", &["alpha", "beta"], Priority::Interrupt),
                    now(),
                )
                .unwrap(),
            AdmitOutcome::Queued { position: 1, .. }
        ));
        assert!(engine.status(&victim.lease_id, 9).unwrap().yield_requested);

        let reclaim_at = now() + chrono::Duration::seconds(20);
        assert!(engine.plan_tick(reclaim_at).unwrap().is_empty());
        let outcome = engine.tick(reclaim_at).unwrap();
        assert!(outcome.preempted.is_empty());
        assert!(outcome.promoted.is_empty());
        assert_eq!(engine.held_len(), 1);
        assert_eq!(engine.queue_len(), 1);
        assert!(matches!(
            engine.commit_preemptions(std::slice::from_ref(&victim.lease_id), reclaim_at),
            Err(LeaseError::InvalidRequest(_))
        ));
    }

    /// The other half of the ruling: when every pool the request asks the
    /// victim to yield in opts in, the grace deadline does reclaim it. The
    /// reclaim is what the daemon turns into the canonical `preempted`
    /// verdict; the lease layer's half of that is the committed preemption and
    /// the promotion it unblocks.
    #[test]
    fn a_co_allocated_victim_is_hard_reclaimed_when_every_pool_opts_in() {
        let mut alpha = pool(1);
        alpha.hard_preempt = true;
        let mut beta = pool(1);
        beta.hard_preempt = true;
        let mut engine = LeaseEngine::new(
            9,
            Duration::from_secs(20),
            BTreeMap::from([("alpha".to_owned(), alpha), ("beta".to_owned(), beta)]),
            None,
        )
        .unwrap();
        let victim = grant(
            engine
                .admit_at(request("victim", &["alpha", "beta"], Priority::Low), now())
                .unwrap(),
        );
        assert!(matches!(
            engine
                .admit_at(
                    request("urgent", &["alpha", "beta"], Priority::Interrupt),
                    now(),
                )
                .unwrap(),
            AdmitOutcome::Queued { position: 1, .. }
        ));

        let reclaim_at = now() + chrono::Duration::seconds(20);
        let planned = engine.plan_tick(reclaim_at).unwrap();
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].lease_id, victim.lease_id);
        let outcome = engine
            .commit_preemptions(std::slice::from_ref(&victim.lease_id), reclaim_at)
            .unwrap();
        assert_eq!(outcome.preempted, [victim]);
        assert_eq!(outcome.promoted[0].job_id, "urgent");
        assert_eq!(engine.queue_len(), 0);
    }

    #[test]
    fn mutex_is_a_generic_single_holder_resource() {
        let mut mutex = pool(1);
        mutex.resource = Some(ResourceKind::Mutex);
        let mut engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("deploy-main".to_owned(), mutex)]),
            None,
        )
        .unwrap();
        grant(
            engine
                .admit_at(request("first", &["deploy-main"], Priority::Low), now())
                .unwrap(),
        );
        assert!(matches!(
            engine
                .admit_at(request("second", &["deploy-main"], Priority::High), now())
                .unwrap(),
            AdmitOutcome::Queued { .. }
        ));
    }

    #[test]
    fn interrupt_requests_yield_then_hard_reclaims_when_opted_in() {
        let mut gpu = pool(1);
        gpu.hard_preempt = true;
        let mut engine = LeaseEngine::new(
            9,
            Duration::from_secs(20),
            BTreeMap::from([("gpu".to_owned(), gpu)]),
            None,
        )
        .unwrap();
        let low = grant(
            engine
                .admit_at(request("low", &["gpu"], Priority::Low), now())
                .unwrap(),
        );
        assert!(matches!(
            engine
                .admit_at(request("urgent", &["gpu"], Priority::Interrupt), now())
                .unwrap(),
            AdmitOutcome::Queued { position: 1, .. }
        ));
        assert!(engine.status(&low.lease_id, 9).unwrap().yield_requested);
        let before = engine.tick(now() + chrono::Duration::seconds(19)).unwrap();
        assert!(before.preempted.is_empty());
        assert!(before.promoted.is_empty());
        let reclaim_at = now() + chrono::Duration::seconds(20);
        let planned = engine.plan_tick(reclaim_at).unwrap();
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].lease_id, low.lease_id);
        assert_eq!(engine.held_len(), 1);
        assert_eq!(engine.queue_len(), 1);
        let outcome = engine
            .commit_preemptions(std::slice::from_ref(&low.lease_id), reclaim_at)
            .unwrap();
        assert_eq!(outcome.preempted[0].job_id, "low");
        assert_eq!(outcome.promoted[0].job_id, "urgent");
    }

    #[test]
    fn non_destructive_interrupt_waits_for_the_holder_then_runs_next() {
        let mut engine = LeaseEngine::new(
            9,
            Duration::from_secs(20),
            BTreeMap::from([("worker-gpu".to_owned(), pool(1))]),
            None,
        )
        .unwrap();
        let active_llm = grant(
            engine
                .admit_at(request("active-llm", &["worker-gpu"], Priority::Low), now())
                .unwrap(),
        );
        assert!(matches!(
            engine
                .admit_at(
                    request("thermal-cooldown", &["worker-gpu"], Priority::Interrupt),
                    now(),
                )
                .unwrap(),
            AdmitOutcome::Queued { position: 1, .. }
        ));
        assert!(
            engine
                .status(&active_llm.lease_id, active_llm.epoch)
                .unwrap()
                .yield_requested
        );

        let much_later = now() + chrono::Duration::hours(24);
        let tick = engine.tick(much_later).unwrap();
        assert!(tick.preempted.is_empty());
        assert!(tick.promoted.is_empty());
        assert_eq!(engine.held_len(), 1);
        assert_eq!(engine.queue_len(), 1);

        let released = engine
            .release_at(&active_llm.lease_id, active_llm.epoch, much_later)
            .unwrap();
        assert_eq!(released.promoted.len(), 1);
        assert_eq!(released.promoted[0].job_id, "thermal-cooldown");
    }

    #[test]
    fn failed_promotion_keeps_pending_ticket_retryable() {
        let mut engine = LeaseEngine::new(
            9,
            Duration::from_secs(20),
            BTreeMap::from([("slot".to_owned(), pool(1))]),
            None,
        )
        .unwrap();
        let holder = grant(
            engine
                .admit_at(request("holder", &["slot"], Priority::Low), now())
                .unwrap(),
        );
        let ticket = match engine
            .admit_at(request("next", &["slot"], Priority::Low), now())
            .unwrap()
        {
            AdmitOutcome::Queued { ticket_id, .. } => ticket_id,
            AdmitOutcome::Granted(_) => panic!("second request must queue"),
        };
        let held = engine.held.get(&holder.lease_id).unwrap().clone();
        engine.remove_held(&held.grant);
        let temp = tempfile::tempdir().unwrap();
        engine.events = Some(LeaseEventLog::new(temp.path()));

        assert!(matches!(engine.promote(now()), Err(LeaseError::Io { .. })));
        assert_eq!(engine.queue_len(), 1);
        assert!(!engine.status(&ticket, 9).unwrap().held);
        assert_eq!(engine.held_len(), 0);
    }

    #[test]
    fn epoch_bumps_durably_and_fences_stale_operations() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(bump_epoch(temp.path()).unwrap(), 1);
        assert_eq!(bump_epoch(temp.path()).unwrap(), 2);
        let mut engine = LeaseEngine::new(
            2,
            Duration::from_secs(20),
            BTreeMap::from([("cpu".to_owned(), pool(1))]),
            None,
        )
        .unwrap();
        let held = grant(
            engine
                .admit_at(request("job", &["cpu"], Priority::Low), now())
                .unwrap(),
        );
        assert!(matches!(
            engine.release_at(&held.lease_id, 1, now()),
            Err(LeaseError::StaleEpoch { .. })
        ));
        assert_eq!(engine.held_len(), 1);
    }

    struct FakeLiveness {
        active: Cell<bool>,
    }

    impl UnitLiveness for FakeLiveness {
        fn is_active(&self, _unit: &str) -> Result<bool, LeaseError> {
            Ok(self.active.get())
        }
    }

    #[test]
    fn local_backend_uses_unit_liveness_without_a_heartbeat_deadline() {
        let engine = LeaseEngine::new(
            1,
            Duration::from_secs(20),
            BTreeMap::from([("cpu".to_owned(), pool(1))]),
            None,
        )
        .unwrap();
        let liveness = FakeLiveness {
            active: Cell::new(true),
        };
        let mut backend = LocalLease::new(engine, liveness);
        let held = grant(
            backend
                .admit(request("job", &["cpu"], Priority::Low), now())
                .unwrap(),
        );
        assert_eq!(
            backend
                .heartbeat(&held.lease_id, held.epoch, now())
                .unwrap(),
            HeartbeatOutcome::Alive
        );
        backend.liveness.active.set(false);
        assert_eq!(
            backend
                .heartbeat(
                    &held.lease_id,
                    held.epoch,
                    now() + chrono::Duration::days(365)
                )
                .unwrap(),
            HeartbeatOutcome::HolderExited
        );
        assert_eq!(backend.engine().held_len(), 0);
    }

    #[test]
    fn systemd_liveness_fails_closed_on_probe_errors_and_unknown_states() {
        assert!(matches!(
            interpret_systemctl_show(
                "tally-job-example.service",
                false,
                b"",
                b"Failed to connect to bus"
            ),
            Err(LeaseError::Liveness(message))
                if message.contains("Failed to connect to bus")
        ));
        assert!(matches!(
            interpret_systemctl_show("tally-job-example.service", true, b"unexpected\n", b""),
            Err(LeaseError::Liveness(message)) if message.contains("unknown ActiveState")
        ));
        assert!(
            !interpret_systemctl_show("tally-job-example.service", true, b"inactive\n", b"")
                .unwrap()
        );
    }

    fn witness_body() -> WitnessBody {
        WitnessBody {
            task_uuid: Some("00000000-0000-4000-8000-000000000001".to_owned()),
            transition_timestamp: timestamp(now()),
            verdict: Verdict::Pass,
            exit_code: 0,
            artifact_content_hash: None,
            store_paths: None,
            drv: None,
            gpu_seconds: Some(99.0),
            wall_clock: 99.0,
            attempt: 1,
            lease_epoch: 3,
            dedup_key: None,
            payload_hash: None,
            brief_hash: None,
            origin: AdmissionOrigin::direct(EnqueueSource::Manual),
            orchestration: None,
            labor_class: LaborClass::Fresh,
            trace_ref: None,
            pools: vec!["api".to_owned()],
            executor: None,
            host_id: None,
            charge: Some(crate::witness::Charge {
                unit: "seconds".to_owned(),
                amount: 99.0,
                class_name: "scraped-advisory".to_owned(),
            }),
            model: None,
            evidence_class: None,
            manifest_hash: None,
            completion: None,
            error: None,
            result_revision: None,
            authorship: None,
            authorship_sessions: None,
        }
    }

    #[test]
    fn rolling_window_rebuild_reads_events_and_verified_witness() {
        let temp = tempfile::tempdir().unwrap();
        let log = LeaseEventLog::in_state_dir(temp.path());
        let witness = temp.path().join("witness.jsonl");
        WitnessLedger::open(&witness)
            .unwrap()
            .append(witness_body())
            .unwrap();
        let pools = BTreeMap::from([("api".to_owned(), window_pool(100))]);
        let mut engine =
            LeaseEngine::new(3, Duration::from_secs(20), pools.clone(), Some(log.clone())).unwrap();
        let mut metered = request("metered", &["api"], Priority::Medium);
        metered.consumption_estimate = Some(40);
        grant(engine.admit_at(metered, now()).unwrap());
        assert_eq!(
            engine.window_reset_at("api", now()).unwrap(),
            Some(timestamp(now() + chrono::Duration::seconds(60)))
        );

        let (report, records) = read_verified_records(&witness).unwrap();
        assert!(report.ok);
        let rebuilt = rebuild_window_usage(&pools, &log, &records, now()).unwrap();
        assert_eq!(rebuilt.usage["api"], 40);
        assert_eq!(rebuilt.witness_records, 1);

        let mut restarted =
            LeaseEngine::from_durable(4, Duration::from_secs(20), pools, log, &records, now())
                .unwrap();
        let mut after_restart = request("after-restart", &["api"], Priority::High);
        after_restart.consumption_estimate = Some(61);
        assert!(matches!(
            restarted.admit_at(after_restart, now()).unwrap(),
            AdmitOutcome::Queued { .. }
        ));
        assert_eq!(
            restarted
                .window_reset_at("api", now() + chrono::Duration::seconds(61))
                .unwrap(),
            None
        );
    }

    #[test]
    fn restarted_admission_debits_a_stable_attempt_only_once() {
        let temp = tempfile::tempdir().unwrap();
        let log = LeaseEventLog::in_state_dir(temp.path());
        let witness = temp.path().join("witness.jsonl");
        drop(WitnessLedger::open(&witness).unwrap());
        let pools = BTreeMap::from([("api".to_owned(), window_pool(100))]);
        let mut first =
            LeaseEngine::new(3, Duration::from_secs(20), pools.clone(), Some(log.clone())).unwrap();
        let mut request = request("stable", &["api"], Priority::Medium);
        request.admission_key = Some("stable:1".to_owned());
        request.consumption_estimate = Some(60);
        grant(first.admit_at(request.clone(), now()).unwrap());
        assert_eq!(first.budget_used_at("api", now()).unwrap(), 60);

        let (report, records) = read_verified_records(&witness).unwrap();
        assert!(report.ok);
        let mut restarted = LeaseEngine::from_durable(
            4,
            Duration::from_secs(20),
            pools.clone(),
            log.clone(),
            &records,
            now(),
        )
        .unwrap();
        grant(restarted.admit_at(request, now()).unwrap());
        assert_eq!(restarted.budget_used_at("api", now()).unwrap(), 60);
        let rebuilt = rebuild_window_usage(&pools, &log, &records, now()).unwrap();
        assert_eq!(rebuilt.usage["api"], 60);
    }

    #[test]
    fn rebuild_deduplicates_indeterminate_grants_by_admission_key() {
        let temp = tempfile::tempdir().unwrap();
        let log = LeaseEventLog::in_state_dir(temp.path());
        let witness = temp.path().join("witness.jsonl");
        drop(WitnessLedger::open(&witness).unwrap());
        for lease_id in ["lease-3-1", "lease-3-2"] {
            log.append(&LeaseEvent {
                schema_version: 1,
                observed_at: timestamp(now()),
                epoch: 3,
                event: LeaseEventKind::Granted {
                    grant: LeaseGrant {
                        lease_id: lease_id.to_owned(),
                        job_id: "stable".to_owned(),
                        unit: "tally-job-stable.service".to_owned(),
                        pools: vec!["api".to_owned()],
                        priority: Priority::Medium,
                        epoch: 3,
                        granted_at: timestamp(now()),
                        admission_key: Some("stable:1".to_owned()),
                        consumption_estimate: Some(60),
                    },
                    budget_debits: vec![BudgetDebit {
                        pool: "api".to_owned(),
                        amount: 60,
                        admitted_at: timestamp(now()),
                    }],
                },
            })
            .unwrap();
        }
        let pools = BTreeMap::from([("api".to_owned(), window_pool(100))]);
        let (report, records) = read_verified_records(&witness).unwrap();
        assert!(report.ok);
        let rebuilt = rebuild_window_usage(&pools, &log, &records, now()).unwrap();
        assert_eq!(rebuilt.usage["api"], 60);
        assert!(rebuilt.debited_admissions.contains("stable:1"));
    }

    #[test]
    #[ignore = "requires an explicitly selected NixOS host with a user manager"]
    // A test harness skip notice; the cargo test runner owns this stream.
    #[allow(clippy::disallowed_macros)]
    fn systemd_user_manager_liveness_smoke() {
        let remote_host = std::env::var("TALLY_TEST_REMOTE_HOST")
            .ok()
            .filter(|value| !value.trim().is_empty());
        if remote_host.is_none() {
            eprintln!(
                "SKIP systemd_user_manager_liveness_smoke: set TALLY_TEST_REMOTE_HOST and run this ignored test on that NixOS host"
            );
            return;
        }

        struct StopUnit(String);
        impl Drop for StopUnit {
            fn drop(&mut self) {
                let _ = Command::new("systemctl")
                    .args(["--user", "stop", &self.0])
                    .status();
            }
        }

        let base = format!("tally-live-liveness-{}", std::process::id());
        let unit = format!("{base}.service");
        let started = Command::new("systemd-run")
            .args(["--user", "--collect", "--unit", &base, "sleep", "30"])
            .output()
            .unwrap();
        assert!(
            started.status.success(),
            "systemd-run failed: {}",
            String::from_utf8_lossy(&started.stderr)
        );
        let cleanup = StopUnit(unit.clone());
        let probe = SystemdUnitLiveness::default();
        let mut active = false;
        for _ in 0..20 {
            if probe.is_active(&unit).unwrap() {
                active = true;
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(active, "transient user unit never became active");
        drop(cleanup);
        assert!(!probe.is_active(&unit).unwrap());
    }
}
