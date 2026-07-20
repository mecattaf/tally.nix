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

use crate::config::Priority;
use crate::evidence::parse_evidence_specs;
use crate::taskdb::{read_acknowledged_events, EnqueueSource, GhOrigin, MAX_GH_ORIGIN_FIELD_BYTES};
use crate::wire::EnqueuePayload;
use crate::witness::Verdict;

pub const IN_SCOPE_PRODUCER_KINDS: &[&str] = &[
    "calendar",
    "events-dir",
    "gh",
    "build-effect",
    "pool-reachability",
];

const MAX_INGRESS_BYTES: u64 = 1024 * 1024;
const INGRESS_SUFFIX: &str = ".producer.json";
const MAX_PRODUCER_NAME_BYTES: usize = 96;
const MAX_CLAIMABLE_NAME_BYTES: usize = 200;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProducerEnqueue {
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default = "default_adapter")]
    pub adapter: String,
    pub pool: String,
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
        now: DateTime<Utc>,
    ) -> Result<EnqueuePayload, ProducerError> {
        let dedup_key = self
            .dedup_key
            .as_deref()
            .map(|key| expand_dedup_key(key, now))
            .transpose()?;
        Ok(EnqueuePayload {
            invocation: None,
            argv: Some(self.argv.clone()),
            pool: Some(self.pool.clone()),
            priority: Some(self.priority),
            adapter: Some(self.adapter.clone()),
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
            caller_job_id: None,
            gh_actor: None,
            gh_self_actor: None,
            gh_origin: None,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhProducer {
    #[serde(default)]
    pub credentials: BTreeMap<String, PathBuf>,
    pub enable: bool,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default = "default_actor_exclude")]
    pub actor_exclude: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_sec: u64,
    #[serde(default)]
    pub post_evidence: bool,
    pub enqueue: ProducerEnqueue,
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
                validate_enqueue(name, "enqueue", &config.enqueue, pools, adapters)?;
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
                    validate_name(source, "GitHub source")?;
                    if !matches!(source.as_str(), "notifications" | "search") {
                        return Err(ProducerError::InvalidConfig(format!(
                            "gh producer {name:?} has unsupported source {source:?}"
                        )));
                    }
                    if !sources.insert(source) {
                        return Err(ProducerError::InvalidConfig(format!(
                            "gh producer {name:?} repeats source {source:?}"
                        )));
                    }
                }
                validate_name(&config.actor_exclude, "GitHub actorExclude")?;
                validate_enqueue(name, "enqueue", &config.enqueue, pools, adapters)?;
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
                validate_enqueue(name, "onKey", &config.on_key, pools, adapters)?;
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
                        validate_enqueue(name, field, enqueue, pools, adapters)?;
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
) -> Result<(), ProducerError> {
    if enqueue.argv.is_empty() {
        return Err(ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} argv must not be empty"
        )));
    }
    if !pools.contains(&enqueue.pool) {
        return Err(ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} references unknown pool {:?}",
            enqueue.pool
        )));
    }
    if !adapters.contains(&enqueue.adapter) {
        return Err(ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} references unknown adapter {:?}",
            enqueue.adapter
        )));
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
    pub item_id: String,
    pub actor: String,
    pub self_actor: String,
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
    Gh {
        #[serde(default)]
        source: Option<String>,
        #[serde(default)]
        item_id: Option<String>,
        #[serde(default)]
        actor: Option<String>,
        #[serde(default)]
        self_actor: Option<String>,
    },
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
}

pub trait GhMutationSink {
    fn post_completed(&mut self, mutation: &GhCompletedMutation) -> Result<(), String>;
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

impl GhCliIntake {
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    fn poll(&self, sources: &[String]) -> Result<Vec<GhObservation>, ProducerError> {
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
        for source in sources {
            match source.as_str() {
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
                        if !matches!(kind, "Issue" | "PullRequest") {
                            continue;
                        }
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
                        observations.push(gh_api_observation(
                            "notifications",
                            &hydrated,
                            &self_actor,
                        )?);
                    }
                }
                "search" => {
                    let response: Value = self.json(&[
                        "api",
                        "--method",
                        "GET",
                        "search/issues",
                        "-f",
                        "q=is:open involves:@me",
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
                        observations.push(gh_api_observation("search", item, &self_actor)?);
                    }
                }
                other => {
                    return Err(ProducerError::InvalidConfig(format!(
                        "unsupported GitHub source {other:?}"
                    )))
                }
            }
        }
        observations.sort_by(|left, right| {
            left.item_id
                .cmp(&right.item_id)
                .then_with(|| left.source.cmp(&right.source))
        });
        observations.dedup_by(|right, left| right.item_id == left.item_id);
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
}

fn gh_api_observation(
    source: &str,
    item: &Value,
    self_actor: &str,
) -> Result<GhObservation, ProducerError> {
    let item_id = item
        .get("node_id")
        .and_then(Value::as_str)
        .filter(|item_id| !item_id.is_empty())
        .ok_or_else(|| ProducerError::GitHub("GitHub issue/PR omitted node_id".to_owned()))?;
    let actor = item
        .pointer("/user/login")
        .and_then(Value::as_str)
        .filter(|actor| !actor.is_empty())
        .ok_or_else(|| ProducerError::GitHub("GitHub issue/PR omitted user.login".to_owned()))?;
    Ok(GhObservation {
        source: source.to_owned(),
        item_id: item_id.to_owned(),
        actor: actor.to_owned(),
        self_actor: self_actor.to_owned(),
    })
}

impl GhMutationSink for GhCliMutationSink {
    fn post_completed(&mut self, mutation: &GhCompletedMutation) -> Result<(), String> {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmitOutcome {
    Emitted(PathBuf),
    Duplicate,
    Filtered,
    Disabled,
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
        let payload = config.enqueue.payload(EnqueueSource::Calendar, now)?;
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
        if actor_is_excluded(config, observation) {
            return Ok(EmitOutcome::Filtered);
        }
        let mut payload = config.enqueue.payload(EnqueueSource::Gh, now)?;
        payload.dedup_key = Some(format!("gh:{}:{}", producer, observation.item_id));
        payload.gh_actor = Some(observation.actor.clone());
        payload.gh_self_actor = Some(observation.self_actor.clone());
        payload.gh_origin = Some(GhOrigin {
            producer: producer.to_owned(),
            source: observation.source.clone(),
            item_id: observation.item_id.clone(),
            actor: observation.actor.clone(),
            self_actor: observation.self_actor.clone(),
            actor_exclude: config.actor_exclude.clone(),
        });
        let key = stable_key(&["gh", producer, &observation.item_id]);
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
            .poll(&config.sources)?
            .iter()
            .map(|observation| self.emit_gh(producer, observation, now))
            .collect()
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
        let observation = gh_observation(origin);
        validate_gh_observation(&origin.producer, config, &observation)?;
        if actor_is_excluded(config, &observation) {
            return Err(ProducerError::InvalidObservation(format!(
                "gh producer {:?} origin actor is excluded",
                origin.producer
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
        self.complete_gh_with_id(origin, None, verdict, evidence, sink)
    }

    pub fn complete_gh_once(
        &self,
        origin: &GhOrigin,
        completion_id: &str,
        verdict: Verdict,
        evidence: Option<Value>,
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
            &origin.item_id,
            completion_id,
        ]);
        let marker_path = completed_dir.join(format!("{marker_key}.json"));
        if path_lexists(&marker_path)? {
            let marker: GhCompletionMarker =
                serde_json::from_slice(&read_bounded_regular(&marker_path, 64 * 1024)?)?;
            if marker.completion_id != completion_id
                || marker.producer != origin.producer
                || marker.source != origin.source
                || marker.item_id != origin.item_id
            {
                return Err(ProducerError::InvalidObservation(format!(
                    "GitHub completion marker {} does not match its identity",
                    marker_path.display()
                )));
            }
            return Ok(false);
        }
        if !self.complete_gh_with_id(origin, Some(completion_id), verdict, evidence, sink)? {
            return Ok(false);
        }
        write_json_atomic(
            &marker_path,
            &GhCompletionMarker {
                completion_id: completion_id.to_owned(),
                producer: origin.producer.clone(),
                source: origin.source.clone(),
                item_id: origin.item_id.clone(),
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
        sink: &mut dyn GhMutationSink,
    ) -> Result<bool, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(&origin.producer)? else {
            return Err(self.kind_mismatch(&origin.producer, "gh"));
        };
        if !config.enable || !config.post_evidence {
            return Ok(false);
        }
        self.validate_gh_origin(origin)?;
        if !matches!(verdict, Verdict::Pass | Verdict::Reused) {
            return Ok(false);
        }
        sink.post_completed(&GhCompletedMutation {
            producer: origin.producer.clone(),
            source: origin.source.clone(),
            item_id: origin.item_id.clone(),
            completion_id: completion_id.map(str::to_owned),
            state: "COMPLETED".to_owned(),
            evidence,
        })
        .map_err(ProducerError::Mutation)?;
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
        let mut payload = config.on_key.payload(EnqueueSource::BuildEffect, now)?;
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
                let payload = enqueue.payload(EnqueueSource::PoolReachability, now)?;
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

fn gh_observation(origin: &GhOrigin) -> GhObservation {
    GhObservation {
        source: origin.source.clone(),
        item_id: origin.item_id.clone(),
        actor: origin.actor.clone(),
        self_actor: origin.self_actor.clone(),
    }
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
    for (value, label) in [
        (&observation.source, "source"),
        (&observation.item_id, "itemId"),
        (&observation.actor, "actor"),
        (&observation.self_actor, "selfActor"),
    ] {
        if value.trim().is_empty()
            || value.len() > MAX_GH_ORIGIN_FIELD_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(ProducerError::InvalidObservation(format!(
                "gh producer {producer:?} observation {label} must be non-empty, at most {MAX_GH_ORIGIN_FIELD_BYTES} bytes, and contain no control characters"
            )));
        }
    }
    if !config
        .sources
        .iter()
        .any(|source| source == &observation.source)
    {
        return Err(ProducerError::InvalidObservation(format!(
            "gh producer {producer:?} rejected unconfigured source {:?}",
            observation.source
        )));
    }
    Ok(())
}

fn actor_is_excluded(config: &GhProducer, observation: &GhObservation) -> bool {
    if config.actor_exclude == "self" {
        observation.actor == observation.self_actor
    } else {
        observation.actor == config.actor_exclude
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
            pool: "slot".to_owned(),
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
                    sources: vec!["notifications".to_owned(), "search".to_owned()],
                    actor_exclude: "self".to_owned(),
                    poll_interval_sec: 60,
                    post_evidence: true,
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

    #[test]
    fn registry_is_strict_open_by_name_and_closed_over_the_in_scope_kinds() {
        let temp = tempdir().unwrap();
        let registry = registry(&temp.path().join("effects.jsonl"));
        validate_registry(
            &registry,
            &BTreeSet::from(["slot".to_owned()]),
            &BTreeSet::from(["shell".to_owned()]),
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
                "itemId": "PR-1",
                "actor": "contributor",
                "selfActor": "tally-bot"
            }))
            .unwrap(),
            ProducerObservation::Gh { item_id, self_actor, .. }
                if item_id.as_deref() == Some("PR-1")
                    && self_actor.as_deref() == Some("tally-bot")
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
        )
        .unwrap_err()
        .to_string()
        .contains("strftime"));
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
        assert_eq!(payload.pool.as_deref(), Some("slot"));
        assert_eq!(payload.adapter.as_deref(), Some("shell"));
        assert_eq!(payload.dedup_key.as_deref(), Some("daily-20260720"));
        assert_eq!(
            payload.credentials["token"],
            PathBuf::from("/run/credentials/calendar-token")
        );
    }

    #[derive(Default)]
    struct RecordingMutation(Vec<GhCompletedMutation>);

    impl GhMutationSink for RecordingMutation {
        fn post_completed(&mut self, mutation: &GhCompletedMutation) -> Result<(), String> {
            self.0.push(mutation.clone());
            Ok(())
        }
    }

    #[test]
    fn github_enforces_sources_actor_exclusion_and_completed_mutation() {
        let temp = tempdir().unwrap();
        let registry = registry(&temp.path().join("effects.jsonl"));
        let engine = ProducerEngine::new(
            &registry,
            temp.path().join("events"),
            temp.path().join("state"),
        );
        let external = GhObservation {
            source: "notifications".to_owned(),
            item_id: "PR_kwABC128".to_owned(),
            actor: "contributor".to_owned(),
            self_actor: "tally-bot".to_owned(),
        };
        let EmitOutcome::Emitted(path) = engine.emit_gh("github", &external, fixed_now()).unwrap()
        else {
            panic!("GitHub observation did not emit")
        };
        let payload: EnqueuePayload =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(payload.source, Some(EnqueueSource::Gh));
        assert_eq!(payload.gh_actor.as_deref(), Some("contributor"));
        assert_eq!(payload.gh_self_actor.as_deref(), Some("tally-bot"));
        assert_eq!(payload.dedup_key.as_deref(), Some("gh:github:PR_kwABC128"));
        let origin = payload.gh_origin.clone().unwrap();
        assert_eq!(origin.producer, "github");
        assert_eq!(origin.source, "notifications");
        assert_eq!(origin.item_id, "PR_kwABC128");
        assert_eq!(
            engine.emit_gh("github", &external, fixed_now()).unwrap(),
            EmitOutcome::Duplicate
        );

        let own = GhObservation {
            actor: "tally-bot".to_owned(),
            ..external.clone()
        };
        assert_eq!(
            engine.emit_gh("github", &own, fixed_now()).unwrap(),
            EmitOutcome::Filtered
        );
        let wrong_source = GhObservation {
            source: "unconfigured".to_owned(),
            ..external.clone()
        };
        assert!(engine
            .emit_gh("github", &wrong_source, fixed_now())
            .unwrap_err()
            .to_string()
            .contains("unconfigured source"));

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
        assert_eq!(mutations.0.len(), 1);
        assert_eq!(mutations.0[0].state, "COMPLETED");
        assert_eq!(mutations.0[0].source, "notifications");
        assert_eq!(mutations.0[0].item_id, "PR_kwABC128");
        assert_eq!(mutations.0[0].evidence.as_ref().unwrap()["witnessSeq"], 4);

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
        assert_eq!(std::fs::read(&calls).unwrap(), b"xxxxx");
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
    fn github_cli_poll_hydrates_issues_and_prs_and_deduplicates_node_ids() {
        let temp = tempdir().unwrap();
        let registry = registry(&temp.path().join("effects.jsonl"));
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
                    "    printf '[{{\"subject\":{{\"type\":\"Issue\",\"url\":\"https://api.github.com/repos/acme/repo/issues/1\"}}}},{{\"subject\":{{\"type\":\"PullRequest\",\"url\":\"https://api.github.com/repos/acme/repo/pulls/2\"}}}}]' ;;\n",
                    "  'api /repos/acme/repo/issues/1') printf '{{\"node_id\":\"I_node_1\",\"user\":{{\"login\":\"contributor\"}}}}' ;;\n",
                    "  'api /repos/acme/repo/pulls/2') printf '{{\"node_id\":\"PR_self\",\"user\":{{\"login\":\"tally-bot\"}}}}' ;;\n",
                    "  'api --method GET search/issues -f q=is:open involves:@me -f per_page=100')\n",
                    "    printf '{{\"items\":[{{\"node_id\":\"I_node_1\",\"user\":{{\"login\":\"contributor\"}}}},{{\"node_id\":\"PR_node_3\",\"user\":{{\"login\":\"reviewer\"}}}}]}}' ;;\n",
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
            2
        );
        assert_eq!(
            first
                .iter()
                .filter(|outcome| matches!(outcome, EmitOutcome::Filtered))
                .count(),
            1
        );
        let second = engine.poll_gh("github", &intake, fixed_now()).unwrap();
        assert_eq!(
            second
                .iter()
                .filter(|outcome| matches!(outcome, EmitOutcome::Duplicate))
                .count(),
            2
        );
        assert_eq!(
            second
                .iter()
                .filter(|outcome| matches!(outcome, EmitOutcome::Filtered))
                .count(),
            1
        );
        assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 10);

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
                "  'api --method GET notifications -f all=false -f participating=false -f per_page=100') printf '[{\"subject\":{\"type\":\"Issue\",\"url\":\"https://api.github.com/repos/acme/repo/issues/9\"}}]' ;;\n",
                "  'api /repos/acme/repo/issues/9') printf '{\"user\":{\"login\":\"contributor\"}}' ;;\n",
                "  'api --method GET search/issues -f q=is:open involves:@me -f per_page=100') printf '{\"items\":[]}' ;;\n",
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
            .contains("omitted node_id"));
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
            .payload(EnqueueSource::EventsDir, fixed_now())
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
