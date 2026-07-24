use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use taskchampion::Uuid;
use thiserror::Error;

use crate::adapters::AdapterJobOptions;
use crate::completion::{
    AcceptanceFact, AcceptanceStatus, GateManifestSpec, GateSummary, GateSummaryStatus,
    SemanticCompletion,
};
use crate::config::Priority;
use crate::evidence::parse_evidence_specs;
use crate::taskdb::{
    gh_trigger_dedup_key, gh_trigger_receipt_id, gh_trigger_task_uuid, read_acknowledged_events,
    AdmissionOrigin, EnqueueSource, GhContextSnapshot, GhItemState, GhItemType, GhOrigin,
    GhTriggeringComment, WorkspaceMetadata, GH_CONTEXT_SCHEMA_VERSION, GH_ORIGIN_SCHEMA_VERSION,
    MAX_GH_ORIGIN_FIELD_BYTES,
};
use crate::wire::EnqueuePayload;
use crate::witness::Verdict;

pub const IN_SCOPE_PRODUCER_KINDS: &[&str] = &[
    "calendar",
    "events-dir",
    "gh",
    "build-effect",
    "pool-reachability",
];
pub const PRODUCER_RUNTIME_SCHEMA_VERSION: u32 = 1;

const MAX_INGRESS_BYTES: u64 = 1024 * 1024;
const INGRESS_SUFFIX: &str = ".producer.json";
const MAX_PRODUCER_NAME_BYTES: usize = 96;
const MAX_CLAIMABLE_NAME_BYTES: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProducerRuntimeRecord {
    pub schema_version: u32,
    pub producer: String,
    pub last_trigger: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_emission: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_outcome: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

pub fn record_producer_runtime(
    state_dir: &Path,
    producer: &str,
    trigger: DateTime<Utc>,
    outcome: Option<Value>,
    error: Option<String>,
) -> Result<(), ProducerError> {
    validate_producer_name(producer)?;
    let emitted = outcome.as_ref().is_some_and(outcome_has_emission);
    let timestamp = trigger.to_rfc3339();
    let previous_emission = read_producer_runtime(state_dir, producer)
        .ok()
        .flatten()
        .and_then(|record| record.last_emission);
    let record = ProducerRuntimeRecord {
        schema_version: PRODUCER_RUNTIME_SCHEMA_VERSION,
        producer: producer.to_owned(),
        last_trigger: timestamp.clone(),
        last_emission: emitted.then_some(timestamp).or(previous_emission),
        last_outcome: outcome,
        last_error: error,
    };
    write_json_atomic(
        &state_dir
            .join("producers")
            .join(format!("{producer}.runtime.json")),
        &record,
    )
}

fn outcome_has_emission(value: &Value) -> bool {
    match value {
        Value::String(path) => path.starts_with('/'),
        Value::Array(items) => items.iter().any(outcome_has_emission),
        Value::Object(fields) => {
            fields
                .get("enqueued")
                .and_then(Value::as_u64)
                .is_some_and(|count| count > 0)
                || fields.get("emitted").is_some_and(outcome_has_emission)
                || fields.get("ingress").is_some_and(outcome_has_emission)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

pub fn read_producer_runtime(
    state_dir: &Path,
    producer: &str,
) -> Result<Option<ProducerRuntimeRecord>, ProducerError> {
    validate_producer_name(producer)?;
    let path = state_dir
        .join("producers")
        .join(format!("{producer}.runtime.json"));
    if !path.exists() {
        return Ok(None);
    }
    let record: ProducerRuntimeRecord =
        serde_json::from_slice(&read_bounded_regular(&path, 256 * 1024)?)?;
    if record.schema_version != PRODUCER_RUNTIME_SCHEMA_VERSION || record.producer != producer {
        return Err(ProducerError::InvalidObservation(format!(
            "producer runtime state {} has an invalid identity or schema",
            path.display()
        )));
    }
    Ok(Some(record))
}

fn default_adapter() -> String {
    "shell".to_owned()
}

const fn default_priority() -> Priority {
    Priority::Low
}

const fn default_poll_interval() -> u64 {
    60
}

const fn default_probe_interval() -> u64 {
    30
}

const fn default_hysteresis() -> u32 {
    3
}

fn default_actor_exclude() -> String {
    "self".to_owned()
}

const fn default_true() -> bool {
    true
}

const fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProducerEnqueue {
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default = "default_adapter")]
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceMetadata>,
    #[serde(default, skip_serializing_if = "AdapterJobOptions::is_default")]
    pub adapter_options: AdapterJobOptions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_manifest: Option<GateManifestSpec>,
    #[serde(
        rename = "pool",
        serialize_with = "crate::poolset::serialize",
        deserialize_with = "crate::poolset::deserialize"
    )]
    pub pools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: Priority,
    #[serde(default)]
    pub dedup_key: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub evidence_class: Option<Value>,
    #[serde(default)]
    pub manifest_hash: Option<String>,
    #[serde(default)]
    pub consumption_estimate: Option<u64>,
    #[serde(default)]
    pub runtime_max_sec: Option<u64>,
    #[serde(default)]
    pub no_enqueue: bool,
    #[serde(default)]
    pub credentials: BTreeMap<String, PathBuf>,
}

impl ProducerEnqueue {
    fn payload(
        &self,
        source: EnqueueSource,
        producer: Option<&str>,
        now: DateTime<Utc>,
        github: Option<&GhOrigin>,
    ) -> Result<EnqueuePayload, ProducerError> {
        let mut pools = self.pools.clone();
        crate::poolset::canonicalize(&mut pools).map_err(|error| {
            ProducerError::InvalidConfig(format!("producer enqueue has invalid pool set: {error}"))
        })?;
        let dedup_key = self
            .dedup_key
            .as_deref()
            .map(|key| expand_dedup_key(key, now))
            .transpose()?;
        let argv = self
            .argv
            .iter()
            .map(|argument| render_origin_template(argument, github))
            .collect::<Result<Vec<_>, _>>()?;
        let cwd = self
            .cwd
            .as_ref()
            .map(|path| {
                let path = path.to_str().ok_or_else(|| {
                    ProducerError::InvalidObservation(
                        "producer cwd template must be valid UTF-8".to_owned(),
                    )
                })?;
                let rendered = render_origin_template(path, github)?;
                let rendered = PathBuf::from(rendered);
                validate_resolved_path(&rendered, "producer cwd")?;
                Ok::<PathBuf, ProducerError>(rendered)
            })
            .transpose()?;
        Ok(EnqueuePayload {
            invocation: None,
            argv: Some(argv),
            pools: Some(pools),
            executor: self.executor.clone(),
            priority: Some(self.priority),
            adapter: Some(self.adapter.clone()),
            cwd,
            workspace: self.workspace.clone(),
            adapter_options: (!self.adapter_options.is_default())
                .then(|| self.adapter_options.clone()),
            gate_manifest: self.gate_manifest.clone(),
            resume_from: None,
            source: Some(source),
            dedup_key,
            parent: None,
            evidence: self.evidence.clone(),
            evidence_class: self.evidence_class.clone(),
            manifest_hash: self.manifest_hash.clone(),
            consumption_estimate: self.consumption_estimate,
            runtime_max_sec: self.runtime_max_sec,
            no_enqueue: self.no_enqueue,
            credentials: self.credentials.clone(),
            origin: Some(match (producer, github) {
                (Some(name), Some(github)) => AdmissionOrigin::github(name, github.clone()),
                (Some(name), None) => AdmissionOrigin::producer(name, source),
                (None, _) => AdmissionOrigin::direct(source),
            }),
            caller_job_id: None,
            gh_trigger_actor: None,
            gh_self_actor: None,
            gh_origin: None,
            task_uuid: None,
            related_trigger: None,
            wait: false,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CalendarProducer {
    #[serde(default)]
    pub credentials: BTreeMap<String, PathBuf>,
    pub on_calendar: String,
    pub enqueue: ProducerEnqueue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EventsDirProducer {
    #[serde(default)]
    pub credentials: BTreeMap<String, PathBuf>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_sec: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GhSourceItemKind {
    Issue,
    PullRequest,
}

impl GhSourceItemKind {
    const fn matches(self, item_type: GhItemType) -> bool {
        matches!(
            (self, item_type),
            (Self::Issue, GhItemType::Issue) | (Self::PullRequest, GhItemType::PullRequest)
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhSourceConstraints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, alias = "repos")]
    pub repositories: Vec<String>,
    #[serde(default)]
    pub owners: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<GhItemState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default)]
    pub kinds: Vec<GhSourceItemKind>,
    #[serde(default)]
    pub notification_reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, alias = "items")]
    pub item_allowlist: Vec<String>,
}

impl GhSourceConstraints {
    fn has_identity_scope(&self) -> bool {
        self.repo.is_some()
            || !self.repositories.is_empty()
            || !self.owners.is_empty()
            || !self.item_allowlist.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GhSource {
    Notifications(GhSourceConstraints),
    Search(GhSourceConstraints),
}

impl GhSource {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Notifications(_) => "notifications",
            Self::Search(_) => "search",
        }
    }

    const fn constraints(&self) -> &GhSourceConstraints {
        match self {
            Self::Notifications(constraints) | Self::Search(constraints) => constraints,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhTriggers {
    #[serde(default)]
    pub command_comments: Vec<String>,
    #[serde(default)]
    pub mentions: Vec<String>,
    #[serde(default)]
    pub assignments: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhProducer {
    #[serde(default)]
    pub credentials: BTreeMap<String, PathBuf>,
    pub enable: bool,
    #[serde(default)]
    pub sources: Vec<GhSource>,
    #[serde(default)]
    pub triggers: GhTriggers,
    #[serde(default = "default_actor_exclude")]
    pub actor_exclude: String,
    #[serde(default)]
    pub allow_self_triggered: bool,
    #[serde(default)]
    pub allowed_actors: Vec<String>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_sec: u64,
    #[serde(default = "default_true")]
    pub post_receipt: bool,
    #[serde(default)]
    pub post_evidence: bool,
    #[serde(default)]
    pub post_gate_summary: bool,
    #[serde(default)]
    pub request_review: bool,
    #[serde(default)]
    pub close_on_acceptance: bool,
    #[serde(default)]
    pub never_mutate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_on_pass: Option<bool>,
    pub enqueue: ProducerEnqueue,
}

impl GhProducer {
    /// Configurations serialized before `closeOnPass` existed retain the old
    /// fused behavior. Nix-rendered current configurations always include the
    /// field, so `false` is an explicit comment-only policy.
    pub fn close_on_pass(&self) -> bool {
        self.close_on_pass.unwrap_or(self.post_evidence)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuildEffectWatch {
    #[default]
    GcRootsDir,
    Jsonl,
    PostBuildHook,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BuildEffectProducer {
    #[serde(default)]
    pub credentials: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub watch: BuildEffectWatch,
    pub path: PathBuf,
    pub on_key: ProducerEnqueue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PoolReachabilityProducer {
    #[serde(default)]
    pub credentials: BTreeMap<String, PathBuf>,
    pub probe_pool: String,
    #[serde(default = "default_probe_interval")]
    pub interval_sec: u64,
    #[serde(default = "default_hysteresis")]
    pub hysteresis: u32,
    #[serde(default)]
    pub on_lost: Option<ProducerEnqueue>,
    #[serde(default)]
    pub on_return: Option<ProducerEnqueue>,
    #[serde(default)]
    pub on_return_attest: Option<ProducerEnqueue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ProducerConfig {
    Calendar(CalendarProducer),
    EventsDir(EventsDirProducer),
    Gh(GhProducer),
    BuildEffect(BuildEffectProducer),
    PoolReachability(Box<PoolReachabilityProducer>),
}

impl ProducerConfig {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Calendar(_) => "calendar",
            Self::EventsDir(_) => "events-dir",
            Self::Gh(_) => "gh",
            Self::BuildEffect(_) => "build-effect",
            Self::PoolReachability(_) => "pool-reachability",
        }
    }

    fn credentials(&self) -> &BTreeMap<String, PathBuf> {
        match self {
            Self::Calendar(config) => &config.credentials,
            Self::EventsDir(config) => &config.credentials,
            Self::Gh(config) => &config.credentials,
            Self::BuildEffect(config) => &config.credentials,
            Self::PoolReachability(config) => &config.credentials,
        }
    }
}

pub fn validate_registry(
    producers: &BTreeMap<String, ProducerConfig>,
    pools: &BTreeSet<String>,
    adapters: &BTreeSet<String>,
    executors: &BTreeSet<String>,
) -> Result<(), ProducerError> {
    let mut reachability_owners = BTreeMap::new();
    for (name, producer) in producers {
        validate_producer_name(name)?;
        validate_credentials(producer.credentials(), &format!("producer {name:?}"))?;
        match producer {
            ProducerConfig::Calendar(config) => {
                if config.on_calendar.trim().is_empty()
                    || config.on_calendar.chars().any(char::is_control)
                {
                    return Err(ProducerError::InvalidConfig(format!(
                        "calendar producer {name:?} requires a non-empty onCalendar"
                    )));
                }
                validate_enqueue(
                    name,
                    "enqueue",
                    &config.enqueue,
                    pools,
                    adapters,
                    executors,
                    false,
                )?;
            }
            ProducerConfig::EventsDir(config) => {
                if config.poll_interval_sec == 0 {
                    return Err(ProducerError::InvalidConfig(format!(
                        "events-dir producer {name:?} requires positive pollIntervalSec"
                    )));
                }
            }
            ProducerConfig::Gh(config) => {
                if config.poll_interval_sec == 0 {
                    return Err(ProducerError::InvalidConfig(format!(
                        "gh producer {name:?} requires positive pollIntervalSec"
                    )));
                }
                if config.enable && config.sources.is_empty() {
                    return Err(ProducerError::InvalidConfig(format!(
                        "enabled gh producer {name:?} requires at least one source"
                    )));
                }
                let mut sources = BTreeSet::new();
                for source in &config.sources {
                    let encoded = serde_json::to_string(source)?;
                    if !sources.insert(encoded) {
                        return Err(ProducerError::InvalidConfig(format!(
                            "gh producer {name:?} repeats source {source:?}"
                        )));
                    }
                    validate_gh_source(name, source)?;
                }
                validate_name(&config.actor_exclude, "GitHub actorExclude")?;
                let mut allowed_actors = BTreeSet::new();
                for actor in &config.allowed_actors {
                    validate_name(actor, "GitHub allowedActors entry")?;
                    if !allowed_actors.insert(actor) {
                        return Err(ProducerError::InvalidConfig(format!(
                            "gh producer {name:?} repeats allowedActors entry {actor:?}"
                        )));
                    }
                }
                validate_gh_triggers(name, &config.triggers)?;
                if config.close_on_pass == Some(true) && !config.post_evidence {
                    return Err(ProducerError::InvalidConfig(format!(
                        "gh producer {name:?} closeOnPass=true requires postEvidence=true"
                    )));
                }
                if (config.post_gate_summary || config.close_on_acceptance)
                    && config.enqueue.gate_manifest.is_none()
                {
                    return Err(ProducerError::InvalidConfig(format!(
                        "gh producer {name:?} postGateSummary/closeOnAcceptance requires enqueue.gateManifest"
                    )));
                }
                validate_enqueue(
                    name,
                    "enqueue",
                    &config.enqueue,
                    pools,
                    adapters,
                    executors,
                    true,
                )?;
            }
            ProducerConfig::BuildEffect(config) => {
                if !config.path.is_absolute() {
                    return Err(ProducerError::InvalidConfig(format!(
                        "build-effect producer {name:?} path must be absolute"
                    )));
                }
                validate_safe_path(
                    &config.path,
                    &format!("build-effect producer {name:?} path"),
                )?;
                validate_enqueue(
                    name,
                    "onKey",
                    &config.on_key,
                    pools,
                    adapters,
                    executors,
                    false,
                )?;
            }
            ProducerConfig::PoolReachability(config) => {
                if config.interval_sec == 0 || config.hysteresis == 0 {
                    return Err(ProducerError::InvalidConfig(format!(
                        "pool-reachability producer {name:?} requires positive intervalSec and hysteresis"
                    )));
                }
                if !pools.contains(&config.probe_pool) {
                    return Err(ProducerError::InvalidConfig(format!(
                        "pool-reachability producer {name:?} references unknown probePool {:?}",
                        config.probe_pool
                    )));
                }
                if let Some(existing) =
                    reachability_owners.insert(config.probe_pool.clone(), name.clone())
                {
                    return Err(ProducerError::InvalidConfig(format!(
                        "pool-reachability producers {existing:?} and {name:?} both own probePool {:?}",
                        config.probe_pool
                    )));
                }
                for (field, enqueue) in [
                    ("onLost", config.on_lost.as_ref()),
                    ("onReturn", config.on_return.as_ref()),
                    ("onReturnAttest", config.on_return_attest.as_ref()),
                ] {
                    if let Some(enqueue) = enqueue {
                        validate_enqueue(name, field, enqueue, pools, adapters, executors, false)?;
                    }
                }
                if config
                    .on_return_attest
                    .as_ref()
                    .is_some_and(|enqueue| !enqueue.no_enqueue)
                {
                    return Err(ProducerError::InvalidConfig(format!(
                        "pool-reachability producer {name:?} onReturnAttest requires noEnqueue=true"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn validate_enqueue(
    producer: &str,
    field: &str,
    enqueue: &ProducerEnqueue,
    pools: &BTreeSet<String>,
    adapters: &BTreeSet<String>,
    executors: &BTreeSet<String>,
    allow_origin_templates: bool,
) -> Result<(), ProducerError> {
    if enqueue.argv.is_empty() {
        return Err(ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} argv must not be empty"
        )));
    }
    let mut canonical_pools = enqueue.pools.clone();
    crate::poolset::canonicalize(&mut canonical_pools).map_err(|error| {
        ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} has invalid pool set: {error}"
        ))
    })?;
    for pool in &canonical_pools {
        if !pools.contains(pool) {
            return Err(ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} references unknown pool {pool:?}"
            )));
        }
    }
    if !adapters.contains(&enqueue.adapter) {
        return Err(ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} references unknown adapter {:?}",
            enqueue.adapter
        )));
    }
    if let Some(executor) = &enqueue.executor {
        if !executors.contains(executor) {
            return Err(ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} references unknown executor {executor:?}"
            )));
        }
    }
    if enqueue
        .dedup_key
        .as_ref()
        .is_some_and(|key| key.trim().is_empty() || key.chars().any(char::is_control))
    {
        return Err(ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} dedupKey must not be empty or contain control characters"
        )));
    }
    if enqueue
        .dedup_key
        .as_deref()
        .is_some_and(|key| StrftimeItems::new(key).any(|item| matches!(item, Item::Error)))
    {
        return Err(ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} dedupKey is not a valid strftime template"
        )));
    }
    if enqueue.runtime_max_sec == Some(0) {
        return Err(ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} runtimeMaxSec must be positive"
        )));
    }
    for argument in &enqueue.argv {
        validate_origin_template(argument, allow_origin_templates).map_err(|detail| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} argv template is invalid: {detail}"
            ))
        })?;
    }
    if let Some(cwd) = &enqueue.cwd {
        let cwd = cwd.to_str().ok_or_else(|| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} cwd must be valid UTF-8"
            ))
        })?;
        validate_origin_template(cwd, allow_origin_templates).map_err(|detail| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} cwd template is invalid: {detail}"
            ))
        })?;
        validate_resolved_path_template(cwd).map_err(|detail| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} cwd is invalid: {detail}"
            ))
        })?;
    }
    if let Some(workspace) = &enqueue.workspace {
        workspace.validate().map_err(|error| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} workspace is invalid: {error}"
            ))
        })?;
    }
    if let Some(gate_manifest) = &enqueue.gate_manifest {
        gate_manifest.validate().map_err(|error| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} gateManifest is invalid: {error}"
            ))
        })?;
    }
    parse_evidence_specs(&enqueue.evidence).map_err(|error| {
        ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} evidence is invalid: {error}"
        ))
    })?;
    validate_credentials(
        &enqueue.credentials,
        &format!("producer {producer:?} {field}"),
    )?;
    Ok(())
}

const ORIGIN_TEMPLATE_FIELDS: &[&str] = &[
    "repoName",
    "gh.source",
    "gh.repo",
    "gh.repoName",
    "gh.number",
    "gh.url",
    "gh.type",
    "gh.headSha",
    "gh.nodeId",
    "gh.itemAuthor",
    "gh.triggerActor",
    "gh.selfActor",
    "gh.notificationReason",
    "gh.triggerKind",
    "gh.eventId",
    "gh.commentId",
    "gh.triggerTimestamp",
    "gh.triggerValue",
];

fn validate_origin_template(template: &str, allowed: bool) -> Result<(), String> {
    if template.contains('\0') {
        return Err("template contains a NUL byte".to_owned());
    }
    let fields = origin_template_fields(template)?;
    if !allowed && !fields.is_empty() {
        return Err(
            "GitHub origin placeholders are valid only in a gh producer enqueue".to_owned(),
        );
    }
    for field in fields {
        if !ORIGIN_TEMPLATE_FIELDS.contains(&field) {
            return Err(format!("unknown placeholder {field:?}"));
        }
    }
    Ok(())
}

fn origin_template_fields(mut template: &str) -> Result<Vec<&str>, String> {
    let mut fields = Vec::new();
    while let Some(start) = template.find("${") {
        template = &template[start + 2..];
        let end = template
            .find('}')
            .ok_or_else(|| "unclosed '${field}' placeholder".to_owned())?;
        let field = &template[..end];
        if field.is_empty()
            || !field
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_'))
        {
            return Err(format!("invalid placeholder name {field:?}"));
        }
        fields.push(field);
        template = &template[end + 1..];
    }
    Ok(fields)
}

fn render_origin_template(
    template: &str,
    origin: Option<&GhOrigin>,
) -> Result<String, ProducerError> {
    validate_origin_template(template, origin.is_some())
        .map_err(ProducerError::InvalidObservation)?;
    let mut rendered = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        rendered.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        let end = rest
            .find('}')
            .expect("validated origin templates have closing braces");
        let field = &rest[..end];
        let origin = origin.ok_or_else(|| {
            ProducerError::InvalidObservation(
                "GitHub origin placeholder has no GitHub observation".to_owned(),
            )
        })?;
        rendered.push_str(origin_template_value(origin, field)?.as_str());
        rest = &rest[end + 1..];
    }
    rendered.push_str(rest);
    if rendered.len() > 64 * 1024 {
        return Err(ProducerError::InvalidObservation(
            "rendered origin template exceeds 65536 bytes".to_owned(),
        ));
    }
    Ok(rendered)
}

fn origin_template_value(origin: &GhOrigin, field: &str) -> Result<String, ProducerError> {
    let value = match field {
        "gh.source" => Some(origin.source.clone()),
        "gh.repo" => Some(origin.repo.clone()),
        "repoName" | "gh.repoName" => origin
            .repo
            .rsplit_once('/')
            .map(|(_, name)| name.to_owned()),
        "gh.number" => Some(origin.number.to_string()),
        "gh.url" => Some(origin.html_url.clone()),
        "gh.type" => origin.item_type.map(|kind| kind.as_str().to_owned()),
        "gh.headSha" => origin.head_sha.clone(),
        "gh.nodeId" => Some(origin.node_id.clone()),
        "gh.itemAuthor" => Some(origin.item_author.clone()),
        "gh.triggerActor" => Some(origin.trigger_actor.clone()),
        "gh.selfActor" => Some(origin.self_actor.clone()),
        "gh.notificationReason" => origin.notification_reason.clone(),
        "gh.triggerKind" => Some(origin.trigger_kind.clone()),
        "gh.eventId" => origin.event_id.clone(),
        "gh.commentId" => origin.comment_id.clone(),
        "gh.triggerTimestamp" => origin.trigger_timestamp.clone(),
        "gh.triggerValue" => origin.trigger_value.clone(),
        _ => None,
    };
    value.ok_or_else(|| {
        ProducerError::InvalidObservation(format!(
            "origin field {field:?} is absent for this GitHub item"
        ))
    })
}

fn validate_resolved_path_template(path: &str) -> Result<(), String> {
    if !path.starts_with('/')
        || path.contains('%')
        || path.contains('\0')
        || path.chars().any(char::is_control)
    {
        return Err(
            "path must be absolute and contain no control characters or systemd specifiers"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_resolved_path(path: &Path, label: &str) -> Result<(), ProducerError> {
    let path = path
        .to_str()
        .ok_or_else(|| ProducerError::InvalidObservation(format!("{label} must be valid UTF-8")))?;
    validate_resolved_path_template(path)
        .map_err(|detail| ProducerError::InvalidObservation(format!("{label}: {detail}")))
}

fn validate_name(value: &str, label: &str) -> Result<(), ProducerError> {
    if value.trim().is_empty()
        || value.len() > MAX_GH_ORIGIN_FIELD_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ProducerError::InvalidConfig(format!(
            "{label} must be non-empty, at most {MAX_GH_ORIGIN_FIELD_BYTES} bytes, and contain no control characters"
        )));
    }
    Ok(())
}

fn validate_gh_source(producer: &str, source: &GhSource) -> Result<(), ProducerError> {
    let constraints = source.constraints();
    let mut repositories = constraints.repositories.clone();
    if let Some(repo) = &constraints.repo {
        repositories.push(repo.clone());
    }
    validate_unique_values(
        &repositories,
        &format!("gh producer {producer:?} {} repositories", source.kind()),
    )?;
    for repo in &repositories {
        validate_repo_constraint(repo)?;
    }
    for (label, values) in [
        ("owners", &constraints.owners),
        ("labels", &constraints.labels),
        ("notificationReasons", &constraints.notification_reasons),
        ("itemAllowlist", &constraints.item_allowlist),
    ] {
        validate_unique_values(
            values,
            &format!("gh producer {producer:?} {} {label}", source.kind()),
        )?;
    }
    for owner in &constraints.owners {
        validate_login(owner, "GitHub source owner")?;
    }
    if let Some(assignee) = &constraints.assignee {
        validate_login(assignee, "GitHub source assignee")?;
    }
    for item in &constraints.item_allowlist {
        parse_gh_item_url(item).map_err(|reason| {
            ProducerError::InvalidConfig(format!(
                "gh producer {producer:?} {} itemAllowlist entry {item:?} is invalid: {reason}",
                source.kind()
            ))
        })?;
    }
    if let Some(query) = &constraints.query {
        validate_name(query, "GitHub raw query")?;
        if !matches!(source, GhSource::Search(_)) {
            return Err(ProducerError::InvalidConfig(format!(
                "gh producer {producer:?} notification source cannot carry query"
            )));
        }
    }
    if !constraints.notification_reasons.is_empty() && !matches!(source, GhSource::Notifications(_))
    {
        return Err(ProducerError::InvalidConfig(format!(
            "gh producer {producer:?} search source cannot carry notificationReasons"
        )));
    }
    Ok(())
}

fn validate_gh_triggers(producer: &str, triggers: &GhTriggers) -> Result<(), ProducerError> {
    for (label, values) in [
        ("commandComments", &triggers.command_comments),
        ("mentions", &triggers.mentions),
        ("assignments", &triggers.assignments),
        ("labels", &triggers.labels),
    ] {
        validate_unique_values(
            values,
            &format!("gh producer {producer:?} triggers.{label}"),
        )?;
    }
    for command in &triggers.command_comments {
        if !valid_explicit_comment_command(command, '/') {
            return Err(ProducerError::InvalidConfig(format!(
                "gh producer {producer:?} command comment {command:?} is not an explicit slash-command grammar"
            )));
        }
    }
    for mention in &triggers.mentions {
        if !valid_explicit_comment_command(mention, '@') {
            return Err(ProducerError::InvalidConfig(format!(
                "gh producer {producer:?} mention {mention:?} is not an explicit mention-command grammar"
            )));
        }
    }
    for actor in &triggers.assignments {
        validate_login(actor, "GitHub assignment trigger")?;
    }
    Ok(())
}

fn validate_unique_values(values: &[String], label: &str) -> Result<(), ProducerError> {
    let mut unique = BTreeSet::new();
    for value in values {
        validate_name(value, label)?;
        if !unique.insert(value) {
            return Err(ProducerError::InvalidConfig(format!(
                "{label} contains duplicate value {value:?}"
            )));
        }
    }
    Ok(())
}

fn validate_repo_constraint(repo: &str) -> Result<(), ProducerError> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err(ProducerError::InvalidConfig(format!(
            "GitHub repository {repo:?} must be owner/name"
        )));
    };
    validate_login(owner, "GitHub repository owner")?;
    if name.is_empty()
        || name.contains('/')
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    {
        return Err(ProducerError::InvalidConfig(format!(
            "GitHub repository {repo:?} must be a safe owner/name pair"
        )));
    }
    Ok(())
}

fn validate_login(login: &str, label: &str) -> Result<(), ProducerError> {
    validate_name(login, label)?;
    if !login
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(ProducerError::InvalidConfig(format!(
            "{label} {login:?} is not a safe GitHub login"
        )));
    }
    Ok(())
}

fn valid_explicit_comment_command(command: &str, prefix: char) -> bool {
    if command.len() > 128
        || command.trim() != command
        || command.chars().any(char::is_control)
        || !command.starts_with(prefix)
    {
        return false;
    }
    let mut tokens = command.split(' ');
    let Some(first) = tokens.next() else {
        return false;
    };
    let valid_token = |token: &str| {
        !token.is_empty()
            && token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    };
    valid_token(&first[1..]) && tokens.all(valid_token)
}

fn validate_producer_name(value: &str) -> Result<(), ProducerError> {
    if value.is_empty()
        || value.len() > MAX_PRODUCER_NAME_BYTES
        || !value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        || matches!(value, "." | "..")
    {
        return Err(ProducerError::InvalidConfig(format!(
            "producer name {value:?} is not a safe file-name component"
        )));
    }
    Ok(())
}

fn validate_credentials(
    credentials: &BTreeMap<String, PathBuf>,
    label: &str,
) -> Result<(), ProducerError> {
    for (name, source) in credentials {
        let name_valid = !name.is_empty()
            && name.len() <= 255
            && name != "."
            && name != ".."
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
        if !name_valid {
            return Err(ProducerError::InvalidConfig(format!(
                "{label} has invalid credential name {name:?}"
            )));
        }
        if !source.is_absolute() {
            return Err(ProducerError::InvalidConfig(format!(
                "{label} credential {name:?} source must be absolute"
            )));
        }
        validate_safe_path(source, &format!("{label} credential {name:?}"))?;
    }
    Ok(())
}

fn validate_safe_path(path: &Path, label: &str) -> Result<(), ProducerError> {
    let Some(path) = path.to_str() else {
        return Err(ProducerError::InvalidConfig(format!(
            "{label} must be valid UTF-8"
        )));
    };
    if path.is_empty() || path.chars().any(char::is_control) || path.contains('%') {
        return Err(ProducerError::InvalidConfig(format!(
            "{label} must be non-empty and contain neither control characters nor systemd specifiers"
        )));
    }
    Ok(())
}

fn expand_dedup_key(template: &str, now: DateTime<Utc>) -> Result<String, ProducerError> {
    if StrftimeItems::new(template).any(|item| matches!(item, Item::Error)) {
        return Err(ProducerError::InvalidObservation(
            "dedupKey is not a valid strftime template".to_owned(),
        ));
    }
    let expanded = now
        .format_with_items(StrftimeItems::new(template))
        .to_string();
    if expanded.trim().is_empty() || expanded.chars().any(char::is_control) {
        return Err(ProducerError::InvalidObservation(
            "strftime-expanded dedupKey is empty or contains control characters".to_owned(),
        ));
    }
    Ok(expanded)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhObservation {
    pub source: String,
    pub repo: String,
    pub number: u64,
    pub html_url: String,
    pub item_type: GhItemType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    pub node_id: String,
    pub item_author: String,
    pub trigger_actor: String,
    pub self_actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_reason: Option<String>,
    pub trigger_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_id: Option<String>,
    pub trigger_timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_value: Option<String>,
    pub context: GhContextSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhObservationInput {
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub item_type: Option<GhItemType>,
    #[serde(default)]
    pub head_sha: Option<String>,
    #[serde(default, alias = "itemId")]
    pub node_id: Option<String>,
    #[serde(default)]
    pub item_author: Option<String>,
    #[serde(default)]
    pub trigger_actor: Option<String>,
    #[serde(default)]
    pub self_actor: Option<String>,
    #[serde(default)]
    pub notification_reason: Option<String>,
    #[serde(default)]
    pub trigger_kind: Option<String>,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub comment_id: Option<String>,
    #[serde(default)]
    pub trigger_timestamp: Option<String>,
    #[serde(default)]
    pub trigger_value: Option<String>,
    #[serde(default)]
    pub context: Option<GhContextSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProducerObservation {
    Calendar,
    EventsDir,
    Gh(Box<GhObservationInput>),
    BuildEffect {
        #[serde(default)]
        store_path: Option<PathBuf>,
    },
    PoolReachability {
        reachable: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhCompletedMutation {
    pub producer: String,
    pub source: String,
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_id: Option<String>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_summary: Option<GateSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<AcceptanceFact>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub request_review: bool,
}

pub trait GhMutationSink {
    fn post_evidence(&mut self, mutation: &GhCompletedMutation) -> Result<(), String>;
    fn close_item(&mut self, mutation: &GhCompletedMutation) -> Result<(), String>;
}

const GH_COMPLETION_STATE_GRAPHQL: &str = r#"query TallyCompletionState($itemId: ID!, $cursor: String) {
  node(id: $itemId) {
    __typename
    ... on Issue { state comments(first: 100, after: $cursor) { nodes { body } pageInfo { hasNextPage endCursor } } }
    ... on PullRequest { state comments(first: 100, after: $cursor) { nodes { body } pageInfo { hasNextPage endCursor } } }
  }
}"#;
const GH_COMPLETION_COMMENT_GRAPHQL: &str = r#"mutation TallyCompletionComment($itemId: ID!, $body: String!) {
  addComment(input: {subjectId: $itemId, body: $body}) { commentEdge { node { id } } }
}"#;
const GH_COMPLETION_ISSUE_GRAPHQL: &str = r#"mutation TallyCompletionIssue($itemId: ID!) {
  closeIssue(input: {issueId: $itemId, stateReason: COMPLETED}) { issue { id state stateReason } }
}"#;
const GH_COMPLETION_PULL_REQUEST_GRAPHQL: &str = r#"mutation TallyCompletionPullRequest($itemId: ID!) {
  closePullRequest(input: {pullRequestId: $itemId}) { pullRequest { id state } }
}"#;
const GH_PROCESS_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_GH_PROCESS_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_GH_COMMENT_PAGES: usize = 100;

#[derive(Debug, Clone)]
pub struct GhCliMutationSink {
    program: PathBuf,
}

#[derive(Debug, Clone)]
pub struct GhCliIntake {
    program: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct GhCliAcknowledgementSink {
    mutation: GhCliMutationSink,
}

impl Default for GhCliMutationSink {
    fn default() -> Self {
        Self {
            program: PathBuf::from("gh"),
        }
    }
}

impl GhCliMutationSink {
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl Default for GhCliIntake {
    fn default() -> Self {
        Self {
            program: PathBuf::from("gh"),
        }
    }
}

impl GhCliAcknowledgementSink {
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            mutation: GhCliMutationSink::with_program(program),
        }
    }
}

impl GhCliIntake {
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    fn poll(&self, config: &GhProducer) -> Result<Vec<GhIntakeCandidate>, ProducerError> {
        let viewer: Value = self.json(&["api", "user"])?;
        let self_actor = viewer
            .get("login")
            .and_then(Value::as_str)
            .filter(|login| !login.is_empty())
            .ok_or_else(|| {
                ProducerError::GitHub("gh api user omitted a non-empty login".to_owned())
            })?
            .to_owned();
        let mut observations = Vec::new();
        for source in &config.sources {
            let constraints = source.constraints();
            if !constraints.has_identity_scope() {
                continue;
            }
            match source.kind() {
                "notifications" => {
                    let notifications: Value = self.json(&[
                        "api",
                        "--method",
                        "GET",
                        "notifications",
                        "-f",
                        "all=false",
                        "-f",
                        "participating=false",
                        "-f",
                        "per_page=100",
                    ])?;
                    let notifications = notifications.as_array().ok_or_else(|| {
                        ProducerError::GitHub(
                            "gh notifications response must be an array".to_owned(),
                        )
                    })?;
                    for notification in notifications {
                        let subject = notification.get("subject").ok_or_else(|| {
                            ProducerError::GitHub("GitHub notification omitted subject".to_owned())
                        })?;
                        let kind =
                            subject.get("type").and_then(Value::as_str).ok_or_else(|| {
                                ProducerError::GitHub(
                                    "GitHub notification subject omitted type".to_owned(),
                                )
                            })?;
                        let item_type = match kind {
                            "Issue" => GhItemType::Issue,
                            "PullRequest" => GhItemType::PullRequest,
                            _ => continue,
                        };
                        let url = subject.get("url").and_then(Value::as_str).ok_or_else(|| {
                            ProducerError::GitHub(
                                "GitHub issue/PR notification omitted subject URL".to_owned(),
                            )
                        })?;
                        let endpoint_offset = url.find("/repos/").ok_or_else(|| {
                            ProducerError::GitHub(format!(
                                "GitHub notification subject URL {url:?} is not a repository issue/PR endpoint"
                            ))
                        })?;
                        let hydrated: Value = self.json(&["api", &url[endpoint_offset..]])?;
                        let triggering_comment = match subject
                            .get("latest_comment_url")
                            .and_then(Value::as_str)
                            .filter(|url| !url.is_empty())
                        {
                            Some(url) => {
                                let offset = url.find("/repos/").ok_or_else(|| {
                                    ProducerError::GitHub(format!(
                                        "GitHub latest comment URL {url:?} is not a repository comment endpoint"
                                    ))
                                })?;
                                let comment = self.json(&["api", &url[offset..]])?;
                                exact_notification_comment(notification, url, &comment)
                            }
                            None => None,
                        };
                        let repo = notification
                            .pointer("/repository/full_name")
                            .and_then(Value::as_str);
                        let notification_timestamp = notification
                            .get("updated_at")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                ProducerError::GitHub(
                                    "GitHub notification omitted updated_at".to_owned(),
                                )
                            })?;
                        let event_trigger = if triggering_comment.is_none() {
                            let repo = repo.ok_or_else(|| {
                                ProducerError::GitHub(
                                    "GitHub notification omitted repository identity".to_owned(),
                                )
                            })?;
                            let number = hydrated
                                .get("number")
                                .and_then(Value::as_u64)
                                .filter(|number| *number > 0)
                                .ok_or_else(|| {
                                    ProducerError::GitHub(
                                        "GitHub notification item omitted number".to_owned(),
                                    )
                                })?;
                            self.notification_event_trigger(
                                repo,
                                number,
                                notification_timestamp,
                                &config.triggers,
                            )?
                        } else {
                            None
                        };
                        let event_id = event_trigger
                            .as_ref()
                            .map(|(event, _)| event.id.clone())
                            .or_else(|| notification.get("id").and_then(json_identifier));
                        let event_timestamp = event_trigger
                            .as_ref()
                            .map(|(_, timestamp)| timestamp.clone())
                            .unwrap_or_else(|| notification_timestamp.to_owned());
                        let event_trigger = event_trigger.map(|(event, _)| event);
                        let reason = notification
                            .get("reason")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned);
                        observations.push(gh_api_candidate(
                            "notifications",
                            &hydrated,
                            &self_actor,
                            GhObservationHints {
                                repo,
                                item_type: Some(item_type),
                                notification_reason: reason,
                                event_id,
                                triggering_comment,
                                event_trigger,
                                trigger_timestamp: Some(&event_timestamp),
                                triggers: &config.triggers,
                            },
                        )?);
                    }
                }
                "search" => {
                    for query in gh_search_queries(constraints) {
                        let query_field = format!("q={query}");
                        let response: Value = self.json(&[
                            "api",
                            "--method",
                            "GET",
                            "search/issues",
                            "-f",
                            &query_field,
                            "-f",
                            "per_page=100",
                        ])?;
                        let items =
                            response
                                .get("items")
                                .and_then(Value::as_array)
                                .ok_or_else(|| {
                                    ProducerError::GitHub(
                                        "GitHub search response omitted items array".to_owned(),
                                    )
                                })?;
                        for item in items {
                            let item_type = if item.get("pull_request").is_some() {
                                GhItemType::PullRequest
                            } else {
                                GhItemType::Issue
                            };
                            let hydrated = if item_type == GhItemType::PullRequest {
                                let url = item
                                    .pointer("/pull_request/url")
                                    .and_then(Value::as_str)
                                    .ok_or_else(|| {
                                        ProducerError::GitHub(
                                            "GitHub PR search result omitted pull_request.url"
                                                .to_owned(),
                                        )
                                    })?;
                                let endpoint_offset = url.find("/repos/").ok_or_else(|| {
                                    ProducerError::GitHub(format!(
                                        "GitHub PR search URL {url:?} is not a repository endpoint"
                                    ))
                                })?;
                                self.json(&["api", &url[endpoint_offset..]])?
                            } else {
                                item.clone()
                            };
                            let repo = item
                                .get("repository_url")
                                .and_then(Value::as_str)
                                .and_then(repo_from_api_url)
                                .ok_or_else(|| {
                                    ProducerError::GitHub(
                                        "GitHub search result omitted repository identity"
                                            .to_owned(),
                                    )
                                })?;
                            let number = hydrated
                                .get("number")
                                .and_then(Value::as_u64)
                                .filter(|number| *number > 0)
                                .ok_or_else(|| {
                                    ProducerError::GitHub(
                                        "GitHub search result omitted item number".to_owned(),
                                    )
                                })?;
                            let comments_endpoint =
                                format!("/repos/{repo}/issues/{number}/comments?per_page=100");
                            let comments = self.json(&["api", &comments_endpoint])?;
                            let comments = comments.as_array().ok_or_else(|| {
                                ProducerError::GitHub(
                                    "GitHub issue comments response must be an array".to_owned(),
                                )
                            })?;
                            for comment in comments {
                                let Some(triggering_comment) = gh_triggering_comment(comment)
                                else {
                                    continue;
                                };
                                if !comment_is_configured_trigger(
                                    &config.triggers,
                                    &triggering_comment.body,
                                ) {
                                    continue;
                                }
                                let timestamp = gh_comment_timestamp(comment).ok_or_else(|| {
                                    ProducerError::GitHub(
                                        "GitHub trigger comment omitted updated_at and created_at"
                                            .to_owned(),
                                    )
                                })?;
                                observations.push(gh_api_candidate(
                                    "search",
                                    &hydrated,
                                    &self_actor,
                                    GhObservationHints {
                                        repo: Some(repo),
                                        item_type: Some(item_type),
                                        notification_reason: None,
                                        event_id: Some(triggering_comment.id.clone()),
                                        triggering_comment: Some(triggering_comment),
                                        event_trigger: None,
                                        trigger_timestamp: Some(timestamp),
                                        triggers: &config.triggers,
                                    },
                                )?);
                            }
                            let events_endpoint =
                                format!("/repos/{repo}/issues/{number}/events?per_page=100");
                            let events = self.json(&["api", &events_endpoint])?;
                            let events = events.as_array().ok_or_else(|| {
                                ProducerError::GitHub(
                                    "GitHub issue events response must be an array".to_owned(),
                                )
                            })?;
                            for event in events {
                                let Some(event_trigger) =
                                    configured_gh_event(event, &config.triggers)
                                else {
                                    continue;
                                };
                                let timestamp = event
                                    .get("created_at")
                                    .and_then(Value::as_str)
                                    .ok_or_else(|| {
                                        ProducerError::GitHub(
                                            "GitHub trigger event omitted created_at".to_owned(),
                                        )
                                    })?;
                                let event_id = event_trigger.id.clone();
                                observations.push(gh_api_candidate(
                                    "search",
                                    &hydrated,
                                    &self_actor,
                                    GhObservationHints {
                                        repo: Some(repo),
                                        item_type: Some(item_type),
                                        notification_reason: None,
                                        event_id: Some(event_id),
                                        triggering_comment: None,
                                        event_trigger: Some(event_trigger),
                                        trigger_timestamp: Some(timestamp),
                                        triggers: &config.triggers,
                                    },
                                )?);
                            }
                        }
                    }
                }
                other => {
                    return Err(ProducerError::InvalidConfig(format!(
                        "unsupported GitHub source {other:?}"
                    )))
                }
            }
        }
        normalize_gh_candidates(config, &mut observations);
        Ok(observations)
    }

    fn json(&self, args: &[&str]) -> Result<Value, ProducerError> {
        let output = run_gh_bounded(&self.program, args, None).map_err(ProducerError::GitHub)?;
        serde_json::from_slice(&output).map_err(|error| {
            ProducerError::GitHub(format!(
                "{} returned invalid JSON: {error}",
                self.program.display()
            ))
        })
    }

    fn notification_event_trigger(
        &self,
        repo: &str,
        number: u64,
        notification_timestamp: &str,
        triggers: &GhTriggers,
    ) -> Result<Option<(GhEventTrigger, String)>, ProducerError> {
        let endpoint = format!("/repos/{repo}/issues/{number}/events?per_page=100");
        let events = self.json(&["api", &endpoint])?;
        let events = events.as_array().ok_or_else(|| {
            ProducerError::GitHub("GitHub issue events response must be an array".to_owned())
        })?;
        for event in events.iter().rev() {
            let Some(timestamp) = event.get("created_at").and_then(Value::as_str) else {
                continue;
            };
            if !gh_timestamps_equal(notification_timestamp, timestamp) {
                continue;
            }
            if let Some(event) = configured_gh_event(event, triggers) {
                return Ok(Some((event, timestamp.to_owned())));
            }
        }
        Ok(None)
    }

    fn item(
        &self,
        config: &GhProducer,
        item_url: &str,
    ) -> Result<Vec<GhIntakeCandidate>, ProducerError> {
        let location = parse_gh_item_url(item_url).map_err(ProducerError::InvalidObservation)?;
        let viewer: Value = self.json(&["api", "user"])?;
        let self_actor = viewer
            .get("login")
            .and_then(Value::as_str)
            .filter(|login| !login.is_empty())
            .ok_or_else(|| {
                ProducerError::GitHub("gh api user omitted a non-empty login".to_owned())
            })?;
        let item_endpoint = match location.item_type {
            GhItemType::Issue => {
                format!("/repos/{}/issues/{}", location.repo, location.number)
            }
            GhItemType::PullRequest => {
                format!("/repos/{}/pulls/{}", location.repo, location.number)
            }
        };
        let item = self.json(&["api", &item_endpoint])?;
        let comments_endpoint = format!(
            "/repos/{}/issues/{}/comments?per_page=100",
            location.repo, location.number
        );
        let comments = self.json(&["api", &comments_endpoint])?;
        let comments = comments.as_array().ok_or_else(|| {
            ProducerError::GitHub("GitHub issue comments response must be an array".to_owned())
        })?;
        let mut candidates = Vec::new();
        for comment in comments {
            let Some(triggering_comment) = gh_triggering_comment(comment) else {
                continue;
            };
            if !comment_is_configured_trigger(&config.triggers, &triggering_comment.body) {
                continue;
            }
            let timestamp = gh_comment_timestamp(comment).ok_or_else(|| {
                ProducerError::GitHub(
                    "GitHub trigger comment omitted updated_at and created_at".to_owned(),
                )
            })?;
            candidates.push(gh_api_candidate(
                "search",
                &item,
                self_actor,
                GhObservationHints {
                    repo: Some(&location.repo),
                    item_type: Some(location.item_type),
                    notification_reason: None,
                    event_id: Some(triggering_comment.id.clone()),
                    triggering_comment: Some(triggering_comment),
                    event_trigger: None,
                    trigger_timestamp: Some(timestamp),
                    triggers: &config.triggers,
                },
            )?);
        }
        let events_endpoint = format!(
            "/repos/{}/issues/{}/events?per_page=100",
            location.repo, location.number
        );
        let events = self.json(&["api", &events_endpoint])?;
        let events = events.as_array().ok_or_else(|| {
            ProducerError::GitHub("GitHub issue events response must be an array".to_owned())
        })?;
        for event in events {
            let Some(event_trigger) = configured_gh_event(event, &config.triggers) else {
                continue;
            };
            let timestamp = event
                .get("created_at")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ProducerError::GitHub("GitHub trigger event omitted created_at".to_owned())
                })?;
            let event_id = event_trigger.id.clone();
            candidates.push(gh_api_candidate(
                "search",
                &item,
                self_actor,
                GhObservationHints {
                    repo: Some(&location.repo),
                    item_type: Some(location.item_type),
                    notification_reason: None,
                    event_id: Some(event_id),
                    triggering_comment: None,
                    event_trigger: Some(event_trigger),
                    trigger_timestamp: Some(timestamp),
                    triggers: &config.triggers,
                },
            )?);
        }
        candidates.sort_by_key(GhIntakeCandidate::dedup_identity);
        candidates.dedup_by(|right, left| right.dedup_identity() == left.dedup_identity());
        Ok(candidates)
    }

    fn diagnostic_observation(
        &self,
        config: &GhProducer,
        item_url: &str,
        trigger_kind: &str,
        actor: &str,
        now: DateTime<Utc>,
    ) -> Result<GhObservation, ProducerError> {
        validate_login(actor, "GitHub diagnostic actor")?;
        let location = parse_gh_item_url(item_url).map_err(ProducerError::InvalidObservation)?;
        let viewer: Value = self.json(&["api", "user"])?;
        let self_actor = viewer
            .get("login")
            .and_then(Value::as_str)
            .filter(|login| !login.is_empty())
            .ok_or_else(|| {
                ProducerError::GitHub("gh api user omitted a non-empty login".to_owned())
            })?;
        let item_endpoint = match location.item_type {
            GhItemType::Issue => {
                format!("/repos/{}/issues/{}", location.repo, location.number)
            }
            GhItemType::PullRequest => {
                format!("/repos/{}/pulls/{}", location.repo, location.number)
            }
        };
        let item = self.json(&["api", &item_endpoint])?;
        let trigger_value = match trigger_kind {
            "command-comment" => config.triggers.command_comments.first(),
            "mention" => config.triggers.mentions.first(),
            "assignment" => config.triggers.assignments.first(),
            "label" => config.triggers.labels.first(),
            _ => {
                return Err(ProducerError::InvalidObservation(format!(
                    "unsupported GitHub diagnostic event {trigger_kind:?}"
                )))
            }
        }
        .ok_or_else(|| {
            ProducerError::InvalidConfig(format!(
                "GitHub diagnostic event {trigger_kind:?} has no configured trigger value"
            ))
        })?
        .clone();
        let timestamp = now.to_rfc3339();
        let diagnostic_id = format!(
            "diagnostic-{}",
            stable_key(&[
                "gh-diagnostic",
                item_url,
                trigger_kind,
                actor,
                &trigger_value,
                &timestamp,
            ])
        );
        let triggering_comment =
            matches!(trigger_kind, "command-comment" | "mention").then(|| GhTriggeringComment {
                id: diagnostic_id.clone(),
                author: actor.to_owned(),
                body: trigger_value.clone(),
            });
        let event_trigger =
            matches!(trigger_kind, "assignment" | "label").then(|| GhEventTrigger {
                id: diagnostic_id.clone(),
                kind: if trigger_kind == "assignment" {
                    "assignment"
                } else {
                    "label"
                },
                actor: actor.to_owned(),
                value: trigger_value,
            });
        let mut first_observation = None;
        for source in config.sources.iter().filter(|source| {
            matches!(source, GhSource::Search(_)) && source.constraints().has_identity_scope()
        }) {
            let candidate = gh_api_candidate(
                "search",
                &item,
                self_actor,
                GhObservationHints {
                    repo: Some(&location.repo),
                    item_type: Some(location.item_type),
                    notification_reason: None,
                    event_id: Some(diagnostic_id.clone()),
                    triggering_comment: triggering_comment.clone(),
                    event_trigger: event_trigger.clone(),
                    trigger_timestamp: Some(&timestamp),
                    triggers: &config.triggers,
                },
            )?;
            let GhIntakeCandidate::Observation(observation) = candidate else {
                return Err(ProducerError::InvalidObservation(
                    "configured GitHub diagnostic trigger could not be classified".to_owned(),
                ));
            };
            if gh_source_constraints_reason(source.constraints(), &observation).is_none() {
                return Ok(*observation);
            }
            first_observation.get_or_insert(*observation);
        }
        first_observation.ok_or_else(|| {
            ProducerError::InvalidConfig(
                "GitHub diagnostic requires at least one identity-scoped search source".to_owned(),
            )
        })
    }
}

fn configured_gh_event(event: &Value, triggers: &GhTriggers) -> Option<GhEventTrigger> {
    let id = event
        .get("id")
        .and_then(json_identifier)
        .or_else(|| event.get("node_id").and_then(json_identifier))?;
    let actor = event
        .pointer("/actor/login")
        .and_then(Value::as_str)
        .filter(|actor| !actor.is_empty())?;
    let (kind, value) = match event.get("event").and_then(Value::as_str) {
        Some("assigned") => (
            "assignment",
            event.pointer("/assignee/login").and_then(Value::as_str)?,
        ),
        Some("labeled") => (
            "label",
            event.pointer("/label/name").and_then(Value::as_str)?,
        ),
        _ => return None,
    };
    let configured = match kind {
        "assignment" => triggers
            .assignments
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(value)),
        "label" => triggers
            .labels
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(value)),
        _ => false,
    };
    configured.then(|| GhEventTrigger {
        id,
        kind,
        actor: actor.to_owned(),
        value: value.to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GhIntakeCandidate {
    Observation(Box<GhObservation>),
    TriggerActorUnavailable { source: String, node_id: String },
}

impl GhIntakeCandidate {
    fn source(&self) -> &str {
        match self {
            Self::Observation(observation) => &observation.source,
            Self::TriggerActorUnavailable { source, .. } => source,
        }
    }

    fn dedup_identity(&self) -> String {
        match self {
            Self::Observation(observation) => format!(
                "{}:{}:{}",
                observation.trigger_kind,
                observation
                    .comment_id
                    .as_deref()
                    .or(observation.event_id.as_deref())
                    .unwrap_or_default(),
                observation.node_id
            ),
            Self::TriggerActorUnavailable { source, node_id } => {
                format!("unavailable:{source}:{node_id}")
            }
        }
    }

    const fn unavailable(&self) -> bool {
        matches!(self, Self::TriggerActorUnavailable { .. })
    }
}

fn normalize_gh_candidates(config: &GhProducer, candidates: &mut Vec<GhIntakeCandidate>) {
    let source_filtered = |candidate: &GhIntakeCandidate| match candidate {
        GhIntakeCandidate::Observation(observation) => {
            gh_source_filter_reason(config, observation).is_some()
        }
        GhIntakeCandidate::TriggerActorUnavailable { .. } => true,
    };
    candidates.sort_by(|left, right| {
        left.dedup_identity()
            .cmp(&right.dedup_identity())
            .then_with(|| source_filtered(left).cmp(&source_filtered(right)))
            .then_with(|| left.unavailable().cmp(&right.unavailable()))
            .then_with(|| left.source().cmp(right.source()))
    });
    candidates.dedup_by(|right, left| right.dedup_identity() == left.dedup_identity());
}

struct GhObservationHints<'a> {
    repo: Option<&'a str>,
    item_type: Option<GhItemType>,
    notification_reason: Option<String>,
    event_id: Option<String>,
    triggering_comment: Option<GhTriggeringComment>,
    event_trigger: Option<GhEventTrigger>,
    trigger_timestamp: Option<&'a str>,
    triggers: &'a GhTriggers,
}

#[derive(Clone)]
struct GhEventTrigger {
    id: String,
    kind: &'static str,
    actor: String,
    value: String,
}

fn gh_api_candidate(
    source: &str,
    item: &Value,
    self_actor: &str,
    hints: GhObservationHints<'_>,
) -> Result<GhIntakeCandidate, ProducerError> {
    let GhObservationHints {
        repo: repo_hint,
        item_type: item_type_hint,
        notification_reason,
        event_id,
        triggering_comment,
        event_trigger,
        trigger_timestamp,
        triggers,
    } = hints;
    let node_id = item
        .get("node_id")
        .and_then(Value::as_str)
        .filter(|node_id| !node_id.is_empty())
        .ok_or_else(|| ProducerError::GitHub("GitHub issue/PR omitted node_id".to_owned()))?;
    let item_author = item
        .pointer("/user/login")
        .and_then(Value::as_str)
        .filter(|actor| !actor.is_empty())
        .ok_or_else(|| ProducerError::GitHub("GitHub issue/PR omitted user.login".to_owned()))?;
    let repo = repo_hint
        .or_else(|| item.pointer("/base/repo/full_name").and_then(Value::as_str))
        .or_else(|| {
            item.get("repository_url")
                .and_then(Value::as_str)
                .and_then(repo_from_api_url)
        })
        .ok_or_else(|| {
            ProducerError::GitHub("GitHub issue/PR omitted repository identity".to_owned())
        })?;
    let number = item
        .get("number")
        .and_then(Value::as_u64)
        .filter(|number| *number > 0)
        .ok_or_else(|| ProducerError::GitHub("GitHub issue/PR omitted number".to_owned()))?;
    let html_url = item
        .get("html_url")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| ProducerError::GitHub("GitHub issue/PR omitted html_url".to_owned()))?;
    let item_type = item_type_hint.unwrap_or_else(|| {
        if item.get("pull_request").is_some() || item.get("head").is_some() {
            GhItemType::PullRequest
        } else {
            GhItemType::Issue
        }
    });
    let head_sha = (item_type == GhItemType::PullRequest)
        .then(|| {
            item.pointer("/head/sha")
                .and_then(Value::as_str)
                .filter(|sha| !sha.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| ProducerError::GitHub("GitHub PR omitted head.sha".to_owned()))
        })
        .transpose()?;
    let title = item
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| ProducerError::GitHub("GitHub issue/PR omitted title".to_owned()))?;
    let body = item.get("body").and_then(Value::as_str).unwrap_or_default();
    let labels = item
        .get("labels")
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .map(|label| {
                    label
                        .get("name")
                        .and_then(Value::as_str)
                        .or_else(|| label.as_str())
                        .filter(|name| !name.is_empty())
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| {
                            ProducerError::GitHub("GitHub issue/PR label omitted a name".to_owned())
                        })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let assignees = item
        .get("assignees")
        .and_then(Value::as_array)
        .map(|assignees| {
            assignees
                .iter()
                .map(|assignee| {
                    assignee
                        .get("login")
                        .and_then(Value::as_str)
                        .filter(|login| !login.is_empty())
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| {
                            ProducerError::GitHub(
                                "GitHub issue/PR assignee omitted login".to_owned(),
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let (comment_id, trigger_actor, trigger_kind, trigger_value, triggering_comment) =
        if let Some(triggering_comment) = triggering_comment {
            let trigger_kind = if triggers
                .command_comments
                .iter()
                .any(|command| command == triggering_comment.body.trim())
            {
                "command-comment"
            } else if triggers
                .mentions
                .iter()
                .any(|command| command == triggering_comment.body.trim())
            {
                "mention"
            } else {
                return Ok(GhIntakeCandidate::TriggerActorUnavailable {
                    source: source.to_owned(),
                    node_id: node_id.to_owned(),
                });
            };
            (
                Some(triggering_comment.id.clone()),
                triggering_comment.author.clone(),
                trigger_kind,
                None,
                Some(triggering_comment),
            )
        } else if let Some(event) = event_trigger {
            (None, event.actor, event.kind, Some(event.value), None)
        } else {
            return Ok(GhIntakeCandidate::TriggerActorUnavailable {
                source: source.to_owned(),
                node_id: node_id.to_owned(),
            });
        };
    let trigger_timestamp = trigger_timestamp.ok_or_else(|| {
        ProducerError::GitHub("GitHub trigger omitted an event timestamp".to_owned())
    })?;
    Ok(GhIntakeCandidate::Observation(Box::new(GhObservation {
        source: source.to_owned(),
        repo: repo.to_owned(),
        number,
        html_url: html_url.to_owned(),
        item_type,
        head_sha: head_sha.clone(),
        node_id: node_id.to_owned(),
        item_author: item_author.to_owned(),
        trigger_actor,
        self_actor: self_actor.to_owned(),
        notification_reason,
        trigger_kind: trigger_kind.to_owned(),
        event_id,
        comment_id,
        trigger_timestamp: trigger_timestamp.to_owned(),
        trigger_value,
        context: GhContextSnapshot {
            schema_version: GH_CONTEXT_SCHEMA_VERSION,
            title: title.to_owned(),
            body: body.to_owned(),
            state: Some(gh_item_state(item)?),
            head_sha: head_sha.clone(),
            labels,
            assignees,
            triggering_comment,
        },
    })))
}

fn exact_notification_comment(
    notification: &Value,
    latest_comment_url: &str,
    comment: &Value,
) -> Option<GhTriggeringComment> {
    let triggering_comment = gh_triggering_comment(comment)?;
    if latest_comment_url.rsplit('/').next()? != triggering_comment.id {
        return None;
    }
    let notification_updated_at = notification.get("updated_at")?.as_str()?;
    comment
        .get("updated_at")
        .and_then(Value::as_str)
        .is_some_and(|comment_at| gh_timestamps_equal(notification_updated_at, comment_at))
        .then_some(triggering_comment)
}

fn gh_comment_timestamp(comment: &Value) -> Option<&str> {
    comment
        .get("updated_at")
        .and_then(Value::as_str)
        .or_else(|| comment.get("created_at").and_then(Value::as_str))
}

fn gh_timestamps_equal(left: &str, right: &str) -> bool {
    DateTime::parse_from_rfc3339(left)
        .ok()
        .zip(DateTime::parse_from_rfc3339(right).ok())
        .is_some_and(|(left, right)| left == right)
}

fn gh_triggering_comment(comment: &Value) -> Option<GhTriggeringComment> {
    let id = comment
        .get("id")
        .and_then(json_identifier)
        .filter(|id| !id.is_empty())?;
    let author = comment
        .pointer("/user/login")
        .and_then(Value::as_str)
        .filter(|author| !author.is_empty())?;
    let body = comment.get("body").and_then(Value::as_str)?;
    Some(GhTriggeringComment {
        id,
        author: author.to_owned(),
        body: body.to_owned(),
    })
}

fn comment_is_configured_trigger(triggers: &GhTriggers, body: &str) -> bool {
    let body = body.trim();
    triggers
        .command_comments
        .iter()
        .chain(triggers.mentions.iter())
        .any(|command| command == body)
}

fn json_identifier(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

fn repo_from_api_url(url: &str) -> Option<&str> {
    url.split_once("/repos/")
        .map(|(_, repo)| repo)
        .filter(|repo| {
            let mut parts = repo.split('/');
            parts.next().is_some_and(|part| !part.is_empty())
                && parts.next().is_some_and(|part| !part.is_empty())
                && parts.next().is_none()
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GhItemLocation {
    repo: String,
    number: u64,
    item_type: GhItemType,
}

fn parse_gh_item_url(url: &str) -> Result<GhItemLocation, String> {
    let location = url
        .strip_prefix("https://")
        .ok_or_else(|| "URL must use HTTPS".to_owned())?;
    let (host, path) = location
        .split_once('/')
        .ok_or_else(|| "URL must contain a host and item path".to_owned())?;
    if host != "github.com" {
        return Err("URL host must be github.com".to_owned());
    }
    if path.contains(['?', '#']) {
        return Err("URL must not contain a query or fragment".to_owned());
    }
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() != 4 || parts.iter().any(|part| part.is_empty()) {
        return Err("URL path must be owner/repo/issues|pull/number".to_owned());
    }
    let repo = format!("{}/{}", parts[0], parts[1]);
    validate_repo_constraint(&repo).map_err(|error| error.to_string())?;
    let item_type = match parts[2] {
        "issues" => GhItemType::Issue,
        "pull" => GhItemType::PullRequest,
        _ => return Err("URL must identify an issue or pull request".to_owned()),
    };
    let number = parts[3]
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| "URL item number must be positive".to_owned())?;
    Ok(GhItemLocation {
        repo,
        number,
        item_type,
    })
}

fn gh_item_state(item: &Value) -> Result<GhItemState, ProducerError> {
    match item.get("state").and_then(Value::as_str) {
        Some(state) if state.eq_ignore_ascii_case("open") => Ok(GhItemState::Open),
        Some(state) if state.eq_ignore_ascii_case("closed") => Ok(GhItemState::Closed),
        _ => Err(ProducerError::GitHub(
            "GitHub issue/PR omitted a supported state".to_owned(),
        )),
    }
}

fn gh_search_queries(constraints: &GhSourceConstraints) -> Vec<String> {
    let mut scopes = BTreeSet::new();
    if let Some(repo) = &constraints.repo {
        scopes.insert(format!("repo:{repo}"));
    }
    scopes.extend(
        constraints
            .repositories
            .iter()
            .map(|repo| format!("repo:{repo}")),
    );
    for owner in &constraints.owners {
        scopes.insert(format!("org:{owner}"));
        scopes.insert(format!("user:{owner}"));
    }
    let mut filters = Vec::new();
    filters.extend(
        constraints
            .labels
            .iter()
            .map(|label| format!("label:{}", quote_gh_query_value(label))),
    );
    if let Some(state) = constraints.state {
        filters.push(format!(
            "state:{}",
            match state {
                GhItemState::Open => "open",
                GhItemState::Closed => "closed",
            }
        ));
    }
    if let Some(assignee) = &constraints.assignee {
        filters.push(format!("assignee:{}", quote_gh_query_value(assignee)));
    }
    if constraints.kinds.len() == 1 {
        filters.push(format!(
            "is:{}",
            match constraints.kinds[0] {
                GhSourceItemKind::Issue => "issue",
                GhSourceItemKind::PullRequest => "pr",
            }
        ));
    }
    if let Some(query) = &constraints.query {
        filters.push(query.clone());
    }
    scopes
        .into_iter()
        .map(|scope| {
            std::iter::once(scope)
                .chain(filters.iter().cloned())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn quote_gh_query_value(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('\"', "\\\""))
}

impl GhMutationSink for GhCliMutationSink {
    fn post_evidence(&mut self, mutation: &GhCompletedMutation) -> Result<(), String> {
        if mutation.state != "COMPLETED" {
            return Err(format!(
                "refusing GitHub mutation state {:?}; expected COMPLETED",
                mutation.state
            ));
        }
        let completion_id = mutation
            .completion_id
            .as_deref()
            .ok_or_else(|| "concrete GitHub mutation requires a durable completionId".to_owned())?;
        let remote_key = stable_key(&["gh-remote-completion", completion_id]);
        let remote_marker = format!("<!-- tally-completion:{remote_key} -->");
        let body = format!(
            "{remote_marker}\n{}",
            serde_json::to_string(mutation)
                .map_err(|error| format!("cannot encode GitHub evidence: {error}"))?
        );
        let (kind, state, comment_exists) =
            self.completion_state(&mutation.item_id, &remote_marker)?;
        if !comment_exists {
            self.graphql(serde_json::json!({
                "query": GH_COMPLETION_COMMENT_GRAPHQL,
                "variables": {"itemId": mutation.item_id, "body": body},
            }))?;
        }
        if !matches!(state.as_str(), "OPEN" | "CLOSED" | "MERGED") {
            return Err(format!(
                "GitHub {kind} {:?} has unsupported state {state:?}",
                mutation.item_id
            ));
        }
        Ok(())
    }

    fn close_item(&mut self, mutation: &GhCompletedMutation) -> Result<(), String> {
        if mutation.state != "COMPLETED" {
            return Err(format!(
                "refusing GitHub mutation state {:?}; expected COMPLETED",
                mutation.state
            ));
        }
        let completion_id = mutation
            .completion_id
            .as_deref()
            .ok_or_else(|| "concrete GitHub mutation requires a durable completionId".to_owned())?;
        let remote_key = stable_key(&["gh-remote-completion", completion_id]);
        let remote_marker = format!("<!-- tally-completion:{remote_key} -->");
        let (kind, state, _) = self.completion_state(&mutation.item_id, &remote_marker)?;
        if state == "OPEN" {
            let query = if kind == "Issue" {
                GH_COMPLETION_ISSUE_GRAPHQL
            } else {
                GH_COMPLETION_PULL_REQUEST_GRAPHQL
            };
            self.graphql(serde_json::json!({
                "query": query,
                "variables": {"itemId": mutation.item_id},
            }))?;
        } else if !matches!(state.as_str(), "CLOSED" | "MERGED") {
            return Err(format!(
                "GitHub {kind} {:?} has unsupported state {state:?}",
                mutation.item_id
            ));
        }
        Ok(())
    }
}

impl GhAcknowledgementSink for GhCliAcknowledgementSink {
    fn post_acknowledgement(
        &mut self,
        acknowledgement: &GhTriggerAcknowledgement,
    ) -> Result<(), String> {
        let decision = match acknowledgement.decision {
            GhDecisionStatus::Accepted => "accepted",
            GhDecisionStatus::Filtered => "filtered",
            GhDecisionStatus::Duplicate => "duplicate",
            _ => {
                return Err(format!(
                    "refusing to acknowledge non-terminal trigger intake decision {:?}",
                    acknowledgement.decision
                ))
            }
        };
        let marker = format!(
            "<!-- tally-trigger:{}:{decision} -->",
            acknowledgement.receipt_id
        );
        let summary = match acknowledgement.decision {
            GhDecisionStatus::Accepted => "Tally accepted this trigger.",
            GhDecisionStatus::Filtered => "Tally filtered this trigger by policy.",
            GhDecisionStatus::Duplicate => "Tally already recorded this trigger.",
            _ => unreachable!("decision was narrowed above"),
        };
        let mut body = format!("{marker}\n{summary}");
        if let Some(task_uuid) = &acknowledgement.task_uuid {
            body.push_str(&format!("\n\nTask: `{task_uuid}`"));
        }
        if let Some(pointer) = &acknowledgement.status_pointer {
            body.push_str(&format!("\nStatus: `{pointer}`"));
        }
        let (_, state, exists) = self
            .mutation
            .completion_state(&acknowledgement.item_id, &marker)?;
        if !exists {
            self.mutation.graphql(serde_json::json!({
                "query": GH_COMPLETION_COMMENT_GRAPHQL,
                "variables": {"itemId": acknowledgement.item_id, "body": body},
            }))?;
        }
        if !matches!(state.as_str(), "OPEN" | "CLOSED" | "MERGED") {
            return Err(format!(
                "GitHub item {:?} has unsupported state {state:?}",
                acknowledgement.item_id
            ));
        }
        Ok(())
    }
}

impl GhCliMutationSink {
    fn completion_state(
        &self,
        item_id: &str,
        remote_marker: &str,
    ) -> Result<(String, String, bool), String> {
        let mut cursor = None::<String>;
        let mut identity = None::<(String, String)>;
        for _ in 0..MAX_GH_COMMENT_PAGES {
            let response = self.graphql(serde_json::json!({
                "query": GH_COMPLETION_STATE_GRAPHQL,
                "variables": {"itemId": item_id, "cursor": cursor},
            }))?;
            let node = response
                .pointer("/data/node")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    format!("GitHub item {item_id:?} did not resolve to an Issue or PullRequest")
                })?;
            let kind = node
                .get("__typename")
                .and_then(Value::as_str)
                .ok_or_else(|| "GitHub completion query omitted node __typename".to_owned())?;
            if !matches!(kind, "Issue" | "PullRequest") {
                return Err(format!(
                    "GitHub item {item_id:?} has unsupported node kind {kind:?}"
                ));
            }
            let state = node
                .get("state")
                .and_then(Value::as_str)
                .ok_or_else(|| "GitHub completion query omitted node state".to_owned())?;
            let current = (kind.to_owned(), state.to_owned());
            if identity
                .as_ref()
                .is_some_and(|identity| identity != &current)
            {
                return Err("GitHub completion identity changed during pagination".to_owned());
            }
            identity = Some(current);
            let comments = node
                .get("comments")
                .ok_or_else(|| "GitHub completion query omitted comments connection".to_owned())?;
            if comments
                .get("nodes")
                .and_then(Value::as_array)
                .is_some_and(|comments| {
                    comments.iter().any(|comment| {
                        comment
                            .get("body")
                            .and_then(Value::as_str)
                            .is_some_and(|comment| comment.contains(remote_marker))
                    })
                })
            {
                let (kind, state) = identity.expect("identity was assigned above");
                return Ok((kind, state, true));
            }
            let page_info = comments
                .get("pageInfo")
                .and_then(Value::as_object)
                .ok_or_else(|| "GitHub completion query omitted comments pageInfo".to_owned())?;
            if !page_info
                .get("hasNextPage")
                .and_then(Value::as_bool)
                .ok_or_else(|| "GitHub comments pageInfo omitted hasNextPage".to_owned())?
            {
                let (kind, state) = identity.expect("identity was assigned above");
                return Ok((kind, state, false));
            }
            cursor = Some(
                page_info
                    .get("endCursor")
                    .and_then(Value::as_str)
                    .filter(|cursor| !cursor.is_empty())
                    .ok_or_else(|| {
                        "GitHub comments pageInfo omitted a continuation cursor".to_owned()
                    })?
                    .to_owned(),
            );
        }
        Err(format!(
            "GitHub item {item_id:?} exceeds the {MAX_GH_COMMENT_PAGES}-page completion scan cap; refusing a possibly duplicate comment"
        ))
    }

    fn graphql(&self, request: Value) -> Result<Value, String> {
        let request = serde_json::to_vec(&request)
            .map_err(|error| format!("cannot encode GitHub GraphQL request: {error}"))?;
        let output = run_gh_bounded(
            &self.program,
            &["api", "graphql", "--input", "-"],
            Some(request),
        )?;
        let response: Value = serde_json::from_slice(&output)
            .map_err(|error| format!("gh api graphql returned invalid JSON: {error}"))?;
        if response
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty())
        {
            return Err(format!(
                "gh api graphql returned errors: {}",
                response["errors"]
            ));
        }
        Ok(response)
    }
}

fn run_gh_bounded(
    program: &Path,
    args: &[&str],
    input: Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot execute {}: {error}", program.display()))?;
    let stdin_task = input.map(|input| {
        let mut stdin = child.stdin.take().expect("requested piped gh stdin");
        thread::spawn(move || -> std::io::Result<()> {
            stdin.write_all(&input)?;
            drop(stdin);
            Ok(())
        })
    });
    let stdout_task = bounded_reader(
        child.stdout.take().expect("requested piped gh stdout"),
        MAX_GH_PROCESS_OUTPUT_BYTES,
    );
    let stderr_task = bounded_reader(
        child.stderr.take().expect("requested piped gh stderr"),
        MAX_GH_PROCESS_OUTPUT_BYTES,
    );
    let deadline = Instant::now() + GH_PROCESS_TIMEOUT;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("cannot poll {}: {error}", program.display()))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            break child.wait().map_err(|error| {
                format!("cannot reap timed-out {}: {error}", program.display())
            })?;
        }
        thread::sleep(Duration::from_millis(10));
    };
    if let Some(task) = stdin_task {
        task.join()
            .map_err(|_| "gh stdin writer panicked".to_owned())?
            .map_err(|error| format!("cannot write gh stdin: {error}"))?;
    }
    let (stdout, stdout_overflow) = stdout_task
        .join()
        .map_err(|_| "gh stdout reader panicked".to_owned())?
        .map_err(|error| format!("cannot read gh stdout: {error}"))?;
    let (stderr, stderr_overflow) = stderr_task
        .join()
        .map_err(|_| "gh stderr reader panicked".to_owned())?
        .map_err(|error| format!("cannot read gh stderr: {error}"))?;
    if timed_out {
        return Err(format!(
            "{} exceeded the {} second timeout",
            program.display(),
            GH_PROCESS_TIMEOUT.as_secs()
        ));
    }
    if stdout_overflow || stderr_overflow {
        return Err(format!(
            "{} output exceeded the {} byte cap",
            program.display(),
            MAX_GH_PROCESS_OUTPUT_BYTES
        ));
    }
    if !status.success() {
        return Err(format!(
            "{} exited {status}: {}",
            program.display(),
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    Ok(stdout)
}

fn bounded_reader(
    mut reader: impl Read + Send + 'static,
    limit: usize,
) -> thread::JoinHandle<std::io::Result<(Vec<u8>, bool)>> {
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut overflow = false;
        let mut buffer = [0_u8; 8192];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            let remaining = limit.saturating_sub(kept.len());
            kept.extend_from_slice(&buffer[..read.min(remaining)]);
            overflow |= read > remaining;
        }
        Ok((kept, overflow))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GhFilterReason {
    SourceNotConfigured,
    SourceUnconstrained,
    RepositoryNotAllowed,
    ItemNotAllowlisted,
    LabelMismatch,
    StateMismatch,
    AssigneeMismatch,
    ItemKindMismatch,
    NotificationReasonMismatch,
    TriggerNotConfigured,
    SelfTriggerDisabled,
    TriggerActorNotAllowed,
    TriggerActorExcluded,
    TriggerActorUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmitOutcome {
    Emitted(PathBuf),
    Duplicate,
    Filtered { reason: GhFilterReason },
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GhDecisionStatus {
    Accepted,
    Filtered,
    Duplicate,
    Malformed,
    WouldEnqueue,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhCandidateSummary {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

impl GhCandidateSummary {
    fn from_observation(observation: &GhObservation) -> Self {
        Self {
            source: observation.source.clone(),
            repo: Some(observation.repo.clone()),
            number: Some(observation.number),
            url: Some(observation.html_url.clone()),
            node_id: Some(observation.node_id.clone()),
            trigger_kind: Some(observation.trigger_kind.clone()),
            trigger_actor: Some(observation.trigger_actor.clone()),
            event_id: observation.event_id.clone(),
            comment_id: observation.comment_id.clone(),
            timestamp: Some(observation.trigger_timestamp.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhEnqueuePreview {
    pub task_uuid: String,
    pub argv: Vec<String>,
    #[serde(rename = "pool", serialize_with = "crate::poolset::serialize")]
    pub pools: Vec<String>,
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_options: Option<AdapterJobOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_manifest: Option<GateManifestSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    pub priority: Priority,
    pub dedup_key: String,
    pub context: GhOrigin,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhDecision {
    pub producer: String,
    pub candidate: GhCandidateSummary,
    pub decision: GhDecisionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<GhFilterReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enqueue: Option<GhEnqueuePreview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhTriggerAcknowledgement {
    pub schema_version: u32,
    pub producer: String,
    pub receipt_id: String,
    pub item_id: String,
    pub decision: GhDecisionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<GhFilterReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_pointer: Option<String>,
}

pub trait GhAcknowledgementSink {
    fn post_acknowledgement(
        &mut self,
        acknowledgement: &GhTriggerAcknowledgement,
    ) -> Result<(), String>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct GhTriggerReceipt {
    schema_version: u32,
    receipt_id: String,
    producer: String,
    source: String,
    item_id: String,
    event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    comment_id: Option<String>,
    trigger_kind: String,
    trigger_actor: String,
    trigger_timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trigger_value: Option<String>,
    primary_decision: GhDecisionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rule: Option<GhFilterReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_uuid: Option<String>,
    primary_acknowledged: bool,
    duplicate_acknowledged: bool,
    duplicate_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReachabilityStable {
    Reachable,
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReachabilityTransition {
    Lost,
    Returned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReachabilityOutcome {
    pub stable: ReachabilityStable,
    pub transition: Option<ReachabilityTransition>,
    pub generation: u64,
    pub emitted: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReachabilityState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    probe_pool: Option<String>,
    stable: ReachabilityStable,
    candidate_reachable: Option<bool>,
    consecutive: u32,
    generation: u64,
    #[serde(default)]
    notified_generation: u64,
}

impl Default for ReachabilityState {
    fn default() -> Self {
        Self {
            probe_pool: None,
            stable: ReachabilityStable::Reachable,
            candidate_reachable: None,
            consecutive: 0,
            generation: 0,
            notified_generation: 0,
        }
    }
}

pub struct ProducerEngine<'a> {
    registry: &'a BTreeMap<String, ProducerConfig>,
    events_dir: PathBuf,
    state_dir: PathBuf,
}

impl<'a> ProducerEngine<'a> {
    pub fn new(
        registry: &'a BTreeMap<String, ProducerConfig>,
        events_dir: impl Into<PathBuf>,
        state_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            registry,
            events_dir: events_dir.into(),
            state_dir: state_dir.into(),
        }
    }

    pub fn producer_kind(&self, producer: &str) -> Result<&'static str, ProducerError> {
        Ok(self.get(producer)?.kind())
    }

    pub fn emit_calendar(
        &self,
        producer: &str,
        now: DateTime<Utc>,
    ) -> Result<EmitOutcome, ProducerError> {
        let ProducerConfig::Calendar(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "calendar"));
        };
        let payload = config
            .enqueue
            .payload(EnqueueSource::Calendar, Some(producer), now, None)?;
        let name = format!("{producer}-calendar-{}{}", Uuid::new_v4(), INGRESS_SUFFIX);
        self.emit_named(&name, &payload)
    }

    pub fn emit_gh(
        &self,
        producer: &str,
        observation: &GhObservation,
        now: DateTime<Utc>,
    ) -> Result<EmitOutcome, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        if !config.enable {
            return Ok(EmitOutcome::Disabled);
        }
        validate_gh_observation(producer, config, observation)?;
        if let Some(reason) = gh_filter_reason(config, observation) {
            return Ok(EmitOutcome::Filtered { reason });
        }
        let origin = gh_origin(producer, config, observation);
        let mut payload =
            config
                .enqueue
                .payload(EnqueueSource::Gh, Some(producer), now, Some(&origin))?;
        payload.dedup_key = Some(gh_trigger_dedup_key(&origin)?);
        payload.gh_trigger_actor = Some(observation.trigger_actor.clone());
        payload.gh_self_actor = Some(observation.self_actor.clone());
        payload.task_uuid = Some(gh_trigger_task_uuid(&origin)?.to_string());
        let key = gh_trigger_receipt_id(&origin)?;
        payload.gh_origin = Some(origin);
        self.emit_named(&format!("{producer}-gh-{key}{INGRESS_SUFFIX}"), &payload)
    }

    pub fn poll_gh(
        &self,
        producer: &str,
        intake: &GhCliIntake,
        now: DateTime<Utc>,
    ) -> Result<Vec<EmitOutcome>, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        if !config.enable {
            return Ok(Vec::new());
        }
        intake
            .poll(config)?
            .iter()
            .map(|candidate| match candidate {
                GhIntakeCandidate::Observation(observation) => {
                    self.emit_gh(producer, observation, now)
                }
                GhIntakeCandidate::TriggerActorUnavailable { .. } => Ok(EmitOutcome::Filtered {
                    reason: GhFilterReason::TriggerActorUnavailable,
                }),
            })
            .collect()
    }

    pub fn preview_gh(
        &self,
        producer: &str,
        intake: &GhCliIntake,
        now: DateTime<Utc>,
    ) -> Result<Vec<GhDecision>, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        if !config.enable {
            return Ok(Vec::new());
        }
        let mut decisions = Vec::new();
        for candidate in intake.poll(config)? {
            match candidate {
                GhIntakeCandidate::Observation(observation) => {
                    decisions.push(
                        match self.preview_gh_observation(producer, &observation, now) {
                            Ok(decision) => decision,
                            Err(error) => malformed_gh_decision(
                                producer,
                                GhCandidateSummary::from_observation(&observation),
                                error.to_string(),
                            ),
                        },
                    );
                }
                GhIntakeCandidate::TriggerActorUnavailable { source, node_id } => {
                    decisions.push(unavailable_actor_decision(producer, source, node_id));
                }
            }
        }
        Ok(decisions)
    }

    pub fn explain_gh(
        &self,
        producer: &str,
        intake: &GhCliIntake,
        item_url: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<GhDecision>, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        let mut decisions = Vec::new();
        for candidate in intake.item(config, item_url)? {
            match candidate {
                GhIntakeCandidate::Observation(observation) => {
                    decisions.push(
                        match self.preview_gh_observation(producer, &observation, now) {
                            Ok(decision) => decision,
                            Err(error) => malformed_gh_decision(
                                producer,
                                GhCandidateSummary::from_observation(&observation),
                                error.to_string(),
                            ),
                        },
                    );
                }
                GhIntakeCandidate::TriggerActorUnavailable { source, node_id } => {
                    decisions.push(unavailable_actor_decision(producer, source, node_id));
                }
            }
        }
        Ok(decisions)
    }

    pub fn diagnostic_gh_observation(
        &self,
        producer: &str,
        intake: &GhCliIntake,
        item_url: &str,
        trigger_kind: &str,
        actor: &str,
        now: DateTime<Utc>,
    ) -> Result<GhObservation, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        intake.diagnostic_observation(config, item_url, trigger_kind, actor, now)
    }

    pub fn poll_gh_with_acknowledgements(
        &self,
        producer: &str,
        intake: &GhCliIntake,
        now: DateTime<Utc>,
        sink: &mut dyn GhAcknowledgementSink,
    ) -> Result<Vec<GhDecision>, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        if !config.enable {
            return Ok(Vec::new());
        }
        let mut decisions = Vec::new();
        for candidate in intake.poll(config)? {
            match candidate {
                GhIntakeCandidate::Observation(observation) => {
                    decisions.push(
                        match self.admit_gh_observation(producer, &observation, now, sink) {
                            Ok(decision) => decision,
                            Err(ProducerError::InvalidObservation(detail)) => {
                                malformed_gh_decision(
                                    producer,
                                    GhCandidateSummary::from_observation(&observation),
                                    detail,
                                )
                            }
                            Err(error) => return Err(error),
                        },
                    );
                }
                GhIntakeCandidate::TriggerActorUnavailable { source, node_id } => {
                    decisions.push(unavailable_actor_decision(producer, source, node_id));
                }
            }
        }
        Ok(decisions)
    }

    pub fn preview_gh_observation(
        &self,
        producer: &str,
        observation: &GhObservation,
        now: DateTime<Utc>,
    ) -> Result<GhDecision, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        if !config.enable {
            return Ok(disabled_gh_decision(producer, observation));
        }
        validate_gh_observation(producer, config, observation)?;
        let origin = gh_origin(producer, config, observation);
        let receipt_id = gh_trigger_receipt_id(&origin)?;
        let task_uuid = gh_trigger_task_uuid(&origin)?.to_string();
        if let Some(receipt) = self.read_gh_receipt(&receipt_id)? {
            return Ok(duplicate_gh_decision(
                producer,
                observation,
                &receipt_id,
                receipt.task_uuid,
            ));
        }
        if let Some(rule) = gh_filter_reason(config, observation) {
            return Ok(filtered_gh_decision(
                producer,
                observation,
                receipt_id,
                rule,
            ));
        }
        would_enqueue_gh_decision(
            producer,
            config,
            observation,
            origin,
            receipt_id,
            task_uuid,
            now,
        )
    }

    pub fn admit_gh_observation(
        &self,
        producer: &str,
        observation: &GhObservation,
        now: DateTime<Utc>,
        sink: &mut dyn GhAcknowledgementSink,
    ) -> Result<GhDecision, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        if !config.enable {
            return Ok(disabled_gh_decision(producer, observation));
        }
        validate_gh_observation(producer, config, observation)?;
        let origin = gh_origin(producer, config, observation);
        let receipt_id = gh_trigger_receipt_id(&origin)?;
        let task_uuid = gh_trigger_task_uuid(&origin)?.to_string();
        let receipts_dir = self.state_dir.join("producers/gh-triggers");
        create_dir_durable(&receipts_dir)?;
        let lock_path = receipts_dir.join(format!("{receipt_id}.lock"));
        let lock = open_private_rw(&lock_path)?;
        lock.lock_exclusive().map_err(|source| ProducerError::Io {
            path: lock_path.clone(),
            source,
        })?;
        let receipt_path = receipts_dir.join(format!("{receipt_id}.json"));
        let (mut receipt, decision, needs_acknowledgement) = if path_lexists(&receipt_path)? {
            let mut receipt: GhTriggerReceipt =
                serde_json::from_slice(&read_bounded_regular(&receipt_path, 64 * 1024)?)?;
            validate_receipt_identity(&receipt, producer, observation, &receipt_id)?;
            if receipt.primary_acknowledged {
                receipt.duplicate_count = receipt.duplicate_count.saturating_add(1);
                let needs_acknowledgement = !receipt.duplicate_acknowledged;
                let decision = duplicate_gh_decision(
                    producer,
                    observation,
                    &receipt_id,
                    receipt.task_uuid.clone(),
                );
                (receipt, decision, needs_acknowledgement)
            } else {
                let decision =
                    primary_receipt_decision(producer, config, observation, &receipt, now)?;
                (receipt, decision, true)
            }
        } else {
            let rule = gh_filter_reason(config, observation);
            let (primary_decision, ingress, receipt_task) = if rule.is_some() {
                (GhDecisionStatus::Filtered, None, None)
            } else {
                let ingress = match self.emit_gh(producer, observation, now)? {
                    EmitOutcome::Emitted(path) => Some(path),
                    EmitOutcome::Duplicate => None,
                    EmitOutcome::Filtered { reason } => {
                        return Err(ProducerError::InvalidObservation(format!(
                            "GitHub trigger changed filter decision while locked: {reason:?}"
                        )))
                    }
                    EmitOutcome::Disabled => {
                        return Ok(disabled_gh_decision(producer, observation))
                    }
                };
                (GhDecisionStatus::Accepted, ingress, Some(task_uuid.clone()))
            };
            let receipt = GhTriggerReceipt {
                schema_version: 1,
                receipt_id: receipt_id.clone(),
                producer: producer.to_owned(),
                source: observation.source.clone(),
                item_id: observation.node_id.clone(),
                event_id: observation
                    .event_id
                    .clone()
                    .expect("current observations require eventId"),
                comment_id: observation.comment_id.clone(),
                trigger_kind: observation.trigger_kind.clone(),
                trigger_actor: observation.trigger_actor.clone(),
                trigger_timestamp: observation.trigger_timestamp.clone(),
                trigger_value: observation.trigger_value.clone(),
                primary_decision,
                rule,
                task_uuid: receipt_task,
                primary_acknowledged: false,
                duplicate_acknowledged: false,
                duplicate_count: 0,
            };
            let decision = match primary_decision {
                GhDecisionStatus::Accepted => accepted_gh_decision(
                    producer,
                    config,
                    observation,
                    origin,
                    receipt_id.clone(),
                    task_uuid,
                    ingress,
                    now,
                )?,
                GhDecisionStatus::Filtered => filtered_gh_decision(
                    producer,
                    observation,
                    receipt_id.clone(),
                    rule.expect("filtered receipt carries its rule"),
                ),
                _ => unreachable!("receipt primary decisions are accepted or filtered"),
            };
            (receipt, decision, true)
        };
        write_json_atomic(&receipt_path, &receipt)?;
        if needs_acknowledgement {
            if config.post_receipt && !config.never_mutate {
                let acknowledgement = acknowledgement_for_decision(&decision, observation)?;
                sink.post_acknowledgement(&acknowledgement)
                    .map_err(ProducerError::Acknowledgement)?;
            }
            match decision.decision {
                GhDecisionStatus::Duplicate => receipt.duplicate_acknowledged = true,
                GhDecisionStatus::Accepted | GhDecisionStatus::Filtered => {
                    receipt.primary_acknowledged = true
                }
                _ => {}
            }
            write_json_atomic(&receipt_path, &receipt)?;
        }
        FileExt::unlock(&lock).map_err(|source| ProducerError::Io {
            path: lock_path,
            source,
        })?;
        Ok(decision)
    }

    fn read_gh_receipt(&self, receipt_id: &str) -> Result<Option<GhTriggerReceipt>, ProducerError> {
        let path = self
            .state_dir
            .join("producers/gh-triggers")
            .join(format!("{receipt_id}.json"));
        if !path_lexists(&path)? {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&read_bounded_regular(
            &path,
            64 * 1024,
        )?)?))
    }

    pub fn validate_gh_origin(&self, origin: &GhOrigin) -> Result<(), ProducerError> {
        origin
            .validate()
            .map_err(|error| ProducerError::InvalidObservation(error.to_string()))?;
        let ProducerConfig::Gh(config) = self.get(&origin.producer)? else {
            return Err(self.kind_mismatch(&origin.producer, "gh"));
        };
        if !config.enable {
            return Err(ProducerError::InvalidObservation(format!(
                "gh producer {:?} is disabled",
                origin.producer
            )));
        }
        if origin.actor_exclude != config.actor_exclude {
            return Err(ProducerError::InvalidObservation(format!(
                "gh producer {:?} origin actorExclude does not match configuration",
                origin.producer
            )));
        }
        if !config
            .sources
            .iter()
            .any(|source| source.kind() == origin.source)
        {
            return Err(ProducerError::InvalidObservation(format!(
                "gh producer {:?} origin source {:?} is not configured",
                origin.producer, origin.source
            )));
        }
        if origin.schema_version == 0 {
            let excluded = if origin.actor_exclude == "self" {
                origin.trigger_actor == origin.self_actor
            } else {
                origin.trigger_actor == origin.actor_exclude
            };
            if excluded {
                return Err(ProducerError::InvalidObservation(format!(
                    "gh producer {:?} legacy origin actor is excluded",
                    origin.producer
                )));
            }
            return Ok(());
        }
        if origin.schema_version == 1 {
            if origin.allow_self_triggered != config.allow_self_triggered
                || origin.allowed_actors.iter().collect::<BTreeSet<_>>()
                    != config.allowed_actors.iter().collect::<BTreeSet<_>>()
            {
                return Err(ProducerError::InvalidObservation(format!(
                    "gh producer {:?} origin actor policy does not match configuration",
                    origin.producer
                )));
            }
            let excluded = (!origin.allowed_actors.is_empty()
                && !origin
                    .allowed_actors
                    .iter()
                    .any(|actor| actor.eq_ignore_ascii_case(&origin.trigger_actor)))
                || (origin.trigger_actor == origin.self_actor && !origin.allow_self_triggered)
                || (origin.actor_exclude != "self"
                    && origin
                        .trigger_actor
                        .eq_ignore_ascii_case(&origin.actor_exclude));
            if excluded {
                return Err(ProducerError::InvalidObservation(format!(
                    "gh producer {:?} legacy origin trigger actor is filtered",
                    origin.producer
                )));
            }
            return Ok(());
        }
        if origin.allow_self_triggered != config.allow_self_triggered
            || origin.allowed_actors.iter().collect::<BTreeSet<_>>()
                != config.allowed_actors.iter().collect::<BTreeSet<_>>()
        {
            return Err(ProducerError::InvalidObservation(format!(
                "gh producer {:?} origin actor policy does not match configuration",
                origin.producer
            )));
        }
        let observation = gh_observation(origin)?;
        validate_gh_observation(&origin.producer, config, &observation)?;
        if let Some(reason) = gh_filter_reason(config, &observation) {
            return Err(ProducerError::InvalidObservation(format!(
                "gh producer {:?} origin trigger actor is filtered: {reason:?}",
                origin.producer,
            )));
        }
        Ok(())
    }

    pub fn complete_gh(
        &self,
        origin: &GhOrigin,
        verdict: Verdict,
        evidence: Option<Value>,
        sink: &mut dyn GhMutationSink,
    ) -> Result<bool, ProducerError> {
        self.complete_gh_with_id(origin, None, verdict, evidence, None, sink)
    }

    pub fn complete_gh_with_completion(
        &self,
        origin: &GhOrigin,
        verdict: Verdict,
        evidence: Option<Value>,
        completion: Option<SemanticCompletion>,
        sink: &mut dyn GhMutationSink,
    ) -> Result<bool, ProducerError> {
        self.complete_gh_with_id(origin, None, verdict, evidence, completion, sink)
    }

    pub fn complete_gh_once(
        &self,
        origin: &GhOrigin,
        completion_id: &str,
        verdict: Verdict,
        evidence: Option<Value>,
        sink: &mut dyn GhMutationSink,
    ) -> Result<bool, ProducerError> {
        self.complete_gh_once_with_completion(origin, completion_id, verdict, evidence, None, sink)
    }

    pub fn complete_gh_once_with_completion(
        &self,
        origin: &GhOrigin,
        completion_id: &str,
        verdict: Verdict,
        evidence: Option<Value>,
        completion: Option<SemanticCompletion>,
        sink: &mut dyn GhMutationSink,
    ) -> Result<bool, ProducerError> {
        if completion_id.trim().is_empty()
            || completion_id.len() > MAX_GH_ORIGIN_FIELD_BYTES
            || completion_id.chars().any(char::is_control)
        {
            return Err(ProducerError::InvalidObservation(
                format!(
                    "GitHub completion id must be non-empty, at most {MAX_GH_ORIGIN_FIELD_BYTES} bytes, and contain no control characters"
                ),
            ));
        }
        let completed_dir = self.state_dir.join("producers/gh-completed");
        create_dir_durable(&completed_dir)?;
        let lock_path = completed_dir.join("mutations.lock");
        let lock = open_private_rw(&lock_path)?;
        lock.lock_exclusive().map_err(|source| ProducerError::Io {
            path: lock_path.clone(),
            source,
        })?;
        let marker_key = stable_key(&[
            "gh-completed",
            &origin.producer,
            &origin.source,
            &origin.node_id,
            completion_id,
        ]);
        let marker_path = completed_dir.join(format!("{marker_key}.json"));
        if path_lexists(&marker_path)? {
            let marker: GhCompletionMarker =
                serde_json::from_slice(&read_bounded_regular(&marker_path, 64 * 1024)?)?;
            if marker.completion_id != completion_id
                || marker.producer != origin.producer
                || marker.source != origin.source
                || marker.item_id != origin.node_id
            {
                return Err(ProducerError::InvalidObservation(format!(
                    "GitHub completion marker {} does not match its identity",
                    marker_path.display()
                )));
            }
            return Ok(false);
        }
        if !self.complete_gh_with_id(
            origin,
            Some(completion_id),
            verdict,
            evidence,
            completion,
            sink,
        )? {
            return Ok(false);
        }
        write_json_atomic(
            &marker_path,
            &GhCompletionMarker {
                completion_id: completion_id.to_owned(),
                producer: origin.producer.clone(),
                source: origin.source.clone(),
                item_id: origin.node_id.clone(),
            },
        )?;
        FileExt::unlock(&lock).map_err(|source| ProducerError::Io {
            path: lock_path,
            source,
        })?;
        Ok(true)
    }

    fn complete_gh_with_id(
        &self,
        origin: &GhOrigin,
        completion_id: Option<&str>,
        verdict: Verdict,
        evidence: Option<Value>,
        completion: Option<SemanticCompletion>,
        sink: &mut dyn GhMutationSink,
    ) -> Result<bool, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(&origin.producer)? else {
            return Err(self.kind_mismatch(&origin.producer, "gh"));
        };
        if !config.enable || config.never_mutate {
            return Ok(false);
        }
        self.validate_gh_origin(origin)?;
        let execution_passed = matches!(verdict, Verdict::Pass | Verdict::Reused);
        let evidence = (config.post_evidence && execution_passed)
            .then_some(evidence)
            .flatten();
        let gate_summary = config
            .post_gate_summary
            .then(|| completion.as_ref().map(|facts| facts.gates.clone()))
            .flatten();
        let acceptance =
            (config.post_gate_summary || config.request_review || config.close_on_acceptance)
                .then(|| completion.as_ref().map(|facts| facts.acceptance.clone()))
                .flatten();
        let request_review = config.request_review
            && acceptance
                .as_ref()
                .is_none_or(|fact| fact.status != AcceptanceStatus::Accepted);
        let close_on_pass = config.close_on_pass()
            && execution_passed
            && completion
                .as_ref()
                .is_none_or(|facts| facts.gates.status == GateSummaryStatus::Pass);
        let close_on_acceptance = config.close_on_acceptance
            && completion
                .as_ref()
                .is_some_and(|facts| facts.acceptance.status == AcceptanceStatus::Accepted);
        let should_post =
            evidence.is_some() || gate_summary.is_some() || acceptance.is_some() || request_review;
        if !should_post && !close_on_pass && !close_on_acceptance {
            return Ok(false);
        }
        let mutation = GhCompletedMutation {
            producer: origin.producer.clone(),
            source: origin.source.clone(),
            item_id: origin.node_id.clone(),
            completion_id: completion_id.map(str::to_owned),
            state: "COMPLETED".to_owned(),
            evidence,
            gate_summary,
            acceptance,
            request_review,
        };
        if should_post {
            sink.post_evidence(&mutation)
                .map_err(ProducerError::Mutation)?;
        }
        if close_on_pass || close_on_acceptance {
            sink.close_item(&mutation)
                .map_err(ProducerError::Mutation)?;
        }
        Ok(true)
    }

    pub fn emit_build_effect(
        &self,
        producer: &str,
        store_path: &Path,
        now: DateTime<Utc>,
    ) -> Result<EmitOutcome, ProducerError> {
        let ProducerConfig::BuildEffect(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "build-effect"));
        };
        let store_path = validate_store_path(store_path)?;
        let dedup_key = format!("build-effect:{producer}:{store_path}");
        if read_acknowledged_events(&self.events_dir)?
            .iter()
            .any(|event| {
                event.row.source == EnqueueSource::BuildEffect
                    && event.row.dedup_key.as_deref() == Some(dedup_key.as_str())
            })
        {
            return Ok(EmitOutcome::Duplicate);
        }
        let mut payload =
            config
                .on_key
                .payload(EnqueueSource::BuildEffect, Some(producer), now, None)?;
        payload.dedup_key = Some(dedup_key);
        let key = stable_key(&["build-effect", producer, &store_path]);
        self.emit_named(
            &format!("{producer}-build-effect-{key}{INGRESS_SUFFIX}"),
            &payload,
        )
    }

    pub fn scan_build_effect(&self, producer: &str) -> Result<Vec<PathBuf>, ProducerError> {
        let ProducerConfig::BuildEffect(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "build-effect"));
        };
        scan_store_paths(config.watch, &config.path)
    }

    pub fn observe_reachability(
        &self,
        producer: &str,
        reachable: bool,
        now: DateTime<Utc>,
    ) -> Result<ReachabilityOutcome, ProducerError> {
        let ProducerConfig::PoolReachability(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "pool-reachability"));
        };
        let producer_state = self.state_dir.join("producers");
        create_dir_durable(&producer_state)?;
        let lock_path = producer_state.join(format!("{producer}.reachability.lock"));
        let lock = open_private_rw(&lock_path)?;
        lock.lock_exclusive().map_err(|source| ProducerError::Io {
            path: lock_path.clone(),
            source,
        })?;
        let state_path = producer_state.join(format!("{producer}.reachability.json"));
        let mut state = read_reachability_state(&state_path)?;
        match state.probe_pool.as_deref() {
            Some(bound) if bound == config.probe_pool => {}
            Some(bound) => {
                return Err(ProducerError::InvalidObservation(format!(
                    "reachability state {} is bound to probePool {bound:?}, not {:?}",
                    state_path.display(),
                    config.probe_pool
                )))
            }
            None if state.generation == 0 => {
                state.probe_pool = Some(config.probe_pool.clone());
            }
            None => {
                return Err(ProducerError::InvalidObservation(format!(
                    "reachability state {} has transitions without a probePool binding",
                    state_path.display()
                )))
            }
        }
        let mut transition = None;
        if state.generation == state.notified_generation {
            let expected_reachable = state.stable == ReachabilityStable::Reachable;
            if reachable == expected_reachable {
                state.candidate_reachable = None;
                state.consecutive = 0;
            } else {
                if state.candidate_reachable == Some(reachable) {
                    state.consecutive = state.consecutive.saturating_add(1);
                } else {
                    state.candidate_reachable = Some(reachable);
                    state.consecutive = 1;
                }
                if state.consecutive >= config.hysteresis {
                    state.stable = if reachable {
                        ReachabilityStable::Reachable
                    } else {
                        ReachabilityStable::Lost
                    };
                    state.candidate_reachable = None;
                    state.consecutive = 0;
                    state.generation = state.generation.checked_add(1).ok_or_else(|| {
                        ProducerError::InvalidObservation(
                            "reachability transition generation overflow".to_owned(),
                        )
                    })?;
                    transition = Some(if reachable {
                        ReachabilityTransition::Returned
                    } else {
                        ReachabilityTransition::Lost
                    });
                }
            }
        }

        let mut emitted = Vec::new();
        if let Some(active_transition) = transition {
            let actions: Vec<(&str, &ProducerEnqueue)> = match active_transition {
                ReachabilityTransition::Lost => config
                    .on_lost
                    .as_ref()
                    .map(|enqueue| vec![("lost", enqueue)])
                    .unwrap_or_default(),
                ReachabilityTransition::Returned => {
                    let mut actions = Vec::new();
                    if let Some(enqueue) = &config.on_return {
                        actions.push(("return", enqueue));
                    }
                    if let Some(enqueue) = &config.on_return_attest {
                        actions.push(("return-attest", enqueue));
                    }
                    actions
                }
            };
            for (action, enqueue) in actions {
                let payload =
                    enqueue.payload(EnqueueSource::PoolReachability, Some(producer), now, None)?;
                let name = format!(
                    "{producer}-reach-{}-{action}{INGRESS_SUFFIX}",
                    state.generation
                );
                if let EmitOutcome::Emitted(path) = self.emit_named(&name, &payload)? {
                    emitted.push(path);
                }
            }
        }
        let pending_transition =
            (state.generation > state.notified_generation).then_some(match state.stable {
                ReachabilityStable::Reachable => ReachabilityTransition::Returned,
                ReachabilityStable::Lost => ReachabilityTransition::Lost,
            });
        write_json_atomic(&state_path, &state)?;
        FileExt::unlock(&lock).map_err(|source| ProducerError::Io {
            path: lock_path,
            source,
        })?;
        Ok(ReachabilityOutcome {
            stable: state.stable,
            transition: pending_transition,
            generation: state.generation,
            emitted,
        })
    }

    pub fn validate_reachability_transition(
        &self,
        producer: &str,
        transition: ReachabilityTransition,
        generation: u64,
    ) -> Result<String, ProducerError> {
        let ProducerConfig::PoolReachability(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "pool-reachability"));
        };
        if generation == 0 {
            return Err(ProducerError::InvalidObservation(
                "reachability transition generation must be positive".to_owned(),
            ));
        }
        let state_path = self
            .state_dir
            .join("producers")
            .join(format!("{producer}.reachability.json"));
        let state = read_reachability_state(&state_path)?;
        validate_reachability_binding(&state, &state_path, &config.probe_pool)?;
        let expected = match state.stable {
            ReachabilityStable::Reachable => ReachabilityTransition::Returned,
            ReachabilityStable::Lost => ReachabilityTransition::Lost,
        };
        if state.generation != generation || expected != transition {
            return Err(ProducerError::InvalidObservation(format!(
                "reachability transition {transition:?} generation {generation} is not the current confirmed state for producer {producer:?}"
            )));
        }
        Ok(config.probe_pool.clone())
    }

    pub fn acknowledge_reachability_transition(
        &self,
        producer: &str,
        generation: u64,
    ) -> Result<(), ProducerError> {
        let ProducerConfig::PoolReachability(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "pool-reachability"));
        };
        let producer_state = self.state_dir.join("producers");
        create_dir_durable(&producer_state)?;
        let lock_path = producer_state.join(format!("{producer}.reachability.lock"));
        let lock = open_private_rw(&lock_path)?;
        lock.lock_exclusive().map_err(|source| ProducerError::Io {
            path: lock_path.clone(),
            source,
        })?;
        let state_path = producer_state.join(format!("{producer}.reachability.json"));
        let mut state = read_reachability_state(&state_path)?;
        validate_reachability_binding(&state, &state_path, &config.probe_pool)?;
        if state.generation != generation {
            return Err(ProducerError::InvalidObservation(format!(
                "cannot acknowledge stale reachability generation {generation}; current generation is {}",
                state.generation
            )));
        }
        state.notified_generation = state.notified_generation.max(generation);
        write_json_atomic(&state_path, &state)?;
        FileExt::unlock(&lock).map_err(|source| ProducerError::Io {
            path: lock_path,
            source,
        })
    }

    pub fn confirmed_pool_returns(&self) -> Result<BTreeSet<String>, ProducerError> {
        let mut pools = BTreeSet::new();
        for (producer, config) in self.registry {
            let ProducerConfig::PoolReachability(config) = config else {
                continue;
            };
            let state_path = self
                .state_dir
                .join("producers")
                .join(format!("{producer}.reachability.json"));
            if !state_path.exists() {
                continue;
            }
            let state = read_reachability_state(&state_path)?;
            validate_reachability_binding(&state, &state_path, &config.probe_pool)?;
            if state.generation > 0 && state.stable == ReachabilityStable::Reachable {
                pools.insert(config.probe_pool.clone());
            }
        }
        Ok(pools)
    }

    pub fn confirmed_pool_losses(&self) -> Result<BTreeSet<String>, ProducerError> {
        let mut pools = BTreeSet::new();
        for (producer, config) in self.registry {
            let ProducerConfig::PoolReachability(config) = config else {
                continue;
            };
            let state_path = self
                .state_dir
                .join("producers")
                .join(format!("{producer}.reachability.json"));
            if !state_path.exists() {
                continue;
            }
            let state = read_reachability_state(&state_path)?;
            validate_reachability_binding(&state, &state_path, &config.probe_pool)?;
            if state.generation > 0 && state.stable == ReachabilityStable::Lost {
                pools.insert(config.probe_pool.clone());
            }
        }
        Ok(pools)
    }

    fn get(&self, producer: &str) -> Result<&ProducerConfig, ProducerError> {
        validate_producer_name(producer)?;
        self.registry
            .get(producer)
            .ok_or_else(|| ProducerError::UnknownProducer(producer.to_owned()))
    }

    fn kind_mismatch(&self, producer: &str, expected: &str) -> ProducerError {
        let actual = self
            .registry
            .get(producer)
            .map_or("unknown", ProducerConfig::kind);
        ProducerError::KindMismatch {
            producer: producer.to_owned(),
            expected: expected.to_owned(),
            actual: actual.to_owned(),
        }
    }

    fn emit_named(
        &self,
        name: &str,
        payload: &EnqueuePayload,
    ) -> Result<EmitOutcome, ProducerError> {
        let _ingress_lock = lock_ingress(&self.events_dir)?;
        if ingress_name_exists(&self.events_dir, name)? {
            return Ok(EmitOutcome::Duplicate);
        }
        create_dir_durable(&self.events_dir)?;
        let bytes = serde_json::to_vec(payload)?;
        if bytes.len().saturating_add(1) > MAX_INGRESS_BYTES as usize {
            return Err(ProducerError::InvalidObservation(format!(
                "producer payload exceeds the {MAX_INGRESS_BYTES} byte ingress limit"
            )));
        }
        let path = self.events_dir.join(name);
        if write_new_atomic(&path, &bytes)? {
            Ok(EmitOutcome::Emitted(path))
        } else {
            Ok(EmitOutcome::Duplicate)
        }
    }
}

fn malformed_gh_decision(
    producer: &str,
    candidate: GhCandidateSummary,
    detail: String,
) -> GhDecision {
    GhDecision {
        producer: producer.to_owned(),
        candidate,
        decision: GhDecisionStatus::Malformed,
        rule: None,
        receipt_id: None,
        task_uuid: None,
        existing_task: None,
        status_pointer: None,
        enqueue: None,
        ingress: None,
        detail: Some(detail),
    }
}

fn unavailable_actor_decision(producer: &str, source: String, node_id: String) -> GhDecision {
    GhDecision {
        producer: producer.to_owned(),
        candidate: GhCandidateSummary {
            source,
            repo: None,
            number: None,
            url: None,
            node_id: Some(node_id),
            trigger_kind: None,
            trigger_actor: None,
            event_id: None,
            comment_id: None,
            timestamp: None,
        },
        decision: GhDecisionStatus::Filtered,
        rule: Some(GhFilterReason::TriggerActorUnavailable),
        receipt_id: None,
        task_uuid: None,
        existing_task: None,
        status_pointer: None,
        enqueue: None,
        ingress: None,
        detail: None,
    }
}

fn disabled_gh_decision(producer: &str, observation: &GhObservation) -> GhDecision {
    GhDecision {
        producer: producer.to_owned(),
        candidate: GhCandidateSummary::from_observation(observation),
        decision: GhDecisionStatus::Disabled,
        rule: None,
        receipt_id: None,
        task_uuid: None,
        existing_task: None,
        status_pointer: None,
        enqueue: None,
        ingress: None,
        detail: None,
    }
}

fn filtered_gh_decision(
    producer: &str,
    observation: &GhObservation,
    receipt_id: String,
    rule: GhFilterReason,
) -> GhDecision {
    GhDecision {
        producer: producer.to_owned(),
        candidate: GhCandidateSummary::from_observation(observation),
        decision: GhDecisionStatus::Filtered,
        rule: Some(rule),
        receipt_id: Some(receipt_id),
        task_uuid: None,
        existing_task: None,
        status_pointer: None,
        enqueue: None,
        ingress: None,
        detail: None,
    }
}

fn duplicate_gh_decision(
    producer: &str,
    observation: &GhObservation,
    receipt_id: &str,
    task_uuid: Option<String>,
) -> GhDecision {
    let status_pointer = task_uuid.as_ref().map(|task| status_pointer(task));
    GhDecision {
        producer: producer.to_owned(),
        candidate: GhCandidateSummary::from_observation(observation),
        decision: GhDecisionStatus::Duplicate,
        rule: None,
        receipt_id: Some(receipt_id.to_owned()),
        task_uuid: task_uuid.clone(),
        existing_task: task_uuid,
        status_pointer,
        enqueue: None,
        ingress: None,
        detail: None,
    }
}

fn would_enqueue_gh_decision(
    producer: &str,
    config: &GhProducer,
    observation: &GhObservation,
    origin: GhOrigin,
    receipt_id: String,
    task_uuid: String,
    now: DateTime<Utc>,
) -> Result<GhDecision, ProducerError> {
    let enqueue = gh_enqueue_preview(config, origin, &task_uuid, now)?;
    Ok(GhDecision {
        producer: producer.to_owned(),
        candidate: GhCandidateSummary::from_observation(observation),
        decision: GhDecisionStatus::WouldEnqueue,
        rule: None,
        receipt_id: Some(receipt_id),
        task_uuid: Some(task_uuid.clone()),
        existing_task: None,
        status_pointer: Some(status_pointer(&task_uuid)),
        enqueue: Some(enqueue),
        ingress: None,
        detail: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn accepted_gh_decision(
    producer: &str,
    config: &GhProducer,
    observation: &GhObservation,
    origin: GhOrigin,
    receipt_id: String,
    task_uuid: String,
    ingress: Option<PathBuf>,
    now: DateTime<Utc>,
) -> Result<GhDecision, ProducerError> {
    let enqueue = gh_enqueue_preview(config, origin, &task_uuid, now)?;
    Ok(GhDecision {
        producer: producer.to_owned(),
        candidate: GhCandidateSummary::from_observation(observation),
        decision: GhDecisionStatus::Accepted,
        rule: None,
        receipt_id: Some(receipt_id),
        task_uuid: Some(task_uuid.clone()),
        existing_task: None,
        status_pointer: Some(status_pointer(&task_uuid)),
        enqueue: Some(enqueue),
        ingress,
        detail: None,
    })
}

fn gh_enqueue_preview(
    config: &GhProducer,
    origin: GhOrigin,
    task_uuid: &str,
    now: DateTime<Utc>,
) -> Result<GhEnqueuePreview, ProducerError> {
    let payload = config.enqueue.payload(
        EnqueueSource::Gh,
        Some(&origin.producer),
        now,
        Some(&origin),
    )?;
    Ok(GhEnqueuePreview {
        task_uuid: task_uuid.to_owned(),
        argv: payload
            .argv
            .expect("producer enqueue payloads always contain direct argv"),
        pools: payload
            .pools
            .expect("producer enqueue payloads always contain pools"),
        adapter: payload
            .adapter
            .expect("producer enqueue payloads always contain an adapter"),
        cwd: payload.cwd,
        workspace: payload.workspace,
        adapter_options: payload.adapter_options,
        gate_manifest: payload.gate_manifest,
        executor: payload.executor,
        priority: payload
            .priority
            .expect("producer enqueue payloads always contain a priority"),
        dedup_key: gh_trigger_dedup_key(&origin)?,
        context: origin,
    })
}

fn status_pointer(task_uuid: &str) -> String {
    format!("tally query log --task {task_uuid}")
}

fn primary_receipt_decision(
    producer: &str,
    config: &GhProducer,
    observation: &GhObservation,
    receipt: &GhTriggerReceipt,
    now: DateTime<Utc>,
) -> Result<GhDecision, ProducerError> {
    match receipt.primary_decision {
        GhDecisionStatus::Accepted => {
            let task_uuid = receipt.task_uuid.clone().ok_or_else(|| {
                ProducerError::InvalidObservation(
                    "accepted GitHub trigger receipt omitted taskUuid".to_owned(),
                )
            })?;
            accepted_gh_decision(
                producer,
                config,
                observation,
                gh_origin(producer, config, observation),
                receipt.receipt_id.clone(),
                task_uuid,
                None,
                now,
            )
        }
        GhDecisionStatus::Filtered => Ok(filtered_gh_decision(
            producer,
            observation,
            receipt.receipt_id.clone(),
            receipt.rule.ok_or_else(|| {
                ProducerError::InvalidObservation(
                    "filtered GitHub trigger receipt omitted its rule".to_owned(),
                )
            })?,
        )),
        _ => Err(ProducerError::InvalidObservation(
            "GitHub trigger receipt has an invalid primary decision".to_owned(),
        )),
    }
}

fn validate_receipt_identity(
    receipt: &GhTriggerReceipt,
    producer: &str,
    observation: &GhObservation,
    receipt_id: &str,
) -> Result<(), ProducerError> {
    if receipt.schema_version != 1
        || receipt.receipt_id != receipt_id
        || receipt.producer != producer
        || receipt.item_id != observation.node_id
        || receipt.comment_id != observation.comment_id
        || receipt.trigger_kind != observation.trigger_kind
        || receipt.trigger_actor != observation.trigger_actor
        || receipt.trigger_timestamp != observation.trigger_timestamp
        || receipt.trigger_value != observation.trigger_value
        || (matches!(observation.trigger_kind.as_str(), "assignment" | "label")
            && receipt.event_id != observation.event_id.as_deref().unwrap_or_default())
    {
        return Err(ProducerError::InvalidObservation(format!(
            "GitHub trigger receipt {receipt_id} does not match the observation"
        )));
    }
    Ok(())
}

fn acknowledgement_for_decision(
    decision: &GhDecision,
    observation: &GhObservation,
) -> Result<GhTriggerAcknowledgement, ProducerError> {
    let receipt_id = decision.receipt_id.clone().ok_or_else(|| {
        ProducerError::InvalidObservation(
            "acknowledgeable GitHub decision omitted receiptId".to_owned(),
        )
    })?;
    if !matches!(
        decision.decision,
        GhDecisionStatus::Accepted | GhDecisionStatus::Filtered | GhDecisionStatus::Duplicate
    ) {
        return Err(ProducerError::InvalidObservation(
            "only accepted, filtered, and duplicate GitHub decisions are acknowledged".to_owned(),
        ));
    }
    Ok(GhTriggerAcknowledgement {
        schema_version: 1,
        producer: decision.producer.clone(),
        receipt_id,
        item_id: observation.node_id.clone(),
        decision: decision.decision,
        rule: decision.rule,
        task_uuid: decision.task_uuid.clone(),
        status_pointer: decision.status_pointer.clone(),
    })
}

fn gh_origin(producer: &str, config: &GhProducer, observation: &GhObservation) -> GhOrigin {
    GhOrigin {
        schema_version: GH_ORIGIN_SCHEMA_VERSION,
        producer: producer.to_owned(),
        source: observation.source.clone(),
        repo: observation.repo.clone(),
        number: observation.number,
        html_url: observation.html_url.clone(),
        item_type: Some(observation.item_type),
        head_sha: observation.head_sha.clone(),
        node_id: observation.node_id.clone(),
        item_author: observation.item_author.clone(),
        trigger_actor: observation.trigger_actor.clone(),
        self_actor: observation.self_actor.clone(),
        notification_reason: observation.notification_reason.clone(),
        trigger_kind: observation.trigger_kind.clone(),
        event_id: observation.event_id.clone(),
        comment_id: observation.comment_id.clone(),
        trigger_timestamp: Some(observation.trigger_timestamp.clone()),
        trigger_value: observation.trigger_value.clone(),
        context: Some(observation.context.clone()),
        actor_exclude: config.actor_exclude.clone(),
        allow_self_triggered: config.allow_self_triggered,
        allowed_actors: config.allowed_actors.clone(),
    }
}

fn gh_observation(origin: &GhOrigin) -> Result<GhObservation, ProducerError> {
    Ok(GhObservation {
        source: origin.source.clone(),
        repo: origin.repo.clone(),
        number: origin.number,
        html_url: origin.html_url.clone(),
        item_type: origin.item_type.ok_or_else(|| {
            ProducerError::InvalidObservation("GitHub origin omitted itemType".to_owned())
        })?,
        head_sha: origin.head_sha.clone(),
        node_id: origin.node_id.clone(),
        item_author: origin.item_author.clone(),
        trigger_actor: origin.trigger_actor.clone(),
        self_actor: origin.self_actor.clone(),
        notification_reason: origin.notification_reason.clone(),
        trigger_kind: origin.trigger_kind.clone(),
        event_id: origin.event_id.clone(),
        comment_id: origin.comment_id.clone(),
        trigger_timestamp: origin.trigger_timestamp.clone().ok_or_else(|| {
            ProducerError::InvalidObservation("GitHub origin omitted triggerTimestamp".to_owned())
        })?,
        trigger_value: origin.trigger_value.clone(),
        context: origin.context.clone().ok_or_else(|| {
            ProducerError::InvalidObservation("GitHub origin omitted context".to_owned())
        })?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct GhCompletionMarker {
    completion_id: String,
    producer: String,
    source: String,
    item_id: String,
}

fn validate_gh_observation(
    producer: &str,
    config: &GhProducer,
    observation: &GhObservation,
) -> Result<(), ProducerError> {
    gh_origin(producer, config, observation)
        .validate()
        .map_err(|error| ProducerError::InvalidObservation(error.to_string()))?;
    Ok(())
}

fn gh_filter_reason(config: &GhProducer, observation: &GhObservation) -> Option<GhFilterReason> {
    if let Some(reason) = gh_source_filter_reason(config, observation) {
        return Some(reason);
    }
    if !gh_trigger_matches(&config.triggers, observation) {
        return Some(GhFilterReason::TriggerNotConfigured);
    }
    if !config.allowed_actors.is_empty()
        && !config
            .allowed_actors
            .iter()
            .any(|actor| actor.eq_ignore_ascii_case(&observation.trigger_actor))
    {
        return Some(GhFilterReason::TriggerActorNotAllowed);
    }
    if observation.trigger_actor == observation.self_actor && !config.allow_self_triggered {
        return Some(GhFilterReason::SelfTriggerDisabled);
    }
    (config.actor_exclude != "self"
        && observation
            .trigger_actor
            .eq_ignore_ascii_case(&config.actor_exclude))
    .then_some(GhFilterReason::TriggerActorExcluded)
}

fn gh_source_filter_reason(
    config: &GhProducer,
    observation: &GhObservation,
) -> Option<GhFilterReason> {
    let matching_kind = config
        .sources
        .iter()
        .filter(|source| source.kind() == observation.source)
        .collect::<Vec<_>>();
    if matching_kind.is_empty() {
        return Some(GhFilterReason::SourceNotConfigured);
    }
    let mut first_reason = None;
    for source in matching_kind {
        match gh_source_constraints_reason(source.constraints(), observation) {
            None => return None,
            Some(reason) if first_reason.is_none() => first_reason = Some(reason),
            Some(_) => {}
        }
    }
    first_reason
}

fn gh_source_constraints_reason(
    constraints: &GhSourceConstraints,
    observation: &GhObservation,
) -> Option<GhFilterReason> {
    if !constraints.has_identity_scope() {
        return Some(GhFilterReason::SourceUnconstrained);
    }
    let explicit_repositories = constraints
        .repo
        .iter()
        .chain(constraints.repositories.iter());
    let repo_allowed = explicit_repositories
        .clone()
        .any(|repo| repo.eq_ignore_ascii_case(&observation.repo));
    let owner = observation.repo.split_once('/').map(|(owner, _)| owner);
    let owner_allowed = owner.is_some_and(|owner| {
        constraints
            .owners
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(owner))
    });
    let item_allowed = constraints
        .item_allowlist
        .iter()
        .any(|item| item == &observation.html_url);
    if !repo_allowed && !owner_allowed && !item_allowed {
        return Some(GhFilterReason::RepositoryNotAllowed);
    }
    if !constraints.item_allowlist.is_empty() && !item_allowed {
        return Some(GhFilterReason::ItemNotAllowlisted);
    }
    if !constraints.labels.iter().all(|required| {
        observation
            .context
            .labels
            .iter()
            .any(|actual| actual.eq_ignore_ascii_case(required))
    }) {
        return Some(GhFilterReason::LabelMismatch);
    }
    if constraints.state.is_some() && constraints.state != observation.context.state {
        return Some(GhFilterReason::StateMismatch);
    }
    if constraints.assignee.as_ref().is_some_and(|required| {
        !observation
            .context
            .assignees
            .iter()
            .any(|actual| actual.eq_ignore_ascii_case(required))
    }) {
        return Some(GhFilterReason::AssigneeMismatch);
    }
    if !constraints.kinds.is_empty()
        && !constraints
            .kinds
            .iter()
            .any(|kind| kind.matches(observation.item_type))
    {
        return Some(GhFilterReason::ItemKindMismatch);
    }
    if !constraints.notification_reasons.is_empty()
        && observation
            .notification_reason
            .as_ref()
            .is_none_or(|reason| {
                !constraints
                    .notification_reasons
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(reason))
            })
    {
        return Some(GhFilterReason::NotificationReasonMismatch);
    }
    None
}

fn gh_trigger_matches(triggers: &GhTriggers, observation: &GhObservation) -> bool {
    match observation.trigger_kind.as_str() {
        "command-comment" => {
            observation
                .context
                .triggering_comment
                .as_ref()
                .is_some_and(|comment| {
                    triggers
                        .command_comments
                        .iter()
                        .any(|command| command == comment.body.trim())
                })
        }
        "mention" => observation
            .context
            .triggering_comment
            .as_ref()
            .is_some_and(|comment| {
                triggers
                    .mentions
                    .iter()
                    .any(|command| command == comment.body.trim())
            }),
        "assignment" => observation.trigger_value.as_ref().is_some_and(|assignee| {
            triggers
                .assignments
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(assignee))
        }),
        "label" => observation.trigger_value.as_ref().is_some_and(|label| {
            triggers
                .labels
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(label))
        }),
        _ => false,
    }
}

fn stable_key(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

fn validate_store_path(path: &Path) -> Result<String, ProducerError> {
    if !path.is_absolute() {
        return Err(ProducerError::InvalidObservation(format!(
            "build-effect store path {} is not absolute",
            path.display()
        )));
    }
    let components = path.components().collect::<Vec<_>>();
    if components.len() != 4
        || components[0] != Component::RootDir
        || components[1].as_os_str() != "nix"
        || components[2].as_os_str() != "store"
    {
        return Err(ProducerError::InvalidObservation(format!(
            "build-effect path {} is not one top-level /nix/store path",
            path.display()
        )));
    }
    let Some(name) = components[3].as_os_str().to_str() else {
        return Err(ProducerError::InvalidObservation(
            "build-effect store path must be valid UTF-8".to_owned(),
        ));
    };
    let Some((hash, output_name)) = name.split_once('-') else {
        return Err(ProducerError::InvalidObservation(format!(
            "build-effect store path {name:?} lacks a store hash"
        )));
    };
    if hash.len() != 32
        || !hash.bytes().all(|byte| {
            matches!(byte, b'0'..=b'9' | b'a'..=b'd' | b'f'..=b'n' | b'p'..=b's' | b'v'..=b'z')
        })
        || output_name.is_empty()
        || output_name.chars().any(char::is_control)
    {
        return Err(ProducerError::InvalidObservation(format!(
            "build-effect store path {name:?} has an invalid store name"
        )));
    }
    Ok(path.to_string_lossy().into_owned())
}

fn scan_store_paths(watch: BuildEffectWatch, path: &Path) -> Result<Vec<PathBuf>, ProducerError> {
    let mut paths = BTreeSet::new();
    match watch {
        BuildEffectWatch::GcRootsDir => {
            let mut entries = std::fs::read_dir(path)
                .map_err(|source| ProducerError::Io {
                    path: path.to_owned(),
                    source,
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| ProducerError::Io {
                    path: path.to_owned(),
                    source,
                })?;
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries {
                let entry_path = entry.path();
                let metadata =
                    std::fs::symlink_metadata(&entry_path).map_err(|source| ProducerError::Io {
                        path: entry_path.clone(),
                        source,
                    })?;
                let candidate = if metadata.file_type().is_symlink() {
                    let target =
                        std::fs::read_link(&entry_path).map_err(|source| ProducerError::Io {
                            path: entry_path.clone(),
                            source,
                        })?;
                    if target.is_absolute() {
                        target
                    } else {
                        path.join(target)
                    }
                } else {
                    continue;
                };
                let normalized = validate_store_path(&candidate)?;
                paths.insert(PathBuf::from(normalized));
            }
        }
        BuildEffectWatch::Jsonl => {
            let bytes = read_bounded_regular(path, MAX_INGRESS_BYTES)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                ProducerError::InvalidObservation(format!(
                    "build-effect JSONL {} is not UTF-8",
                    path.display()
                ))
            })?;
            for (index, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(line).map_err(|error| {
                    ProducerError::InvalidObservation(format!(
                        "build-effect JSONL {} line {} is invalid: {error}",
                        path.display(),
                        index + 1
                    ))
                })?;
                collect_json_store_paths(&value, &mut paths)?;
            }
        }
        BuildEffectWatch::PostBuildHook => {
            let bytes = read_bounded_regular(path, MAX_INGRESS_BYTES)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                ProducerError::InvalidObservation(format!(
                    "build-effect post-build-hook stream {} is not UTF-8",
                    path.display()
                ))
            })?;
            for candidate in text.split_ascii_whitespace() {
                paths.insert(PathBuf::from(validate_store_path(Path::new(candidate))?));
            }
        }
    }
    Ok(paths.into_iter().collect())
}

fn collect_json_store_paths(
    value: &Value,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<(), ProducerError> {
    let candidates = match value {
        Value::String(path) => vec![path.as_str()],
        Value::Object(object) => {
            if let Some(path) = object
                .get("storePath")
                .or_else(|| object.get("store_path"))
                .and_then(Value::as_str)
            {
                vec![path]
            } else if let Some(outputs) = object.get("outputs").and_then(Value::as_array) {
                outputs
                    .iter()
                    .map(|output| {
                        output.as_str().ok_or_else(|| {
                            ProducerError::InvalidObservation(
                                "build-effect JSONL outputs must contain only strings".to_owned(),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                return Err(ProducerError::InvalidObservation(
                    "build-effect JSONL object requires storePath, store_path, or outputs"
                        .to_owned(),
                ));
            }
        }
        _ => {
            return Err(ProducerError::InvalidObservation(
                "build-effect JSONL entry must be a string or object".to_owned(),
            ))
        }
    };
    for candidate in candidates {
        paths.insert(PathBuf::from(validate_store_path(Path::new(candidate))?));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressClaim {
    pub path: PathBuf,
    pub original_name: String,
    pub ingress_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngressOutcome {
    pub file: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_to: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

pub fn claim_ingress_files(events_dir: &Path) -> Result<Vec<IngressClaim>, ProducerError> {
    create_ingress_dirs(events_dir)?;
    let _ingress_lock = lock_ingress(events_dir)?;
    let processing = events_dir.join("processing");
    let mut claims = existing_claims(&processing)?;
    let mut candidates = std::fs::read_dir(events_dir)
        .map_err(|source| ProducerError::Io {
            path: events_dir.to_owned(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| ProducerError::Io {
            path: events_dir.to_owned(),
            source,
        })?;
    candidates.sort_by_key(std::fs::DirEntry::file_name);
    for entry in candidates {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !is_ingress_candidate(&name) {
            continue;
        }
        let source_path = entry.path();
        if name.len() > MAX_CLAIMABLE_NAME_BYTES {
            let rejected_base = events_dir
                .join("rejected")
                .join(format!("overlong-{}.json", stable_key(&[&name])));
            rename_unique(&source_path, &rejected_base)?;
            sync_directory(&events_dir.join("rejected"))?;
            sync_directory(events_dir)?;
            continue;
        }
        let metadata =
            std::fs::symlink_metadata(&source_path).map_err(|source| ProducerError::Io {
                path: source_path.clone(),
                source,
            })?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            continue;
        }
        let ingress_id = Uuid::new_v4().to_string();
        let claimed_name = format!("{ingress_id}--{name}");
        let claimed_path = processing.join(&claimed_name);
        match std::fs::rename(&source_path, &claimed_path) {
            Ok(()) => {
                sync_directory(&processing)?;
                sync_directory(events_dir)?;
                claims.push(IngressClaim {
                    path: claimed_path,
                    original_name: name,
                    ingress_id,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(ProducerError::Io {
                    path: source_path,
                    source,
                })
            }
        }
    }
    claims.sort_by(|left, right| {
        left.original_name
            .cmp(&right.original_name)
            .then_with(|| left.ingress_id.cmp(&right.ingress_id))
    });
    Ok(claims)
}

pub fn read_ingress_payload(claim: &IngressClaim) -> Result<EnqueuePayload, ProducerError> {
    let bytes = read_bounded_regular(&claim.path, MAX_INGRESS_BYTES)?;
    serde_json::from_slice(&bytes).map_err(ProducerError::Json)
}

pub fn acknowledged_ingress_ids(events_dir: &Path) -> Result<BTreeSet<String>, ProducerError> {
    Ok(read_acknowledged_events(events_dir)?
        .iter()
        .filter_map(|event| event.ingress_id.clone())
        .collect())
}

pub fn archive_ingress_claim(
    events_dir: &Path,
    claim: &IngressClaim,
    accepted: bool,
) -> Result<PathBuf, ProducerError> {
    let destination_dir = events_dir.join(if accepted { "done" } else { "rejected" });
    create_ingress_dirs(events_dir)?;
    let _ingress_lock = lock_ingress(events_dir)?;
    let destination = rename_unique(&claim.path, &destination_dir.join(&claim.original_name))?;
    sync_directory(&destination_dir)?;
    sync_directory(&events_dir.join("processing"))?;
    Ok(destination)
}

fn rename_unique(source: &Path, destination_base: &Path) -> Result<PathBuf, ProducerError> {
    let mut destination = destination_base.to_owned();
    let mut suffix = 1_u64;
    while !rename_noreplace(source, &destination)? {
        let file_name = destination_base
            .file_name()
            .ok_or_else(|| {
                ProducerError::InvalidObservation(format!(
                    "archive path {} has no file name",
                    destination_base.display()
                ))
            })?
            .to_string_lossy();
        destination = destination_base.with_file_name(format!("{file_name}.{suffix}"));
        suffix = suffix.checked_add(1).ok_or_else(|| {
            ProducerError::InvalidObservation("ingress archive suffix overflow".to_owned())
        })?;
    }
    Ok(destination)
}

fn rename_noreplace(source: &Path, destination: &Path) -> Result<bool, ProducerError> {
    let source_c = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        ProducerError::InvalidObservation(format!(
            "source path {} contains an interior NUL",
            source.display()
        ))
    })?;
    let destination_c = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        ProducerError::InvalidObservation(format!(
            "destination path {} contains an interior NUL",
            destination.display()
        ))
    })?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source_c.as_ptr(),
            libc::AT_FDCWD,
            destination_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Ok(true);
    }
    let source_error = std::io::Error::last_os_error();
    if source_error.raw_os_error() == Some(libc::EEXIST) {
        Ok(false)
    } else {
        Err(ProducerError::Io {
            path: source.to_owned(),
            source: source_error,
        })
    }
}

fn is_ingress_candidate(name: &str) -> bool {
    !name.starts_with('.') && name.ends_with(".json") && !name.ends_with(".enqueue.json")
}

fn existing_claims(processing: &Path) -> Result<Vec<IngressClaim>, ProducerError> {
    let mut claims = Vec::new();
    let mut entries = std::fs::read_dir(processing)
        .map_err(|source| ProducerError::Io {
            path: processing.to_owned(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| ProducerError::Io {
            path: processing.to_owned(),
            source,
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some((ingress_id, original_name)) = name.split_once("--") else {
            continue;
        };
        if Uuid::parse_str(ingress_id).is_err() || !is_ingress_candidate(original_name) {
            continue;
        }
        claims.push(IngressClaim {
            path: entry.path(),
            original_name: original_name.to_owned(),
            ingress_id: ingress_id.to_owned(),
        });
    }
    Ok(claims)
}

fn create_ingress_dirs(events_dir: &Path) -> Result<(), ProducerError> {
    create_dir_durable(events_dir)?;
    for name in ["processing", "done", "rejected"] {
        create_dir_durable(&events_dir.join(name))?;
    }
    Ok(())
}

fn lock_ingress(events_dir: &Path) -> Result<File, ProducerError> {
    create_dir_durable(events_dir)?;
    let path = events_dir.join(".producer-ingress.lock");
    let lock = open_private_rw(&path)?;
    lock.lock_exclusive()
        .map_err(|source| ProducerError::Io { path, source })?;
    Ok(lock)
}

fn ingress_name_exists(events_dir: &Path, name: &str) -> Result<bool, ProducerError> {
    for directory in ["", "done", "rejected"] {
        let path = if directory.is_empty() {
            events_dir.join(name)
        } else {
            events_dir.join(directory).join(name)
        };
        if path_lexists(&path)? {
            return Ok(true);
        }
    }
    let processing = events_dir.join("processing");
    if processing.exists() {
        for entry in std::fs::read_dir(&processing).map_err(|source| ProducerError::Io {
            path: processing.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| ProducerError::Io {
                path: processing.clone(),
                source,
            })?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|candidate| candidate.ends_with(&format!("--{name}")))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn path_lexists(path: &Path) -> Result<bool, ProducerError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(ProducerError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

fn create_dir_durable(path: &Path) -> Result<(), ProducerError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => return Ok(()),
        Ok(_) => {
            return Err(ProducerError::InvalidObservation(format!(
                "{} is not a real directory",
                path.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ProducerError::Io {
                path: path.to_owned(),
                source,
            })
        }
    }
    let parent = path.parent().ok_or_else(|| {
        ProducerError::InvalidObservation(format!("directory {} has no parent", path.display()))
    })?;
    create_dir_durable(parent)?;
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(path).map_err(|source| ProducerError::Io {
                path: path.to_owned(),
                source,
            })?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(ProducerError::InvalidObservation(format!(
                    "{} is not a real directory",
                    path.display()
                )));
            }
        }
        Err(source) => {
            return Err(ProducerError::Io {
                path: path.to_owned(),
                source,
            })
        }
    }
    sync_directory(parent)
}

fn write_new_atomic(path: &Path, bytes: &[u8]) -> Result<bool, ProducerError> {
    let parent = path.parent().ok_or_else(|| {
        ProducerError::InvalidObservation(format!("path {} has no parent", path.display()))
    })?;
    create_dir_durable(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            ProducerError::InvalidObservation(format!(
                "path {} has a non-Unicode file name",
                path.display()
            ))
        })?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|source| ProducerError::Io {
            path: temporary.clone(),
            source,
        })?;
    file.write_all(bytes).map_err(|source| ProducerError::Io {
        path: temporary.clone(),
        source,
    })?;
    file.write_all(b"\n").map_err(|source| ProducerError::Io {
        path: temporary.clone(),
        source,
    })?;
    file.sync_all().map_err(|source| ProducerError::Io {
        path: temporary.clone(),
        source,
    })?;
    let linked = match std::fs::hard_link(&temporary, path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(source) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(ProducerError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    std::fs::remove_file(&temporary).map_err(|source| ProducerError::Io {
        path: temporary,
        source,
    })?;
    sync_directory(parent)?;
    Ok(linked)
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), ProducerError> {
    let parent = path.parent().ok_or_else(|| {
        ProducerError::InvalidObservation(format!("path {} has no parent", path.display()))
    })?;
    create_dir_durable(parent)?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|source| ProducerError::Io {
            path: temporary.clone(),
            source,
        })?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n").map_err(|source| ProducerError::Io {
        path: temporary.clone(),
        source,
    })?;
    file.sync_all().map_err(|source| ProducerError::Io {
        path: temporary.clone(),
        source,
    })?;
    std::fs::rename(&temporary, path).map_err(|source| ProducerError::Io {
        path: path.to_owned(),
        source,
    })?;
    sync_directory(parent)
}

fn read_reachability_state(path: &Path) -> Result<ReachabilityState, ProducerError> {
    if !path.exists() {
        return Ok(ReachabilityState::default());
    }
    let bytes = read_bounded_regular(path, 64 * 1024)?;
    let state: ReachabilityState = serde_json::from_slice(&bytes)?;
    let candidate_is_coherent = matches!(
        (state.candidate_reachable, state.consecutive),
        (None, 0) | (Some(_), 1..)
    );
    let generation_is_coherent = matches!(
        (state.stable, state.generation % 2),
        (ReachabilityStable::Reachable, 0) | (ReachabilityStable::Lost, 1)
    );
    if !candidate_is_coherent
        || !generation_is_coherent
        || (state.probe_pool.is_none() && state.generation > 0)
        || state.notified_generation > state.generation
    {
        return Err(ProducerError::InvalidObservation(format!(
            "reachability state {} is internally inconsistent",
            path.display()
        )));
    }
    Ok(state)
}

fn validate_reachability_binding(
    state: &ReachabilityState,
    path: &Path,
    probe_pool: &str,
) -> Result<(), ProducerError> {
    if state.probe_pool.as_deref() == Some(probe_pool) {
        Ok(())
    } else {
        Err(ProducerError::InvalidObservation(format!(
            "reachability state {} is not bound to configured probePool {probe_pool:?}",
            path.display()
        )))
    }
}

fn read_bounded_regular(path: &Path, limit: u64) -> Result<Vec<u8>, ProducerError> {
    let preopen_metadata = std::fs::symlink_metadata(path).map_err(|source| ProducerError::Io {
        path: path.to_owned(),
        source,
    })?;
    if !preopen_metadata.is_file() || preopen_metadata.file_type().is_symlink() {
        return Err(ProducerError::InvalidObservation(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| {
            if source.raw_os_error() == Some(libc::ELOOP) {
                ProducerError::InvalidObservation(format!(
                    "{} is a symlink, not a regular file",
                    path.display()
                ))
            } else {
                ProducerError::Io {
                    path: path.to_owned(),
                    source,
                }
            }
        })?;
    let metadata = file.metadata().map_err(|source| ProducerError::Io {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(ProducerError::InvalidObservation(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > limit {
        return Err(ProducerError::InvalidObservation(format!(
            "{} exceeds the {} byte limit",
            path.display(),
            limit
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| ProducerError::Io {
            path: path.to_owned(),
            source,
        })?;
    if bytes.len() as u64 > limit {
        return Err(ProducerError::InvalidObservation(format!(
            "{} grew beyond the {} byte limit while reading",
            path.display(),
            limit
        )));
    }
    Ok(bytes)
}

fn open_private_rw(path: &Path) -> Result<File, ProducerError> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| ProducerError::Io {
            path: path.to_owned(),
            source,
        })?;
    if !file
        .metadata()
        .map_err(|source| ProducerError::Io {
            path: path.to_owned(),
            source,
        })?
        .is_file()
    {
        return Err(ProducerError::InvalidObservation(format!(
            "{} is not a regular lock file",
            path.display()
        )));
    }
    Ok(file)
}

fn sync_directory(path: &Path) -> Result<(), ProducerError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| ProducerError::Io {
            path: path.to_owned(),
            source,
        })
}

#[derive(Debug, Error)]
pub enum ProducerError {
    #[error("invalid producer configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid producer observation: {0}")]
    InvalidObservation(String),
    #[error("unknown producer {0:?}")]
    UnknownProducer(String),
    #[error("producer {producer:?} has kind {actual:?}, expected {expected:?}")]
    KindMismatch {
        producer: String,
        expected: String,
        actual: String,
    },
    #[error("producer I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("producer JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("durable event error: {0}")]
    DurableEvent(#[from] crate::taskdb::TaskDbError),
    #[error("GitHub COMPLETED mutation failed: {0}")]
    Mutation(String),
    #[error("GitHub trigger acknowledgement failed: {0}")]
    Acknowledgement(String),
    #[error("GitHub intake failed: {0}")]
    GitHub(String),
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Barrier};

    use chrono::TimeZone;
    use tempfile::tempdir;

    use super::*;

    const STORE_A: &str = "/nix/store/00000000000000000000000000000000-output-a";
    const STORE_B: &str = "/nix/store/11111111111111111111111111111111-output-b";

    fn enqueue(command: &str) -> ProducerEnqueue {
        ProducerEnqueue {
            argv: vec![command.to_owned()],
            adapter: "shell".to_owned(),
            cwd: None,
            workspace: None,
            adapter_options: AdapterJobOptions::default(),
            gate_manifest: None,
            pools: vec!["slot".to_owned()],
            executor: None,
            priority: Priority::Low,
            dedup_key: None,
            evidence: vec!["exit:0".to_owned()],
            evidence_class: None,
            manifest_hash: None,
            consumption_estimate: None,
            runtime_max_sec: None,
            no_enqueue: false,
            credentials: BTreeMap::new(),
        }
    }

    fn registry(watch_path: &Path) -> BTreeMap<String, ProducerConfig> {
        let mut attest = enqueue("assess-return");
        attest.no_enqueue = true;
        BTreeMap::from([
            (
                "daily".to_owned(),
                ProducerConfig::Calendar(CalendarProducer {
                    credentials: BTreeMap::new(),
                    on_calendar: "daily".to_owned(),
                    enqueue: ProducerEnqueue {
                        dedup_key: Some("daily-%Y%m%d".to_owned()),
                        ..enqueue("calendar-job")
                    },
                }),
            ),
            (
                "drop".to_owned(),
                ProducerConfig::EventsDir(EventsDirProducer {
                    credentials: BTreeMap::new(),
                    poll_interval_sec: 60,
                }),
            ),
            (
                "github".to_owned(),
                ProducerConfig::Gh(GhProducer {
                    credentials: BTreeMap::new(),
                    enable: true,
                    sources: vec![
                        GhSource::Notifications(GhSourceConstraints {
                            repo: Some("acme/widgets".to_owned()),
                            ..GhSourceConstraints::default()
                        }),
                        GhSource::Search(GhSourceConstraints {
                            repo: Some("acme/widgets".to_owned()),
                            ..GhSourceConstraints::default()
                        }),
                    ],
                    triggers: GhTriggers {
                        mentions: vec!["@tally-bot please run".to_owned()],
                        assignments: vec!["tally-bot".to_owned()],
                        ..GhTriggers::default()
                    },
                    actor_exclude: "self".to_owned(),
                    allow_self_triggered: false,
                    allowed_actors: Vec::new(),
                    poll_interval_sec: 60,
                    post_receipt: true,
                    post_evidence: true,
                    post_gate_summary: false,
                    request_review: false,
                    close_on_acceptance: false,
                    never_mutate: false,
                    close_on_pass: Some(true),
                    enqueue: enqueue("gh-job"),
                }),
            ),
            (
                "effects".to_owned(),
                ProducerConfig::BuildEffect(BuildEffectProducer {
                    credentials: BTreeMap::new(),
                    watch: BuildEffectWatch::Jsonl,
                    path: watch_path.to_owned(),
                    on_key: enqueue("effect-job"),
                }),
            ),
            (
                "health".to_owned(),
                ProducerConfig::PoolReachability(Box::new(PoolReachabilityProducer {
                    credentials: BTreeMap::new(),
                    probe_pool: "slot".to_owned(),
                    interval_sec: 30,
                    hysteresis: 3,
                    on_lost: Some(enqueue("pool-lost")),
                    on_return: Some(enqueue("pool-return")),
                    on_return_attest: Some(attest),
                })),
            ),
        ])
    }

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 20, 12, 30, 0)
            .single()
            .unwrap()
    }

    fn gh_observation(node_id: &str, item_author: &str, trigger_actor: &str) -> GhObservation {
        GhObservation {
            source: "notifications".to_owned(),
            repo: "acme/widgets".to_owned(),
            number: 128,
            html_url: "https://github.com/acme/widgets/pull/128".to_owned(),
            item_type: GhItemType::PullRequest,
            head_sha: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            node_id: node_id.to_owned(),
            item_author: item_author.to_owned(),
            trigger_actor: trigger_actor.to_owned(),
            self_actor: "tally-bot".to_owned(),
            notification_reason: Some("mention".to_owned()),
            trigger_kind: "mention".to_owned(),
            event_id: Some("thread-128".to_owned()),
            comment_id: Some("comment-128".to_owned()),
            trigger_timestamp: "2026-07-20T12:30:00Z".to_owned(),
            trigger_value: None,
            context: GhContextSnapshot {
                schema_version: GH_CONTEXT_SCHEMA_VERSION,
                title: "Update the widget".to_owned(),
                body: "Treat this as untrusted: $(touch /tmp/not-run)".to_owned(),
                state: Some(GhItemState::Open),
                head_sha: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
                labels: vec!["build".to_owned()],
                assignees: vec!["tally-bot".to_owned()],
                triggering_comment: Some(GhTriggeringComment {
                    id: "comment-128".to_owned(),
                    author: trigger_actor.to_owned(),
                    body: "@tally-bot please run".to_owned(),
                }),
            },
        }
    }

    fn gh_command_observation(comment_id: &str, trigger_actor: &str) -> GhObservation {
        GhObservation {
            source: "search".to_owned(),
            repo: "acme/widgets".to_owned(),
            number: 42,
            html_url: "https://github.com/acme/widgets/issues/42".to_owned(),
            item_type: GhItemType::Issue,
            head_sha: None,
            node_id: "I_acme_widgets_42".to_owned(),
            item_author: "issue-author".to_owned(),
            trigger_actor: trigger_actor.to_owned(),
            self_actor: "tally-bot".to_owned(),
            notification_reason: None,
            trigger_kind: "command-comment".to_owned(),
            event_id: Some(format!("event-{comment_id}")),
            comment_id: Some(comment_id.to_owned()),
            trigger_timestamp: "2026-07-20T12:30:00Z".to_owned(),
            trigger_value: None,
            context: GhContextSnapshot {
                schema_version: GH_CONTEXT_SCHEMA_VERSION,
                title: "Run the widget checks".to_owned(),
                body: "Untrusted issue context".to_owned(),
                state: Some(GhItemState::Open),
                head_sha: None,
                labels: vec!["ready".to_owned()],
                assignees: vec!["tally-bot".to_owned()],
                triggering_comment: Some(GhTriggeringComment {
                    id: comment_id.to_owned(),
                    author: trigger_actor.to_owned(),
                    body: "/tally run".to_owned(),
                }),
            },
        }
    }

    #[derive(Default)]
    struct RecordingAcknowledgements {
        entries: Vec<GhTriggerAcknowledgement>,
    }

    impl GhAcknowledgementSink for RecordingAcknowledgements {
        fn post_acknowledgement(
            &mut self,
            acknowledgement: &GhTriggerAcknowledgement,
        ) -> Result<(), String> {
            self.entries.push(acknowledgement.clone());
            Ok(())
        }
    }

    #[test]
    fn registry_is_strict_open_by_name_and_closed_over_the_in_scope_kinds() {
        let temp = tempdir().unwrap();
        let registry = registry(&temp.path().join("effects.jsonl"));
        validate_registry(
            &registry,
            &BTreeSet::from(["slot".to_owned()]),
            &BTreeSet::from(["shell".to_owned()]),
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(
            registry
                .values()
                .map(ProducerConfig::kind)
                .collect::<BTreeSet<_>>(),
            IN_SCOPE_PRODUCER_KINDS
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
        );

        assert!(serde_json::from_value::<ProducerConfig>(serde_json::json!({
            "kind": "r2",
            "enqueue": {"argv": ["x"], "pool": "slot"}
        }))
        .is_err());
        assert!(serde_json::from_value::<ProducerConfig>(serde_json::json!({
            "kind": "calendar",
            "onCalendar": "daily",
            "pool": "producer-owned-is-forbidden",
            "enqueue": {"argv": ["x"], "pool": "slot"}
        }))
        .is_err());
        assert!(matches!(
            serde_json::from_value::<ProducerObservation>(serde_json::json!({
                "kind": "gh",
                "source": "notifications",
                "nodeId": "PR-1",
                "triggerActor": "contributor",
                "selfActor": "tally-bot"
            }))
            .unwrap(),
            ProducerObservation::Gh(observation)
                if observation.node_id.as_deref() == Some("PR-1")
                    && observation.self_actor.as_deref() == Some("tally-bot")
        ));

        let mut invalid_attest = registry.clone();
        let ProducerConfig::PoolReachability(health) = invalid_attest.get_mut("health").unwrap()
        else {
            unreachable!()
        };
        health.on_return_attest.as_mut().unwrap().no_enqueue = false;
        assert!(validate_registry(
            &invalid_attest,
            &BTreeSet::from(["slot".to_owned()]),
            &BTreeSet::from(["shell".to_owned()]),
            &BTreeSet::new(),
        )
        .unwrap_err()
        .to_string()
        .contains("noEnqueue=true"));

        let mut duplicate_reachability = registry.clone();
        let duplicate = duplicate_reachability.get("health").unwrap().clone();
        duplicate_reachability.insert("health-backup".to_owned(), duplicate);
        assert!(validate_registry(
            &duplicate_reachability,
            &BTreeSet::from(["slot".to_owned()]),
            &BTreeSet::from(["shell".to_owned()]),
            &BTreeSet::new(),
        )
        .unwrap_err()
        .to_string()
        .contains("both own probePool"));

        for invalid_name in [".hidden", "-option"] {
            let mut invalid_names = registry.clone();
            invalid_names.insert(
                invalid_name.to_owned(),
                invalid_names.get("daily").unwrap().clone(),
            );
            assert!(validate_registry(
                &invalid_names,
                &BTreeSet::from(["slot".to_owned()]),
                &BTreeSet::from(["shell".to_owned()]),
                &BTreeSet::new(),
            )
            .unwrap_err()
            .to_string()
            .contains("invalid producer configuration"));
        }

        let mut relative_credential = registry;
        let ProducerConfig::Calendar(calendar) = relative_credential.get_mut("daily").unwrap()
        else {
            unreachable!()
        };
        calendar
            .enqueue
            .credentials
            .insert("token".to_owned(), PathBuf::from("relative/token"));
        assert!(validate_registry(
            &relative_credential,
            &BTreeSet::from(["slot".to_owned()]),
            &BTreeSet::from(["shell".to_owned()]),
            &BTreeSet::new(),
        )
        .unwrap_err()
        .to_string()
        .contains("must be absolute"));

        let mut invalid_strftime = relative_credential;
        let ProducerConfig::Calendar(calendar) = invalid_strftime.get_mut("daily").unwrap() else {
            unreachable!()
        };
        calendar.enqueue.credentials.clear();
        calendar.enqueue.dedup_key = Some("daily-%Q".to_owned());
        assert!(validate_registry(
            &invalid_strftime,
            &BTreeSet::from(["slot".to_owned()]),
            &BTreeSet::from(["shell".to_owned()]),
            &BTreeSet::new(),
        )
        .unwrap_err()
        .to_string()
        .contains("strftime"));

        let mut invalid_close = invalid_strftime;
        let ProducerConfig::Calendar(calendar) = invalid_close.get_mut("daily").unwrap() else {
            unreachable!()
        };
        calendar.enqueue.dedup_key = None;
        let ProducerConfig::Gh(github) = invalid_close.get_mut("github").unwrap() else {
            unreachable!()
        };
        github.post_evidence = false;
        github.close_on_pass = Some(true);
        assert!(validate_registry(
            &invalid_close,
            &BTreeSet::from(["slot".to_owned()]),
            &BTreeSet::from(["shell".to_owned()]),
            &BTreeSet::new(),
        )
        .unwrap_err()
        .to_string()
        .contains("closeOnPass=true requires postEvidence=true"));
    }

    #[test]
    fn serialized_github_config_preserves_legacy_close_and_accepts_explicit_comment_only() {
        let config = |close_on_pass: Option<bool>| {
            let mut value = serde_json::json!({
                "kind": "gh",
                "enable": true,
                "sources": [{"notifications": {"repo": "acme/widgets"}}],
                "triggers": {"assignments": ["tally-bot"]},
                "postEvidence": true,
                "enqueue": {"argv": ["gh-job"], "pool": "slot"}
            });
            if let Some(close_on_pass) = close_on_pass {
                value["closeOnPass"] = Value::Bool(close_on_pass);
            }
            let ProducerConfig::Gh(config) =
                serde_json::from_value::<ProducerConfig>(value).unwrap()
            else {
                unreachable!()
            };
            config
        };

        let legacy = config(None);
        assert_eq!(legacy.close_on_pass, None);
        assert!(legacy.close_on_pass());
        let comment_only = config(Some(false));
        assert_eq!(comment_only.close_on_pass, Some(false));
        assert!(!comment_only.close_on_pass());
    }

    #[test]
    fn github_search_queries_are_derived_only_from_declared_scopes() {
        let scoped = GhSourceConstraints {
            repo: Some("agency-agency/spec".to_owned()),
            labels: vec!["agency:codex-ready".to_owned()],
            state: Some(GhItemState::Open),
            assignee: Some("tally-bot".to_owned()),
            kinds: vec![GhSourceItemKind::Issue],
            query: Some("draft:false".to_owned()),
            ..GhSourceConstraints::default()
        };
        assert_eq!(
            gh_search_queries(&scoped),
            ["repo:agency-agency/spec label:\"agency:codex-ready\" state:open assignee:\"tally-bot\" is:issue draft:false"]
        );

        let query_without_identity = GhSourceConstraints {
            query: Some("state:open".to_owned()),
            ..GhSourceConstraints::default()
        };
        assert!(gh_search_queries(&query_without_identity).is_empty());
    }

    #[test]
    fn github_explicit_comment_assignment_and_label_triggers_are_classified_exactly() {
        let triggers = GhTriggers {
            command_comments: vec!["/tally run".to_owned()],
            mentions: vec!["@tally-bot run".to_owned()],
            assignments: vec!["tally-bot".to_owned()],
            labels: vec!["tally:run".to_owned()],
        };
        let command = gh_command_observation("command", "maintainer");
        assert!(gh_trigger_matches(&triggers, &command));

        let mut mention = command.clone();
        mention.trigger_kind = "mention".to_owned();
        mention.context.triggering_comment.as_mut().unwrap().body = "@tally-bot run".to_owned();
        assert!(gh_trigger_matches(&triggers, &mention));

        let assignment = configured_gh_event(
            &serde_json::json!({
                "id": 42,
                "event": "assigned",
                "actor": {"login": "maintainer"},
                "assignee": {"login": "tally-bot"}
            }),
            &triggers,
        )
        .unwrap();
        assert_eq!(assignment.id, "42");
        assert_eq!(assignment.kind, "assignment");
        assert_eq!(assignment.actor, "maintainer");
        assert_eq!(assignment.value, "tally-bot");

        let label = configured_gh_event(
            &serde_json::json!({
                "node_id": "LE_label_43",
                "event": "labeled",
                "actor": {"login": "maintainer"},
                "label": {"name": "tally:run"}
            }),
            &triggers,
        )
        .unwrap();
        assert_eq!(label.id, "LE_label_43");
        assert_eq!(label.kind, "label");
        assert_eq!(label.value, "tally:run");

        assert!(configured_gh_event(
            &serde_json::json!({
                "id": 44,
                "event": "labeled",
                "actor": {"login": "maintainer"},
                "label": {"name": "unconfigured"}
            }),
            &triggers,
        )
        .is_none());
    }

    #[test]
    fn github_remaining_source_constraints_are_fail_closed() {
        let constraints = GhSourceConstraints {
            owners: vec!["acme".to_owned()],
            assignee: Some("tally-bot".to_owned()),
            kinds: vec![GhSourceItemKind::Issue],
            notification_reasons: vec!["mention".to_owned()],
            item_allowlist: vec!["https://github.com/acme/widgets/issues/42".to_owned()],
            ..GhSourceConstraints::default()
        };
        let mut matching = gh_command_observation("constraints", "maintainer");
        matching.source = "notifications".to_owned();
        matching.notification_reason = Some("mention".to_owned());
        assert_eq!(gh_source_constraints_reason(&constraints, &matching), None);

        let mut wrong_item = matching.clone();
        wrong_item.html_url = "https://github.com/acme/widgets/issues/43".to_owned();
        assert_eq!(
            gh_source_constraints_reason(&constraints, &wrong_item),
            Some(GhFilterReason::ItemNotAllowlisted)
        );
        let mut wrong_assignee = matching.clone();
        wrong_assignee.context.assignees.clear();
        assert_eq!(
            gh_source_constraints_reason(&constraints, &wrong_assignee),
            Some(GhFilterReason::AssigneeMismatch)
        );
        let mut wrong_kind = matching.clone();
        wrong_kind.item_type = GhItemType::PullRequest;
        assert_eq!(
            gh_source_constraints_reason(&constraints, &wrong_kind),
            Some(GhFilterReason::ItemKindMismatch)
        );
        let mut wrong_reason = matching;
        wrong_reason.notification_reason = Some("subscribed".to_owned());
        assert_eq!(
            gh_source_constraints_reason(&constraints, &wrong_reason),
            Some(GhFilterReason::NotificationReasonMismatch)
        );
    }

    #[test]
    fn producer_multi_pool_validation_rejects_empty_duplicate_and_unknown_sets() {
        let temp = tempdir().unwrap();
        let error_for = |requested: Vec<String>| {
            let mut registry = registry(&temp.path().join("effects.jsonl"));
            let ProducerConfig::Calendar(calendar) = registry.get_mut("daily").unwrap() else {
                unreachable!()
            };
            calendar.enqueue.pools = requested;
            validate_registry(
                &registry,
                &BTreeSet::from(["slot".to_owned()]),
                &BTreeSet::from(["shell".to_owned()]),
                &BTreeSet::new(),
            )
            .unwrap_err()
            .to_string()
        };

        assert!(error_for(Vec::new()).contains("at least one"));
        assert!(error_for(vec!["slot".to_owned(), "slot".to_owned()]).contains("duplicate"));
        assert!(error_for(vec!["slot".to_owned(), "missing".to_owned()])
            .contains("references unknown pool \"missing\""));
    }

    #[test]
    fn calendar_emits_a_direct_payload_with_strftime_dedup_and_credentials() {
        let temp = tempdir().unwrap();
        let mut registry = registry(&temp.path().join("effects.jsonl"));
        let ProducerConfig::Calendar(calendar) = registry.get_mut("daily").unwrap() else {
            unreachable!()
        };
        calendar.enqueue.credentials.insert(
            "token".to_owned(),
            PathBuf::from("/run/credentials/calendar-token"),
        );
        let engine = ProducerEngine::new(
            &registry,
            temp.path().join("events"),
            temp.path().join("state"),
        );
        let EmitOutcome::Emitted(path) = engine.emit_calendar("daily", fixed_now()).unwrap() else {
            panic!("calendar did not emit")
        };
        let payload: EnqueuePayload =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(payload.source, Some(EnqueueSource::Calendar));
        assert_eq!(
            payload.pools.as_deref(),
            Some(["slot".to_owned()].as_slice())
        );
        assert_eq!(payload.adapter.as_deref(), Some("shell"));
        assert_eq!(payload.dedup_key.as_deref(), Some("daily-20260720"));
        assert_eq!(
            payload.credentials["token"],
            PathBuf::from("/run/credentials/calendar-token")
        );
    }

    #[test]
    fn github_origin_templates_render_into_literal_argv_and_cwd_without_a_shell() {
        let temp = tempdir().unwrap();
        let marker = temp.path().join("must-not-exist");
        let mut registry = registry(&temp.path().join("effects.jsonl"));
        let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
            unreachable!()
        };
        github.enqueue.argv = vec![
            "review".to_owned(),
            "${gh.url}".to_owned(),
            "${gh.headSha}".to_owned(),
            format!("$(touch {})", marker.display()),
        ];
        github.enqueue.cwd = Some(PathBuf::from("/worktrees/${repoName}"));
        validate_registry(
            &registry,
            &BTreeSet::from(["slot".to_owned()]),
            &BTreeSet::from(["shell".to_owned()]),
            &BTreeSet::new(),
        )
        .unwrap();

        let engine = ProducerEngine::new(
            &registry,
            temp.path().join("events"),
            temp.path().join("state"),
        );
        let observation = gh_observation("PR_template", "author", "contributor");
        let EmitOutcome::Emitted(path) =
            engine.emit_gh("github", &observation, fixed_now()).unwrap()
        else {
            panic!("GitHub observation did not emit")
        };
        let payload: EnqueuePayload =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(
            payload.argv.unwrap(),
            vec![
                "review".to_owned(),
                "https://github.com/acme/widgets/pull/128".to_owned(),
                "0123456789abcdef0123456789abcdef01234567".to_owned(),
                format!("$(touch {})", marker.display()),
            ]
        );
        assert_eq!(
            payload.cwd.as_deref(),
            Some(Path::new("/worktrees/widgets"))
        );
        assert!(!marker.exists());

        let mut unknown = registry.clone();
        let ProducerConfig::Gh(github) = unknown.get_mut("github").unwrap() else {
            unreachable!()
        };
        github.enqueue.argv = vec!["${gh.body}".to_owned()];
        assert!(validate_registry(
            &unknown,
            &BTreeSet::from(["slot".to_owned()]),
            &BTreeSet::from(["shell".to_owned()]),
            &BTreeSet::new(),
        )
        .unwrap_err()
        .to_string()
        .contains("unknown placeholder"));

        let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
            unreachable!()
        };
        github
            .triggers
            .command_comments
            .push("/tally run".to_owned());
        let issue = gh_command_observation("missing-head", "contributor");
        let missing_engine = ProducerEngine::new(
            &registry,
            temp.path().join("missing-events"),
            temp.path().join("missing-state"),
        );
        let error = missing_engine
            .emit_gh("github", &issue, fixed_now())
            .unwrap_err()
            .to_string();
        assert!(error.contains("gh.headSha"), "{error}");
    }

    struct RecordingMutation {
        comments: Vec<GhCompletedMutation>,
        closes: Vec<GhCompletedMutation>,
        item_open: bool,
    }

    impl Default for RecordingMutation {
        fn default() -> Self {
            Self {
                comments: Vec::new(),
                closes: Vec::new(),
                item_open: true,
            }
        }
    }

    impl GhMutationSink for RecordingMutation {
        fn post_evidence(&mut self, mutation: &GhCompletedMutation) -> Result<(), String> {
            self.comments.push(mutation.clone());
            Ok(())
        }

        fn close_item(&mut self, mutation: &GhCompletedMutation) -> Result<(), String> {
            self.closes.push(mutation.clone());
            self.item_open = false;
            Ok(())
        }
    }

    fn semantic_completion(
        gate_status: GateSummaryStatus,
        acceptance_status: AcceptanceStatus,
    ) -> SemanticCompletion {
        SemanticCompletion {
            schema_version: crate::completion::GATE_MANIFEST_SCHEMA_VERSION,
            execution: crate::completion::ExecutionFact::exited(0),
            gates: GateSummary {
                status: gate_status,
                artifact: Some(serde_json::json!({"commit": "abc"})),
                gates: Vec::new(),
                missing_required_gate_ids: if gate_status == GateSummaryStatus::Fail {
                    vec!["live".to_owned()]
                } else {
                    Vec::new()
                },
                manifest_error: None,
            },
            acceptance: AcceptanceFact {
                status: acceptance_status,
                policy: crate::completion::AcceptancePolicy::ExecutionAndGates,
                reason: "test policy result".to_owned(),
            },
        }
    }

    #[test]
    fn github_gate_failure_and_not_run_remain_open_and_never_mutate_wins() {
        let temp = tempdir().unwrap();
        let mut registry = registry(&temp.path().join("effects.jsonl"));
        let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
            unreachable!()
        };
        github.post_gate_summary = true;
        github.request_review = true;
        github.close_on_acceptance = true;
        github.close_on_pass = Some(true);
        let observation = gh_observation("PR_policy", "author", "contributor");
        let origin = gh_origin("github", github, &observation);
        let engine = ProducerEngine::new(
            &registry,
            temp.path().join("events"),
            temp.path().join("state"),
        );

        for completion in [
            semantic_completion(GateSummaryStatus::Fail, AcceptanceStatus::Rejected),
            semantic_completion(GateSummaryStatus::NotRun, AcceptanceStatus::Pending),
        ] {
            let mut sink = RecordingMutation::default();
            assert!(engine
                .complete_gh_with_completion(
                    &origin,
                    Verdict::Pass,
                    Some(serde_json::json!({"witnessSeq": 28})),
                    Some(completion),
                    &mut sink,
                )
                .unwrap());
            assert_eq!(sink.comments.len(), 1);
            assert!(sink.comments[0].request_review);
            assert!(sink.closes.is_empty());
            assert!(sink.item_open);
        }

        let mut accepted_sink = RecordingMutation::default();
        assert!(engine
            .complete_gh_with_completion(
                &origin,
                Verdict::Pass,
                Some(serde_json::json!({"witnessSeq": 29})),
                Some(semantic_completion(
                    GateSummaryStatus::Pass,
                    AcceptanceStatus::Accepted,
                )),
                &mut accepted_sink,
            )
            .unwrap());
        assert_eq!(accepted_sink.closes.len(), 1);
        assert!(!accepted_sink.comments[0].request_review);

        let mut inert_registry = registry;
        let ProducerConfig::Gh(github) = inert_registry.get_mut("github").unwrap() else {
            unreachable!()
        };
        github.never_mutate = true;
        let inert_engine = ProducerEngine::new(
            &inert_registry,
            temp.path().join("inert-events"),
            temp.path().join("inert-state"),
        );
        let mut inert_sink = RecordingMutation::default();
        assert!(!inert_engine
            .complete_gh_with_completion(
                &origin,
                Verdict::Pass,
                None,
                Some(semantic_completion(
                    GateSummaryStatus::Pass,
                    AcceptanceStatus::Accepted,
                )),
                &mut inert_sink,
            )
            .unwrap());
        assert!(inert_sink.comments.is_empty());
        assert!(inert_sink.closes.is_empty());
    }

    #[test]
    fn github_enforces_sources_trigger_actor_policy_and_completion_mutations() {
        let temp = tempdir().unwrap();
        let registry = registry(&temp.path().join("effects.jsonl"));
        let engine = ProducerEngine::new(
            &registry,
            temp.path().join("events"),
            temp.path().join("state"),
        );
        let external = gh_observation("PR_kwABC128", "issue-author", "contributor");
        let EmitOutcome::Emitted(path) = engine.emit_gh("github", &external, fixed_now()).unwrap()
        else {
            panic!("GitHub observation did not emit")
        };
        let payload: EnqueuePayload =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(payload.source, Some(EnqueueSource::Gh));
        assert_eq!(payload.gh_trigger_actor.as_deref(), Some("contributor"));
        assert_eq!(payload.gh_self_actor.as_deref(), Some("tally-bot"));
        let origin = payload.gh_origin.clone().unwrap();
        assert_eq!(
            payload.dedup_key.as_deref(),
            Some(gh_trigger_dedup_key(&origin).unwrap().as_str())
        );
        assert_eq!(origin.producer, "github");
        assert_eq!(origin.source, "notifications");
        assert_eq!(origin.node_id, "PR_kwABC128");
        assert_eq!(origin.item_author, "issue-author");
        assert_eq!(origin.trigger_actor, "contributor");
        assert_eq!(
            engine.emit_gh("github", &external, fixed_now()).unwrap(),
            EmitOutcome::Duplicate
        );

        let own = GhObservation {
            trigger_actor: "tally-bot".to_owned(),
            context: GhContextSnapshot {
                triggering_comment: Some(GhTriggeringComment {
                    author: "tally-bot".to_owned(),
                    ..external.context.triggering_comment.clone().unwrap()
                }),
                ..external.context.clone()
            },
            ..external.clone()
        };
        assert_eq!(
            engine.emit_gh("github", &own, fixed_now()).unwrap(),
            EmitOutcome::Filtered {
                reason: GhFilterReason::SelfTriggerDisabled
            }
        );
        let wrong_source = GhObservation {
            source: "unconfigured".to_owned(),
            ..external.clone()
        };
        assert_eq!(
            engine
                .emit_gh("github", &wrong_source, fixed_now())
                .unwrap(),
            EmitOutcome::Filtered {
                reason: GhFilterReason::SourceNotConfigured
            }
        );

        let mut mutations = RecordingMutation::default();
        assert!(!engine
            .complete_gh(&origin, Verdict::Failed, None, &mut mutations,)
            .unwrap());
        assert!(engine
            .complete_gh(
                &origin,
                Verdict::Pass,
                Some(serde_json::json!({"witnessSeq": 4})),
                &mut mutations,
            )
            .unwrap());
        assert_eq!(mutations.comments.len(), 1);
        assert_eq!(mutations.closes.len(), 1);
        assert!(!mutations.item_open);
        assert_eq!(mutations.comments[0].state, "COMPLETED");
        assert_eq!(mutations.comments[0].source, "notifications");
        assert_eq!(mutations.comments[0].item_id, "PR_kwABC128");
        assert_eq!(
            mutations.comments[0].evidence.as_ref().unwrap()["witnessSeq"],
            4
        );

        let mut comment_only_registry = registry.clone();
        let ProducerConfig::Gh(comment_only) = comment_only_registry.get_mut("github").unwrap()
        else {
            unreachable!()
        };
        comment_only.close_on_pass = Some(false);
        let comment_only_engine = ProducerEngine::new(
            &comment_only_registry,
            temp.path().join("comment-only-events"),
            temp.path().join("comment-only-state"),
        );
        let mut comment_only_sink = RecordingMutation::default();
        assert!(comment_only_engine
            .complete_gh(
                &origin,
                Verdict::Pass,
                Some(serde_json::json!({"witnessSeq": 5})),
                &mut comment_only_sink,
            )
            .unwrap());
        assert_eq!(comment_only_sink.comments.len(), 1);
        assert!(comment_only_sink.closes.is_empty());
        assert!(comment_only_sink.item_open);

        let gh = temp.path().join("fake-gh");
        let requests = temp.path().join("gh-requests.jsonl");
        let calls = temp.path().join("gh-calls");
        let commented = temp.path().join("gh-commented");
        let failed_close = temp.path().join("gh-failed-close");
        let completion_id = "task-1:attempt-1:witness-5";
        let remote_key = stable_key(&["gh-remote-completion", completion_id]);
        std::fs::write(
            &gh,
            format!(
                concat!(
                    "#!/bin/sh\n",
                    "[ \"$1 $2 $3 $4\" = 'api graphql --input -' ] || exit 91\n",
                    "request=$(cat)\n",
                    "printf '%s\\n' \"$request\" >> '{}'\n",
                    "printf x >> '{}'\n",
                    "case \"$request\" in\n",
                    "  *TallyCompletionState*)\n",
                    "    if test -e '{}'; then comments='[{{\"body\":\"<!-- tally-completion:{} -->\"}}]'; else comments='[]'; fi\n",
                    "    printf '{{\"data\":{{\"node\":{{\"__typename\":\"PullRequest\",\"state\":\"OPEN\",\"comments\":{{\"nodes\":%s,\"pageInfo\":{{\"hasNextPage\":false,\"endCursor\":null}}}}}}}}}}' \"$comments\"\n",
                    "    ;;\n",
                    "  *TallyCompletionComment*) touch '{}'; printf '{{\"data\":{{\"addComment\":{{}}}}}}' ;;\n",
                    "  *TallyCompletionPullRequest*)\n",
                    "    if test ! -e '{}'; then touch '{}'; printf close-failed >&2; exit 92; fi\n",
                    "    printf '{{\"data\":{{\"closePullRequest\":{{}}}}}}'\n",
                    "    ;;\n",
                    "  *) exit 93 ;;\n",
                    "esac\n"
                ),
                requests.display(),
                calls.display(),
                commented.display(),
                remote_key,
                commented.display(),
                failed_close.display(),
                failed_close.display(),
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&gh).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&gh, permissions).unwrap();
        let mut cli = GhCliMutationSink::with_program(&gh);
        assert!(engine
            .complete_gh_once(
                &origin,
                completion_id,
                Verdict::Reused,
                Some(serde_json::json!({"witnessSeq": 5})),
                &mut cli,
            )
            .unwrap_err()
            .to_string()
            .contains("close-failed"));
        assert!(engine
            .complete_gh_once(
                &origin,
                completion_id,
                Verdict::Reused,
                Some(serde_json::json!({"witnessSeq": 5})),
                &mut cli,
            )
            .unwrap());
        assert!(!engine
            .complete_gh_once(
                &origin,
                completion_id,
                Verdict::Reused,
                Some(serde_json::json!({"witnessSeq": 5})),
                &mut cli,
            )
            .unwrap());
        assert_eq!(std::fs::read(&calls).unwrap(), b"xxxxxxx");
        let requests = std::fs::read_to_string(requests)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["query"]
                    .as_str()
                    .unwrap()
                    .contains("TallyCompletionComment"))
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["query"]
                    .as_str()
                    .unwrap()
                    .contains("TallyCompletionPullRequest"))
                .count(),
            2
        );
        let comment = requests
            .iter()
            .find(|request| {
                request["query"]
                    .as_str()
                    .unwrap()
                    .contains("TallyCompletionComment")
            })
            .unwrap();
        assert_eq!(comment["variables"]["itemId"], "PR_kwABC128");
        assert!(comment["variables"]["body"]
            .as_str()
            .unwrap()
            .contains("witnessSeq"));
    }

    #[test]
    fn github_self_trigger_policy_authorizes_the_trigger_actor_with_a_reasoned_rejection() {
        let temp = tempdir().unwrap();
        let mut registry = registry(&temp.path().join("effects.jsonl"));
        let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
            unreachable!()
        };
        github.allow_self_triggered = true;
        github.allowed_actors = vec!["tally-bot".to_owned()];
        let engine = ProducerEngine::new(
            &registry,
            temp.path().join("events"),
            temp.path().join("state"),
        );

        let allowed = gh_observation("I_self_authored", "tally-bot", "tally-bot");
        assert!(matches!(
            engine.emit_gh("github", &allowed, fixed_now()).unwrap(),
            EmitOutcome::Emitted(_)
        ));

        let rejected = gh_observation("I_self_authored", "tally-bot", "untrusted-user");
        assert_eq!(
            engine.emit_gh("github", &rejected, fixed_now()).unwrap(),
            EmitOutcome::Filtered {
                reason: GhFilterReason::TriggerActorNotAllowed
            }
        );
    }

    #[test]
    fn github_trigger_acknowledgement_marker_is_stable_and_remote_idempotent() {
        let temp = tempdir().unwrap();
        let gh = temp.path().join("fake-gh-ack");
        let requests = temp.path().join("ack-requests.jsonl");
        let marker = temp.path().join("ack-marker");
        std::fs::write(
            &gh,
            format!(
                concat!(
                    "#!/bin/sh\n",
                    "[ \"$1 $2 $3 $4\" = 'api graphql --input -' ] || exit 91\n",
                    "request=$(cat)\n",
                    "printf '%s\\n' \"$request\" >> '{}'\n",
                    "case \"$request\" in\n",
                    "  *TallyCompletionState*)\n",
                    "    if test -e '{}'; then comments='[{{\"body\":\"<!-- tally-trigger:receipt-42:accepted -->\"}}]'; else comments='[]'; fi\n",
                    "    printf '{{\"data\":{{\"node\":{{\"__typename\":\"Issue\",\"state\":\"OPEN\",\"comments\":{{\"nodes\":%s,\"pageInfo\":{{\"hasNextPage\":false,\"endCursor\":null}}}}}}}}}}' \"$comments\"\n",
                    "    ;;\n",
                    "  *TallyCompletionComment*) touch '{}'; printf '{{\"data\":{{\"addComment\":{{}}}}}}' ;;\n",
                    "  *) exit 92 ;;\n",
                    "esac\n"
                ),
                requests.display(),
                marker.display(),
                marker.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700)).unwrap();
        File::open(&gh).unwrap().sync_all().unwrap();
        sync_directory(temp.path()).unwrap();
        let acknowledgement = GhTriggerAcknowledgement {
            schema_version: 1,
            producer: "github".to_owned(),
            receipt_id: "receipt-42".to_owned(),
            item_id: "I_widget_42".to_owned(),
            decision: GhDecisionStatus::Accepted,
            rule: None,
            task_uuid: Some("00000000-0000-5000-8000-000000000042".to_owned()),
            status_pointer: Some(
                "tally query log --task 00000000-0000-5000-8000-000000000042".to_owned(),
            ),
        };
        let mut sink = GhCliAcknowledgementSink::with_program(&gh);
        sink.post_acknowledgement(&acknowledgement).unwrap();
        sink.post_acknowledgement(&acknowledgement).unwrap();

        let requests = std::fs::read_to_string(requests)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let comments = requests
            .iter()
            .filter(|request| {
                request["query"]
                    .as_str()
                    .unwrap()
                    .contains("TallyCompletionComment")
            })
            .collect::<Vec<_>>();
        assert_eq!(comments.len(), 1);
        let body = comments[0]["variables"]["body"].as_str().unwrap();
        assert!(body.contains("<!-- tally-trigger:receipt-42:accepted -->"));
        assert!(body.contains("00000000-0000-5000-8000-000000000042"));
        assert!(body.contains("tally query log --task"));
    }

    #[test]
    fn github_issue_21_repo_label_and_state_scope_admits_only_the_exact_match() {
        let temp = tempdir().unwrap();
        let mut registry = registry(&temp.path().join("effects.jsonl"));
        let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
            unreachable!()
        };
        github.sources = vec![GhSource::Search(GhSourceConstraints {
            repo: Some("agency-agency/spec".to_owned()),
            labels: vec!["agency:codex-ready".to_owned()],
            state: Some(GhItemState::Open),
            ..GhSourceConstraints::default()
        })];
        github.triggers = GhTriggers {
            command_comments: vec!["/tally run".to_owned()],
            ..GhTriggers::default()
        };
        github.allowed_actors = vec!["contributor".to_owned()];
        let engine = ProducerEngine::new(
            &registry,
            temp.path().join("events"),
            temp.path().join("state"),
        );

        let mut matching = gh_command_observation("issue-21-comment", "contributor");
        matching.repo = "agency-agency/spec".to_owned();
        matching.number = 21;
        matching.html_url = "https://github.com/agency-agency/spec/issues/21".to_owned();
        matching.node_id = "I_agency_spec_21".to_owned();
        matching.context.labels = vec!["agency:codex-ready".to_owned()];
        assert!(matches!(
            engine.emit_gh("github", &matching, fixed_now()).unwrap(),
            EmitOutcome::Emitted(_)
        ));

        let mut wrong_repo = matching.clone();
        wrong_repo.repo = "agency-agency/other".to_owned();
        wrong_repo.html_url = "https://github.com/agency-agency/other/issues/21".to_owned();
        assert_eq!(
            engine.emit_gh("github", &wrong_repo, fixed_now()).unwrap(),
            EmitOutcome::Filtered {
                reason: GhFilterReason::RepositoryNotAllowed
            }
        );

        let mut wrong_label = matching.clone();
        wrong_label.context.labels = vec!["agency:triage".to_owned()];
        assert_eq!(
            engine.emit_gh("github", &wrong_label, fixed_now()).unwrap(),
            EmitOutcome::Filtered {
                reason: GhFilterReason::LabelMismatch
            }
        );

        let mut wrong_state = matching.clone();
        wrong_state.context.state = Some(GhItemState::Closed);
        assert_eq!(
            engine.emit_gh("github", &wrong_state, fixed_now()).unwrap(),
            EmitOutcome::Filtered {
                reason: GhFilterReason::StateMismatch
            }
        );

        let mut unscoped_registry = registry.clone();
        let ProducerConfig::Gh(unscoped) = unscoped_registry.get_mut("github").unwrap() else {
            unreachable!()
        };
        unscoped.sources = vec![GhSource::Search(GhSourceConstraints::default())];
        let unscoped_engine = ProducerEngine::new(
            &unscoped_registry,
            temp.path().join("unscoped-events"),
            temp.path().join("unscoped-state"),
        );
        assert_eq!(
            unscoped_engine
                .emit_gh("github", &matching, fixed_now())
                .unwrap(),
            EmitOutcome::Filtered {
                reason: GhFilterReason::SourceUnconstrained
            }
        );
    }

    #[test]
    fn github_cross_source_replay_prefers_the_source_whose_scope_matches() {
        let temp = tempdir().unwrap();
        let mut registry = registry(&temp.path().join("effects.jsonl"));
        let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
            unreachable!()
        };
        github.sources = vec![
            GhSource::Notifications(GhSourceConstraints {
                repo: Some("acme/widgets".to_owned()),
                labels: vec!["notification-only".to_owned()],
                ..GhSourceConstraints::default()
            }),
            GhSource::Search(GhSourceConstraints {
                repo: Some("acme/widgets".to_owned()),
                labels: vec!["ready".to_owned()],
                ..GhSourceConstraints::default()
            }),
        ];
        let search = gh_command_observation("cross-source-comment", "contributor");
        let notification = GhObservation {
            source: "notifications".to_owned(),
            notification_reason: Some("mention".to_owned()),
            event_id: Some("notification-42".to_owned()),
            ..search.clone()
        };
        let mut candidates = vec![
            GhIntakeCandidate::Observation(Box::new(notification)),
            GhIntakeCandidate::Observation(Box::new(search)),
        ];
        normalize_gh_candidates(github, &mut candidates);
        assert_eq!(candidates.len(), 1);
        let GhIntakeCandidate::Observation(observation) = &candidates[0] else {
            panic!("expected the matching concrete observation");
        };
        assert_eq!(observation.source, "search");
    }

    #[test]
    fn github_comment_receipts_ack_one_accept_one_duplicate_and_a_later_job() {
        let temp = tempdir().unwrap();
        let mut registry = registry(&temp.path().join("effects.jsonl"));
        let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
            unreachable!()
        };
        github.sources = vec![GhSource::Search(GhSourceConstraints {
            repo: Some("acme/widgets".to_owned()),
            ..GhSourceConstraints::default()
        })];
        github.triggers = GhTriggers {
            command_comments: vec!["/tally run".to_owned()],
            ..GhTriggers::default()
        };
        github.allowed_actors = vec!["contributor".to_owned()];
        let events = temp.path().join("events");
        let state = temp.path().join("state");
        let engine = ProducerEngine::new(&registry, &events, &state);
        let first_observation = gh_command_observation("comment-1", "contributor");
        let mut acknowledgements = RecordingAcknowledgements::default();

        let first = engine
            .admit_gh_observation(
                "github",
                &first_observation,
                fixed_now(),
                &mut acknowledgements,
            )
            .unwrap();
        assert_eq!(first.decision, GhDecisionStatus::Accepted);
        let first_task = first.task_uuid.clone().unwrap();
        assert_eq!(acknowledgements.entries.len(), 1);
        assert_eq!(
            acknowledgements.entries[0].decision,
            GhDecisionStatus::Accepted
        );
        assert_eq!(
            acknowledgements.entries[0].task_uuid.as_deref(),
            Some(first_task.as_str())
        );

        let duplicate = engine
            .admit_gh_observation(
                "github",
                &first_observation,
                fixed_now(),
                &mut acknowledgements,
            )
            .unwrap();
        assert_eq!(duplicate.decision, GhDecisionStatus::Duplicate);
        assert_eq!(
            duplicate.existing_task.as_deref(),
            Some(first_task.as_str())
        );
        assert_eq!(acknowledgements.entries.len(), 2);
        assert_eq!(
            acknowledgements.entries[1].decision,
            GhDecisionStatus::Duplicate
        );

        let mut later_observation = gh_command_observation("comment-2", "contributor");
        later_observation.trigger_timestamp = "2026-07-20T12:35:00Z".to_owned();
        let later = engine
            .admit_gh_observation(
                "github",
                &later_observation,
                fixed_now(),
                &mut acknowledgements,
            )
            .unwrap();
        assert_eq!(later.decision, GhDecisionStatus::Accepted);
        assert_ne!(later.task_uuid.as_deref(), Some(first_task.as_str()));
        assert_eq!(acknowledgements.entries.len(), 3);
        assert_eq!(
            acknowledgements.entries[2].decision,
            GhDecisionStatus::Accepted
        );
        assert_eq!(
            std::fs::read_dir(&events)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(INGRESS_SUFFIX))
                .count(),
            2
        );

        let third_replay = engine
            .admit_gh_observation(
                "github",
                &first_observation,
                fixed_now(),
                &mut acknowledgements,
            )
            .unwrap();
        assert_eq!(third_replay.decision, GhDecisionStatus::Duplicate);
        assert_eq!(
            acknowledgements.entries.len(),
            3,
            "the stable duplicate acknowledgement is emitted only once"
        );
    }

    #[test]
    fn github_event_receipt_rejects_a_mutated_value_under_the_same_identity() {
        let temp = tempdir().unwrap();
        let mut registry = registry(&temp.path().join("effects.jsonl"));
        let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
            unreachable!()
        };
        github.sources = vec![GhSource::Search(GhSourceConstraints {
            repo: Some("acme/widgets".to_owned()),
            ..GhSourceConstraints::default()
        })];
        github.triggers = GhTriggers {
            assignments: vec!["tally-bot".to_owned()],
            ..GhTriggers::default()
        };
        github.allowed_actors = vec!["maintainer".to_owned()];
        let engine = ProducerEngine::new(
            &registry,
            temp.path().join("events"),
            temp.path().join("state"),
        );
        let mut observation = gh_command_observation("event-1", "maintainer");
        observation.trigger_kind = "assignment".to_owned();
        observation.event_id = Some("event-1".to_owned());
        observation.comment_id = None;
        observation.trigger_value = Some("tally-bot".to_owned());
        observation.context.triggering_comment = None;
        let mut acknowledgements = RecordingAcknowledgements::default();

        let accepted = engine
            .admit_gh_observation("github", &observation, fixed_now(), &mut acknowledgements)
            .unwrap();
        assert_eq!(accepted.decision, GhDecisionStatus::Accepted);

        let mut mutated = observation;
        mutated.trigger_value = Some("different-bot".to_owned());
        let error = engine
            .admit_gh_observation("github", &mutated, fixed_now(), &mut acknowledgements)
            .unwrap_err();
        assert!(error.to_string().contains("does not match the observation"));
        assert_eq!(acknowledgements.entries.len(), 1);
    }

    #[test]
    fn github_unauthorized_command_records_rule_acknowledges_and_never_enqueues() {
        let temp = tempdir().unwrap();
        let mut registry = registry(&temp.path().join("effects.jsonl"));
        let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
            unreachable!()
        };
        github.sources = vec![GhSource::Search(GhSourceConstraints {
            repo: Some("acme/widgets".to_owned()),
            ..GhSourceConstraints::default()
        })];
        github.triggers = GhTriggers {
            command_comments: vec!["/tally run".to_owned()],
            ..GhTriggers::default()
        };
        github.allowed_actors = vec!["maintainer".to_owned()];
        let events = temp.path().join("events");
        let state = temp.path().join("state");
        let engine = ProducerEngine::new(&registry, &events, &state);
        let observation = gh_command_observation("unauthorized-comment", "outsider");
        let mut acknowledgements = RecordingAcknowledgements::default();

        let decision = engine
            .admit_gh_observation("github", &observation, fixed_now(), &mut acknowledgements)
            .unwrap();
        assert_eq!(decision.decision, GhDecisionStatus::Filtered);
        assert_eq!(decision.rule, Some(GhFilterReason::TriggerActorNotAllowed));
        assert!(!events.exists() || std::fs::read_dir(&events).unwrap().next().is_none());
        assert_eq!(acknowledgements.entries.len(), 1);
        assert_eq!(
            acknowledgements.entries[0].decision,
            GhDecisionStatus::Filtered
        );
        assert_eq!(
            acknowledgements.entries[0].rule,
            Some(GhFilterReason::TriggerActorNotAllowed)
        );
        assert!(acknowledgements.entries[0].task_uuid.is_none());

        let receipt_id = decision.receipt_id.unwrap();
        let receipt: Value = serde_json::from_slice(
            &std::fs::read(
                state
                    .join("producers/gh-triggers")
                    .join(format!("{receipt_id}.json")),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(receipt["primaryDecision"], "filtered");
        assert_eq!(receipt["rule"], "trigger-actor-not-allowed");
        assert_eq!(receipt["primaryAcknowledged"], true);
    }

    #[test]
    fn github_cli_poll_requires_exact_trigger_actor_and_deduplicates_event_ids() {
        let temp = tempdir().unwrap();
        let mut registry = registry(&temp.path().join("effects.jsonl"));
        let ProducerConfig::Gh(github) = registry.get_mut("github").unwrap() else {
            unreachable!()
        };
        github.sources = vec![GhSource::Notifications(GhSourceConstraints {
            repo: Some("acme/repo".to_owned()),
            ..GhSourceConstraints::default()
        })];
        github.triggers = GhTriggers {
            mentions: vec!["@tally-bot please run".to_owned()],
            ..GhTriggers::default()
        };
        github.allowed_actors = vec!["contributor".to_owned()];
        let events = temp.path().join("events");
        let state = temp.path().join("state");
        let gh = temp.path().join("fake-gh-intake");
        let calls = temp.path().join("gh-intake-calls");
        std::fs::write(
            &gh,
            format!(
                concat!(
                    "#!/bin/sh\n",
                    "printf '%s\\n' \"$*\" >> '{}'\n",
                    "case \"$*\" in\n",
                    "  'api user') printf '{{\"login\":\"tally-bot\"}}' ;;\n",
                    "  'api --method GET notifications -f all=false -f participating=false -f per_page=100')\n",
                    "    printf '[{{\"id\":\"N1\",\"reason\":\"mention\",\"updated_at\":\"2026-07-20T12:00:00Z\",\"repository\":{{\"full_name\":\"acme/repo\"}},\"subject\":{{\"type\":\"Issue\",\"url\":\"https://api.github.com/repos/acme/repo/issues/1\",\"latest_comment_url\":\"https://api.github.com/repos/acme/repo/issues/comments/101\"}}}},{{\"id\":\"N2\",\"reason\":\"subscribed\",\"updated_at\":\"2026-07-20T12:10:00Z\",\"repository\":{{\"full_name\":\"acme/repo\"}},\"subject\":{{\"type\":\"Issue\",\"url\":\"https://api.github.com/repos/acme/repo/issues/2\",\"latest_comment_url\":\"https://api.github.com/repos/acme/repo/issues/comments/202\"}}}}]' ;;\n",
                    "  'api /repos/acme/repo/issues/1') printf '{{\"node_id\":\"I_node_1\",\"number\":1,\"html_url\":\"https://github.com/acme/repo/issues/1\",\"title\":\"Issue one\",\"body\":\"untrusted issue body\",\"state\":\"open\",\"user\":{{\"login\":\"tally-bot\"}},\"labels\":[{{\"name\":\"bug\"}}],\"assignees\":[{{\"login\":\"tally-bot\"}}]}}' ;;\n",
                    "  'api /repos/acme/repo/issues/comments/101') printf '{{\"id\":101,\"body\":\"@tally-bot please run\",\"created_at\":\"2026-07-20T12:00:00Z\",\"updated_at\":\"2026-07-20T12:00:00Z\",\"user\":{{\"login\":\"contributor\"}}}}' ;;\n",
                    "  'api /repos/acme/repo/issues/2') printf '{{\"node_id\":\"I_node_2\",\"number\":2,\"html_url\":\"https://github.com/acme/repo/issues/2\",\"title\":\"Issue two\",\"body\":null,\"state\":\"open\",\"user\":{{\"login\":\"issue-author\"}},\"labels\":[],\"assignees\":[]}}' ;;\n",
                    "  'api /repos/acme/repo/issues/comments/202') printf '{{\"id\":202,\"body\":\"older unrelated comment\",\"created_at\":\"2026-07-20T11:00:00Z\",\"updated_at\":\"2026-07-20T11:00:00Z\",\"user\":{{\"login\":\"other\"}}}}' ;;\n",
                    "  'api /repos/acme/repo/issues/2/events?per_page=100') printf '[]' ;;\n",
                    "  *) exit 91 ;;\n",
                    "esac\n"
                ),
                calls.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700)).unwrap();
        let intake = GhCliIntake::with_program(&gh);
        let engine = ProducerEngine::new(&registry, &events, &state);
        let first = engine.poll_gh("github", &intake, fixed_now()).unwrap();
        assert_eq!(
            first
                .iter()
                .filter(|outcome| matches!(outcome, EmitOutcome::Emitted(_)))
                .count(),
            1
        );
        assert_eq!(
            first
                .iter()
                .filter(|outcome| {
                    matches!(
                        outcome,
                        EmitOutcome::Filtered {
                            reason: GhFilterReason::TriggerActorUnavailable
                        }
                    )
                })
                .count(),
            1
        );
        assert!(!first.iter().any(|outcome| {
            matches!(
                outcome,
                EmitOutcome::Filtered {
                    reason: GhFilterReason::SelfTriggerDisabled
                }
            )
        }));
        let emitted = first
            .iter()
            .find_map(|outcome| match outcome {
                EmitOutcome::Emitted(path) => Some(path),
                _ => None,
            })
            .unwrap();
        let payload: EnqueuePayload =
            serde_json::from_slice(&std::fs::read(emitted).unwrap()).unwrap();
        let origin = payload.gh_origin.unwrap();
        assert_eq!(origin.item_author, "tally-bot");
        assert_eq!(origin.trigger_actor, "contributor");
        assert_eq!(origin.comment_id.as_deref(), Some("101"));
        assert_eq!(
            origin.context.unwrap().triggering_comment.unwrap().author,
            "contributor"
        );
        let second = engine.poll_gh("github", &intake, fixed_now()).unwrap();
        assert_eq!(
            second
                .iter()
                .filter(|outcome| matches!(outcome, EmitOutcome::Duplicate))
                .count(),
            1
        );
        assert_eq!(
            second
                .iter()
                .filter(|outcome| {
                    matches!(
                        outcome,
                        EmitOutcome::Filtered {
                            reason: GhFilterReason::TriggerActorUnavailable
                        }
                    )
                })
                .count(),
            1
        );
        assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 14);

        let mut disabled_registry = registry.clone();
        let ProducerConfig::Gh(disabled) = disabled_registry.get_mut("github").unwrap() else {
            unreachable!()
        };
        disabled.enable = false;
        assert!(ProducerEngine::new(&disabled_registry, &events, &state)
            .poll_gh(
                "github",
                &GhCliIntake::with_program(temp.path().join("absent-gh")),
                fixed_now(),
            )
            .unwrap()
            .is_empty());

        let malformed_gh = temp.path().join("malformed-gh-intake");
        std::fs::write(
            &malformed_gh,
            concat!(
                "#!/bin/sh\n",
                "case \"$*\" in\n",
                "  'api user') printf '{\"login\":\"tally-bot\"}' ;;\n",
                "  'api --method GET notifications -f all=false -f participating=false -f per_page=100') printf '[{\"id\":\"N9\",\"updated_at\":\"2026-07-20T12:00:00Z\",\"repository\":{\"full_name\":\"acme/repo\"},\"subject\":{\"type\":\"Issue\",\"url\":\"https://api.github.com/repos/acme/repo/issues/9\"}}]' ;;\n",
                "  'api /repos/acme/repo/issues/9') printf '{\"user\":{\"login\":\"contributor\"}}' ;;\n",
                "  'api /repos/acme/repo/issues/9/events?per_page=100') printf '[]' ;;\n",
                "  *) exit 91 ;;\n",
                "esac\n",
            ),
        )
        .unwrap();
        std::fs::set_permissions(&malformed_gh, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(engine
            .poll_gh(
                "github",
                &GhCliIntake::with_program(malformed_gh),
                fixed_now(),
            )
            .unwrap_err()
            .to_string()
            .contains("omitted number"));
    }

    #[test]
    fn build_effect_is_atomic_single_flight_per_store_path() {
        let temp = tempdir().unwrap();
        let registry = Arc::new(registry(&temp.path().join("effects.jsonl")));
        let events = temp.path().join("events");
        let state = temp.path().join("state");
        let barrier = Arc::new(Barrier::new(2));
        let outcomes = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..2 {
                let registry = registry.clone();
                let events = events.clone();
                let state = state.clone();
                let barrier = barrier.clone();
                handles.push(scope.spawn(move || {
                    barrier.wait();
                    ProducerEngine::new(&registry, events, state)
                        .emit_build_effect("effects", Path::new(STORE_A), fixed_now())
                        .unwrap()
                }));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, EmitOutcome::Emitted(_)))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, EmitOutcome::Duplicate))
                .count(),
            1
        );
        let emitted = outcomes
            .into_iter()
            .find_map(|outcome| match outcome {
                EmitOutcome::Emitted(path) => Some(path),
                _ => None,
            })
            .unwrap();
        let payload: EnqueuePayload =
            serde_json::from_slice(&std::fs::read(&emitted).unwrap()).unwrap();
        assert_eq!(payload.source, Some(EnqueueSource::BuildEffect));
        assert_eq!(
            payload.dedup_key.as_deref(),
            Some(format!("build-effect:effects:{STORE_A}").as_str())
        );

        let claims = claim_ingress_files(&events).unwrap();
        assert_eq!(claims.len(), 1);
        archive_ingress_claim(&events, &claims[0], true).unwrap();
        assert_eq!(
            ProducerEngine::new(&registry, &events, &state)
                .emit_build_effect("effects", Path::new(STORE_A), fixed_now())
                .unwrap(),
            EmitOutcome::Duplicate
        );
        assert!(ProducerEngine::new(&registry, &events, &state)
            .emit_build_effect("effects", Path::new(STORE_B), fixed_now())
            .is_ok());
    }

    #[test]
    fn build_effect_scanners_cover_all_bounded_watch_shapes() {
        let temp = tempdir().unwrap();
        let roots = temp.path().join("roots");
        std::fs::create_dir(&roots).unwrap();
        std::os::unix::fs::symlink(STORE_B, roots.join("b-root")).unwrap();
        std::os::unix::fs::symlink(STORE_A, roots.join("a-root")).unwrap();
        assert_eq!(
            scan_store_paths(BuildEffectWatch::GcRootsDir, &roots).unwrap(),
            [PathBuf::from(STORE_A), PathBuf::from(STORE_B)]
        );

        let jsonl = temp.path().join("effects.jsonl");
        std::fs::write(
            &jsonl,
            format!(
                "{}\n{}\n",
                serde_json::to_string(STORE_B).unwrap(),
                serde_json::json!({"outputs": [STORE_A, STORE_B]})
            ),
        )
        .unwrap();
        assert_eq!(
            scan_store_paths(BuildEffectWatch::Jsonl, &jsonl).unwrap(),
            [PathBuf::from(STORE_A), PathBuf::from(STORE_B)]
        );

        let hook = temp.path().join("post-build");
        std::fs::write(&hook, format!("{STORE_B} {STORE_A}\n")).unwrap();
        assert_eq!(
            scan_store_paths(BuildEffectWatch::PostBuildHook, &hook).unwrap(),
            [PathBuf::from(STORE_A), PathBuf::from(STORE_B)]
        );
    }

    #[test]
    fn pool_return_actions_require_symmetric_sustained_hysteresis() {
        let temp = tempdir().unwrap();
        let registry = registry(&temp.path().join("effects.jsonl"));
        let events = temp.path().join("events");
        let state = temp.path().join("state");
        let engine = ProducerEngine::new(&registry, &events, &state);

        for failed in 1..=2 {
            let outcome = engine
                .observe_reachability("health", false, fixed_now())
                .unwrap();
            assert_eq!(outcome.stable, ReachabilityStable::Reachable);
            assert_eq!(outcome.transition, None, "failed probe {failed}");
            assert!(outcome.emitted.is_empty());
        }
        let lost = engine
            .observe_reachability("health", false, fixed_now())
            .unwrap();
        assert_eq!(lost.stable, ReachabilityStable::Lost);
        assert_eq!(lost.transition, Some(ReachabilityTransition::Lost));
        assert_eq!(lost.generation, 1);
        assert_eq!(lost.emitted.len(), 1);
        let pending = engine
            .observe_reachability("health", false, fixed_now())
            .unwrap();
        assert_eq!(pending.transition, Some(ReachabilityTransition::Lost));
        assert_eq!(pending.generation, lost.generation);
        assert!(pending.emitted.is_empty());
        for success in 1..=3 {
            let still_pending = engine
                .observe_reachability("health", true, fixed_now())
                .unwrap();
            assert_eq!(still_pending.stable, ReachabilityStable::Lost);
            assert_eq!(
                still_pending.transition,
                Some(ReachabilityTransition::Lost),
                "opposite probe {success} must not overwrite an unacknowledged loss"
            );
            assert_eq!(still_pending.generation, lost.generation);
            assert!(still_pending.emitted.is_empty());
        }
        engine
            .validate_reachability_transition(
                "health",
                ReachabilityTransition::Lost,
                lost.generation,
            )
            .unwrap();
        engine
            .acknowledge_reachability_transition("health", lost.generation)
            .unwrap();

        for success in 1..=2 {
            let outcome = engine
                .observe_reachability("health", true, fixed_now())
                .unwrap();
            assert_eq!(outcome.stable, ReachabilityStable::Lost);
            assert_eq!(outcome.transition, None, "successful probe {success}");
            assert!(outcome.emitted.is_empty());
        }
        let returned = engine
            .observe_reachability("health", true, fixed_now())
            .unwrap();
        assert_eq!(returned.stable, ReachabilityStable::Reachable);
        assert_eq!(returned.transition, Some(ReachabilityTransition::Returned));
        assert_eq!(returned.generation, 2);
        assert_eq!(returned.emitted.len(), 2);
        let payloads = returned
            .emitted
            .iter()
            .map(|path| {
                serde_json::from_slice::<EnqueuePayload>(&std::fs::read(path).unwrap()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            payloads.iter().filter(|payload| payload.no_enqueue).count(),
            1
        );
        assert_eq!(
            payloads
                .iter()
                .filter(|payload| !payload.no_enqueue)
                .count(),
            1
        );
        assert!(payloads
            .iter()
            .all(|payload| payload.source == Some(EnqueueSource::PoolReachability)));
        assert_eq!(
            engine.confirmed_pool_returns().unwrap(),
            BTreeSet::from(["slot".to_owned()])
        );
        engine
            .acknowledge_reachability_transition("health", returned.generation)
            .unwrap();

        let reopened = ProducerEngine::new(&registry, &events, &state);
        let stable = reopened
            .observe_reachability("health", true, fixed_now())
            .unwrap();
        assert_eq!(stable.stable, ReachabilityStable::Reachable);
        assert_eq!(stable.transition, None);
        assert!(stable.emitted.is_empty());

        let mut rebound_registry = registry.clone();
        let ProducerConfig::PoolReachability(rebound) = rebound_registry.get_mut("health").unwrap()
        else {
            unreachable!()
        };
        rebound.probe_pool = "different-slot".to_owned();
        let rebound = ProducerEngine::new(&rebound_registry, &events, &state);
        assert!(rebound
            .confirmed_pool_returns()
            .unwrap_err()
            .to_string()
            .contains("not bound"));
    }

    #[test]
    fn ingress_claims_are_atomic_recoverable_and_nofollow() {
        let temp = tempdir().unwrap();
        let events = temp.path().join("events");
        std::fs::create_dir(&events).unwrap();
        let payload = enqueue("from-file")
            .payload(EnqueueSource::EventsDir, Some("events"), fixed_now(), None)
            .unwrap();
        std::fs::write(
            events.join("valid.json"),
            serde_json::to_vec(&payload).unwrap(),
        )
        .unwrap();
        std::fs::write(events.join("internal.enqueue.json"), b"not ingress").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", events.join("hostile.json")).unwrap();
        let fifo = events.join("hostile-fifo.json");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let overlong = format!("{}.json", "a".repeat(MAX_CLAIMABLE_NAME_BYTES));
        std::fs::write(
            events.join(&overlong),
            serde_json::to_vec(&payload).unwrap(),
        )
        .unwrap();

        let claims = claim_ingress_files(&events).unwrap();
        assert_eq!(claims.len(), 3);
        assert!(!events.join("valid.json").exists());
        assert!(events.join("internal.enqueue.json").exists());
        assert!(!events.join(&overlong).exists());
        assert!(std::fs::read_dir(events.join("rejected"))
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with("overlong-")));
        std::fs::write(
            events.join(&overlong),
            serde_json::to_vec(&payload).unwrap(),
        )
        .unwrap();
        let resumed = claim_ingress_files(&events).unwrap();
        assert_eq!(resumed, claims);
        assert!(!events.join(&overlong).exists());
        assert_eq!(
            std::fs::read_dir(events.join("rejected"))
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("overlong-"))
                .count(),
            2
        );

        for claim in claims {
            if claim.original_name == "valid.json" {
                let decoded = read_ingress_payload(&claim).unwrap();
                assert_eq!(decoded, payload);
                std::fs::write(events.join("done/valid.json"), b"prior archive").unwrap();
                let archived = archive_ingress_claim(&events, &claim, true).unwrap();
                assert_eq!(archived, events.join("done/valid.json.1"));
                assert_eq!(
                    std::fs::read(events.join("done/valid.json")).unwrap(),
                    b"prior archive"
                );
            } else if claim.original_name == "hostile.json" {
                assert!(read_ingress_payload(&claim).is_err());
                let archived = archive_ingress_claim(&events, &claim, false).unwrap();
                assert_eq!(archived, events.join("rejected/hostile.json"));
                assert!(std::fs::symlink_metadata(archived)
                    .unwrap()
                    .file_type()
                    .is_symlink());
            } else {
                assert_eq!(claim.original_name, "hostile-fifo.json");
                assert!(read_ingress_payload(&claim).is_err());
                let archived = archive_ingress_claim(&events, &claim, false).unwrap();
                assert_eq!(archived, events.join("rejected/hostile-fifo.json"));
                assert!(std::fs::symlink_metadata(archived)
                    .unwrap()
                    .file_type()
                    .is_fifo());
            }
        }
        assert!(claim_ingress_files(&events).unwrap().is_empty());
    }
}
