use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::adapters::{AdapterConfig, AdapterEngine, AdapterError};
use crate::producers::{validate_registry, ProducerConfig, ProducerError};

fn default_ssh_port() -> u16 {
    22
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
    #[serde(default)]
    pub resource: ResourceKind,
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
            resource: ResourceKind::default(),
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
    pub fn auto_resume_enabled(&self) -> bool {
        self.auto_resume
            .unwrap_or(matches!(self.resource, ResourceKind::Vram))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct JournaldConfig {
    #[serde(default)]
    pub native: bool,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub enqueue: EnqueueConfig,
    #[serde(default)]
    pub lease: LeaseConfig,
    #[serde(default)]
    pub pools: BTreeMap<String, PoolConfig>,
    #[serde(default)]
    pub adapters: BTreeMap<String, AdapterConfig>,
    #[serde(default)]
    pub producers: BTreeMap<String, ProducerConfig>,
    #[serde(default)]
    pub executors: BTreeMap<String, ExecutionTargetConfig>,
    #[serde(default)]
    pub journald: JournaldConfig,
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
    #[error("enqueue depthCap and fanoutCap must both be positive")]
    InvalidEnqueueGuardrail,
    #[error("lease graceSec, yieldPollSec, and yieldGraceSec must all be positive")]
    InvalidLeaseGuardrail,
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
        if self.enqueue.depth_cap == 0 || self.enqueue.fanout_cap == 0 {
            return Err(ConfigError::InvalidEnqueueGuardrail);
        }
        if self.lease.grace_sec == 0
            || self.lease.yield_poll_sec == 0
            || self.lease.yield_grace_sec == 0
        {
            return Err(ConfigError::InvalidLeaseGuardrail);
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
                if pool.resource != ResourceKind::Budget {
                    return Err(ConfigError::InvalidWindowResource { pool: name.clone() });
                }
            }
            if pool.resource == ResourceKind::Mutex
                && (pool.capacity != 1 || !matches!(pool.predicate, PoolPredicate::CoResidency(_)))
            {
                return Err(ConfigError::InvalidMutex { pool: name.clone() });
            }
            if pool.budget_gb.is_some()
                && (pool.resource != ResourceKind::Vram
                    || pool.capacity <= 1
                    || !matches!(pool.predicate, PoolPredicate::CoResidency(_)))
            {
                return Err(ConfigError::InvalidBudgetGb { pool: name.clone() });
            }
            if let Some(meter) = &pool.usage_meter {
                if pool.resource != ResourceKind::Budget
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
    fn rejects_unknown_and_deferred_enforcement() {
        let unknown = serde_json::from_str::<Config>(r#"{"pools":{},"role":"worker"}"#);
        assert!(unknown.is_err());

        let dmem =
            serde_json::from_str::<Config>(r#"{"pools":{"gpu":{"capacity":1,"enforce":"dmem"}}}"#);
        assert!(dmem.is_err());
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
                        "sources": ["notifications"],
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
}
