use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use taskchampion::chrono::Utc;
use taskchampion::storage::AccessMode;
use taskchampion::{Operations, Replica, SqliteStorage, Status, Task, Uuid};
use thiserror::Error;

use crate::config::Priority;
use crate::evidence::parse_evidence_specs;
use crate::recovery::{RecoveryPlan, RecoveryRowState};
use crate::witness::{read_verified_records, LaborClass, Verdict, WitnessError, WitnessRecord};

pub const TASKDATA_DIRECTORY: &str = "taskdata";
const MAX_DURABLE_EVENT_BYTES: u64 = 1024 * 1024;
pub const MAX_GH_ORIGIN_FIELD_BYTES: usize = 4096;
pub const MAX_GH_CONTEXT_BYTES: usize = 256 * 1024;
pub const GH_ORIGIN_SCHEMA_VERSION: u32 = 1;
pub const GH_CONTEXT_SCHEMA_VERSION: u32 = 1;
const MAX_GH_PRODUCER_BYTES: usize = 96;
const MAX_GH_TITLE_BYTES: usize = 16 * 1024;
const MAX_GH_BODY_BYTES: usize = 128 * 1024;
const MAX_GH_COMMENT_BODY_BYTES: usize = 64 * 1024;
const MAX_GH_LIST_ITEMS: usize = 100;
const MAX_GH_LIST_ITEM_BYTES: usize = 1024;

pub const TALLY_UDA_NAMES: &[&str] = &[
    "adapter",
    "labor_class",
    "pool",
    "executor",
    "session_ref",
    "model",
    "cwd",
    "dedup_key",
    "lease_epoch",
    "source",
    "priority_class",
    "attempt",
    "argv_json",
    "evidence_json",
    "parent_uuid",
    "consumption_estimate",
    "runtime_max_sec",
    "no_enqueue",
    "credentials_json",
    "gh_origin_json",
    "evidence_class",
    "manifest_hash",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnqueueSource {
    Manual,
    Orchestrator,
    Calendar,
    EventsDir,
    Gh,
    BuildEffect,
    PoolReachability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GhItemType {
    Issue,
    PullRequest,
}

impl GhItemType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::PullRequest => "pull_request",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhTriggeringComment {
    pub id: String,
    pub author: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhContextSnapshot {
    pub schema_version: u32,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub assignees: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triggering_comment: Option<GhTriggeringComment>,
}

impl GhContextSnapshot {
    pub fn validate(&self) -> Result<(), TaskDbError> {
        if self.schema_version != GH_CONTEXT_SCHEMA_VERSION {
            return Err(TaskDbError::InvalidSeed(format!(
                "GitHub context has unsupported schema version {}",
                self.schema_version
            )));
        }
        validate_gh_text("context title", &self.title, MAX_GH_TITLE_BYTES, false)?;
        validate_gh_text("context body", &self.body, MAX_GH_BODY_BYTES, true)?;
        if self
            .head_sha
            .as_deref()
            .is_some_and(|sha| !valid_gh_sha(sha))
        {
            return Err(TaskDbError::InvalidSeed(
                "GitHub context headSha must be a 40- to 64-character hexadecimal commit SHA"
                    .to_owned(),
            ));
        }
        validate_gh_list("context labels", &self.labels)?;
        validate_gh_list("context assignees", &self.assignees)?;
        if let Some(comment) = &self.triggering_comment {
            validate_gh_scalar("context triggeringComment id", &comment.id)?;
            validate_gh_scalar("context triggeringComment author", &comment.author)?;
            validate_gh_text(
                "context triggeringComment body",
                &comment.body,
                MAX_GH_COMMENT_BODY_BYTES,
                true,
            )?;
        }
        let encoded = serde_json::to_vec(self)?;
        if encoded.len() > MAX_GH_CONTEXT_BYTES {
            return Err(TaskDbError::InvalidSeed(format!(
                "GitHub context exceeds the {MAX_GH_CONTEXT_BYTES} byte limit"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhOrigin {
    pub schema_version: u32,
    pub producer: String,
    pub source: String,
    pub repo: String,
    pub number: u64,
    pub html_url: String,
    pub item_type: Option<GhItemType>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<GhContextSnapshot>,
    pub actor_exclude: String,
    pub allow_self_triggered: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_actors: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct GhOriginWire {
    #[serde(default)]
    schema_version: u32,
    producer: String,
    source: String,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    number: u64,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    item_type: Option<GhItemType>,
    #[serde(default)]
    head_sha: Option<String>,
    #[serde(default, alias = "itemId")]
    node_id: String,
    #[serde(default)]
    item_author: Option<String>,
    #[serde(default)]
    trigger_actor: Option<String>,
    #[serde(default)]
    actor: Option<String>,
    self_actor: String,
    #[serde(default)]
    notification_reason: Option<String>,
    #[serde(default)]
    trigger_kind: String,
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default)]
    comment_id: Option<String>,
    #[serde(default)]
    context: Option<GhContextSnapshot>,
    actor_exclude: String,
    #[serde(default)]
    allow_self_triggered: bool,
    #[serde(default)]
    allowed_actors: Vec<String>,
}

impl<'de> Deserialize<'de> for GhOrigin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = GhOriginWire::deserialize(deserializer)?;
        let legacy_actor = wire.actor.unwrap_or_default();
        Ok(Self {
            schema_version: wire.schema_version,
            producer: wire.producer,
            source: wire.source,
            repo: wire.repo,
            number: wire.number,
            html_url: wire.html_url,
            item_type: wire.item_type,
            head_sha: wire.head_sha,
            node_id: wire.node_id,
            item_author: wire.item_author.unwrap_or_else(|| legacy_actor.clone()),
            trigger_actor: wire.trigger_actor.unwrap_or(legacy_actor),
            self_actor: wire.self_actor,
            notification_reason: wire.notification_reason,
            trigger_kind: wire.trigger_kind,
            event_id: wire.event_id,
            comment_id: wire.comment_id,
            context: wire.context,
            actor_exclude: wire.actor_exclude,
            allow_self_triggered: wire.allow_self_triggered,
            allowed_actors: wire.allowed_actors,
        })
    }
}

impl GhOrigin {
    pub const fn is_current(&self) -> bool {
        self.schema_version == GH_ORIGIN_SCHEMA_VERSION
    }

    pub fn validate(&self) -> Result<(), TaskDbError> {
        let producer_valid = !self.producer.is_empty()
            && self.producer.len() <= MAX_GH_PRODUCER_BYTES
            && self.producer != "."
            && self.producer != ".."
            && self
                .producer
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            && self
                .producer
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
        if !producer_valid {
            return Err(TaskDbError::InvalidSeed(
                "GitHub origin producer is not a safe registry name".to_owned(),
            ));
        }
        for (label, value) in [
            ("source", &self.source),
            ("nodeId", &self.node_id),
            ("itemAuthor", &self.item_author),
            ("triggerActor", &self.trigger_actor),
            ("selfActor", &self.self_actor),
            ("actorExclude", &self.actor_exclude),
        ] {
            validate_gh_scalar(&format!("origin {label}"), value)?;
        }
        if self.schema_version == 0 {
            return Ok(());
        }
        if self.schema_version != GH_ORIGIN_SCHEMA_VERSION {
            return Err(TaskDbError::InvalidSeed(format!(
                "GitHub origin has unsupported schema version {}",
                self.schema_version
            )));
        }
        validate_gh_repo(&self.repo)?;
        if self.number == 0 {
            return Err(TaskDbError::InvalidSeed(
                "GitHub origin number must be positive".to_owned(),
            ));
        }
        validate_gh_scalar("origin triggerKind", &self.trigger_kind)?;
        for (label, value) in [
            ("notificationReason", self.notification_reason.as_deref()),
            ("eventId", self.event_id.as_deref()),
            ("commentId", self.comment_id.as_deref()),
        ] {
            if let Some(value) = value {
                validate_gh_scalar(&format!("origin {label}"), value)?;
            }
        }
        if self.source == "notifications"
            && (self.notification_reason.is_none() || self.event_id.is_none())
        {
            return Err(TaskDbError::InvalidSeed(
                "GitHub notification origin requires notificationReason and eventId".to_owned(),
            ));
        }
        let item_type = self.item_type.ok_or_else(|| {
            TaskDbError::InvalidSeed("GitHub origin requires an item type".to_owned())
        })?;
        match (item_type, self.head_sha.as_deref()) {
            (GhItemType::Issue, None) => {}
            (GhItemType::PullRequest, Some(sha)) if valid_gh_sha(sha) => {}
            (GhItemType::Issue, Some(_)) => {
                return Err(TaskDbError::InvalidSeed(
                    "GitHub issue origin must not carry a PR head SHA".to_owned(),
                ));
            }
            (GhItemType::PullRequest, _) => {
                return Err(TaskDbError::InvalidSeed(
                    "GitHub pull request origin requires a valid head SHA".to_owned(),
                ));
            }
        }
        validate_gh_url(&self.html_url, &self.repo, self.number, item_type)?;
        if self.allowed_actors.len() > MAX_GH_LIST_ITEMS {
            return Err(TaskDbError::InvalidSeed(format!(
                "GitHub origin allowedActors exceeds {MAX_GH_LIST_ITEMS} entries"
            )));
        }
        let mut actors = self.allowed_actors.clone();
        for actor in &actors {
            validate_gh_scalar("origin allowedActors entry", actor)?;
        }
        actors.sort();
        if actors.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(TaskDbError::InvalidSeed(
                "GitHub origin allowedActors contains duplicates".to_owned(),
            ));
        }
        let context = self.context.as_ref().ok_or_else(|| {
            TaskDbError::InvalidSeed("GitHub origin requires a context snapshot".to_owned())
        })?;
        context.validate()?;
        if context.head_sha != self.head_sha {
            return Err(TaskDbError::InvalidSeed(
                "GitHub origin and context headSha must match".to_owned(),
            ));
        }
        match (&self.comment_id, &context.triggering_comment) {
            (Some(comment_id), Some(comment))
                if comment_id == &comment.id && self.trigger_actor == comment.author => {}
            (None, None) => {}
            _ => {
                return Err(TaskDbError::InvalidSeed(
                    "GitHub origin commentId and triggerActor must match the triggering context comment"
                        .to_owned(),
                ));
            }
        }
        let encoded = serde_json::to_vec(self)?;
        if encoded.len() > MAX_DURABLE_EVENT_BYTES as usize {
            return Err(TaskDbError::InvalidSeed(format!(
                "GitHub origin exceeds the {MAX_DURABLE_EVENT_BYTES} byte durable-event limit"
            )));
        }
        Ok(())
    }
}

fn validate_gh_scalar(label: &str, value: &str) -> Result<(), TaskDbError> {
    if value.trim().is_empty()
        || value.len() > MAX_GH_ORIGIN_FIELD_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(TaskDbError::InvalidSeed(format!(
            "GitHub {label} must be non-empty, at most {MAX_GH_ORIGIN_FIELD_BYTES} bytes, and contain no control characters"
        )));
    }
    Ok(())
}

fn validate_gh_text(
    label: &str,
    value: &str,
    limit: usize,
    allow_empty: bool,
) -> Result<(), TaskDbError> {
    if (!allow_empty && value.trim().is_empty()) || value.len() > limit || value.contains('\0') {
        let empty_requirement = if allow_empty { "" } else { "non-empty, " };
        return Err(TaskDbError::InvalidSeed(format!(
            "GitHub {label} must be {empty_requirement}at most {limit} bytes and contain no NUL bytes"
        )));
    }
    Ok(())
}

fn validate_gh_list(label: &str, values: &[String]) -> Result<(), TaskDbError> {
    if values.len() > MAX_GH_LIST_ITEMS {
        return Err(TaskDbError::InvalidSeed(format!(
            "GitHub {label} exceeds {MAX_GH_LIST_ITEMS} entries"
        )));
    }
    for value in values {
        validate_gh_text(label, value, MAX_GH_LIST_ITEM_BYTES, false)?;
    }
    Ok(())
}

fn validate_gh_repo(repo: &str) -> Result<(), TaskDbError> {
    validate_gh_scalar("origin repo", repo)?;
    let Some((owner, name)) = repo.split_once('/') else {
        return Err(TaskDbError::InvalidSeed(
            "GitHub origin repo must be owner/name".to_owned(),
        ));
    };
    let valid_component = |component: &str| {
        !component.is_empty()
            && component
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    };
    if !valid_component(owner) || !valid_component(name) || name.contains('/') {
        return Err(TaskDbError::InvalidSeed(
            "GitHub origin repo must be a safe owner/name pair".to_owned(),
        ));
    }
    Ok(())
}

fn validate_gh_url(
    url: &str,
    repo: &str,
    number: u64,
    item_type: GhItemType,
) -> Result<(), TaskDbError> {
    validate_gh_scalar("origin htmlUrl", url)?;
    let Some(location) = url.strip_prefix("https://") else {
        return Err(TaskDbError::InvalidSeed(
            "GitHub origin htmlUrl must be an absolute HTTPS URL".to_owned(),
        ));
    };
    let Some((host, path)) = location.split_once('/') else {
        return Err(TaskDbError::InvalidSeed(
            "GitHub origin htmlUrl must be an absolute HTTPS URL".to_owned(),
        ));
    };
    let item_segment = match item_type {
        GhItemType::Issue => "issues",
        GhItemType::PullRequest => "pull",
    };
    if host.is_empty() || path != format!("{repo}/{item_segment}/{number}") {
        return Err(TaskDbError::InvalidSeed(
            "GitHub origin htmlUrl does not match repo, number, and itemType".to_owned(),
        ));
    }
    Ok(())
}

fn valid_gh_sha(sha: &str) -> bool {
    (40..=64).contains(&sha.len()) && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionInput {
    pub source: EnqueueSource,
    pub live_orchestrator_spawned: bool,
    pub autonomous: bool,
    pub crash_survivable: bool,
    pub needs_cross_source_urgency: bool,
}

pub fn admits_durable_row(input: &AdmissionInput) -> bool {
    !input.live_orchestrator_spawned
        && (input.autonomous || input.crash_survivable || input.needs_cross_source_urgency)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RowSeed {
    pub uuid: Uuid,
    pub description: String,
    pub priority: Priority,
    pub source: EnqueueSource,
    pub adapter: String,
    #[serde(
        rename = "pool",
        serialize_with = "crate::poolset::serialize",
        deserialize_with = "crate::poolset::deserialize"
    )]
    pub pools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub dedup_key: Option<String>,
    #[serde(default)]
    pub session_ref: Option<String>,
    pub lease_epoch: u64,
    #[serde(default = "default_attempt")]
    pub attempt: u32,
    pub argv: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub parent_uuid: Option<Uuid>,
    #[serde(default)]
    pub consumption_estimate: Option<u64>,
    #[serde(default)]
    pub runtime_max_sec: Option<u64>,
    #[serde(default)]
    pub no_enqueue: bool,
    #[serde(default)]
    pub credentials: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub gh_origin: Option<GhOrigin>,
    #[serde(default)]
    pub evidence_class: Option<Value>,
    #[serde(default)]
    pub manifest_hash: Option<Value>,
}

const fn default_attempt() -> u32 {
    1
}

impl RowSeed {
    pub fn validate(&self) -> Result<(), TaskDbError> {
        if self.description.trim().is_empty() {
            return Err(TaskDbError::InvalidSeed(
                "description must not be empty".to_owned(),
            ));
        }
        if self.argv.is_empty() {
            return Err(TaskDbError::InvalidSeed(
                "argv must contain at least one direct-exec argument".to_owned(),
            ));
        }
        if self.attempt == 0 {
            return Err(TaskDbError::InvalidSeed(
                "attempt must be positive".to_owned(),
            ));
        }
        if self.lease_epoch == 0 {
            return Err(TaskDbError::InvalidSeed(
                "leaseEpoch must be positive".to_owned(),
            ));
        }
        let mut pools = self.pools.clone();
        crate::poolset::canonicalize(&mut pools)
            .map_err(|error| TaskDbError::InvalidSeed(error.to_string()))?;
        if self.adapter.trim().is_empty() || self.adapter.chars().any(char::is_control) {
            return Err(TaskDbError::InvalidSeed(
                "adapter must be non-empty and contain no control characters".to_owned(),
            ));
        }
        if self.executor.as_ref().is_some_and(|executor| {
            executor.is_empty()
                || executor.len() > 96
                || matches!(executor.as_str(), "." | "..")
                || !executor
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                || !executor
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        }) {
            return Err(TaskDbError::InvalidSeed(
                "executor is not a safe registry component".to_owned(),
            ));
        }
        if self.runtime_max_sec == Some(0) {
            return Err(TaskDbError::InvalidSeed(
                "runtimeMaxSec must be positive when set".to_owned(),
            ));
        }
        for (name, source) in &self.credentials {
            let valid_name = !name.is_empty()
                && name.len() <= 255
                && name != "."
                && name != ".."
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
            let valid_source = source.is_absolute()
                && source.to_str().is_some_and(|source| {
                    !source.contains('%') && !source.chars().any(char::is_control)
                });
            if !valid_name || !valid_source {
                return Err(TaskDbError::InvalidSeed(format!(
                    "credential {name:?} is invalid for systemd"
                )));
            }
        }
        if let Some(origin) = &self.gh_origin {
            if self.source != EnqueueSource::Gh {
                return Err(TaskDbError::InvalidSeed(
                    "ghOrigin is valid only for source=gh".to_owned(),
                ));
            }
            origin.validate()?;
        }
        parse_evidence_specs(&self.evidence)
            .map_err(|error| TaskDbError::InvalidSeed(format!("invalid evidence: {error}")))?;
        Ok(())
    }

    pub fn canonicalize(&mut self) -> Result<(), TaskDbError> {
        self.validate()?;
        crate::poolset::canonicalize(&mut self.pools)
            .map_err(|error| TaskDbError::InvalidSeed(error.to_string()))?;
        self.evidence = parse_evidence_specs(&self.evidence)
            .map_err(|error| TaskDbError::InvalidSeed(format!("invalid evidence: {error}")))?
            .render();
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DurableReuse {
    pub matched_witness_seq: u64,
    pub artifact_content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DurableEnqueueEvent {
    pub schema_version: u32,
    pub event_id: Uuid,
    pub acknowledged: bool,
    #[serde(default)]
    pub guardrail_depth: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reuse: Option<DurableReuse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress_id: Option<String>,
    pub row: RowSeed,
}

impl DurableEnqueueEvent {
    pub fn new(row: RowSeed) -> Result<Self, TaskDbError> {
        Self::new_with_depth(row, 0)
    }

    pub fn new_with_depth(mut row: RowSeed, guardrail_depth: u32) -> Result<Self, TaskDbError> {
        row.canonicalize()?;
        Ok(Self {
            schema_version: 1,
            event_id: Uuid::new_v4(),
            acknowledged: true,
            guardrail_depth,
            reuse: None,
            ingress_id: None,
            row,
        })
    }

    pub fn new_reuse_with_depth(
        mut row: RowSeed,
        guardrail_depth: u32,
        matched_witness_seq: u64,
        artifact_content_hash: String,
    ) -> Result<Self, TaskDbError> {
        row.canonicalize()?;
        let event = Self {
            schema_version: 2,
            event_id: Uuid::new_v4(),
            acknowledged: true,
            guardrail_depth,
            reuse: Some(DurableReuse {
                matched_witness_seq,
                artifact_content_hash,
            }),
            ingress_id: None,
            row,
        };
        event.validate()?;
        Ok(event)
    }

    pub fn validate(&self) -> Result<(), TaskDbError> {
        if self.ingress_id.as_ref().is_some_and(|ingress_id| {
            ingress_id.trim().is_empty() || ingress_id.chars().any(char::is_control)
        }) {
            return Err(TaskDbError::InvalidEvent {
                path: PathBuf::from(format!("event: {}", self.event_id)),
                reason: "ingressId must not be empty or contain control characters".to_owned(),
            });
        }
        match (self.schema_version, &self.reuse) {
            (1, None) => {}
            (2, Some(reuse)) => {
                if reuse.matched_witness_seq == 0 {
                    return Err(TaskDbError::InvalidEvent {
                        path: PathBuf::from(format!("event: {}", self.event_id)),
                        reason: "reuse matchedWitnessSeq must be positive".to_owned(),
                    });
                }
                let Some(hash) = reuse.artifact_content_hash.strip_prefix("sha256:") else {
                    return Err(TaskDbError::InvalidEvent {
                        path: PathBuf::from(format!("event: {}", self.event_id)),
                        reason: "reuse artifactContentHash must be sha256:<hex>".to_owned(),
                    });
                };
                if hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err(TaskDbError::InvalidEvent {
                        path: PathBuf::from(format!("event: {}", self.event_id)),
                        reason: "reuse artifactContentHash must be lowercase sha256 hex".to_owned(),
                    });
                }
                if self.row.dedup_key.as_deref().is_none_or(str::is_empty) {
                    return Err(TaskDbError::InvalidEvent {
                        path: PathBuf::from(format!("event: {}", self.event_id)),
                        reason: "reuse event requires a non-empty row dedupKey".to_owned(),
                    });
                }
            }
            (version, _) => {
                return Err(TaskDbError::EventVersion {
                    path: PathBuf::from(format!("event: {}", self.event_id)),
                    version,
                });
            }
        }
        self.row.validate()
    }

    pub fn with_ingress_id(mut self, ingress_id: Option<String>) -> Result<Self, TaskDbError> {
        self.ingress_id = ingress_id;
        self.validate()?;
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub uuid: Uuid,
    pub description: String,
    pub status: Status,
    pub priority: String,
    pub uda: BTreeMap<String, String>,
}

impl TaskRow {
    pub fn value(&self, name: &str) -> Option<&str> {
        self.uda.get(name).map(String::as_str)
    }

    pub fn argv(&self) -> Result<Vec<String>, TaskDbError> {
        let json = self
            .value("argv_json")
            .ok_or_else(|| TaskDbError::InvalidRow("argv_json is absent".to_owned()))?;
        serde_json::from_str(json).map_err(TaskDbError::Json)
    }

    pub fn evidence(&self) -> Result<Vec<String>, TaskDbError> {
        let json = self
            .value("evidence_json")
            .ok_or_else(|| TaskDbError::InvalidRow("evidence_json is absent".to_owned()))?;
        serde_json::from_str(json).map_err(TaskDbError::Json)
    }
}

#[derive(Debug)]
pub struct PreparedRow {
    pub uuid: Uuid,
    operations: Operations,
}

pub struct TaskAdmission {
    pub task_uuid: Option<Uuid>,
    pub prepared: Option<PreparedRow>,
}

#[derive(Debug, Error)]
pub enum TaskDbError {
    #[error("TaskChampion error: {0}")]
    TaskChampion(#[from] taskchampion::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("witness error: {0}")]
    Witness(#[from] WitnessError),
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid durable row seed: {0}")]
    InvalidSeed(String),
    #[error("invalid TaskChampion row: {0}")]
    InvalidRow(String),
    #[error("event {path} has unsupported schema version {version}")]
    EventVersion { path: PathBuf, version: u32 },
    #[error("invalid durable event at {path}: {reason}")]
    InvalidEvent { path: PathBuf, reason: String },
    #[error("witness chain is invalid: {0}")]
    InvalidWitness(String),
}

fn io_error(path: &Path, source: std::io::Error) -> TaskDbError {
    TaskDbError::Io {
        path: path.to_owned(),
        source,
    }
}

pub struct TaskDb {
    replica: Replica<SqliteStorage>,
    taskdata_dir: PathBuf,
    access_mode: AccessMode,
}

impl TaskDb {
    pub async fn open(data_dir: &Path) -> Result<Self, TaskDbError> {
        Self::open_with_mode(
            &data_dir.join(TASKDATA_DIRECTORY),
            AccessMode::ReadWrite,
            true,
        )
        .await
    }

    pub async fn open_read_only(taskdata_dir: &Path) -> Result<Self, TaskDbError> {
        Self::open_with_mode(taskdata_dir, AccessMode::ReadOnly, false).await
    }

    async fn open_with_mode(
        taskdata_dir: &Path,
        access_mode: AccessMode,
        create_if_missing: bool,
    ) -> Result<Self, TaskDbError> {
        let storage = SqliteStorage::new(taskdata_dir, access_mode, create_if_missing).await?;
        Ok(Self {
            replica: Replica::new(storage),
            taskdata_dir: taskdata_dir.to_owned(),
            access_mode,
        })
    }

    pub fn taskdata_dir(&self) -> &Path {
        &self.taskdata_dir
    }

    pub fn access_mode(&self) -> AccessMode {
        self.access_mode
    }

    pub async fn prepare_admission(
        &mut self,
        admission: &AdmissionInput,
        seed: RowSeed,
    ) -> Result<TaskAdmission, TaskDbError> {
        if !admits_durable_row(admission) {
            return Ok(TaskAdmission {
                task_uuid: None,
                prepared: None,
            });
        }
        let uuid = seed.uuid;
        let prepared = self
            .prepare_row(seed, Status::Pending, LaborClass::Fresh)
            .await?;
        Ok(TaskAdmission {
            task_uuid: Some(uuid),
            prepared: Some(prepared),
        })
    }

    pub async fn prepare_row(
        &mut self,
        mut seed: RowSeed,
        status: Status,
        labor_class: LaborClass,
    ) -> Result<PreparedRow, TaskDbError> {
        if self.access_mode != AccessMode::ReadWrite {
            return Err(TaskDbError::InvalidRow(
                "cannot prepare a mutation through a read-only replica".to_owned(),
            ));
        }
        seed.canonicalize()?;
        let mut operations = Operations::new();
        let mut task = self.replica.create_task(seed.uuid, &mut operations).await?;
        populate_task(
            &mut task,
            seed.clone(),
            status,
            labor_class,
            &mut operations,
        )?;

        Ok(PreparedRow {
            uuid: seed.uuid,
            operations,
        })
    }

    pub async fn commit_prepared(
        &mut self,
        prepared: impl IntoIterator<Item = PreparedRow>,
    ) -> Result<usize, TaskDbError> {
        if self.access_mode != AccessMode::ReadWrite {
            return Err(TaskDbError::InvalidRow(
                "cannot commit through a read-only replica".to_owned(),
            ));
        }
        let mut operations = Operations::new();
        let mut count = 0;
        for row in prepared {
            operations.extend(row.operations);
            count += 1;
        }
        self.replica.commit_operations(operations).await?;
        Ok(count)
    }

    pub async fn get_row(&mut self, uuid: Uuid) -> Result<Option<TaskRow>, TaskDbError> {
        self.replica
            .get_task(uuid)
            .await?
            .map(task_to_row)
            .transpose()
    }

    pub async fn all_rows(&mut self) -> Result<HashMap<Uuid, TaskRow>, TaskDbError> {
        let tasks = self.replica.all_tasks().await?;
        tasks
            .into_iter()
            .map(|(uuid, task)| task_to_row(task).map(|row| (uuid, row)))
            .collect()
    }

    pub async fn rebuild_from_sources(
        &mut self,
        events_dir: &Path,
        witness_path: &Path,
    ) -> Result<usize, TaskDbError> {
        let events = read_acknowledged_events(events_dir)?;
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
            return Err(TaskDbError::InvalidWitness(detail));
        }
        let terminal = terminal_witness_by_task(witness);
        let mut authoritative = Vec::new();
        for event in events {
            let mut row = event.row;
            let (status, labor_class) =
                terminal
                    .get(&row.uuid)
                    .map_or((Status::Pending, LaborClass::Fresh), |record| {
                        row.attempt = record.attempt;
                        row.lease_epoch = record.lease_epoch;
                        (status_for_verdict(record.verdict), record.labor_class)
                    });
            authoritative.push((row, status, labor_class));
        }
        self.replace_with_authoritative_rows(authoritative).await
    }

    pub async fn rebuild_from_recovery_plan(
        &mut self,
        plan: &RecoveryPlan,
    ) -> Result<usize, TaskDbError> {
        let authoritative = plan
            .rows
            .iter()
            .map(|row| {
                let status = match row.state {
                    RecoveryRowState::Pending => Status::Pending,
                    RecoveryRowState::Deleted => Status::Deleted,
                    RecoveryRowState::Completed
                    | RecoveryRowState::AdoptedRunning
                    | RecoveryRowState::AwaitingReconciliation => Status::Completed,
                };
                (row.row.clone(), status, row.labor_class)
            })
            .collect();
        self.replace_with_authoritative_rows(authoritative).await
    }

    async fn replace_with_authoritative_rows(
        &mut self,
        rows: Vec<(RowSeed, Status, LaborClass)>,
    ) -> Result<usize, TaskDbError> {
        if self.access_mode != AccessMode::ReadWrite {
            return Err(TaskDbError::InvalidRow(
                "cannot rebuild through a read-only replica".to_owned(),
            ));
        }
        let mut desired = std::collections::HashSet::new();
        for (row, _, _) in &rows {
            if !desired.insert(row.uuid) {
                return Err(TaskDbError::InvalidRow(format!(
                    "authoritative recovery plan repeats row {}",
                    row.uuid
                )));
            }
        }
        let existing = self.replica.all_tasks().await?;
        let mut prepared = Vec::new();
        for (mut row, status, labor_class) in rows {
            row.canonicalize()?;
            let uuid = row.uuid;
            let mut operations = Operations::new();
            let mut task = match self.replica.get_task(uuid).await? {
                Some(task) => task,
                None => self.replica.create_task(uuid, &mut operations).await?,
            };
            let existing_udas = task
                .get_user_defined_attributes()
                .map(|(name, _)| name.to_owned())
                .collect::<Vec<_>>();
            for name in existing_udas {
                task.remove_user_defined_attribute(name, &mut operations)?;
            }
            populate_task(&mut task, row, status, labor_class, &mut operations)?;
            prepared.push(PreparedRow { uuid, operations });
        }
        for (uuid, mut task) in existing {
            if desired.contains(&uuid) || task.get_status() == Status::Deleted {
                continue;
            }
            let mut operations = Operations::new();
            task.set_status(Status::Deleted, &mut operations)?;
            prepared.push(PreparedRow { uuid, operations });
        }
        self.commit_prepared(prepared).await?;
        Ok(desired.len())
    }
}

fn populate_task(
    task: &mut Task,
    seed: RowSeed,
    status: Status,
    labor_class: LaborClass,
    operations: &mut Operations,
) -> Result<(), TaskDbError> {
    task.set_description(seed.description.clone(), operations)?;
    task.set_status(status, operations)?;
    task.set_entry(Some(Utc::now()), operations)?;
    task.set_priority(native_priority(seed.priority).to_owned(), operations)?;

    let mut attributes = BTreeMap::new();
    attributes.insert("adapter", seed.adapter);
    attributes.insert("labor_class", labor_class_name(labor_class).to_owned());
    attributes.insert("pool", crate::poolset::encoded(&seed.pools)?);
    if let Some(executor) = seed.executor {
        attributes.insert("executor", executor);
    }
    attributes.insert("lease_epoch", seed.lease_epoch.to_string());
    attributes.insert("source", source_name(&seed.source).to_owned());
    attributes.insert("priority_class", priority_name(seed.priority).to_owned());
    attributes.insert("attempt", seed.attempt.to_string());
    attributes.insert("argv_json", serde_json::to_string(&seed.argv)?);
    attributes.insert("evidence_json", serde_json::to_string(&seed.evidence)?);
    attributes.insert("no_enqueue", seed.no_enqueue.to_string());
    attributes.insert(
        "credentials_json",
        serde_json::to_string(&seed.credentials)?,
    );
    if let Some(gh_origin) = seed.gh_origin {
        attributes.insert("gh_origin_json", serde_json::to_string(&gh_origin)?);
    }
    if let Some(model) = seed.model {
        attributes.insert("model", model);
    }
    if let Some(cwd) = seed.cwd {
        attributes.insert("cwd", cwd.to_string_lossy().into_owned());
    }
    if let Some(dedup_key) = seed.dedup_key {
        attributes.insert("dedup_key", dedup_key);
    }
    if let Some(session_ref) = seed.session_ref {
        attributes.insert("session_ref", session_ref);
    }
    if let Some(parent_uuid) = seed.parent_uuid {
        attributes.insert("parent_uuid", parent_uuid.to_string());
    }
    if let Some(estimate) = seed.consumption_estimate {
        attributes.insert("consumption_estimate", estimate.to_string());
    }
    if let Some(runtime_max_sec) = seed.runtime_max_sec {
        attributes.insert("runtime_max_sec", runtime_max_sec.to_string());
    }
    if let Some(evidence_class) = seed.evidence_class {
        attributes.insert("evidence_class", serde_json::to_string(&evidence_class)?);
    }
    if let Some(manifest_hash) = seed.manifest_hash {
        attributes.insert("manifest_hash", serde_json::to_string(&manifest_hash)?);
    }
    for (name, value) in attributes {
        task.set_user_defined_attribute(name, value, operations)?;
    }
    Ok(())
}

fn native_priority(priority: Priority) -> &'static str {
    match priority {
        Priority::Interrupt | Priority::High => "H",
        Priority::Medium => "M",
        Priority::Low => "L",
    }
}

fn priority_name(priority: Priority) -> &'static str {
    match priority {
        Priority::Interrupt => "interrupt",
        Priority::High => "high",
        Priority::Medium => "medium",
        Priority::Low => "low",
    }
}

fn labor_class_name(class: LaborClass) -> &'static str {
    match class {
        LaborClass::Fresh => "fresh",
        LaborClass::Recovered => "recovered",
        LaborClass::Reused => "reused",
    }
}

fn source_name(source: &EnqueueSource) -> &'static str {
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

fn task_to_row(task: Task) -> Result<TaskRow, TaskDbError> {
    let mut uda = BTreeMap::new();
    for name in TALLY_UDA_NAMES {
        if let Some(value) = task.get_value(*name) {
            uda.insert((*name).to_owned(), value.to_owned());
        }
    }
    Ok(TaskRow {
        uuid: task.get_uuid(),
        description: task.get_description().to_owned(),
        status: task.get_status(),
        priority: task.get_priority().to_owned(),
        uda,
    })
}

fn status_for_verdict(verdict: Verdict) -> Status {
    match verdict {
        Verdict::Pass
        | Verdict::CleanExitNoArtifact
        | Verdict::Failed
        | Verdict::Reused
        | Verdict::PoolVanished
        | Verdict::Preempted
        | Verdict::RuntimeExceeded => Status::Completed,
        Verdict::Cancelled => Status::Deleted,
    }
}

fn terminal_witness_by_task(records: Vec<WitnessRecord>) -> HashMap<Uuid, WitnessRecord> {
    let mut terminal = HashMap::new();
    for record in records {
        if let Some(uuid) = record
            .task_uuid
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok())
        {
            terminal.insert(uuid, record);
        }
    }
    terminal
}

pub fn write_enqueue_event_atomic(
    events_dir: &Path,
    event: &DurableEnqueueEvent,
) -> Result<PathBuf, TaskDbError> {
    write_enqueue_event_atomic_with_sync(events_dir, event, |directory| {
        File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(|source| io_error(directory, source))
    })
}

fn write_enqueue_event_atomic_with_sync(
    events_dir: &Path,
    event: &DurableEnqueueEvent,
    mut sync_directory: impl FnMut(&Path) -> Result<(), TaskDbError>,
) -> Result<PathBuf, TaskDbError> {
    let mut event = event.clone();
    event.row.canonicalize()?;
    event.validate()?;
    let bytes = serde_json::to_vec(&event)?;
    if bytes.len().saturating_add(1) > MAX_DURABLE_EVENT_BYTES as usize {
        return Err(TaskDbError::InvalidEvent {
            path: events_dir.to_owned(),
            reason: format!("event exceeds the {MAX_DURABLE_EVENT_BYTES} byte durable-event limit"),
        });
    }
    std::fs::create_dir_all(events_dir).map_err(|source| io_error(events_dir, source))?;
    // Always repeat the parent fsync. A previous attempt may have created the
    // directory but reported a failed sync; visibility alone is not durability.
    let parent = events_dir
        .parent()
        .ok_or_else(|| TaskDbError::InvalidEvent {
            path: events_dir.to_owned(),
            reason: "events directory has no parent".to_owned(),
        })?;
    sync_directory(parent)?;
    let final_path = events_dir.join(format!("{}.enqueue.json", event.event_id));
    let temporary_path = events_dir.join(format!(".{}.enqueue.tmp", event.event_id));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary_path)
        .map_err(|source| io_error(&temporary_path, source))?;
    file.write_all(&bytes)
        .map_err(|source| io_error(&temporary_path, source))?;
    file.write_all(b"\n")
        .map_err(|source| io_error(&temporary_path, source))?;
    file.sync_all()
        .map_err(|source| io_error(&temporary_path, source))?;
    std::fs::rename(&temporary_path, &final_path)
        .map_err(|source| io_error(&final_path, source))?;
    sync_directory(events_dir)?;
    Ok(final_path)
}

pub fn read_acknowledged_events(
    events_dir: &Path,
) -> Result<Vec<DurableEnqueueEvent>, TaskDbError> {
    if !events_dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(events_dir).map_err(|source| io_error(events_dir, source))? {
        let path = entry.map_err(|source| io_error(events_dir, source))?.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".enqueue.json"))
        {
            paths.push(path);
        }
    }
    paths.sort();
    let mut events = Vec::new();
    for path in paths {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|source| io_error(&path, source))?;
        let metadata = file.metadata().map_err(|source| io_error(&path, source))?;
        if !metadata.is_file() {
            return Err(TaskDbError::InvalidEvent {
                path,
                reason: "event is not a regular file".to_owned(),
            });
        }
        if metadata.len() > MAX_DURABLE_EVENT_BYTES {
            return Err(TaskDbError::InvalidEvent {
                path,
                reason: format!(
                    "event exceeds the {MAX_DURABLE_EVENT_BYTES} byte durable-event limit"
                ),
            });
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_DURABLE_EVENT_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|source| io_error(&path, source))?;
        if bytes.len() as u64 > MAX_DURABLE_EVENT_BYTES {
            return Err(TaskDbError::InvalidEvent {
                path,
                reason: format!(
                    "event grew beyond the {MAX_DURABLE_EVENT_BYTES} byte durable-event limit while reading"
                ),
            });
        }
        let event: DurableEnqueueEvent = serde_json::from_slice(&bytes)?;
        event.validate().map_err(|error| match error {
            TaskDbError::EventVersion { version, .. } => TaskDbError::EventVersion {
                path: path.clone(),
                version,
            },
            other => TaskDbError::InvalidEvent {
                path: path.clone(),
                reason: other.to_string(),
            },
        })?;
        let expected_name = format!("{}.enqueue.json", event.event_id);
        if path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
            return Err(TaskDbError::InvalidEvent {
                path,
                reason: "file name does not match eventId".to_owned(),
            });
        }
        if event.acknowledged {
            event.row.validate()?;
            let mut canonical = event.row.clone();
            canonical.canonicalize()?;
            if canonical != event.row {
                return Err(TaskDbError::InvalidEvent {
                    path,
                    reason: "acknowledged row is not canonical".to_owned(),
                });
            }
            events.push(event);
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    use super::*;

    fn seed(uuid: Uuid) -> RowSeed {
        RowSeed {
            uuid,
            description: "durable OCR leaf".to_owned(),
            priority: Priority::High,
            source: EnqueueSource::EventsDir,
            adapter: "shell".to_owned(),
            pools: vec!["worker-gpu".to_owned()],
            executor: None,
            model: None,
            cwd: Some(PathBuf::from("/work")),
            dedup_key: Some("ocr:paper-1".to_owned()),
            session_ref: None,
            lease_epoch: 7,
            attempt: 1,
            argv: vec!["ocr".to_owned(), "paper.pdf".to_owned()],
            evidence: vec!["artifact:/work/paper.txt".to_owned()],
            parent_uuid: Some(Uuid::new_v4()),
            consumption_estimate: Some(60),
            runtime_max_sec: Some(300),
            no_enqueue: false,
            credentials: BTreeMap::new(),
            gh_origin: None,
            evidence_class: Some(Value::String("artifact".to_owned())),
            manifest_hash: Some(Value::String("sha256:manifest".to_owned())),
        }
    }

    #[test]
    fn durable_argv_preserves_empty_non_executable_arguments() {
        let mut row = seed(Uuid::new_v4());
        row.argv = vec![
            "agent-input".to_owned(),
            String::new(),
            "--literal".to_owned(),
        ];
        row.validate().unwrap();
        row.argv.clear();
        assert!(row.validate().is_err());
    }

    #[test]
    fn durable_pool_compatibility_migrates_legacy_scalars_and_canonicalizes_multi() {
        let singleton = seed(Uuid::new_v4());
        let scalar = serde_json::to_value(&singleton).unwrap();
        assert_eq!(scalar["pool"], "worker-gpu");
        let restored: RowSeed = serde_json::from_value(scalar).unwrap();
        assert_eq!(restored.pools, ["worker-gpu"]);

        let mut multi = seed(Uuid::new_v4());
        multi.pools = vec!["zeta".to_owned(), "alpha".to_owned()];
        let event = DurableEnqueueEvent::new(multi).unwrap();
        assert_eq!(event.row.pools, ["alpha", "zeta"]);
        let encoded = serde_json::to_value(&event).unwrap();
        assert_eq!(encoded["row"]["pool"], serde_json::json!(["alpha", "zeta"]));
        assert_eq!(
            serde_json::from_value::<DurableEnqueueEvent>(encoded)
                .unwrap()
                .row
                .pools,
            ["alpha", "zeta"]
        );
    }

    #[test]
    fn legacy_github_origin_state_deserializes_without_inventing_current_identity() {
        let origin: GhOrigin = serde_json::from_value(serde_json::json!({
            "producer": "github",
            "source": "notifications",
            "itemId": "I_legacy",
            "actor": "contributor",
            "selfActor": "tally-bot",
            "actorExclude": "self"
        }))
        .unwrap();
        origin.validate().unwrap();
        assert_eq!(origin.schema_version, 0);
        assert_eq!(origin.node_id, "I_legacy");
        assert_eq!(origin.item_author, "contributor");
        assert_eq!(origin.trigger_actor, "contributor");
        assert!(origin.repo.is_empty());
        assert!(origin.item_type.is_none());
        assert!(origin.context.is_none());
    }

    #[test]
    fn github_context_is_versioned_bounded_and_revision_consistent() {
        let context = GhContextSnapshot {
            schema_version: GH_CONTEXT_SCHEMA_VERSION,
            title: "PR context".to_owned(),
            body: "untrusted".to_owned(),
            head_sha: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            labels: vec!["build".to_owned()],
            assignees: vec!["tally-bot".to_owned()],
            triggering_comment: None,
        };
        context.validate().unwrap();

        let mut oversized = context.clone();
        oversized.body = "x".repeat(MAX_GH_BODY_BYTES + 1);
        assert!(oversized.validate().is_err());

        let origin = GhOrigin {
            schema_version: GH_ORIGIN_SCHEMA_VERSION,
            producer: "github".to_owned(),
            source: "notifications".to_owned(),
            repo: "acme/widgets".to_owned(),
            number: 42,
            html_url: "https://github.com/acme/widgets/pull/42".to_owned(),
            item_type: Some(GhItemType::PullRequest),
            head_sha: Some("fedcba9876543210fedcba9876543210fedcba98".to_owned()),
            node_id: "PR_current".to_owned(),
            item_author: "author".to_owned(),
            trigger_actor: "maintainer".to_owned(),
            self_actor: "tally-bot".to_owned(),
            notification_reason: Some("review-requested".to_owned()),
            trigger_kind: "notification".to_owned(),
            event_id: Some("event-42".to_owned()),
            comment_id: None,
            context: Some(context),
            actor_exclude: "self".to_owned(),
            allow_self_triggered: false,
            allowed_actors: Vec::new(),
        };
        assert!(origin
            .validate()
            .unwrap_err()
            .to_string()
            .contains("headSha must match"));
    }

    #[test]
    fn durable_credentials_require_absolute_systemd_safe_sources() {
        let mut row = seed(Uuid::new_v4());
        row.credentials
            .insert("token".to_owned(), PathBuf::from("relative/token"));
        assert!(row
            .validate()
            .unwrap_err()
            .to_string()
            .contains("invalid for systemd"));
    }

    fn durable(source: EnqueueSource) -> AdmissionInput {
        AdmissionInput {
            source,
            live_orchestrator_spawned: false,
            autonomous: true,
            crash_survivable: true,
            needs_cross_source_urgency: true,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn batched_commit_round_trips_static_row_and_parent() {
        let temp = tempfile::tempdir().unwrap();
        let uuid = Uuid::new_v4();
        let parent = seed(uuid).parent_uuid.unwrap();
        let mut row_seed = seed(uuid);
        row_seed.parent_uuid = Some(parent);
        let mut db = TaskDb::open(temp.path()).await.unwrap();
        let admission = db
            .prepare_admission(&durable(EnqueueSource::EventsDir), row_seed)
            .await
            .unwrap();
        assert_eq!(admission.task_uuid, Some(uuid));
        db.commit_prepared([admission.prepared.unwrap()])
            .await
            .unwrap();

        let row = db.get_row(uuid).await.unwrap().unwrap();
        assert_eq!(row.priority, "H");
        assert_eq!(row.value("parent_uuid"), Some(parent.to_string().as_str()));
        assert_eq!(row.argv().unwrap(), ["ocr", "paper.pdf"]);
        assert_eq!(row.evidence().unwrap(), ["artifact:/work/paper.txt"]);
        assert_eq!(row.value("consumption_estimate"), Some("60"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn live_orchestrator_work_has_no_task_uuid() {
        let temp = tempfile::tempdir().unwrap();
        let uuid = Uuid::new_v4();
        let mut db = TaskDb::open(temp.path()).await.unwrap();
        let admission = AdmissionInput {
            source: EnqueueSource::Orchestrator,
            live_orchestrator_spawned: true,
            autonomous: false,
            crash_survivable: false,
            needs_cross_source_urgency: false,
        };
        let result = db.prepare_admission(&admission, seed(uuid)).await.unwrap();
        assert_eq!(result.task_uuid, None);
        assert!(result.prepared.is_none());
        assert!(db.get_row(uuid).await.unwrap().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prepared_row_is_invisible_until_post_ack_commit() {
        let temp = tempfile::tempdir().unwrap();
        let uuid = Uuid::new_v4();
        let taskdata_dir;
        let prepared;
        {
            let mut db = TaskDb::open(temp.path()).await.unwrap();
            taskdata_dir = db.taskdata_dir().to_owned();
            prepared = db
                .prepare_row(seed(uuid), Status::Pending, LaborClass::Fresh)
                .await
                .unwrap();
            assert!(db.get_row(uuid).await.unwrap().is_none());
            db.commit_prepared([prepared]).await.unwrap();
        }
        let mut viewer = TaskDb::open_read_only(&taskdata_dir).await.unwrap();
        assert!(viewer.get_row(uuid).await.unwrap().is_some());
        assert_eq!(viewer.access_mode(), AccessMode::ReadOnly);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_only_replica_refuses_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let taskdata_dir;
        {
            let db = TaskDb::open(temp.path()).await.unwrap();
            taskdata_dir = db.taskdata_dir().to_owned();
        }
        let mut viewer = TaskDb::open_read_only(&taskdata_dir).await.unwrap();
        let error = viewer
            .prepare_row(seed(Uuid::new_v4()), Status::Pending, LaborClass::Fresh)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("read-only"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ack_to_commit_loss_rebuilds_from_event() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state");
        let events = state.join("events");
        let witness = data.join("witness.jsonl");
        let uuid = Uuid::new_v4();
        let event = DurableEnqueueEvent::new(seed(uuid)).unwrap();
        write_enqueue_event_atomic(&events, &event).unwrap();
        let unacked_uuid = Uuid::new_v4();
        let mut unacked = DurableEnqueueEvent::new(seed(unacked_uuid)).unwrap();
        unacked.acknowledged = false;
        write_enqueue_event_atomic(&events, &unacked).unwrap();

        let mut db = TaskDb::open(&data).await.unwrap();
        assert!(db.get_row(uuid).await.unwrap().is_none());
        assert_eq!(db.rebuild_from_sources(&events, &witness).await.unwrap(), 1);
        let row = db.get_row(uuid).await.unwrap().unwrap();
        assert_eq!(row.uuid, uuid);
        assert_eq!(row.value("dedup_key"), Some("ocr:paper-1"));
        assert!(db.get_row(unacked_uuid).await.unwrap().is_none());
    }

    #[test]
    fn enqueue_event_syncs_parent_before_event_directory() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let events = state.join("events");
        std::fs::create_dir_all(&state).unwrap();
        let event = DurableEnqueueEvent::new(seed(Uuid::new_v4())).unwrap();
        let synced = std::cell::RefCell::new(Vec::<PathBuf>::new());
        let path = write_enqueue_event_atomic_with_sync(&events, &event, |directory| {
            synced.borrow_mut().push(directory.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(&*synced.borrow(), &[state.clone(), events.clone()]);
        assert!(path.exists());

        let second = DurableEnqueueEvent::new(seed(Uuid::new_v4())).unwrap();
        let error = write_enqueue_event_atomic_with_sync(&events, &second, |directory| {
            Err(TaskDbError::InvalidEvent {
                path: directory.to_owned(),
                reason: "injected parent sync failure".to_owned(),
            })
        })
        .unwrap_err();
        assert!(error.to_string().contains("injected parent sync failure"));
        assert!(!events
            .join(format!("{}.enqueue.json", second.event_id))
            .exists());
        assert!(!events
            .join(format!(".{}.enqueue.tmp", second.event_id))
            .exists());
    }

    #[test]
    fn durable_event_reader_is_bounded_and_never_blocks_on_a_fifo() {
        let temp = tempfile::tempdir().unwrap();
        let fifo = temp.path().join("hostile.enqueue.json");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        assert!(matches!(
            read_acknowledged_events(temp.path()),
            Err(TaskDbError::InvalidEvent { reason, .. }) if reason.contains("not a regular file")
        ));

        std::fs::remove_file(&fifo).unwrap();
        let oversized = temp.path().join("oversized.enqueue.json");
        let file = File::create(&oversized).unwrap();
        file.set_len(MAX_DURABLE_EVENT_BYTES + 1).unwrap();
        assert!(matches!(
            read_acknowledged_events(temp.path()),
            Err(TaskDbError::InvalidEvent { reason, .. }) if reason.contains("durable-event limit")
        ));

        let unwritten = temp.path().join("unwritten-events");
        let mut event = DurableEnqueueEvent::new(seed(Uuid::new_v4())).unwrap();
        event.row.description = "x".repeat(MAX_DURABLE_EVENT_BYTES as usize);
        assert!(matches!(
            write_enqueue_event_atomic(&unwritten, &event),
            Err(TaskDbError::InvalidEvent { reason, .. }) if reason.contains("durable-event limit")
        ));
        assert!(!unwritten.exists());
    }

    #[test]
    fn deferred_driver_fields_are_not_udas() {
        for forbidden in ["worktree", "trust", "review_state", "dmem"] {
            assert!(!TALLY_UDA_NAMES.contains(&forbidden));
        }
        assert!(TALLY_UDA_NAMES.contains(&"parent_uuid"));
    }

    #[test]
    fn source_rebuild_does_not_invent_automatic_retry_policy() {
        for verdict in [
            Verdict::Pass,
            Verdict::CleanExitNoArtifact,
            Verdict::Failed,
            Verdict::Reused,
            Verdict::PoolVanished,
            Verdict::Preempted,
            Verdict::RuntimeExceeded,
        ] {
            assert_eq!(status_for_verdict(verdict), Status::Completed);
        }
        assert_eq!(status_for_verdict(Verdict::Cancelled), Status::Deleted);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn recovery_plan_overwrites_mutated_cache_and_deletes_unknown_rows() {
        use crate::recovery::{RecoveryPlan, RecoveryRow, RecoveryRowState};

        let temp = tempfile::tempdir().unwrap();
        let desired_uuid = Uuid::new_v4();
        let rogue_uuid = Uuid::new_v4();
        let desired_seed = seed(desired_uuid);
        let mut db = TaskDb::open(temp.path()).await.unwrap();
        let desired = db
            .prepare_row(desired_seed.clone(), Status::Completed, LaborClass::Fresh)
            .await
            .unwrap();
        let rogue = db
            .prepare_row(seed(rogue_uuid), Status::Pending, LaborClass::Fresh)
            .await
            .unwrap();
        db.commit_prepared([desired, rogue]).await.unwrap();

        let mut malicious = Operations::new();
        let mut task = db.replica.get_task(desired_uuid).await.unwrap().unwrap();
        task.set_status(Status::Pending, &mut malicious).unwrap();
        task.set_description("externally mutated".to_owned(), &mut malicious)
            .unwrap();
        task.set_user_defined_attribute("attempt", "999", &mut malicious)
            .unwrap();
        task.set_user_defined_attribute("model", "untrusted", &mut malicious)
            .unwrap();
        task.set_user_defined_attribute("external_only", "untrusted", &mut malicious)
            .unwrap();
        db.replica.commit_operations(malicious).await.unwrap();

        let plan = RecoveryPlan {
            witness_lsn: 7,
            rows: vec![RecoveryRow {
                row: desired_seed.clone(),
                state: RecoveryRowState::Completed,
                labor_class: LaborClass::Fresh,
                guardrail_depth: 0,
            }],
            actions: Vec::new(),
            lease_epoch_fences: Vec::new(),
            advisory_return_attestations: Vec::new(),
        };
        assert_eq!(db.rebuild_from_recovery_plan(&plan).await.unwrap(), 1);
        let restored = db.get_row(desired_uuid).await.unwrap().unwrap();
        assert_eq!(restored.description, desired_seed.description);
        assert_eq!(restored.status, Status::Completed);
        assert_eq!(restored.value("attempt"), Some("1"));
        assert_eq!(restored.value("model"), None);
        assert_eq!(
            db.replica
                .get_task(desired_uuid)
                .await
                .unwrap()
                .unwrap()
                .get_user_defined_attribute("external_only"),
            None
        );
        assert_eq!(
            db.get_row(rogue_uuid).await.unwrap().unwrap().status,
            Status::Deleted
        );
    }

    #[test]
    fn durable_rows_reject_malformed_evidence_before_storage() {
        let mut row = seed(Uuid::new_v4());
        row.evidence = vec!["artifact:".to_owned()];
        assert!(row
            .validate()
            .unwrap_err()
            .to_string()
            .contains("invalid evidence"));
        assert!(DurableEnqueueEvent::new(row).is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_durable_paths_store_only_canonical_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let canonical_hash = format!("hash:sha256:{}", "a".repeat(64));
        let mut direct = seed(Uuid::new_v4());
        direct.evidence = vec![
            format!("hash:sha256:{}", "A".repeat(64)),
            "exit:+0".to_owned(),
        ];
        let event = DurableEnqueueEvent::new(direct.clone()).unwrap();
        assert_eq!(event.row.evidence, [canonical_hash.as_str(), "exit:0"]);

        let events = temp.path().join("events");
        let path = write_enqueue_event_atomic(&events, &event).unwrap();
        let stored: DurableEnqueueEvent =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(stored.row.evidence, [canonical_hash.as_str(), "exit:0"]);

        let mut db = TaskDb::open(temp.path()).await.unwrap();
        let prepared = db
            .prepare_row(direct, Status::Pending, LaborClass::Fresh)
            .await
            .unwrap();
        db.commit_prepared([prepared]).await.unwrap();
        let row = db.get_row(event.row.uuid).await.unwrap().unwrap();
        assert_eq!(
            row.evidence().unwrap(),
            [canonical_hash, "exit:0".to_owned()]
        );
    }
}
