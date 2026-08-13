use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
pub use tally_client::DEFAULT_MAX_FRAME_BYTES;
use thiserror::Error;

use crate::adapters::{AdapterConfig, AdapterEngine, AdapterError};
use crate::producers::{validate_registry, ProducerConfig, ProducerError};

pub const DEFAULT_AGING_THRESHOLD_SEC: u64 = 3_600;
pub const DEFAULT_RETENTION_HORIZON: &str = "30d";
pub const DEFAULT_RETENTION_CALENDAR: &str = "daily";

fn default_ssh_port() -> u16 {
    22
}

const fn default_max_frame_bytes() -> u64 {
    DEFAULT_MAX_FRAME_BYTES
}

const fn default_aging_threshold_sec() -> u64 {
    DEFAULT_AGING_THRESHOLD_SEC
}

fn default_connect_timeout_sec() -> u64 {
    10
}

fn default_server_alive_interval_sec() -> u64 {
    15
}

fn default_server_alive_count_max() -> u32 {
    3
}

fn default_retry_interval_ms() -> u64 {
    1_000
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecAttestationConfig {
    #[serde(default = "default_true")]
    pub enable: bool,
}

impl Default for ExecAttestationConfig {
    fn default() -> Self {
        Self { enable: true }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AttestationsConfig {
    #[serde(default)]
    pub exec: ExecAttestationConfig,
}

/// A daemonless execution target reached through a single, explicitly
/// configured OpenSSH identity. The remote side runs the same `tally` binary
/// as a short-lived protocol helper; it does not run a tally daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SshExecutorConfig {
    pub host: String,
    pub user: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub ssh_program: PathBuf,
    pub identity_file: PathBuf,
    pub known_hosts_file: PathBuf,
    pub program: PathBuf,
    pub state_dir: PathBuf,
    #[serde(default = "default_connect_timeout_sec")]
    pub connect_timeout_sec: u64,
    #[serde(default = "default_server_alive_interval_sec")]
    pub server_alive_interval_sec: u64,
    #[serde(default = "default_server_alive_count_max")]
    pub server_alive_count_max: u32,
    #[serde(default = "default_retry_interval_ms")]
    pub retry_interval_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ExecutionTargetConfig {
    Ssh(SshExecutorConfig),
}

impl ExecutionTargetConfig {
    pub const fn ssh(&self) -> &SshExecutorConfig {
        match self {
            Self::Ssh(config) => config,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Priority {
    Interrupt,
    High,
    Medium,
    Low,
}

impl Priority {
    pub const fn rank(self) -> u16 {
        match self {
            Self::Interrupt => 1000,
            Self::High => 100,
            Self::Medium => 50,
            Self::Low => 10,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Enforce {
    #[default]
    Cooperative,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceKind {
    #[default]
    Vram,
    BuildSlot,
    CpuSlot,
    Slot,
    Budget,
    Mutex,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoResidencyPredicate {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WindowedConsumptionPredicate {
    pub window_sec: u64,
    pub consumption_cap: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PoolPredicate {
    CoResidency(CoResidencyPredicate),
    WindowedConsumption(WindowedConsumptionPredicate),
}

impl Default for PoolPredicate {
    fn default() -> Self {
        Self::CoResidency(CoResidencyPredicate {})
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EnqueueConfig {
    #[serde(default = "default_depth_cap")]
    pub depth_cap: u32,
    #[serde(default = "default_fanout_cap")]
    pub fanout_cap: u32,
    #[serde(default = "default_true")]
    pub require_dedup_key: bool,
}

impl Default for EnqueueConfig {
    fn default() -> Self {
        Self {
            depth_cap: default_depth_cap(),
            fanout_cap: default_fanout_cap(),
            require_dedup_key: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LeaseConfig {
    #[serde(default = "default_lease_grace_sec")]
    pub grace_sec: u64,
    #[serde(default = "default_yield_poll_sec")]
    pub yield_poll_sec: u64,
    #[serde(default = "default_yield_grace_sec")]
    pub yield_grace_sec: u64,
}

impl Default for LeaseConfig {
    fn default() -> Self {
        Self {
            grace_sec: default_lease_grace_sec(),
            yield_poll_sec: default_yield_poll_sec(),
            yield_grace_sec: default_yield_grace_sec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RetentionConfig {
    #[serde(default = "default_true")]
    pub enable: bool,
    #[serde(default = "default_retention_horizon")]
    pub horizon: String,
    #[serde(default = "default_retention_calendar")]
    pub on_calendar: String,
    /// Ratified state-directory envelope. These horizons drive the pruners the
    /// same single retention sweep runs; the daemon itself never reads them,
    /// but rendering them here makes `--mode check-config` reject a bad value
    /// at build time rather than at the next timer firing.
    #[serde(default = "default_capture_archive_horizon")]
    pub capture_archive_horizon: String,
    #[serde(default = "default_events_done_horizon")]
    pub events_done_horizon: String,
    #[serde(default = "default_events_rejected_horizon")]
    pub events_rejected_horizon: String,
    #[serde(default = "default_events_rejected_max_count")]
    pub events_rejected_max_count: usize,
    #[serde(default = "default_producer_marker_horizon")]
    pub producer_marker_horizon: String,
    #[serde(default = "default_lifecycle_horizon")]
    pub lifecycle_horizon: String,
    #[serde(default = "default_lifecycle_max_bytes")]
    pub lifecycle_max_bytes: u64,
}

pub const DEFAULT_STORAGE_WARNING_BYTES: u64 = 32 * 1024 * 1024 * 1024;
pub const DEFAULT_STORAGE_HARD_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub const DEFAULT_STORAGE_WARNING_FREE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub const DEFAULT_STORAGE_MINIMUM_FREE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const DEFAULT_STORAGE_POLL_INTERVAL_SEC: u64 = 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StorageBudgetConfig {
    #[serde(default = "default_storage_warning_bytes")]
    pub warning_bytes: u64,
    #[serde(default = "default_storage_hard_bytes")]
    pub hard_bytes: u64,
    #[serde(default = "default_storage_warning_free_bytes")]
    pub warning_free_bytes: u64,
    #[serde(default = "default_storage_minimum_free_bytes")]
    pub minimum_free_bytes: u64,
}

impl Default for StorageBudgetConfig {
    fn default() -> Self {
        Self {
            warning_bytes: default_storage_warning_bytes(),
            hard_bytes: default_storage_hard_bytes(),
            warning_free_bytes: default_storage_warning_free_bytes(),
            minimum_free_bytes: default_storage_minimum_free_bytes(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StorageConfig {
    #[serde(default = "default_storage_poll_interval_sec")]
    pub poll_interval_sec: u64,
    #[serde(default)]
    pub data_dir: StorageBudgetConfig,
    #[serde(default)]
    pub state_dir: StorageBudgetConfig,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            poll_interval_sec: default_storage_poll_interval_sec(),
            data_dir: StorageBudgetConfig::default(),
            state_dir: StorageBudgetConfig::default(),
        }
    }
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            enable: true,
            horizon: DEFAULT_RETENTION_HORIZON.to_owned(),
            on_calendar: DEFAULT_RETENTION_CALENDAR.to_owned(),
            capture_archive_horizon: default_capture_archive_horizon(),
            events_done_horizon: default_events_done_horizon(),
            events_rejected_horizon: default_events_rejected_horizon(),
            events_rejected_max_count: default_events_rejected_max_count(),
            producer_marker_horizon: default_producer_marker_horizon(),
            lifecycle_horizon: default_lifecycle_horizon(),
            lifecycle_max_bytes: default_lifecycle_max_bytes(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeterBudgetClass {
    #[default]
    Programmatic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageMeterConfig {
    pub argv: Vec<String>,
    #[serde(default = "default_meter_poll_interval_sec")]
    pub poll_interval_sec: u64,
    #[serde(default)]
    pub budget_class: MeterBudgetClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PoolConfig {
    /// `None` when the operator declared no `resource` at all, distinct
    /// from `Some(ResourceKind::Vram)`. `ResourceKind::Vram` is the
    /// *effective* default for every admission decision that predates
    /// #382 (see [`PoolConfig::resource`]) — but a witness fact that reads
    /// "this job held a GPU pool" must not be derivable from an operator
    /// saying nothing, so #382's `gpuSeconds` gate keys off this field
    /// directly and only ever fires on an explicit `Some(Vram)`.
    #[serde(default)]
    pub resource: Option<ResourceKind>,
    #[serde(default = "default_capacity")]
    pub capacity: u32,
    #[serde(default)]
    pub budget_gb: Option<u64>,
    #[serde(default)]
    pub predicate: PoolPredicate,
    #[serde(default)]
    pub enforce: Enforce,
    #[serde(default)]
    pub hard_preempt: bool,
    #[serde(default)]
    pub auto_resume: Option<bool>,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub credentials: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub usage_meter: Option<UsageMeterConfig>,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            resource: None,
            capacity: default_capacity(),
            budget_gb: None,
            predicate: PoolPredicate::default(),
            enforce: Enforce::default(),
            hard_preempt: false,
            auto_resume: None,
            priority: 0,
            credentials: BTreeMap::new(),
            usage_meter: None,
        }
    }
}

impl PoolConfig {
    /// The effective resource kind for admission and every other decision
    /// this pool's shape drives — unchanged by #382. An undeclared
    /// `resource` reads as `ResourceKind::default()` (`vram`) here exactly
    /// as it always has; this is deliberately the *wide* reading, not the
    /// narrow one `gpuSeconds` gates on (see the field doc).
    pub fn resource(&self) -> ResourceKind {
        self.resource.unwrap_or_default()
    }

    pub fn auto_resume_enabled(&self) -> bool {
        self.auto_resume
            .unwrap_or(self.resource() == ResourceKind::Vram)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct JournaldConfig {
    #[serde(default)]
    pub native: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FlowRegistration {
    pub script: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_mutex: Option<String>,
}

const fn default_capacity() -> u32 {
    1
}

const fn default_depth_cap() -> u32 {
    3
}

const fn default_fanout_cap() -> u32 {
    64
}

const fn default_true() -> bool {
    true
}

fn default_retention_horizon() -> String {
    DEFAULT_RETENTION_HORIZON.to_owned()
}

fn default_retention_calendar() -> String {
    DEFAULT_RETENTION_CALENDAR.to_owned()
}

fn default_capture_archive_horizon() -> String {
    crate::retention::DEFAULT_CAPTURE_ARCHIVE_MAX_AGE.to_owned()
}

fn default_events_done_horizon() -> String {
    crate::retention::DEFAULT_EVENTS_DONE_MAX_AGE.to_owned()
}

fn default_events_rejected_horizon() -> String {
    crate::retention::DEFAULT_EVENTS_REJECTED_MAX_AGE.to_owned()
}

const fn default_events_rejected_max_count() -> usize {
    crate::retention::DEFAULT_EVENTS_REJECTED_MAX_COUNT
}

fn default_producer_marker_horizon() -> String {
    crate::retention::DEFAULT_PRODUCER_MARKER_MAX_AGE.to_owned()
}

fn default_lifecycle_horizon() -> String {
    "30d".to_owned()
}

const fn default_lifecycle_max_bytes() -> u64 {
    256 * 1024 * 1024
}

const fn default_storage_warning_bytes() -> u64 {
    DEFAULT_STORAGE_WARNING_BYTES
}

const fn default_storage_hard_bytes() -> u64 {
    DEFAULT_STORAGE_HARD_BYTES
}

const fn default_storage_warning_free_bytes() -> u64 {
    DEFAULT_STORAGE_WARNING_FREE_BYTES
}

const fn default_storage_minimum_free_bytes() -> u64 {
    DEFAULT_STORAGE_MINIMUM_FREE_BYTES
}

const fn default_storage_poll_interval_sec() -> u64 {
    DEFAULT_STORAGE_POLL_INTERVAL_SEC
}

const fn default_lease_grace_sec() -> u64 {
    90
}

const fn default_yield_poll_sec() -> u64 {
    5
}

const fn default_yield_grace_sec() -> u64 {
    20
}

const fn default_meter_poll_interval_sec() -> u64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    #[serde(default = "default_max_frame_bytes")]
    pub max_frame_bytes: u64,
    #[serde(default = "default_aging_threshold_sec")]
    pub aging_threshold_sec: u64,
    #[serde(default)]
    pub enqueue: EnqueueConfig,
    #[serde(default)]
    pub lease: LeaseConfig,
    #[serde(default)]
    pub retention: RetentionConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub attestations: AttestationsConfig,
    #[serde(default)]
    pub pools: BTreeMap<String, PoolConfig>,
    #[serde(default)]
    pub flows: BTreeMap<String, FlowRegistration>,
    #[serde(default)]
    pub adapters: BTreeMap<String, AdapterConfig>,
    #[serde(default)]
    pub producers: BTreeMap<String, ProducerConfig>,
    #[serde(default)]
    pub executors: BTreeMap<String, ExecutionTargetConfig>,
    #[serde(default)]
    pub journald: JournaldConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            aging_threshold_sec: DEFAULT_AGING_THRESHOLD_SEC,
            enqueue: EnqueueConfig::default(),
            lease: LeaseConfig::default(),
            retention: RetentionConfig::default(),
            storage: StorageConfig::default(),
            attestations: AttestationsConfig::default(),
            pools: BTreeMap::new(),
            flows: BTreeMap::new(),
            adapters: BTreeMap::new(),
            producers: BTreeMap::new(),
            executors: BTreeMap::new(),
            journald: JournaldConfig::default(),
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read config {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid JSON configuration: {0}")]
    Json(#[from] serde_json::Error),
    #[error("pool {0:?} must have a positive capacity")]
    ZeroCapacity(String),
    #[error("pool {0:?} has an empty name")]
    EmptyPoolName(String),
    #[error("windowed-consumption pool {pool:?} must have positive windowSec and consumptionCap")]
    InvalidWindow { pool: String },
    #[error("mutex pool {pool:?} must use co-residency with capacity 1")]
    InvalidMutex { pool: String },
    #[error("pool {pool:?} budgetGb is valid only for a co-resident vram pool with capacity > 1")]
    InvalidBudgetGb { pool: String },
    #[error("windowed-consumption pool {pool:?} must use resource=budget")]
    InvalidWindowResource { pool: String },
    #[error("pool {pool:?} usageMeter requires a windowed-consumption budget pool")]
    InvalidUsageMeterPool { pool: String },
    #[error(
        "pool {pool:?} usageMeter requires a non-empty direct argv and positive pollIntervalSec"
    )]
    InvalidUsageMeter { pool: String },
    #[error("pool {pool:?} has an invalid credential: {detail}")]
    InvalidCredential { pool: String, detail: String },
    #[error("flow {flow:?} is invalid: {detail}")]
    InvalidFlow { flow: String, detail: String },
    #[error("enqueue depthCap and fanoutCap must both be positive")]
    InvalidEnqueueGuardrail,
    #[error("lease graceSec, yieldPollSec, and yieldGraceSec must all be positive")]
    InvalidLeaseGuardrail,
    #[error("retention horizon is invalid: {0}")]
    InvalidRetentionHorizon(String),
    #[error("retention onCalendar must be non-empty")]
    InvalidRetentionCalendar,
    #[error("storage pollIntervalSec must be positive")]
    InvalidStoragePollInterval,
    #[error(
        "storage {store} budget requires 0 < warningBytes < hardBytes and 0 < minimumFreeBytes < warningFreeBytes"
    )]
    InvalidStorageBudget { store: &'static str },
    #[error("maxFrameBytes and agingThresholdSec must both be positive")]
    InvalidFlowRuntimeLimit,
    #[error("executor {executor:?} is invalid: {detail}")]
    InvalidExecutor { executor: String, detail: String },
    #[error("adapter configuration is invalid: {0}")]
    Adapter(#[from] AdapterError),
    #[error("producer configuration is invalid: {0}")]
    Producer(#[from] ProducerError),
}

impl Config {
    pub fn from_path(path: &Path) -> Result<Self, ConfigError> {
        let bytes = std::fs::read(path).map_err(|source| ConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let config: Self = serde_json::from_slice(&bytes)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        AdapterEngine::new(&self.adapters).validate_all()?;
        if self.max_frame_bytes == 0 || self.aging_threshold_sec == 0 {
            return Err(ConfigError::InvalidFlowRuntimeLimit);
        }
        if self.enqueue.depth_cap == 0 || self.enqueue.fanout_cap == 0 {
            return Err(ConfigError::InvalidEnqueueGuardrail);
        }
        if self.lease.grace_sec == 0
            || self.lease.yield_poll_sec == 0
            || self.lease.yield_grace_sec == 0
        {
            return Err(ConfigError::InvalidLeaseGuardrail);
        }
        for horizon in [
            &self.retention.horizon,
            &self.retention.capture_archive_horizon,
            &self.retention.events_done_horizon,
            &self.retention.events_rejected_horizon,
            &self.retention.lifecycle_horizon,
        ] {
            crate::retention::parse_horizon(horizon)
                .map_err(|error| ConfigError::InvalidRetentionHorizon(error.to_string()))?;
        }
        if self.retention.on_calendar.trim().is_empty() {
            return Err(ConfigError::InvalidRetentionCalendar);
        }
        if self.retention.lifecycle_max_bytes == 0 {
            return Err(ConfigError::InvalidRetentionHorizon(
                "lifecycleMaxBytes must be positive".to_owned(),
            ));
        }
        if self.storage.poll_interval_sec == 0 {
            return Err(ConfigError::InvalidStoragePollInterval);
        }
        for (store, budget) in [
            ("dataDir", &self.storage.data_dir),
            ("stateDir", &self.storage.state_dir),
        ] {
            if budget.warning_bytes == 0
                || budget.warning_bytes >= budget.hard_bytes
                || budget.minimum_free_bytes == 0
                || budget.minimum_free_bytes >= budget.warning_free_bytes
            {
                return Err(ConfigError::InvalidStorageBudget { store });
            }
        }
        for (name, pool) in &self.pools {
            if name.trim().is_empty() {
                return Err(ConfigError::EmptyPoolName(name.clone()));
            }
            if pool.capacity == 0 {
                return Err(ConfigError::ZeroCapacity(name.clone()));
            }
            if let PoolPredicate::WindowedConsumption(window) = &pool.predicate {
                if window.window_sec == 0 || window.consumption_cap == 0 {
                    return Err(ConfigError::InvalidWindow { pool: name.clone() });
                }
                if pool.resource() != ResourceKind::Budget {
                    return Err(ConfigError::InvalidWindowResource { pool: name.clone() });
                }
            }
            if pool.resource() == ResourceKind::Mutex
                && (pool.capacity != 1 || !matches!(pool.predicate, PoolPredicate::CoResidency(_)))
            {
                return Err(ConfigError::InvalidMutex { pool: name.clone() });
            }
            if pool.budget_gb.is_some()
                && (pool.resource() != ResourceKind::Vram
                    || pool.capacity <= 1
                    || !matches!(pool.predicate, PoolPredicate::CoResidency(_)))
            {
                return Err(ConfigError::InvalidBudgetGb { pool: name.clone() });
            }
            if let Some(meter) = &pool.usage_meter {
                if pool.resource() != ResourceKind::Budget
                    || !matches!(pool.predicate, PoolPredicate::WindowedConsumption(_))
                {
                    return Err(ConfigError::InvalidUsageMeterPool { pool: name.clone() });
                }
                if meter.argv.is_empty()
                    || meter.argv[0].is_empty()
                    || meter.argv.iter().any(|argument| argument.contains('\0'))
                    || meter.poll_interval_sec == 0
                {
                    return Err(ConfigError::InvalidUsageMeter { pool: name.clone() });
                }
            }
            for (credential, source) in &pool.credentials {
                validate_credential(credential, source).map_err(|detail| {
                    ConfigError::InvalidCredential {
                        pool: name.clone(),
                        detail,
                    }
                })?;
            }
        }
        for (name, flow) in &self.flows {
            validate_executor_name(name).map_err(|detail| ConfigError::InvalidFlow {
                flow: name.clone(),
                detail,
            })?;
            if !flow.script.is_absolute() {
                return Err(ConfigError::InvalidFlow {
                    flow: name.clone(),
                    detail: "script must be an absolute path".to_owned(),
                });
            }
            let Some(mutex_name) = flow.workload_mutex.as_deref() else {
                continue;
            };
            if mutex_name.is_empty() || matches!(mutex_name, "flow" | "build") {
                return Err(ConfigError::InvalidFlow {
                    flow: name.clone(),
                    detail: "workloadMutex must be a non-reserved pool name".to_owned(),
                });
            }
            let Some(pool) = self.pools.get(mutex_name) else {
                return Err(ConfigError::InvalidFlow {
                    flow: name.clone(),
                    detail: format!("workloadMutex references unknown pool {mutex_name:?}"),
                });
            };
            if pool.resource() != ResourceKind::Mutex
                || pool.capacity != 1
                || !matches!(pool.predicate, PoolPredicate::CoResidency(_))
            {
                return Err(ConfigError::InvalidFlow {
                    flow: name.clone(),
                    detail: format!(
                        "workloadMutex pool {mutex_name:?} must be a capacity-1 co-residency mutex"
                    ),
                });
            }
        }
        for (name, target) in &self.executors {
            validate_executor_name(name).map_err(|detail| ConfigError::InvalidExecutor {
                executor: name.clone(),
                detail,
            })?;
            validate_ssh_executor(target.ssh()).map_err(|detail| ConfigError::InvalidExecutor {
                executor: name.clone(),
                detail,
            })?;
        }
        validate_registry(
            &self.producers,
            &self.pools.keys().cloned().collect(),
            &self.adapters.keys().cloned().collect(),
            &self.executors.keys().cloned().collect(),
        )?;
        Ok(())
    }
}

fn validate_executor_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 96
        || matches!(name, "." | "..")
        || !name
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    {
        return Err("name is not a safe registry component".to_owned());
    }
    Ok(())
}

fn validate_ssh_executor(config: &SshExecutorConfig) -> Result<(), String> {
    let safe_host = !config.host.is_empty()
        && !config.host.starts_with('-')
        && config
            .host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'));
    if !safe_host {
        return Err("host must be a non-option DNS name or IP literal".to_owned());
    }
    let safe_user = !config.user.is_empty()
        && !config.user.starts_with('-')
        && config
            .user
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
    if !safe_user {
        return Err("user is not a safe OpenSSH login name".to_owned());
    }
    if config.port == 0 {
        return Err("port must be positive".to_owned());
    }
    for (label, path) in [
        ("sshProgram", &config.ssh_program),
        ("identityFile", &config.identity_file),
        ("knownHostsFile", &config.known_hosts_file),
        ("program", &config.program),
        ("stateDir", &config.state_dir),
    ] {
        let Some(value) = path.to_str() else {
            return Err(format!("{label} must be valid UTF-8"));
        };
        if !path.is_absolute() || value.chars().any(char::is_control) || value.contains('\0') {
            return Err(format!(
                "{label} must be an absolute path without control characters"
            ));
        }
    }
    // OpenSSH joins the fixed remote helper argv into one remote command. The
    // program path therefore uses a deliberately narrow shell-word alphabet;
    // job argv itself is carried only in JSON stdin.
    if !config.program.to_string_lossy().bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'/' | b'_' | b'.' | b'+' | b',' | b'@' | b'=' | b'-')
    }) {
        return Err(
            "program contains characters unsafe for the fixed SSH helper command".to_owned(),
        );
    }
    if config.connect_timeout_sec == 0
        || config.server_alive_interval_sec == 0
        || config.server_alive_count_max == 0
        || !(10..=60_000).contains(&config.retry_interval_ms)
    {
        return Err(
            "timeouts and retryIntervalMs must be positive (retryIntervalMs 10..=60000)".to_owned(),
        );
    }
    Ok(())
}

fn validate_credential(name: &str, source: &Path) -> Result<(), String> {
    let valid_name = !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
    if !valid_name {
        return Err(format!("invalid name {name:?}"));
    }
    let Some(source) = source.to_str() else {
        return Err(format!("credential {name:?} path must be valid UTF-8"));
    };
    if !source.starts_with('/') || source.contains('%') || source.chars().any(char::is_control) {
        return Err(format!(
            "credential {name:?} source must be an absolute, systemd-safe path"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_priority_ranks_are_stable() {
        assert_eq!(Priority::Interrupt.rank(), 1000);
        assert_eq!(Priority::High.rank(), 100);
        assert_eq!(Priority::Medium.rank(), 50);
        assert_eq!(Priority::Low.rank(), 10);
    }

    #[test]
    fn flow_runtime_limits_are_serde_defaulted_and_positive() {
        let legacy: Config = serde_json::from_str(r#"{"pools":{}}"#).unwrap();
        assert_eq!(legacy.max_frame_bytes, DEFAULT_MAX_FRAME_BYTES);
        assert_eq!(legacy.aging_threshold_sec, DEFAULT_AGING_THRESHOLD_SEC);
        assert_eq!(legacy.retention, RetentionConfig::default());
        assert_eq!(legacy.attestations, AttestationsConfig::default());
        legacy.validate().unwrap();

        let configured: Config = serde_json::from_str(
            r#"{"maxFrameBytes":20971520,"agingThresholdSec":900,"pools":{}}"#,
        )
        .unwrap();
        assert_eq!(configured.max_frame_bytes, 20 * 1024 * 1024);
        assert_eq!(configured.aging_threshold_sec, 900);
        configured.validate().unwrap();

        for invalid in [
            r#"{"maxFrameBytes":0,"pools":{}}"#,
            r#"{"agingThresholdSec":0,"pools":{}}"#,
        ] {
            assert!(matches!(
                serde_json::from_str::<Config>(invalid).unwrap().validate(),
                Err(ConfigError::InvalidFlowRuntimeLimit)
            ));
        }
    }

    #[test]
    fn execution_attestations_are_default_on_and_strictly_shaped() {
        let configured: Config =
            serde_json::from_str(r#"{"pools":{},"attestations":{"exec":{"enable":false}}}"#)
                .unwrap();
        assert!(!configured.attestations.exec.enable);
        assert!(serde_json::from_str::<Config>(
            r#"{"pools":{},"attestations":{"exec":{"enable":true,"ledger":"elsewhere"}}}"#
        )
        .is_err());
    }

    #[test]
    fn retention_is_default_on_strict_and_uses_a_systemd_timespan() {
        let configured: Config = serde_json::from_str(
            r#"{"pools":{},"retention":{"enable":false,"horizon":"2h 30min","onCalendar":"weekly"}}"#,
        )
        .unwrap();
        configured.validate().unwrap();
        assert!(!configured.retention.enable);

        let envelope: Config = serde_json::from_str(
            r#"{"pools":{},"retention":{"captureArchiveHorizon":"7d","eventsDoneHorizon":"1y","eventsRejectedHorizon":"12h","eventsRejectedMaxCount":25}}"#,
        )
        .unwrap();
        envelope.validate().unwrap();
        assert_eq!(envelope.retention.events_rejected_max_count, 25);
        assert_eq!(
            Config::default().retention.events_rejected_max_count,
            crate::retention::DEFAULT_EVENTS_REJECTED_MAX_COUNT
        );
        assert_eq!(
            Config::default().retention.lifecycle_max_bytes,
            256 * 1024 * 1024
        );

        for invalid in [
            r#"{"pools":{},"retention":{"horizon":"never"}}"#,
            r#"{"pools":{},"retention":{"onCalendar":""}}"#,
            r#"{"pools":{},"retention":{"enable":true,"extra":false}}"#,
            r#"{"pools":{},"retention":{"captureArchiveHorizon":"never"}}"#,
            r#"{"pools":{},"retention":{"eventsDoneHorizon":""}}"#,
            r#"{"pools":{},"retention":{"eventsRejectedHorizon":"1fortnight"}}"#,
            r#"{"pools":{},"retention":{"lifecycleHorizon":""}}"#,
            r#"{"pools":{},"retention":{"lifecycleMaxBytes":0}}"#,
        ] {
            let result = serde_json::from_str::<Config>(invalid)
                .map_err(ConfigError::from)
                .and_then(|config| config.validate());
            assert!(result.is_err());
        }
    }

    #[test]
    fn storage_budgets_are_defaulted_strict_and_ordered() {
        let defaults: Config = serde_json::from_str(r#"{"pools":{}}"#).unwrap();
        defaults.validate().unwrap();
        assert_eq!(defaults.storage.poll_interval_sec, 60);
        assert_eq!(
            defaults.storage.data_dir.warning_bytes,
            32 * 1024 * 1024 * 1024
        );
        assert_eq!(
            defaults.storage.data_dir.hard_bytes,
            64 * 1024 * 1024 * 1024
        );
        assert_eq!(
            defaults.storage.data_dir.minimum_free_bytes,
            8 * 1024 * 1024 * 1024
        );
        assert_eq!(
            defaults.storage.data_dir.warning_free_bytes,
            16 * 1024 * 1024 * 1024
        );

        let configured: Config = serde_json::from_str(
            r#"{"pools":{},"storage":{"pollIntervalSec":5,"dataDir":{"warningBytes":10,"hardBytes":20,"warningFreeBytes":4,"minimumFreeBytes":2},"stateDir":{"warningBytes":30,"hardBytes":40,"warningFreeBytes":5,"minimumFreeBytes":3}}}"#,
        )
        .unwrap();
        configured.validate().unwrap();
        assert_eq!(configured.storage.data_dir.minimum_free_bytes, 2);
        for invalid in [
            r#"{"pools":{},"storage":{"pollIntervalSec":0}}"#,
            r#"{"pools":{},"storage":{"dataDir":{"warningBytes":20,"hardBytes":20}}}"#,
            r#"{"pools":{},"storage":{"stateDir":{"warningBytes":40,"hardBytes":30}}}"#,
            r#"{"pools":{},"storage":{"dataDir":{"minimumFreeBytes":0}}}"#,
            r#"{"pools":{},"storage":{"dataDir":{"minimumFreeBytes":10,"warningFreeBytes":10}}}"#,
            r#"{"pools":{},"storage":{"extra":1}}"#,
        ] {
            let result = serde_json::from_str::<Config>(invalid)
                .map_err(ConfigError::from)
                .and_then(|config| config.validate());
            assert!(result.is_err(), "accepted invalid storage policy {invalid}");
        }
    }

    #[test]
    fn rejects_unknown_and_deferred_enforcement() {
        let unknown = serde_json::from_str::<Config>(r#"{"pools":{},"role":"worker"}"#);
        assert!(unknown.is_err());

        let dmem =
            serde_json::from_str::<Config>(r#"{"pools":{"gpu":{"capacity":1,"enforce":"dmem"}}}"#);
        assert!(dmem.is_err());
    }

    #[test]
    fn retired_authorship_config_key_fails_load() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.json");
        // Exercise the retired wire spelling without restoring it as a source symbol.
        let removed_key = ["git", "Ai"].concat();
        let config = format!(r#"{{"pools":{{}},"{removed_key}":{{"enable":true}}}}"#);
        std::fs::write(&config_path, config).unwrap();

        let error = Config::from_path(&config_path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("unknown field `{removed_key}`")),
            "stale config failed without identifying the removed key: {error}"
        );
    }

    #[test]
    fn journald_native_is_an_explicit_default_off_toggle() {
        let default: Config = serde_json::from_str(r#"{"pools":{}}"#).unwrap();
        assert!(!default.journald.native);
        let native: Config =
            serde_json::from_str(r#"{"pools":{},"journald":{"native":true}}"#).unwrap();
        assert!(native.journald.native);
        assert!(serde_json::from_str::<Config>(
            r#"{"pools":{},"journald":{"native":true,"fallback":true}}"#
        )
        .is_err());
    }

    #[test]
    fn adapters_are_open_typed_and_strictly_validated() {
        let config: Config = serde_json::from_str(
            r#"{
                "pools": {},
                "adapters": {
                    "from-pure-nix": {
                        "argv": ["agent", "--json"],
                        "resume": ["agent", "resume", "%<sessionRef>%"],
                        "scrape": {
                            "sessionRef": {"mode": "jsonPath", "pattern": "$..session_id"}
                        },
                        "yieldHook": ["tally", "lease", "status"],
                        "env": {"NO_COLOR": "1"},
                        "skillRevision": "review-agent-v3",
                        "extraConfig": {"modelFlag": "--model"}
                    }
                }
            }"#,
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(
            config.adapters["from-pure-nix"].extra_config["modelFlag"],
            "--model"
        );
        assert_eq!(
            config.adapters["from-pure-nix"]
                .resolved_skill_revision()
                .as_deref(),
            Some("review-agent-v3")
        );

        let unknown_field = serde_json::from_str::<Config>(
            r#"{"pools":{},"adapters":{"bad":{"argv":[],"shell":"echo nope"}}}"#,
        );
        assert!(unknown_field.is_err());
    }

    #[test]
    fn tagged_predicates_and_mutex_are_validated() {
        let valid: Config = serde_json::from_str(
            r#"{
                "pools": {
                    "api": {
                        "resource": "budget",
                        "predicate": {"windowed-consumption": {"windowSec": 60, "consumptionCap": 100}}
                    },
                    "deploy": {"resource": "mutex", "capacity": 1}
                }
            }"#,
        )
        .unwrap();
        valid.validate().unwrap();

        let invalid: Config =
            serde_json::from_str(r#"{"pools":{"deploy":{"resource":"mutex","capacity":2}}}"#)
                .unwrap();
        assert!(matches!(
            invalid.validate(),
            Err(ConfigError::InvalidMutex { .. })
        ));
    }

    #[test]
    fn counted_slot_accepts_multi_holder_external_capacity() {
        let config: Config = serde_json::from_str(
            r#"{
                "pools": {
                    "codex-window": {"resource": "slot", "capacity": 16}
                }
            }"#,
        )
        .unwrap();
        config.validate().unwrap();

        let pool = &config.pools["codex-window"];
        assert_eq!(pool.resource, Some(ResourceKind::Slot));
        assert_eq!(pool.resource(), ResourceKind::Slot);
        assert_eq!(pool.capacity, 16);
        assert!(matches!(pool.predicate, PoolPredicate::CoResidency(_)));
        assert_eq!(
            serde_json::to_value(pool.resource).unwrap(),
            serde_json::json!("slot")
        );
    }

    #[test]
    fn flow_registrations_bind_only_typed_workload_mutexes() {
        let valid: Config = serde_json::from_str(
            r#"{
                "pools": {
                    "flow": {"resource": "cpu-slot", "capacity": 8},
                    "review": {"resource": "mutex", "capacity": 1}
                },
                "flows": {
                    "monthly-review": {
                        "script": "/nix/store/00000000000000000000000000000000-monthly-review.js",
                        "workloadMutex": "review"
                    }
                }
            }"#,
        )
        .unwrap();
        valid.validate().unwrap();

        for mutex in ["missing", "flow"] {
            let mut invalid = valid.clone();
            invalid
                .flows
                .get_mut("monthly-review")
                .unwrap()
                .workload_mutex = Some(mutex.to_owned());
            assert!(matches!(
                invalid.validate(),
                Err(ConfigError::InvalidFlow { .. })
            ));
        }

        let mut wrong_shape = valid;
        wrong_shape
            .flows
            .get_mut("monthly-review")
            .unwrap()
            .workload_mutex = Some("flow-slot".to_owned());
        wrong_shape
            .pools
            .insert("flow-slot".to_owned(), PoolConfig::default());
        assert!(matches!(
            wrong_shape.validate(),
            Err(ConfigError::InvalidFlow { .. })
        ));
    }

    #[test]
    fn ssh_executors_are_explicit_strict_and_shell_safe() {
        let valid: Config = serde_json::from_value(serde_json::json!({
            "pools": {},
            "executors": {
                "worker": {
                    "kind": "ssh",
                    "host": "worker.example",
                    "user": "tally-worker",
                    "port": 2222,
                    "sshProgram": "/run/current-system/sw/bin/ssh",
                    "identityFile": "/run/credentials/tally-worker-key",
                    "knownHostsFile": "/etc/tally/worker-known-hosts",
                    "program": "/run/current-system/sw/bin/tally",
                    "stateDir": "/var/lib/tally-remote"
                }
            }
        }))
        .unwrap();
        valid.validate().unwrap();
        let rendered = serde_json::to_value(&valid).unwrap();
        assert_eq!(rendered["executors"]["worker"]["kind"], "ssh");
        assert_eq!(rendered["executors"]["worker"]["port"], 2222);

        for mutate in [
            |config: &mut SshExecutorConfig| config.host = "-oProxyCommand=bad".to_owned(),
            |config: &mut SshExecutorConfig| config.user = "bad user".to_owned(),
            |config: &mut SshExecutorConfig| config.identity_file = PathBuf::from("relative"),
            |config: &mut SshExecutorConfig| config.program = PathBuf::from("/run/tally;touch-bad"),
            |config: &mut SshExecutorConfig| config.retry_interval_ms = 1,
        ] {
            let mut invalid = valid.clone();
            let ExecutionTargetConfig::Ssh(target) = invalid.executors.get_mut("worker").unwrap();
            mutate(target);
            assert!(matches!(
                invalid.validate(),
                Err(ConfigError::InvalidExecutor { .. })
            ));
        }

        assert!(serde_json::from_value::<Config>(serde_json::json!({
            "pools": {},
            "executors": {
                "worker": {
                    "kind": "ssh",
                    "host": "worker.example",
                    "user": "tally-worker",
                    "sshProgram": "/bin/ssh",
                    "identityFile": "/key",
                    "knownHostsFile": "/known-hosts",
                    "program": "/bin/tally",
                    "stateDir": "/state",
                    "ambientConfig": true
                }
            }
        }))
        .is_err());
    }

    #[test]
    fn producer_registry_is_strict_and_validates_the_reference_graph() {
        let valid: Config = serde_json::from_str(
            r#"{
                "pools": {"slot": {"resource": "build-slot"}},
                "adapters": {"shell": {"argv": []}},
                "producers": {
                    "daily": {
                        "kind": "calendar",
                        "onCalendar": "daily",
                        "enqueue": {"argv": ["daily-job"], "pool": "slot"}
                    },
                    "drop": {"kind": "events-dir"},
                    "github": {
                        "kind": "gh",
                        "enable": true,
                        "sources": [{"notifications": {"repo": "acme/widgets"}}],
                        "triggers": {"assignments": ["tally-bot"]},
                        "enqueue": {"argv": ["gh-job"], "pool": "slot"}
                    },
                    "effect": {
                        "kind": "build-effect",
                        "path": "/var/lib/tally/effects.jsonl",
                        "onKey": {"argv": ["effect-job"], "pool": "slot"}
                    },
                    "health": {
                        "kind": "pool-reachability",
                        "probePool": "slot",
                        "onReturnAttest": {
                            "argv": ["assess"],
                            "pool": "slot",
                            "noEnqueue": true
                        }
                    }
                }
            }"#,
        )
        .unwrap();
        valid.validate().unwrap();

        assert!(serde_json::from_str::<Config>(
            r#"{
                "pools": {"slot": {}},
                "adapters": {"shell": {"argv": []}},
                "producers": {
                    "deferred": {"kind": "r2", "enqueue": {"argv": ["x"], "pool": "slot"}}
                }
            }"#
        )
        .is_err());
        assert!(serde_json::from_str::<Config>(
            r#"{
                "pools": {"slot": {}},
                "adapters": {"shell": {"argv": []}},
                "producers": {
                    "bad": {
                        "kind": "calendar",
                        "onCalendar": "daily",
                        "pool": "slot",
                        "enqueue": {"argv": ["x"], "pool": "slot"}
                    }
                }
            }"#
        )
        .is_err());

        let unknown_pool: Config = serde_json::from_str(
            r#"{
                "pools": {"slot": {}},
                "adapters": {"shell": {"argv": []}},
                "producers": {
                    "daily": {
                        "kind": "calendar",
                        "onCalendar": "daily",
                        "enqueue": {"argv": ["x"], "pool": "missing"}
                    }
                }
            }"#,
        )
        .unwrap();
        assert!(unknown_pool
            .validate()
            .unwrap_err()
            .to_string()
            .contains("unknown pool"));

        let unknown_executor: Config = serde_json::from_str(
            r#"{
                "pools": {"slot": {}},
                "adapters": {"shell": {"argv": []}},
                "producers": {
                    "daily": {
                        "kind": "calendar",
                        "onCalendar": "daily",
                        "enqueue": {
                            "argv": ["x"],
                            "pool": "slot",
                            "executor": "missing-worker"
                        }
                    }
                }
            }"#,
        )
        .unwrap();
        assert!(unknown_executor
            .validate()
            .unwrap_err()
            .to_string()
            .contains("unknown executor"));
    }

    /// The pinned cross-language vector for #382's HIGH-1 repair.
    ///
    /// `test/fixtures/pools/resource-declaration.golden.json` is rendered by
    /// `nix/modules/common.nix`'s `mkRuntimeConfig`/`renderPool` from a
    /// pool that declares no `resource` and one that declares `"vram"`
    /// explicitly (`.#checks.<system>.pool-resource-declaration` re-renders
    /// it live and `cmp`s against this exact file, so Nix's rendering and
    /// this fixture cannot drift apart silently). This test pins the other
    /// half of the contract: that `PoolConfig`'s `Deserialize` reads that
    /// exact rendered shape the way #382 requires — an absent `resource`
    /// key as `None` (undeclared), never as `Some(Vram)` by way of
    /// defaulting, and a present `"vram"` key as `Some(Vram)` (declared).
    #[test]
    fn pool_config_reads_the_nix_rendered_declared_vs_undeclared_fixture_correctly() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test/fixtures/pools/resource-declaration.golden.json");
        let rendered: BTreeMap<String, PoolConfig> =
            serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();

        let undeclared = &rendered["undeclared"];
        assert_eq!(undeclared.resource, None);
        assert_eq!(
            undeclared.resource(),
            ResourceKind::Vram,
            "the effective admission reading stays vram, unchanged by #382"
        );

        let declared = &rendered["declared"];
        assert_eq!(declared.resource, Some(ResourceKind::Vram));
        assert_eq!(declared.resource(), ResourceKind::Vram);
    }
}
