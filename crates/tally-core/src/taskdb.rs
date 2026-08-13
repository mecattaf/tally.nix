use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
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
const MAX_PROVENANCE_FIELD_BYTES: usize = 4096;
pub const ADMISSION_ORIGIN_SCHEMA_VERSION: u32 = 1;
pub const CURRENT_ROW_VERSION: u32 = 5;
const MAX_GH_PRODUCER_BYTES: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnqueueSource {
    Manual,
    Orchestrator,
    Calendar,
    EventsDir,
    Gh,
}

impl EnqueueSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Orchestrator => "orchestrator",
            Self::Calendar => "calendar",
            Self::EventsDir => "events-dir",
            Self::Gh => "gh",
        }
    }

    pub const fn is_producer(self) -> bool {
        matches!(self, Self::Calendar | Self::EventsDir | Self::Gh)
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
}

impl AdmissionOrigin {
    pub fn direct(source: EnqueueSource) -> Self {
        Self {
            schema_version: ADMISSION_ORIGIN_SCHEMA_VERSION,
            source,
            producer: None,
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
        }
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
                || value.len() > MAX_PROVENANCE_FIELD_BYTES
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

fn validate_gh_scalar(label: &str, value: &str) -> Result<(), TaskDbError> {
    if value.trim().is_empty()
        || value.len() > MAX_PROVENANCE_FIELD_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(TaskDbError::InvalidSeed(format!(
            "GitHub {label} must be non-empty, at most {MAX_PROVENANCE_FIELD_BYTES} bytes, and contain no control characters"
        )));
    }
    Ok(())
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
            self.origin = Some(AdmissionOrigin::direct(self.source));
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
            related_trigger: None,
            evidence_class: Some(Value::String("artifact".to_owned())),
            manifest_hash: Some(Value::String("sha256:manifest".to_owned())),
            usage: None,
            context_tokens: None,
            context_window: None,
        }
    }

    fn property_source(selector: u8) -> EnqueueSource {
        match selector % 4 {
            0 => EnqueueSource::Manual,
            1 => EnqueueSource::Orchestrator,
            2 => EnqueueSource::Calendar,
            3 => EnqueueSource::EventsDir,
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
    fn rows_below_the_floor_are_refused_without_rewriting_and_name_the_last_upgrade_pin() {
        assert!(migrations::ROW_MIGRATIONS.is_empty());

        for row_version in 1..CURRENT_ROW_VERSION {
            let mut row = seed(Uuid::new_v4());
            row.row_version = row_version;
            let error = migrations::migrate_to_current(&row).unwrap_err();
            assert!(error.contains(&format!("rowVersion {row_version} predates this binary")));
            assert!(error.contains(migrations::LAST_ROW_MIGRATION_PIN));
            assert!(error.contains("start tally once to migrate its durable rows"));
        }

        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events");
        fs::create_dir_all(&events).unwrap();
        let mut row = seed(Uuid::new_v4());
        row.row_version = CURRENT_ROW_VERSION - 1;
        let event = DurableEnqueueEvent {
            schema_version: 1,
            event_id: Uuid::new_v4(),
            acknowledged: true,
            guardrail_depth: 0,
            reuse: None,
            ingress_id: None,
            retries: Vec::new(),
            row,
        };
        let path = events.join(format!("{}.enqueue.json", event.event_id));
        let mut bytes = serde_json::to_vec(&event).unwrap();
        bytes.push(b'\n');
        fs::write(&path, &bytes).unwrap();

        let error = migrate_acknowledged_events(&events).unwrap_err();
        assert!(error
            .to_string()
            .contains(migrations::LAST_ROW_MIGRATION_PIN));
        assert_eq!(fs::read(path).unwrap(), bytes);
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
