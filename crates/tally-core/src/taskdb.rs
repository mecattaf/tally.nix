use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::adapters::AdapterJobOptions;
use crate::completion::GateManifestSpec;
use crate::config::Priority;
use crate::evidence::parse_evidence_specs;
use crate::occupancy::ContextWindow;
use crate::provenance::Orchestration;
use crate::usage::{UsageAccounting, UsageObservation, UsagePredecessor};
use crate::witness::{Derivation, WitnessError};

pub mod migrations;

const MAX_DURABLE_EVENT_BYTES: u64 = 1024 * 1024;
pub const MAX_GH_ORIGIN_FIELD_BYTES: usize = 4096;
pub const MAX_GH_CONTEXT_BYTES: usize = 256 * 1024;
pub const GH_ORIGIN_SCHEMA_VERSION: u32 = 2;
pub const GH_CONTEXT_SCHEMA_VERSION: u32 = 2;
pub const ADMISSION_ORIGIN_SCHEMA_VERSION: u32 = 1;
pub const CURRENT_ROW_VERSION: u32 = 5;
const MAX_GH_PRODUCER_BYTES: usize = 96;
const MAX_GH_TITLE_BYTES: usize = 16 * 1024;
const MAX_GH_BODY_BYTES: usize = 128 * 1024;
const MAX_GH_COMMENT_BODY_BYTES: usize = 64 * 1024;
const MAX_GH_LIST_ITEMS: usize = 100;
const MAX_GH_LIST_ITEM_BYTES: usize = 1024;

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

impl EnqueueSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Orchestrator => "orchestrator",
            Self::Calendar => "calendar",
            Self::EventsDir => "events-dir",
            Self::Gh => "gh",
            Self::BuildEffect => "build-effect",
            Self::PoolReachability => "pool-reachability",
        }
    }

    pub const fn is_producer(self) -> bool {
        matches!(
            self,
            Self::Calendar
                | Self::EventsDir
                | Self::Gh
                | Self::BuildEffect
                | Self::PoolReachability
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProducerOrigin {
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AdmissionOrigin {
    pub schema_version: u32,
    pub source: EnqueueSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<ProducerOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GhOrigin>,
}

impl AdmissionOrigin {
    pub fn direct(source: EnqueueSource) -> Self {
        Self {
            schema_version: ADMISSION_ORIGIN_SCHEMA_VERSION,
            source,
            producer: None,
            github: None,
        }
    }

    pub fn producer(name: impl Into<String>, source: EnqueueSource) -> Self {
        Self {
            schema_version: ADMISSION_ORIGIN_SCHEMA_VERSION,
            source,
            producer: Some(ProducerOrigin {
                name: name.into(),
                kind: source.as_str().to_owned(),
            }),
            github: None,
        }
    }

    pub fn github(name: impl Into<String>, github: GhOrigin) -> Self {
        let mut origin = Self::producer(name, EnqueueSource::Gh);
        origin.github = Some(github);
        origin
    }

    pub fn validate(&self) -> Result<(), TaskDbError> {
        if self.schema_version != ADMISSION_ORIGIN_SCHEMA_VERSION {
            return Err(TaskDbError::InvalidSeed(format!(
                "origin has unsupported schema version {}",
                self.schema_version
            )));
        }
        if let Some(producer) = &self.producer {
            let valid_name = !producer.name.is_empty()
                && producer.name.len() <= MAX_GH_PRODUCER_BYTES
                && producer.name != "."
                && producer.name != ".."
                && producer
                    .name
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                && producer
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
            if !valid_name {
                return Err(TaskDbError::InvalidSeed(
                    "origin producer name is not a safe registry component".to_owned(),
                ));
            }
            if !self.source.is_producer() || producer.kind != self.source.as_str() {
                return Err(TaskDbError::InvalidSeed(
                    "origin producer kind does not match its admission source".to_owned(),
                ));
            }
        }
        if let Some(github) = &self.github {
            if self.source != EnqueueSource::Gh {
                return Err(TaskDbError::InvalidSeed(
                    "origin github detail is valid only for source=gh".to_owned(),
                ));
            }
            if self.producer.is_none() {
                return Err(TaskDbError::InvalidSeed(
                    "origin github detail requires its generic producer identity".to_owned(),
                ));
            }
            github.validate()?;
            if self
                .producer
                .as_ref()
                .is_some_and(|producer| producer.name != github.producer)
            {
                return Err(TaskDbError::InvalidSeed(
                    "origin producer and nested GitHub producer disagree".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WorkspaceMetadata {
    pub repo: String,
    pub base_rev: String,
    pub branch: String,
    pub worktree_path: PathBuf,
}

impl WorkspaceMetadata {
    pub fn validate(&self) -> Result<(), TaskDbError> {
        for (label, value) in [
            ("repo", self.repo.as_str()),
            ("baseRev", self.base_rev.as_str()),
            ("branch", self.branch.as_str()),
        ] {
            if value.trim().is_empty()
                || value.len() > MAX_GH_ORIGIN_FIELD_BYTES
                || value.contains('\0')
                || value.chars().any(char::is_control)
            {
                return Err(TaskDbError::InvalidSeed(format!(
                    "workspace {label} must be non-empty, bounded, and contain no control characters"
                )));
            }
        }
        validate_absolute_path(&self.worktree_path, "workspace worktreePath")
    }
}

/// The one working directory a submission actually executes in.
///
/// Flow node specs deliberately carry no raw `cwd`; they carry structured
/// workspace metadata instead, and the lane's worktree is where the process
/// must run. Every consumer -- the adapter argv render and the execution
/// request alike -- resolves it through here, so the witnessed argv and the
/// process cwd cannot drift apart the way they did when only the request had
/// the workspace fallback. An explicit payload cwd always wins.
pub fn effective_cwd<'a>(
    cwd: Option<&'a Path>,
    workspace: Option<&'a WorkspaceMetadata>,
) -> Option<&'a Path> {
    cwd.or_else(|| workspace.map(|workspace| workspace.worktree_path.as_path()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GhItemType {
    Issue,
    PullRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GhItemState {
    Open,
    Closed,
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
    pub state: Option<GhItemState>,
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
        if !matches!(self.schema_version, 1 | GH_CONTEXT_SCHEMA_VERSION) {
            return Err(TaskDbError::InvalidSeed(format!(
                "GitHub context has unsupported schema version {}",
                self.schema_version
            )));
        }
        if self.schema_version == GH_CONTEXT_SCHEMA_VERSION && self.state.is_none() {
            return Err(TaskDbError::InvalidSeed(
                "current GitHub context requires an item state".to_owned(),
            ));
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
    pub trigger_timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_value: Option<String>,
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
    trigger_timestamp: Option<String>,
    #[serde(default)]
    trigger_value: Option<String>,
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
            trigger_timestamp: wire.trigger_timestamp,
            trigger_value: wire.trigger_value,
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
        if !matches!(self.schema_version, 1 | GH_ORIGIN_SCHEMA_VERSION) {
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
            ("triggerTimestamp", self.trigger_timestamp.as_deref()),
            ("triggerValue", self.trigger_value.as_deref()),
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
        if self.schema_version == GH_ORIGIN_SCHEMA_VERSION {
            if !matches!(
                self.trigger_kind.as_str(),
                "command-comment" | "mention" | "assignment" | "label"
            ) {
                return Err(TaskDbError::InvalidSeed(
                    "current GitHub origin has an unsupported triggerKind".to_owned(),
                ));
            }
            let timestamp = self.trigger_timestamp.as_deref().ok_or_else(|| {
                TaskDbError::InvalidSeed(
                    "current GitHub origin requires triggerTimestamp".to_owned(),
                )
            })?;
            chrono::DateTime::parse_from_rfc3339(timestamp).map_err(|_| {
                TaskDbError::InvalidSeed(
                    "GitHub origin triggerTimestamp must be RFC 3339".to_owned(),
                )
            })?;
            if self.event_id.is_none() {
                return Err(TaskDbError::InvalidSeed(
                    "current GitHub origin requires eventId".to_owned(),
                ));
            }
            match self.trigger_kind.as_str() {
                "command-comment" | "mention" if self.comment_id.is_none() => {
                    return Err(TaskDbError::InvalidSeed(
                        "comment and mention triggers require commentId".to_owned(),
                    ));
                }
                "assignment" | "label" if self.trigger_value.is_none() => {
                    return Err(TaskDbError::InvalidSeed(
                        "assignment and label triggers require triggerValue".to_owned(),
                    ));
                }
                _ => {}
            }
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
        if self.schema_version == GH_ORIGIN_SCHEMA_VERSION
            && context.schema_version != GH_CONTEXT_SCHEMA_VERSION
        {
            return Err(TaskDbError::InvalidSeed(
                "current GitHub origin requires a current context snapshot".to_owned(),
            ));
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelatedTriggerOutcome {
    NotObserved,
    Filtered,
    MissingContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RelatedTrigger {
    pub producer: String,
    pub event_id: String,
    pub outcome: RelatedTriggerOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<String>,
}

impl RelatedTrigger {
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
                "related trigger producer is not a safe registry name".to_owned(),
            ));
        }
        validate_gh_scalar("related trigger eventId", &self.event_id)?;
        if let Some(receipt_id) = &self.receipt_id {
            validate_gh_scalar("related trigger receiptId", receipt_id)?;
        }
        Ok(())
    }
}

pub fn gh_trigger_receipt_id(origin: &GhOrigin) -> Result<String, TaskDbError> {
    if origin.schema_version != GH_ORIGIN_SCHEMA_VERSION {
        return Err(TaskDbError::InvalidSeed(
            "stable trigger identity requires a current GitHub origin".to_owned(),
        ));
    }
    let identity = match origin.trigger_kind.as_str() {
        "command-comment" | "mention" => origin.comment_id.as_deref().ok_or_else(|| {
            TaskDbError::InvalidSeed("GitHub comment trigger omitted commentId".to_owned())
        })?,
        "assignment" | "label" => origin.event_id.as_deref().ok_or_else(|| {
            TaskDbError::InvalidSeed("GitHub event trigger omitted eventId".to_owned())
        })?,
        _ => {
            return Err(TaskDbError::InvalidSeed(
                "GitHub trigger kind cannot form a stable identity".to_owned(),
            ));
        }
    };
    let mut hash = Sha256::new();
    for part in [
        origin.producer.as_str(),
        origin.repo.as_str(),
        origin.node_id.as_str(),
        origin.trigger_kind.as_str(),
        identity,
    ] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Project a GitHub-triggered parent into the fallback provenance carried by
/// work that it orchestrates. The child did not observe the GitHub event
/// directly, so it retains its honest source and links back to the accepted
/// receipt as `not-observed` provenance.
pub fn related_trigger_from_gh_origin(origin: &GhOrigin) -> Result<RelatedTrigger, TaskDbError> {
    let event_id = match origin.trigger_kind.as_str() {
        "command-comment" | "mention" => origin.comment_id.clone().ok_or_else(|| {
            TaskDbError::InvalidSeed("GitHub comment trigger omitted commentId".to_owned())
        })?,
        "assignment" | "label" => origin.event_id.clone().ok_or_else(|| {
            TaskDbError::InvalidSeed("GitHub event trigger omitted eventId".to_owned())
        })?,
        _ => {
            return Err(TaskDbError::InvalidSeed(
                "GitHub trigger kind cannot form related provenance".to_owned(),
            ));
        }
    };
    let related = RelatedTrigger {
        producer: origin.producer.clone(),
        event_id,
        outcome: RelatedTriggerOutcome::NotObserved,
        receipt_id: Some(gh_trigger_receipt_id(origin)?),
    };
    related.validate()?;
    Ok(related)
}

pub fn gh_trigger_dedup_key(origin: &GhOrigin) -> Result<String, TaskDbError> {
    Ok(format!(
        "gh:{}:{}",
        origin.producer,
        gh_trigger_receipt_id(origin)?
    ))
}

pub fn gh_trigger_task_uuid(origin: &GhOrigin) -> Result<Uuid, TaskDbError> {
    let receipt = gh_trigger_receipt_id(origin)?;
    let mut bytes = [0_u8; 16];
    for (index, pair) in receipt.as_bytes().chunks_exact(2).take(16).enumerate() {
        let pair = std::str::from_utf8(pair).expect("receipt IDs are ASCII hexadecimal");
        bytes[index] =
            u8::from_str_radix(pair, 16).expect("receipt IDs contain only hexadecimal characters");
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let encoded = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    );
    Uuid::parse_str(&encoded)
        .map_err(|_| TaskDbError::InvalidSeed("stable GitHub trigger UUID is invalid".to_owned()))
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

/// Where an attempt ran, as recorded beside the session pointer it produced.
///
/// Two facts a bare `Option<PathBuf>` cannot hold apart: a row that declared a
/// working directory, and a row that declared none and therefore ran wherever
/// the service manager put the daemon. Both are records. The absence of a
/// record is the *outer* `Option` on [`RowSeed::session_cwd`], and it means
/// something else again: nobody wrote one down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedLaunchCwd {
    /// The attempt's unit was given this working directory verbatim.
    In(PathBuf),
    /// The attempt declared no working directory, so its unit inherited the
    /// daemon's. A later attempt that also declares none inherits the same one.
    ServiceManagerDefault,
}

impl RecordedLaunchCwd {
    /// Record an attempt's effective cwd, absence included.
    #[must_use]
    pub fn of(effective_cwd: Option<&Path>) -> Self {
        effective_cwd.map_or(Self::ServiceManagerDefault, |cwd| {
            Self::In(cwd.to_path_buf())
        })
    }

    /// The declared directory, if one was declared.
    #[must_use]
    pub fn declared(&self) -> Option<&Path> {
        match self {
            Self::In(cwd) => Some(cwd),
            Self::ServiceManagerDefault => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RowSeed {
    #[serde(default = "default_row_version")]
    pub row_version: u32,
    pub uuid: Uuid,
    pub description: String,
    pub priority: Priority,
    pub source: EnqueueSource,
    pub adapter: String,
    #[serde(
        rename = "pool",
        serialize_with = "crate::poolset::serialize_array",
        deserialize_with = "crate::poolset::deserialize"
    )]
    pub pools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceMetadata>,
    #[serde(default, skip_serializing_if = "AdapterJobOptions::is_default")]
    pub adapter_options: AdapterJobOptions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_manifest: Option<GateManifestSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_from: Option<String>,
    #[serde(default)]
    pub dedup_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration: Option<Orchestration>,
    #[serde(default)]
    pub session_ref: Option<String>,
    /// Exact attempt whose session counters this row's current invocation
    /// continues. Absence means the current invocation is fresh; attempt
    /// number alone is never a resume signal.
    ///
    /// Unlike scraped observations this is durable admission/execution
    /// metadata. Recovery needs it before the next scrape exists, both to
    /// reconstruct a resumed argv and to refuse cumulative accounting when
    /// the named predecessor evidence is missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_predecessor: Option<UsagePredecessor>,
    /// Where the attempt that yielded `session_ref` ran, recorded beside the
    /// pointer at the moment the pointer was observed.
    ///
    /// A harness that resolves a session by its launch directory — `pi` is the
    /// measured one, see [`crate::adapters::AdapterConfig::resume_requires_launch_cwd`] —
    /// cannot reach that session from anywhere else, so the pointer alone is
    /// not enough to resume: what is missing is where it points *from*.
    ///
    /// The outer `Option` and the inner variant are two different facts and are
    /// deliberately not collapsed. `None` is **no record**: a row whose pointer
    /// predates this field, or one whose pointer arrived without one.
    /// `Some(ServiceManagerDefault)` is a record that says the attempt declared
    /// no directory and therefore ran wherever the service manager put the
    /// daemon — which is a place a later attempt declaring none reaches too. A
    /// single `None` for both would refuse that continuation and blame a
    /// missing record for it.
    ///
    /// **Deliberately transport-only: `#[serde(skip)]`, so no write path can
    /// persist it and no wire shape widens.** That is not a shortcut around a
    /// row migration; it is the same rule `session_ref` itself lives under.
    /// Neither survives a restart as bytes — startup re-derives both from the
    /// retained captures and the durable row that produced them, so this field
    /// is exactly as durable as the pointer it qualifies, and never more.
    #[serde(skip)]
    pub session_cwd: Option<RecordedLaunchCwd>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_message: Option<String>,
    /// Normalized usage for the attempt this row last recorded a scrape for.
    /// Absent means no attempt has been scraped yet; a present typed absence
    /// means an attempt was scraped and carried no usage. Rows written before
    /// this field existed read back as the former, which is what they were.
    ///
    /// **Deliberately transport-only: no write path ever persists it.** The
    /// daemon sets it on the in-memory row and on the query detail; the
    /// durable seat for a usage record is the advisory attestation ledger,
    /// keyed by task, attempt, and lease epoch. That is what makes it correct
    /// for this field to remain absent from serialized rows. Row version 5
    /// exists for the durable `usage_predecessor` beside it, not to make this
    /// projection durable. Anything that starts persisting `usage` owes a
    /// later versioned migration and an N−1 fixture first
    /// (`crate::taskdb::migrations`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageObservation>,
    /// Per-attempt reduction corresponding to `usage`, projected in memory
    /// from the durable scrape attestation. The attestation is its durable
    /// seat; enqueue writers always carry `None` here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_accounting: Option<UsageAccounting>,
    /// Occupancy computed alongside `usage` at the same scrape, kept
    /// separate because it answers a different question ("can this session
    /// absorb another task", never "what did this attempt cost"). Transport-
    /// only for the exact reason `usage` is: no write path persists it, both
    /// are recomputable from the adapter configuration and the retained
    /// captures, and adding them owed no row migration because nothing here
    /// widens what an enqueue event may carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<ContextWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_token_hash: Option<String>,
    pub lease_epoch: u64,
    #[serde(default = "default_attempt")]
    pub attempt: u32,
    pub argv: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drv: Option<Derivation>,
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
    pub origin: Option<AdmissionOrigin>,
    /// Protocol-2 compatibility input. Current rows also carry this identity
    /// beneath `origin.github`.
    #[serde(default)]
    pub gh_origin: Option<GhOrigin>,
    #[serde(default)]
    pub related_trigger: Option<RelatedTrigger>,
    #[serde(default)]
    pub evidence_class: Option<Value>,
    #[serde(default)]
    pub manifest_hash: Option<Value>,
}

const fn default_row_version() -> u32 {
    1
}

const fn default_attempt() -> u32 {
    1
}

fn validate_absolute_path(path: &Path, label: &str) -> Result<(), TaskDbError> {
    if !path.is_absolute() {
        return Err(TaskDbError::InvalidSeed(format!(
            "{label} must be absolute"
        )));
    }
    let path = path
        .to_str()
        .ok_or_else(|| TaskDbError::InvalidSeed(format!("{label} must be valid UTF-8")))?;
    if path.contains('%') || path.contains('\0') || path.chars().any(char::is_control) {
        return Err(TaskDbError::InvalidSeed(format!(
            "{label} must contain no control characters or systemd specifiers"
        )));
    }
    Ok(())
}

impl RowSeed {
    /// See [`effective_cwd`]: the durable row's working directory, workspace
    /// fallback included.
    pub fn effective_cwd(&self) -> Option<&Path> {
        effective_cwd(self.cwd.as_deref(), self.workspace.as_ref())
    }

    /// Record where this row's attempt ran, beside the session pointer its
    /// stream just yielded. Called at every seam that writes `session_ref`
    /// from a scrape, so the two are always written together or not at all.
    ///
    /// The directory is this row's own effective cwd because this row is the
    /// attempt that produced the pointer: the executor passes
    /// [`Self::effective_cwd`] to the unit verbatim as its working directory,
    /// so it is the launch directory rather than a guess at one. A row that
    /// declares none records that fact rather than recording nothing — see
    /// [`RecordedLaunchCwd`].
    pub fn record_session_launch_cwd(&mut self) {
        self.session_cwd = Some(RecordedLaunchCwd::of(self.effective_cwd()));
    }

    pub fn validate(&self) -> Result<(), TaskDbError> {
        if self.row_version == 0 || self.row_version > CURRENT_ROW_VERSION {
            return Err(TaskDbError::InvalidSeed(format!(
                "rowVersion {} is unsupported; current rowVersion is {CURRENT_ROW_VERSION}",
                self.row_version
            )));
        }
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
        if self.payload_hash.as_ref().is_some_and(|payload_hash| {
            payload_hash.len() != 71
                || !payload_hash.starts_with("sha256:")
                || !payload_hash[7..]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err(TaskDbError::InvalidSeed(
                "payloadHash must be lowercase sha256 hex".to_owned(),
            ));
        }
        if self.brief_hash.as_ref().is_some_and(|brief_hash| {
            brief_hash.len() != 71
                || !brief_hash.starts_with("sha256:")
                || !brief_hash[7..]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err(TaskDbError::InvalidSeed(
                "briefHash must be lowercase sha256 hex".to_owned(),
            ));
        }
        if self.job_token_hash.as_ref().is_some_and(|job_token_hash| {
            job_token_hash.len() != 71
                || !job_token_hash.starts_with("sha256:")
                || !job_token_hash[7..]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err(TaskDbError::InvalidSeed(
                "jobTokenHash must be lowercase sha256 hex".to_owned(),
            ));
        }
        if let Some(orchestration) = &self.orchestration {
            orchestration.validate().map_err(TaskDbError::InvalidSeed)?;
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
        if let Some(cwd) = &self.cwd {
            validate_absolute_path(cwd, "cwd")?;
        }
        if let Some(workspace) = &self.workspace {
            workspace.validate()?;
        }
        if let Some(gate_manifest) = &self.gate_manifest {
            gate_manifest
                .validate()
                .map_err(|error| TaskDbError::InvalidSeed(error.to_string()))?;
        }
        if let Some(resumed_from) = &self.resumed_from {
            Uuid::parse_str(resumed_from).map_err(|_| {
                TaskDbError::InvalidSeed("resumedFrom must be a task UUID".to_owned())
            })?;
        }
        if let Some(predecessor) = &self.usage_predecessor {
            Uuid::parse_str(&predecessor.task_uuid).map_err(|_| {
                TaskDbError::InvalidSeed("usagePredecessor.taskUuid must be a task UUID".to_owned())
            })?;
            if predecessor.attempt == 0 || predecessor.lease_epoch == 0 {
                return Err(TaskDbError::InvalidSeed(
                    "usagePredecessor attempt and leaseEpoch must be positive".to_owned(),
                ));
            }
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
        if let Some(origin) = &self.origin {
            if origin.source != self.source {
                return Err(TaskDbError::InvalidSeed(
                    "origin source does not match row source".to_owned(),
                ));
            }
            origin.validate()?;
            if self.gh_origin.as_ref() != origin.github.as_ref() {
                return Err(TaskDbError::InvalidSeed(
                    "legacy ghOrigin and nested origin github detail disagree".to_owned(),
                ));
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
        if let Some(related) = &self.related_trigger {
            if self.source == EnqueueSource::Gh {
                return Err(TaskDbError::InvalidSeed(
                    "relatedTrigger is fallback provenance and is invalid for source=gh".to_owned(),
                ));
            }
            related.validate()?;
        }
        let evidence = parse_evidence_specs(&self.evidence)
            .map_err(|error| TaskDbError::InvalidSeed(format!("invalid evidence: {error}")))?;
        if let Some(drv) = &self.drv {
            drv.validate().map_err(TaskDbError::InvalidSeed)?;
            let expected_key = format!("drv:{}", drv.drv_path);
            if self.pools != ["build"]
                || self.dedup_key.as_deref() != Some(expected_key.as_str())
                || evidence.declared_store_paths() != drv.output_paths()
            {
                return Err(TaskDbError::InvalidSeed(
                    "drv rows require pool [\"build\"], dedupKey drv:<drvPath>, and store evidence exactly matching all outputs"
                        .to_owned(),
                ));
            }
        }
        Ok(())
    }

    pub fn canonicalize(&mut self) -> Result<(), TaskDbError> {
        if self.origin.is_none() {
            self.origin = Some(match &self.gh_origin {
                Some(github) => AdmissionOrigin::github(&github.producer, github.clone()),
                None => AdmissionOrigin::direct(self.source),
            });
        }
        if self.gh_origin.is_none() {
            self.gh_origin = self
                .origin
                .as_ref()
                .and_then(|origin| origin.github.clone());
        }
        self.validate()?;
        crate::poolset::canonicalize(&mut self.pools)
            .map_err(|error| TaskDbError::InvalidSeed(error.to_string()))?;
        self.evidence = parse_evidence_specs(&self.evidence)
            .map_err(|error| TaskDbError::InvalidSeed(format!("invalid evidence: {error}")))?
            .render();
        if let Some(drv) = &mut self.drv {
            drv.canonicalize().map_err(TaskDbError::InvalidSeed)?;
        }
        Ok(())
    }

    fn canonicalize_for_current_write(&mut self) -> Result<(), TaskDbError> {
        if self.row_version == 0 || self.row_version > CURRENT_ROW_VERSION {
            return Err(TaskDbError::InvalidSeed(format!(
                "rowVersion {} is unsupported; current rowVersion is {CURRENT_ROW_VERSION}",
                self.row_version
            )));
        }
        self.row_version = CURRENT_ROW_VERSION;
        self.canonicalize()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DurableReuse {
    pub matched_witness_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DurableRetry {
    pub attempt: u32,
    pub previous_witness_seq: u64,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retries: Vec<DurableRetry>,
    pub row: RowSeed,
}

impl DurableEnqueueEvent {
    pub fn new(row: RowSeed) -> Result<Self, TaskDbError> {
        Self::new_with_depth(row, 0)
    }

    pub fn new_with_depth(mut row: RowSeed, guardrail_depth: u32) -> Result<Self, TaskDbError> {
        row.canonicalize_for_current_write()?;
        Ok(Self {
            schema_version: 1,
            event_id: Uuid::new_v4(),
            acknowledged: true,
            guardrail_depth,
            reuse: None,
            ingress_id: None,
            retries: Vec::new(),
            row,
        })
    }

    pub fn new_reuse_with_depth(
        mut row: RowSeed,
        guardrail_depth: u32,
        matched_witness_seq: u64,
        artifact_content_hash: Option<String>,
        store_paths: Option<Vec<String>>,
    ) -> Result<Self, TaskDbError> {
        row.canonicalize_for_current_write()?;
        let event = Self {
            schema_version: 3,
            event_id: Uuid::new_v4(),
            acknowledged: true,
            guardrail_depth,
            reuse: Some(DurableReuse {
                matched_witness_seq,
                artifact_content_hash,
                store_paths,
            }),
            ingress_id: None,
            retries: Vec::new(),
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
            (2 | 3, Some(reuse)) => {
                if reuse.matched_witness_seq == 0 {
                    return Err(TaskDbError::InvalidEvent {
                        path: PathBuf::from(format!("event: {}", self.event_id)),
                        reason: "reuse matchedWitnessSeq must be positive".to_owned(),
                    });
                }
                if self.schema_version == 2
                    && (reuse.artifact_content_hash.is_none() || reuse.store_paths.is_some())
                {
                    return Err(TaskDbError::InvalidEvent {
                        path: PathBuf::from(format!("event: {}", self.event_id)),
                        reason: "schemaVersion 2 reuse requires only artifactContentHash"
                            .to_owned(),
                    });
                }
                if self.schema_version == 3
                    && reuse.artifact_content_hash.is_none()
                    && reuse.store_paths.is_none()
                {
                    return Err(TaskDbError::InvalidEvent {
                        path: PathBuf::from(format!("event: {}", self.event_id)),
                        reason: "schemaVersion 3 reuse requires artifactContentHash or storePaths"
                            .to_owned(),
                    });
                }
                if let Some(artifact_content_hash) = &reuse.artifact_content_hash {
                    let Some(hash) = artifact_content_hash.strip_prefix("sha256:") else {
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
                            reason: "reuse artifactContentHash must be lowercase sha256 hex"
                                .to_owned(),
                        });
                    }
                }
                if let Some(store_paths) = &reuse.store_paths {
                    if store_paths.is_empty()
                        || store_paths
                            .iter()
                            .any(|path| !crate::witness::is_nix_store_path(path))
                        || store_paths.windows(2).any(|pair| pair[0] >= pair[1])
                    {
                        return Err(TaskDbError::InvalidEvent {
                            path: PathBuf::from(format!("event: {}", self.event_id)),
                            reason: "reuse storePaths must be non-empty, valid, sorted, and unique"
                                .to_owned(),
                        });
                    }
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
        let mut previous_attempt = self.row.attempt;
        let mut previous_witness_seq = 0;
        for retry in &self.retries {
            if retry.attempt <= previous_attempt {
                return Err(TaskDbError::InvalidEvent {
                    path: PathBuf::from(format!("event: {}", self.event_id)),
                    reason: "retry attempts must be positive and increasing".to_owned(),
                });
            }
            if retry.previous_witness_seq == 0 || retry.previous_witness_seq <= previous_witness_seq
            {
                return Err(TaskDbError::InvalidEvent {
                    path: PathBuf::from(format!("event: {}", self.event_id)),
                    reason: "retry previousWitnessSeq values must be positive and increasing"
                        .to_owned(),
                });
            }
            previous_attempt = retry.attempt;
            previous_witness_seq = retry.previous_witness_seq;
        }
        self.row.validate()
    }

    pub fn with_ingress_id(mut self, ingress_id: Option<String>) -> Result<Self, TaskDbError> {
        self.ingress_id = ingress_id;
        self.validate()?;
        Ok(self)
    }
}

// `TaskStatus`, `TaskRow`, and `impl From<&TaskRow> for RowFact` stood here until
// the TaskChampion projection was deleted. Nothing in the workspace ever
// constructed them afterwards — query rows are built exclusively from `RowSeed`
// through `daemon/rpc/query.rs` and `daemon/startup.rs` — and `pub` in a library
// crate means `dead_code` never fired on them. An unexercised row type next to
// the durable event types reads as a live contract; it was not one.

#[derive(Debug, Error)]
pub enum TaskDbError {
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
    #[error("event {path} has unsupported schema version {version}")]
    EventVersion { path: PathBuf, version: u32 },
    #[error("invalid durable event at {path}: {reason}")]
    InvalidEvent { path: PathBuf, reason: String },
}

fn io_error(path: &Path, source: std::io::Error) -> TaskDbError {
    TaskDbError::Io {
        path: path.to_owned(),
        source,
    }
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

pub fn update_enqueue_event_atomic(
    events_dir: &Path,
    event: &DurableEnqueueEvent,
) -> Result<PathBuf, TaskDbError> {
    let mut event = event.clone();
    event.row.canonicalize()?;
    if event.row.row_version != CURRENT_ROW_VERSION {
        return Err(TaskDbError::InvalidEvent {
            path: events_dir.to_owned(),
            reason: format!(
                "rowVersion {} requires the ordered startup migration to rowVersion {CURRENT_ROW_VERSION}",
                event.row.row_version
            ),
        });
    }
    event.validate()?;
    let bytes = serde_json::to_vec(&event)?;
    if bytes.len().saturating_add(1) > MAX_DURABLE_EVENT_BYTES as usize {
        return Err(TaskDbError::InvalidEvent {
            path: events_dir.to_owned(),
            reason: format!("event exceeds the {MAX_DURABLE_EVENT_BYTES} byte durable-event limit"),
        });
    }
    std::fs::create_dir_all(events_dir).map_err(|source| io_error(events_dir, source))?;
    let final_path = events_dir.join(format!("{}.enqueue.json", event.event_id));
    if !final_path.exists() {
        return Err(TaskDbError::InvalidEvent {
            path: final_path,
            reason: "cannot update a missing acknowledged enqueue event".to_owned(),
        });
    }
    let temporary_path = events_dir.join(format!(
        ".{}.{}.enqueue.tmp",
        event.event_id,
        Uuid::new_v4()
    ));
    let write_result = (|| {
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
        File::open(events_dir)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| io_error(events_dir, source))?;
        Ok(final_path.clone())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    write_result
}

fn write_enqueue_event_atomic_with_sync(
    events_dir: &Path,
    event: &DurableEnqueueEvent,
    mut sync_directory: impl FnMut(&Path) -> Result<(), TaskDbError>,
) -> Result<PathBuf, TaskDbError> {
    let mut event = event.clone();
    event.row.canonicalize_for_current_write()?;
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

fn enqueue_event_paths(events_dir: &Path) -> Result<Vec<PathBuf>, TaskDbError> {
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
    Ok(paths)
}

fn read_enqueue_event(path: &Path) -> Result<DurableEnqueueEvent, TaskDbError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !metadata.is_file() {
        return Err(TaskDbError::InvalidEvent {
            path: path.to_owned(),
            reason: "event is not a regular file".to_owned(),
        });
    }
    if metadata.len() > MAX_DURABLE_EVENT_BYTES {
        return Err(TaskDbError::InvalidEvent {
            path: path.to_owned(),
            reason: format!("event exceeds the {MAX_DURABLE_EVENT_BYTES} byte durable-event limit"),
        });
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_DURABLE_EVENT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.len() as u64 > MAX_DURABLE_EVENT_BYTES {
        return Err(TaskDbError::InvalidEvent {
            path: path.to_owned(),
            reason: format!(
                "event grew beyond the {MAX_DURABLE_EVENT_BYTES} byte durable-event limit while reading"
            ),
        });
    }
    serde_json::from_slice(&bytes).map_err(TaskDbError::Json)
}

fn validate_enqueue_event_path(
    path: &Path,
    event: &DurableEnqueueEvent,
) -> Result<(), TaskDbError> {
    event.validate().map_err(|error| match error {
        TaskDbError::EventVersion { version, .. } => TaskDbError::EventVersion {
            path: path.to_owned(),
            version,
        },
        other => TaskDbError::InvalidEvent {
            path: path.to_owned(),
            reason: other.to_string(),
        },
    })?;
    let expected_name = format!("{}.enqueue.json", event.event_id);
    if path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
        return Err(TaskDbError::InvalidEvent {
            path: path.to_owned(),
            reason: "file name does not match eventId".to_owned(),
        });
    }
    Ok(())
}

pub fn migrate_acknowledged_events(events_dir: &Path) -> Result<usize, TaskDbError> {
    let mut rewrites = Vec::new();
    for path in enqueue_event_paths(events_dir)? {
        let event = read_enqueue_event(&path)?;
        validate_enqueue_event_path(&path, &event)?;
        if !event.acknowledged {
            continue;
        }
        let migrated = migrations::migrate_to_current(&event.row).map_err(|reason| {
            TaskDbError::InvalidEvent {
                path: path.clone(),
                reason,
            }
        })?;
        let mut canonical = migrated.clone();
        canonical
            .canonicalize()
            .map_err(|error| TaskDbError::InvalidEvent {
                path: path.clone(),
                reason: error.to_string(),
            })?;
        if canonical != migrated {
            return Err(TaskDbError::InvalidEvent {
                path,
                reason: "acknowledged row is not canonical after migration".to_owned(),
            });
        }
        if migrated != event.row {
            let mut migrated_event = event;
            migrated_event.row = migrated;
            rewrites.push(migrated_event);
        }
    }

    // Classification above completes before the first mutation. Per-file
    // replacement is atomic, so a crash leaves only untouched files for the
    // next ordered startup pass.
    for event in &rewrites {
        update_enqueue_event_atomic(events_dir, event)?;
    }
    Ok(rewrites.len())
}

pub fn read_acknowledged_events(
    events_dir: &Path,
) -> Result<Vec<DurableEnqueueEvent>, TaskDbError> {
    let mut events = Vec::new();
    for path in enqueue_event_paths(events_dir)? {
        let event = read_enqueue_event(&path)?;
        validate_enqueue_event_path(&path, &event)?;
        if event.acknowledged {
            if event.row.row_version != CURRENT_ROW_VERSION {
                return Err(TaskDbError::InvalidEvent {
                    path,
                    reason: format!(
                        "rowVersion {} requires the ordered startup migration to rowVersion {CURRENT_ROW_VERSION}",
                        event.row.row_version
                    ),
                });
            }
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
    use std::fs;
    use std::os::unix::ffi::OsStrExt;

    use proptest::prelude::*;

    use super::*;

    const LEGACY_NO_ORIGIN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test/fixtures/ledger/events/legacy-no-origin.enqueue.json"
    ));
    const LEGACY_BAD_POOLS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test/fixtures/ledger/events/legacy-bad-pools.enqueue.json"
    ));
    const LEGACY_BAD_EVIDENCE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test/fixtures/ledger/events/legacy-bad-evidence.enqueue.json"
    ));
    const LEGACY_BAD_ORIGIN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test/fixtures/ledger/events/legacy-bad-origin.enqueue.json"
    ));
    const LEGACY_NO_DRV: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test/fixtures/ledger/events/legacy-no-drv.enqueue.json"
    ));
    const LEGACY_NO_JOB_TOKEN_HASH: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test/fixtures/ledger/events/legacy-no-job-token-hash.enqueue.json"
    ));
    const LEGACY_GH_ORIGIN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test/fixtures/ledger/events/legacy-gh-origin.enqueue.json"
    ));

    fn install_literal_event(events_dir: &Path, bytes: &[u8]) -> PathBuf {
        let value: Value = serde_json::from_slice(bytes).unwrap();
        let event_id = value["eventId"].as_str().unwrap();
        fs::create_dir_all(events_dir).unwrap();
        let path = events_dir.join(format!("{event_id}.enqueue.json"));
        fs::write(&path, bytes).unwrap();
        path
    }

    fn enqueue_bytes(events_dir: &Path) -> BTreeMap<String, Vec<u8>> {
        fs::read_dir(events_dir)
            .unwrap()
            .filter_map(|entry| {
                let path = entry.unwrap().path();
                let name = path.file_name()?.to_str()?.to_owned();
                name.ends_with(".enqueue.json")
                    .then(|| (name, fs::read(path).unwrap()))
            })
            .collect()
    }

    fn seed(uuid: Uuid) -> RowSeed {
        RowSeed {
            row_version: CURRENT_ROW_VERSION,
            uuid,
            description: "durable OCR leaf".to_owned(),
            priority: Priority::High,
            source: EnqueueSource::EventsDir,
            adapter: "shell".to_owned(),
            pools: vec!["worker-gpu".to_owned()],
            executor: None,
            model: None,
            cwd: Some(PathBuf::from("/work")),
            workspace: None,
            adapter_options: Default::default(),
            gate_manifest: None,
            resumed_from: None,
            dedup_key: Some("ocr:paper-1".to_owned()),
            payload_hash: None,
            brief_hash: None,
            orchestration: None,
            session_ref: None,
            usage_predecessor: None,
            session_cwd: None,
            final_message: None,
            usage_accounting: None,
            job_token_hash: None,
            lease_epoch: 7,
            attempt: 1,
            argv: vec!["ocr".to_owned(), "paper.pdf".to_owned()],
            evidence: vec!["artifact:/work/paper.txt".to_owned()],
            drv: None,
            parent_uuid: Some(Uuid::new_v4()),
            consumption_estimate: Some(60),
            runtime_max_sec: Some(300),
            no_enqueue: false,
            credentials: BTreeMap::new(),
            origin: None,
            gh_origin: None,
            related_trigger: None,
            evidence_class: Some(Value::String("artifact".to_owned())),
            manifest_hash: Some(Value::String("sha256:manifest".to_owned())),
            usage: None,
            context_tokens: None,
            context_window: None,
        }
    }

    fn property_source(selector: u8) -> EnqueueSource {
        match selector % 6 {
            0 => EnqueueSource::Manual,
            1 => EnqueueSource::Orchestrator,
            2 => EnqueueSource::Calendar,
            3 => EnqueueSource::EventsDir,
            4 => EnqueueSource::BuildEffect,
            5 => EnqueueSource::PoolReachability,
            _ => unreachable!(),
        }
    }

    fn property_seed(
        ids: (u128, u128),
        source: u8,
        pool_ids: &std::collections::BTreeSet<u16>,
        ordering: (usize, bool),
        evidence: (u8, [u8; 32]),
    ) -> RowSeed {
        let (uuid, parent_uuid) = ids;
        let (rotation, reversed) = ordering;
        let (exit_code, hash_bytes) = evidence;
        let mut row = seed(Uuid::from_u128(uuid));
        row.parent_uuid = Some(Uuid::from_u128(parent_uuid));
        row.source = property_source(source);
        row.origin = None;
        row.gh_origin = None;
        row.description = format!("property row {uuid}");
        row.pools = pool_ids.iter().map(|id| format!("pool-{id}")).collect();
        let pool_count = row.pools.len();
        row.pools.rotate_left(rotation % pool_count);
        if reversed {
            row.pools.reverse();
        }
        let uppercase_hash = hash_bytes
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<String>();
        row.evidence = vec![
            format!("hash:sha256:{uppercase_hash}"),
            format!("exit:{exit_code}"),
            format!("artifact:/work/property-{uuid}"),
        ];
        row
    }

    proptest! {
        #[test]
        fn generated_rows_canonicalize_to_a_fixed_point(
            uuid in any::<u128>(),
            parent_uuid in any::<u128>(),
            source in any::<u8>(),
            pool_ids in prop::collection::btree_set(any::<u16>(), 1..9),
            rotation in any::<usize>(),
            reversed in any::<bool>(),
            exit_code in any::<u8>(),
            hash_bytes in any::<[u8; 32]>(),
        ) {
            let mut row = property_seed(
                (uuid, parent_uuid),
                source,
                &pool_ids,
                (rotation, reversed),
                (exit_code, hash_bytes),
            );
            let mut expected_pools = row.pools.clone();
            expected_pools.sort();

            row.canonicalize().unwrap();
            prop_assert_eq!(row.pools.as_slice(), expected_pools.as_slice());
            let canonical = row.clone();
            row.canonicalize().unwrap();
            prop_assert_eq!(row, canonical);
        }

        #[test]
        fn acknowledged_row_migration_is_idempotent_and_canonical(
            row_uuid in any::<u128>(),
            parent_uuid in any::<u128>(),
            event_uuid in any::<u128>(),
            source in any::<u8>(),
            pool_ids in prop::collection::btree_set(any::<u16>(), 1..9),
            rotation in any::<usize>(),
            reversed in any::<bool>(),
            exit_code in any::<u8>(),
            hash_bytes in any::<[u8; 32]>(),
            legacy_version in 1_u32..=4,
        ) {
            let mut legacy = property_seed(
                (row_uuid, parent_uuid),
                source,
                &pool_ids,
                (rotation, reversed),
                (exit_code, hash_bytes),
            );
            legacy.canonicalize().unwrap();
            legacy.row_version = legacy_version;
            if legacy_version == 1 {
                legacy.origin = None;
            }

            let migrated = migrations::migrate_to_current(&legacy).unwrap();
            let migrated_again = migrations::migrate_to_current(&migrated).unwrap();
            prop_assert_eq!(&migrated_again, &migrated);
            let mut recanonicalized = migrated.clone();
            recanonicalized.canonicalize().unwrap();
            prop_assert_eq!(&recanonicalized, &migrated);

            let event = DurableEnqueueEvent {
                schema_version: 1,
                event_id: Uuid::from_u128(event_uuid),
                acknowledged: true,
                guardrail_depth: 0,
                reuse: None,
                ingress_id: None,
                retries: Vec::new(),
                row: legacy,
            };
            let temp = tempfile::tempdir().unwrap();
            let events = temp.path().join("events");
            fs::create_dir_all(&events).unwrap();
            let path = events.join(format!("{}.enqueue.json", event.event_id));
            let mut bytes = serde_json::to_vec(&event).unwrap();
            bytes.push(b'\n');
            fs::write(&path, bytes).unwrap();

            prop_assert_eq!(migrate_acknowledged_events(&events).unwrap(), 1);
            let once = fs::read(&path).unwrap();
            prop_assert_eq!(migrate_acknowledged_events(&events).unwrap(), 0);
            prop_assert_eq!(fs::read(&path).unwrap(), once);
            let loaded = read_acknowledged_events(&events).unwrap();
            prop_assert_eq!(loaded.len(), 1);
            prop_assert_eq!(&loaded[0].row, &migrated);
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
    fn durable_job_token_hash_accepts_only_lowercase_sha256() {
        let mut row = seed(Uuid::new_v4());
        row.job_token_hash = Some(format!("sha256:{}", "a".repeat(64)));
        row.validate().unwrap();

        for invalid in [
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "A".repeat(64)),
            "sha512:not-a-token-hash".to_owned(),
        ] {
            row.job_token_hash = Some(invalid);
            assert!(row.validate().is_err());
        }
    }

    #[test]
    fn durable_pool_emission_is_always_array_and_legacy_scalars_still_load() {
        let singleton = seed(Uuid::new_v4());
        let encoded = serde_json::to_value(&singleton).unwrap();
        assert_eq!(encoded["rowVersion"], CURRENT_ROW_VERSION);
        assert_eq!(encoded["pool"], serde_json::json!(["worker-gpu"]));

        let mut scalar = encoded;
        scalar["pool"] = serde_json::json!("worker-gpu");
        let restored: RowSeed = serde_json::from_value(scalar).unwrap();
        assert_eq!(restored.pools, ["worker-gpu"]);

        let mut multi = seed(Uuid::new_v4());
        multi.pools = vec!["zeta".to_owned(), "alpha".to_owned()];
        let event = DurableEnqueueEvent::new(multi).unwrap();
        assert_eq!(event.row.pools, ["alpha", "zeta"]);
        let encoded = serde_json::to_value(&event).unwrap();
        assert_eq!(encoded["row"]["pool"], serde_json::json!(["alpha", "zeta"]));
        assert!(encoded["row"].get("payloadHash").is_none());
        assert!(encoded["row"].get("jobTokenHash").is_none());
        assert!(encoded.get("retries").is_none());
        assert_eq!(
            serde_json::from_value::<DurableEnqueueEvent>(encoded)
                .unwrap()
                .row
                .pools,
            ["alpha", "zeta"]
        );
    }

    #[test]
    fn corrupt_orchestration_in_events_dir_fails_read_back() {
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events");
        fs::create_dir_all(&events).unwrap();
        let event = DurableEnqueueEvent::new(seed(Uuid::new_v4())).unwrap();
        let path = events.join(format!("{}.enqueue.json", event.event_id));
        let mut encoded = serde_json::to_value(event).unwrap();
        encoded["row"]["orchestration"] = serde_json::json!({
            "flowRunId": "corrupt-flow-run-id",
            "nodeOrdinal": 0
        });
        fs::write(path, serde_json::to_vec(&encoded).unwrap()).unwrap();

        let error = read_acknowledged_events(&events).unwrap_err();
        assert!(error
            .to_string()
            .contains("orchestration flowRunId must be a UUID string"));
    }

    #[test]
    fn literal_legacy_origin_migration_rewrites_once_then_is_byte_stable() {
        assert!(!LEGACY_NO_ORIGIN.contains("\"rowVersion\""));
        assert!(!LEGACY_NO_ORIGIN.contains("\"origin\""));
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events");
        let path = install_literal_event(&events, LEGACY_NO_ORIGIN.as_bytes());
        let legacy_bytes = fs::read(&path).unwrap();

        let error = read_acknowledged_events(&events).unwrap_err();
        assert!(error.to_string().contains("ordered startup migration"));
        assert_eq!(fs::read(&path).unwrap(), legacy_bytes);

        assert_eq!(migrate_acknowledged_events(&events).unwrap(), 1);
        let migrated_bytes = fs::read(&path).unwrap();
        assert_ne!(migrated_bytes, legacy_bytes);
        let migrated_json: Value = serde_json::from_slice(&migrated_bytes).unwrap();
        assert_eq!(migrated_json["row"]["rowVersion"], CURRENT_ROW_VERSION);
        assert_eq!(
            migrated_json["row"]["pool"],
            serde_json::json!(["worker-gpu"])
        );
        assert_eq!(
            migrated_json["row"]["origin"],
            serde_json::json!({
                "schemaVersion": ADMISSION_ORIGIN_SCHEMA_VERSION,
                "source": "calendar"
            })
        );
        assert_eq!(read_acknowledged_events(&events).unwrap().len(), 1);
        assert!(events.read_dir().unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.')));

        assert_eq!(migrate_acknowledged_events(&events).unwrap(), 0);
        assert_eq!(fs::read(path).unwrap(), migrated_bytes);
    }

    #[test]
    fn literal_row_version_two_migrates_only_drv_absence_then_stabilizes() {
        assert!(LEGACY_NO_DRV.contains("\"rowVersion\": 2"));
        assert!(!LEGACY_NO_DRV.contains("\"drv\""));
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events");
        let path = install_literal_event(&events, LEGACY_NO_DRV.as_bytes());
        let legacy_bytes = fs::read(&path).unwrap();

        assert!(read_acknowledged_events(&events).is_err());
        assert_eq!(fs::read(&path).unwrap(), legacy_bytes);
        assert_eq!(migrate_acknowledged_events(&events).unwrap(), 1);
        let migrated_bytes = fs::read(&path).unwrap();
        let migrated: Value = serde_json::from_slice(&migrated_bytes).unwrap();
        assert_eq!(migrated["row"]["rowVersion"], CURRENT_ROW_VERSION);
        assert!(migrated["row"].get("drv").is_none());
        assert_eq!(migrate_acknowledged_events(&events).unwrap(), 0);
        assert_eq!(fs::read(path).unwrap(), migrated_bytes);
    }

    #[test]
    fn literal_row_version_three_migrates_only_job_token_hash_absence_then_stabilizes() {
        assert!(LEGACY_NO_JOB_TOKEN_HASH.contains("\"rowVersion\": 3"));
        assert!(!LEGACY_NO_JOB_TOKEN_HASH.contains("\"jobTokenHash\""));
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events");
        let path = install_literal_event(&events, LEGACY_NO_JOB_TOKEN_HASH.as_bytes());
        let legacy_bytes = fs::read(&path).unwrap();

        assert!(read_acknowledged_events(&events).is_err());
        assert_eq!(fs::read(&path).unwrap(), legacy_bytes);
        assert_eq!(migrate_acknowledged_events(&events).unwrap(), 1);
        let migrated_bytes = fs::read(&path).unwrap();
        let migrated: Value = serde_json::from_slice(&migrated_bytes).unwrap();
        assert_eq!(migrated["row"]["rowVersion"], CURRENT_ROW_VERSION);
        assert!(migrated["row"].get("jobTokenHash").is_none());
        assert_eq!(migrate_acknowledged_events(&events).unwrap(), 0);
        assert_eq!(fs::read(path).unwrap(), migrated_bytes);
    }

    #[test]
    fn unacknowledged_legacy_event_is_ignored_and_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events");
        let bytes = LEGACY_NO_ORIGIN
            .replacen("\"acknowledged\": true", "\"acknowledged\": false", 1)
            .into_bytes();
        let path = install_literal_event(&events, &bytes);

        assert_eq!(migrate_acknowledged_events(&events).unwrap(), 0);
        assert!(read_acknowledged_events(&events).unwrap().is_empty());
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn origin_migration_rejects_every_delta_outside_its_allowlist() {
        for (fixture, expected) in [
            (LEGACY_BAD_POOLS, "beyond origin back-fill"),
            (LEGACY_BAD_EVIDENCE, "beyond origin back-fill"),
            (LEGACY_BAD_ORIGIN, "origin source does not match"),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let events = temp.path().join("events");
            let path = install_literal_event(&events, fixture.as_bytes());
            let before = fs::read(&path).unwrap();
            let error = migrate_acknowledged_events(&events).unwrap_err();
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?} in {error}"
            );
            assert_eq!(fs::read(path).unwrap(), before);
        }
    }

    #[test]
    fn row_migration_registry_is_ordered_and_refuses_a_preexisting_origin() {
        assert_eq!(
            migrations::ROW_MIGRATIONS
                .iter()
                .map(|migration| (migration.from, migration.to))
                .collect::<Vec<_>>(),
            [(1, 2), (2, 3), (3, 4), (4, 5)]
        );
        let mut legacy = seed(Uuid::new_v4());
        legacy.row_version = 1;
        legacy.canonicalize().unwrap();
        let error = migrations::migrate_to_current(&legacy).unwrap_err();
        assert!(error.contains("requires the legacy origin field to be absent"));
    }

    #[test]
    fn migration_classifies_the_full_directory_before_rewriting() {
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events");
        let valid = install_literal_event(&events, LEGACY_NO_ORIGIN.as_bytes());
        install_literal_event(&events, LEGACY_BAD_POOLS.as_bytes());
        let before = fs::read(&valid).unwrap();

        assert!(migrate_acknowledged_events(&events).is_err());
        assert_eq!(fs::read(valid).unwrap(), before);
    }

    #[test]
    fn legacy_gh_origin_migrates_into_the_nested_generic_origin() {
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events");
        let path = install_literal_event(&events, LEGACY_GH_ORIGIN.as_bytes());

        assert_eq!(migrate_acknowledged_events(&events).unwrap(), 1);
        let migrated: DurableEnqueueEvent =
            serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let github = migrated.row.gh_origin.as_ref().unwrap();
        let origin = migrated.row.origin.as_ref().unwrap();
        assert_eq!(origin.source, EnqueueSource::Gh);
        assert_eq!(origin.producer.as_ref().unwrap().name, "github");
        assert_eq!(origin.github.as_ref(), Some(github));
    }

    #[test]
    fn synthesized_coordinator_corpus_migrates_reloads_and_stabilizes() {
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events");
        let template: Value = serde_json::from_str(LEGACY_NO_ORIGIN).unwrap();
        let mut expected_sources = BTreeMap::<&str, usize>::new();

        for index in 0..45_u128 {
            let source = match index {
                0..=20 => "calendar",
                21..=43 => "orchestrator",
                44 => "manual",
                _ => unreachable!(),
            };
            *expected_sources.entry(source).or_default() += 1;
            let event_id = Uuid::from_u128(0x4000_8000_0000_0000_0000 + index);
            let row_id = Uuid::from_u128(0x4000_8000_0000_0001_0000 + index);
            let mut value = template.clone();
            value["eventId"] = Value::String(event_id.to_string());
            value["row"]["uuid"] = Value::String(row_id.to_string());
            value["row"]["source"] = Value::String(source.to_owned());
            value["row"]["description"] =
                Value::String(format!("coordinator legacy fixture {index}"));
            let object = value["row"].as_object_mut().unwrap();
            object.remove("rowVersion");
            object.remove("origin");
            let bytes = serde_json::to_vec(&value).unwrap();
            install_literal_event(&events, &bytes);
        }

        assert_eq!(
            expected_sources,
            BTreeMap::from([("calendar", 21), ("manual", 1), ("orchestrator", 23)])
        );
        assert_eq!(migrate_acknowledged_events(&events).unwrap(), 45);
        let first_boot_bytes = enqueue_bytes(&events);
        let loaded = read_acknowledged_events(&events).unwrap();
        assert_eq!(loaded.len(), 45);
        let mut actual_sources = BTreeMap::<&str, usize>::new();
        for event in &loaded {
            assert_eq!(event.row.row_version, CURRENT_ROW_VERSION);
            assert_eq!(event.row.origin.as_ref().unwrap().source, event.row.source);
            *actual_sources.entry(event.row.source.as_str()).or_default() += 1;
        }
        assert_eq!(actual_sources, expected_sources);

        assert_eq!(migrate_acknowledged_events(&events).unwrap(), 0);
        assert_eq!(read_acknowledged_events(&events).unwrap(), loaded);
        assert_eq!(enqueue_bytes(&events), first_boot_bytes);
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
    fn wave_one_github_origin_and_context_remain_valid_after_schema_two() {
        let origin: GhOrigin = serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "producer": "github",
            "source": "search",
            "repo": "acme/widgets",
            "number": 42,
            "htmlUrl": "https://github.com/acme/widgets/issues/42",
            "itemType": "issue",
            "nodeId": "I_wave_one",
            "itemAuthor": "author",
            "triggerActor": "maintainer",
            "selfActor": "tally-bot",
            "triggerKind": "search",
            "context": {
                "schemaVersion": 1,
                "title": "Wave one context",
                "body": "untrusted",
                "labels": ["ready"],
                "assignees": []
            },
            "actorExclude": "self",
            "allowSelfTriggered": false
        }))
        .unwrap();
        origin.validate().unwrap();
        assert_eq!(origin.schema_version, 1);
        assert_eq!(origin.context.as_ref().unwrap().schema_version, 1);
        assert!(origin.context.as_ref().unwrap().state.is_none());
        assert!(origin.trigger_timestamp.is_none());
    }

    #[test]
    fn github_context_is_versioned_bounded_and_revision_consistent() {
        let context = GhContextSnapshot {
            schema_version: GH_CONTEXT_SCHEMA_VERSION,
            title: "PR context".to_owned(),
            body: "untrusted".to_owned(),
            state: Some(GhItemState::Open),
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
            trigger_kind: "assignment".to_owned(),
            event_id: Some("event-42".to_owned()),
            comment_id: None,
            trigger_timestamp: Some("2026-07-20T12:30:00Z".to_owned()),
            trigger_value: Some("tally-bot".to_owned()),
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
    fn current_github_event_identity_is_source_independent_and_uuid_stable() {
        let context = GhContextSnapshot {
            schema_version: GH_CONTEXT_SCHEMA_VERSION,
            title: "Issue context".to_owned(),
            body: "untrusted".to_owned(),
            state: Some(GhItemState::Open),
            head_sha: None,
            labels: vec!["ready".to_owned()],
            assignees: vec!["tally-bot".to_owned()],
            triggering_comment: None,
        };
        let origin = GhOrigin {
            schema_version: GH_ORIGIN_SCHEMA_VERSION,
            producer: "github".to_owned(),
            source: "notifications".to_owned(),
            repo: "acme/widgets".to_owned(),
            number: 42,
            html_url: "https://github.com/acme/widgets/issues/42".to_owned(),
            item_type: Some(GhItemType::Issue),
            head_sha: None,
            node_id: "I_current".to_owned(),
            item_author: "author".to_owned(),
            trigger_actor: "maintainer".to_owned(),
            self_actor: "tally-bot".to_owned(),
            notification_reason: Some("subscribed".to_owned()),
            trigger_kind: "assignment".to_owned(),
            event_id: Some("event-42".to_owned()),
            comment_id: None,
            trigger_timestamp: Some("2026-07-20T12:30:00Z".to_owned()),
            trigger_value: Some("tally-bot".to_owned()),
            context: Some(context),
            actor_exclude: "self".to_owned(),
            allow_self_triggered: false,
            allowed_actors: vec!["maintainer".to_owned()],
        };
        origin.validate().unwrap();
        let receipt = gh_trigger_receipt_id(&origin).unwrap();
        let task_uuid = gh_trigger_task_uuid(&origin).unwrap();
        assert_eq!(task_uuid, gh_trigger_task_uuid(&origin).unwrap());

        let from_search = GhOrigin {
            source: "search".to_owned(),
            notification_reason: None,
            ..origin.clone()
        };
        from_search.validate().unwrap();
        assert_eq!(receipt, gh_trigger_receipt_id(&from_search).unwrap());
        assert_eq!(task_uuid, gh_trigger_task_uuid(&from_search).unwrap());

        let later_event = GhOrigin {
            event_id: Some("event-43".to_owned()),
            trigger_timestamp: Some("2026-07-20T12:31:00Z".to_owned()),
            ..from_search
        };
        assert_ne!(receipt, gh_trigger_receipt_id(&later_event).unwrap());
        assert_ne!(task_uuid, gh_trigger_task_uuid(&later_event).unwrap());
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

    #[test]
    fn live_orchestrator_work_is_not_admitted_as_a_durable_row() {
        assert!(admits_durable_row(&durable(EnqueueSource::EventsDir)));
        assert!(!admits_durable_row(&AdmissionInput {
            source: EnqueueSource::Orchestrator,
            live_orchestrator_spawned: true,
            autonomous: false,
            crash_survivable: false,
            needs_cross_source_urgency: false,
        }));
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
    fn acknowledged_enqueue_event_retry_frontier_is_replaced_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events");
        let mut event = DurableEnqueueEvent::new(seed(Uuid::new_v4())).unwrap();
        let path = write_enqueue_event_atomic(&events, &event).unwrap();
        event.retries.push(DurableRetry {
            attempt: 2,
            previous_witness_seq: 7,
        });
        assert_eq!(update_enqueue_event_atomic(&events, &event).unwrap(), path);
        let restored = read_acknowledged_events(&events).unwrap();
        assert_eq!(restored, [event]);
        assert!(events.read_dir().unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.')));
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

    #[test]
    fn direct_durable_paths_store_only_canonical_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let canonical_hash = format!("hash:sha256:{}", "a".repeat(64));
        let mut direct = seed(Uuid::new_v4());
        direct.evidence = vec![
            format!("hash:sha256:{}", "A".repeat(64)),
            "exit:+0".to_owned(),
        ];
        let event = DurableEnqueueEvent::new(direct).unwrap();
        assert_eq!(event.row.evidence, [canonical_hash.as_str(), "exit:0"]);

        let events = temp.path().join("events");
        let path = write_enqueue_event_atomic(&events, &event).unwrap();
        let stored: DurableEnqueueEvent =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(stored.row.evidence, [canonical_hash.as_str(), "exit:0"]);
        assert_eq!(
            read_acknowledged_events(&events).unwrap()[0].row.evidence,
            [canonical_hash.as_str(), "exit:0"]
        );
    }
}
