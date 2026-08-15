use std::io::Write;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::config::Priority;
use crate::provenance::{SpecBuildNodeRole, TaskRef};
use crate::taskdb::EnqueueSource;
use crate::witness::LaborClass;

pub const JOURNAL_SOCKET: &str = "/run/systemd/journal/socket";
pub const JOURNAL_IDENTIFIER: &str = "tally";
pub const MAX_NATIVE_RECORD_BYTES: usize = 64 * 1024;
// Includes the trailing newline and therefore stays below journald's default
// 48 KiB LineMax for the JSON payload itself.
pub const MAX_STDOUT_RECORD_BYTES: usize = 48 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TallyEvent {
    Enqueued,
    Dispatched,
    Started,
    Heartbeat,
    Preempted,
    Resumed,
    Completed,
    Failed,
    EvidencePass,
    EvidenceFail,
    WitnessEmitted,
}

pub const TALLY_EVENTS: &[TallyEvent] = &[
    TallyEvent::Enqueued,
    TallyEvent::Dispatched,
    TallyEvent::Started,
    TallyEvent::Heartbeat,
    TallyEvent::Preempted,
    TallyEvent::Resumed,
    TallyEvent::Completed,
    TallyEvent::Failed,
    TallyEvent::EvidencePass,
    TallyEvent::EvidenceFail,
    TallyEvent::WitnessEmitted,
];

impl TallyEvent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enqueued => "enqueued",
            Self::Dispatched => "dispatched",
            Self::Started => "started",
            Self::Heartbeat => "heartbeat",
            Self::Preempted => "preempted",
            Self::Resumed => "resumed",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::EvidencePass => "evidence_pass",
            Self::EvidenceFail => "evidence_fail",
            Self::WitnessEmitted => "witness_emitted",
        }
    }

    const fn rank(self) -> u8 {
        match self {
            Self::Enqueued => 0,
            Self::Dispatched => 1,
            Self::Started => 2,
            Self::Heartbeat => 3,
            Self::Preempted => 4,
            Self::Resumed => 5,
            Self::Completed => 6,
            Self::Failed => 7,
            Self::EvidencePass => 8,
            Self::EvidenceFail => 9,
            Self::WitnessEmitted => 10,
        }
    }

    pub const fn is_evidence(self) -> bool {
        matches!(self, Self::EvidencePass | Self::EvidenceFail)
    }
}

impl std::fmt::Display for TallyEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for TallyEvent {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "enqueued" => Ok(Self::Enqueued),
            "dispatched" => Ok(Self::Dispatched),
            "started" => Ok(Self::Started),
            "heartbeat" => Ok(Self::Heartbeat),
            "preempted" => Ok(Self::Preempted),
            "resumed" => Ok(Self::Resumed),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "evidence_pass" => Ok(Self::EvidencePass),
            "evidence_fail" => Ok(Self::EvidenceFail),
            "witness_emitted" => Ok(Self::WitnessEmitted),
            _ => Err(()),
        }
    }
}

/// Semantic lifetime of a spec-build node's journal projection.
///
/// The human-readable `MESSAGE` remains an outcome-first diagnostic. This
/// closed field is the machine-readable statement of whether that narration
/// describes one execution attempt, one worklist task, one reconcile pass, or
/// the campaign as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JournalProjectionScope {
    Attempt,
    Task,
    Pass,
    Campaign,
}

impl JournalProjectionScope {
    pub const ALL: [Self; 4] = [Self::Attempt, Self::Task, Self::Pass, Self::Campaign];

    /// Classify every role in the closed spec-build vocabulary.
    ///
    /// Checks, receipts, and diagnoses qualify one candidate attempt;
    /// workspace/publication transitions qualify one task; reconciliation and
    /// continuation describe one pass; sweeping and terminal escalation govern
    /// the whole campaign.
    #[must_use]
    pub const fn for_spec_build_role(role: SpecBuildNodeRole) -> Self {
        match role {
            SpecBuildNodeRole::Agent
            | SpecBuildNodeRole::CheckpointRecord
            | SpecBuildNodeRole::Constraint
            | SpecBuildNodeRole::Diagnosis
            | SpecBuildNodeRole::Gate
            | SpecBuildNodeRole::Ownership
            | SpecBuildNodeRole::Retry
            | SpecBuildNodeRole::Steering => Self::Attempt,
            SpecBuildNodeRole::Cleanup
            | SpecBuildNodeRole::Merge
            | SpecBuildNodeRole::Prep
            | SpecBuildNodeRole::Publish
            | SpecBuildNodeRole::Rebase => Self::Task,
            SpecBuildNodeRole::Continue | SpecBuildNodeRole::Reconcile => Self::Pass,
            SpecBuildNodeRole::Escalate | SpecBuildNodeRole::Sweep => Self::Campaign,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldRequirement {
    Always,
    AtDispatch,
    AtStart,
    AtCompleted,
    AtCompletedOrFailed,
    AtEvidence,
    Conditional,
}

pub const TALLY_FIELD_MATRIX: &[(&str, FieldRequirement)] = &[
    ("SYSLOG_IDENTIFIER", FieldRequirement::Always),
    ("TALLY_EVENT", FieldRequirement::Always),
    ("TALLY_TASK_UUID", FieldRequirement::Always),
    ("TALLY_TASK_REF", FieldRequirement::Conditional),
    ("TALLY_JOURNAL_SCOPE", FieldRequirement::Conditional),
    ("TALLY_PROJECTION_SCOPE", FieldRequirement::Conditional),
    ("TALLY_CLASS", FieldRequirement::Always),
    ("TALLY_SOURCE", FieldRequirement::Always),
    ("MESSAGE", FieldRequirement::Always),
    ("TALLY_AGENT", FieldRequirement::AtDispatch),
    ("TALLY_ATTEMPT", FieldRequirement::AtDispatch),
    ("TALLY_LEASE_EPOCH", FieldRequirement::AtDispatch),
    ("TALLY_UNIT", FieldRequirement::AtStart),
    ("TALLY_SESSION_REF", FieldRequirement::Conditional),
    ("TALLY_EXIT_CODE", FieldRequirement::AtCompletedOrFailed),
    ("TALLY_STDERR_TAIL", FieldRequirement::Conditional),
    ("TALLY_STDERR_TRUNCATED", FieldRequirement::Conditional),
    // Was `AtCompletedOrFailed` until #382: every completion fabricated
    // `Some(0.0)` to satisfy this, which is exactly the fabricated-zero
    // pattern #382 removes. The field is real cgroup accounting for a
    // GPU-pool job now, present only when the exit recorder measured it.
    ("TALLY_GPU_SECONDS", FieldRequirement::Conditional),
    // Occupancy, recorded beside `TALLY_SESSION_REF` for the same reason: a
    // query surface reconstructed after retention still needs it. Never
    // required, since not every adapter declares a usage scrape and a
    // config-declared context window is independent of one.
    ("TALLY_CONTEXT_TOKENS", FieldRequirement::Conditional),
    ("TALLY_CONTEXT_WINDOW", FieldRequirement::Conditional),
    ("TALLY_LABOR_CLASS", FieldRequirement::AtCompletedOrFailed),
    ("TALLY_ARTIFACT_HASH", FieldRequirement::AtCompleted),
    ("TALLY_EVIDENCE", FieldRequirement::AtEvidence),
    ("TALLY_JOB_ID", FieldRequirement::Conditional),
    ("TALLY_PARENT", FieldRequirement::Conditional),
    ("TALLY_POOL", FieldRequirement::Conditional),
    ("TALLY_EXECUTOR", FieldRequirement::Conditional),
];

pub fn tally_agent_label(adapter: &str) -> Result<String, JournalError> {
    if adapter.trim().is_empty() || adapter.chars().any(char::is_control) {
        return Err(JournalError::Invalid(
            "agent adapter label must be non-empty and contain no control characters".to_owned(),
        ));
    }
    Ok(match adapter {
        "claude-code" => "cc".to_owned(),
        other => other.to_owned(),
    })
}

pub fn adapter_from_tally_agent(label: &str) -> Result<String, JournalError> {
    if label.trim().is_empty() || label.chars().any(char::is_control) {
        return Err(JournalError::Invalid(
            "TALLY_AGENT must be non-empty and contain no control characters".to_owned(),
        ));
    }
    Ok(match label {
        "cc" => "claude-code".to_owned(),
        other => other.to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TallyFields {
    #[serde(rename = "SYSLOG_IDENTIFIER")]
    pub syslog_identifier: String,
    #[serde(rename = "TALLY_EVENT")]
    pub event: TallyEvent,
    #[serde(rename = "TALLY_TASK_UUID")]
    pub task_uuid: String,
    #[serde(
        rename = "TALLY_TASK_REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub task_ref: Option<TaskRef>,
    /// Flow-run identity that scopes this record to one campaign pass.
    ///
    /// Optional for non-flow jobs and old lifecycle records.
    #[serde(
        rename = "TALLY_JOURNAL_SCOPE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub journal_scope: Option<String>,
    /// Semantic lifetime of a spec-build node's projected narration.
    #[serde(
        rename = "TALLY_PROJECTION_SCOPE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub projection_scope: Option<JournalProjectionScope>,
    #[serde(rename = "TALLY_CLASS")]
    pub class: Priority,
    #[serde(rename = "TALLY_SOURCE")]
    pub source: EnqueueSource,
    #[serde(rename = "MESSAGE")]
    pub message: String,
    #[serde(rename = "TALLY_AGENT", skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(rename = "TALLY_SESSION_REF", skip_serializing_if = "Option::is_none")]
    pub session_ref: Option<String>,
    #[serde(rename = "TALLY_UNIT", skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(rename = "TALLY_EXIT_CODE", skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(
        rename = "TALLY_STDERR_TAIL",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub stderr_tail: Option<String>,
    #[serde(
        rename = "TALLY_STDERR_TRUNCATED",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub stderr_truncated: Option<bool>,
    #[serde(rename = "TALLY_GPU_SECONDS", skip_serializing_if = "Option::is_none")]
    pub gpu_seconds: Option<f64>,
    #[serde(
        rename = "TALLY_CONTEXT_TOKENS",
        skip_serializing_if = "Option::is_none"
    )]
    pub context_tokens: Option<u64>,
    #[serde(
        rename = "TALLY_CONTEXT_WINDOW",
        skip_serializing_if = "Option::is_none"
    )]
    pub context_window: Option<u64>,
    #[serde(
        rename = "TALLY_ARTIFACT_HASH",
        skip_serializing_if = "Option::is_none"
    )]
    pub artifact_hash: Option<String>,
    #[serde(rename = "TALLY_EVIDENCE", skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(rename = "TALLY_ATTEMPT", skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(rename = "TALLY_LEASE_EPOCH", skip_serializing_if = "Option::is_none")]
    pub lease_epoch: Option<u64>,
    #[serde(rename = "TALLY_LABOR_CLASS", skip_serializing_if = "Option::is_none")]
    pub labor_class: Option<LaborClass>,
    #[serde(rename = "TALLY_JOB_ID", skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(rename = "TALLY_PARENT", skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(
        rename = "TALLY_POOL",
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::poolset::serialize_encoded_optional",
        deserialize_with = "crate::poolset::deserialize_encoded_optional"
    )]
    pub pools: Option<Vec<String>>,
    #[serde(
        rename = "TALLY_EXECUTOR",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub executor: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmitEvent {
    pub event: TallyEvent,
    pub task_uuid: String,
    pub task_ref: Option<TaskRef>,
    pub journal_scope: Option<String>,
    pub projection_scope: Option<JournalProjectionScope>,
    pub class: Priority,
    pub source: EnqueueSource,
    pub message: Option<String>,
    pub agent: Option<String>,
    pub session_ref: Option<String>,
    pub unit: Option<String>,
    pub exit_code: Option<i32>,
    pub stderr_tail: Option<String>,
    pub stderr_truncated: Option<bool>,
    pub gpu_seconds: Option<f64>,
    pub context_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub artifact_hash: Option<String>,
    pub evidence: Option<String>,
    pub attempt: Option<u32>,
    pub lease_epoch: Option<u64>,
    pub labor_class: Option<LaborClass>,
    pub job_id: Option<String>,
    pub parent: Option<String>,
    pub pools: Option<Vec<String>>,
    pub executor: Option<String>,
}

impl EmitEvent {
    pub fn enqueued(task_uuid: impl Into<String>, class: Priority, source: EnqueueSource) -> Self {
        Self {
            event: TallyEvent::Enqueued,
            task_uuid: task_uuid.into(),
            task_ref: None,
            journal_scope: None,
            projection_scope: None,
            class,
            source,
            message: None,
            agent: None,
            session_ref: None,
            unit: None,
            exit_code: None,
            stderr_tail: None,
            stderr_truncated: None,
            gpu_seconds: None,
            context_tokens: None,
            context_window: None,
            artifact_hash: None,
            evidence: None,
            attempt: None,
            lease_epoch: None,
            labor_class: None,
            job_id: None,
            parent: None,
            pools: None,
            executor: None,
        }
    }

    pub fn into_fields(self) -> Result<TallyFields, JournalError> {
        let message = self
            .message
            .clone()
            .unwrap_or_else(|| synthesize_message(&self));
        let fields = TallyFields {
            syslog_identifier: JOURNAL_IDENTIFIER.to_owned(),
            event: self.event,
            task_uuid: self.task_uuid,
            task_ref: self.task_ref,
            journal_scope: self.journal_scope,
            projection_scope: self.projection_scope,
            class: self.class,
            source: self.source,
            message,
            agent: self.agent,
            session_ref: self.session_ref,
            unit: self.unit,
            exit_code: self.exit_code,
            stderr_tail: self.stderr_tail,
            stderr_truncated: self.stderr_truncated,
            gpu_seconds: self.gpu_seconds,
            context_tokens: self.context_tokens,
            context_window: self.context_window,
            artifact_hash: self.artifact_hash,
            evidence: self.evidence,
            attempt: self.attempt,
            lease_epoch: self.lease_epoch,
            labor_class: self.labor_class,
            job_id: self.job_id,
            parent: self.parent,
            pools: self.pools,
            executor: self.executor,
        };
        validate_fields(&fields)?;
        Ok(fields)
    }
}

/// The past-tense opening verb for each of the 11 lifecycle events' default
/// `MESSAGE` template (#385): an outcome-first leading word, so a reader of
/// the raw journal sees what happened before any key=value detail, the same
/// content contract the narrate slot enforces at the publish boundary. Most
/// events use a bare `Enqueued`/`Started`-shaped past participle; the two
/// that read as an internal record of an action needed a said action
/// spelled out (`Recorded a heartbeat for`, `Emitted the witness record
/// for`) to stay grammatical.
fn synthesize_message_verb(event: TallyEvent) -> &'static str {
    match event {
        TallyEvent::Enqueued => "Enqueued",
        TallyEvent::Dispatched => "Dispatched",
        TallyEvent::Started => "Started",
        TallyEvent::Heartbeat => "Recorded a heartbeat for",
        TallyEvent::Preempted => "Preempted",
        TallyEvent::Resumed => "Resumed",
        TallyEvent::Completed => "Completed",
        TallyEvent::Failed => "Failed",
        TallyEvent::EvidencePass => "Passed the evidence check for",
        TallyEvent::EvidenceFail => "Failed the evidence check for",
        TallyEvent::WitnessEmitted => "Emitted the witness record for",
    }
}

fn synthesize_message(event: &EmitEvent) -> String {
    let mut parts = vec![format!(
        "{} {}",
        synthesize_message_verb(event.event),
        event.task_uuid
    )];
    if let Some(task_ref) = &event.task_ref {
        parts.push(format!("taskRef={task_ref}"));
    }
    match event.event {
        TallyEvent::Completed | TallyEvent::Failed => {
            if let Some(exit_code) = event.exit_code {
                parts.push(format!("exit={exit_code}"));
            }
            if let Some(gpu_seconds) = event.gpu_seconds {
                parts.push(format!("gpu={gpu_seconds}s"));
            }
        }
        TallyEvent::EvidencePass | TallyEvent::EvidenceFail => {
            if let Some(evidence) = &event.evidence {
                parts.push(evidence.clone());
            }
        }
        TallyEvent::Dispatched | TallyEvent::Started => {
            if let Some(unit) = &event.unit {
                parts.push(unit.clone());
            }
            if let Some(attempt) = event.attempt {
                parts.push(format!("attempt={attempt}"));
            }
        }
        _ => {}
    }
    parts.join(" ")
}

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("invalid journal record: {0}")]
    Invalid(String),
    #[error("journal JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("journal {action} failed at {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("journal record is {size} bytes, exceeding the {limit}-byte limit")]
    TooLarge { size: usize, limit: usize },
    #[error("journald accepted only {sent} of {expected} datagram bytes")]
    ShortDatagram { sent: usize, expected: usize },
}

fn required(requirement: FieldRequirement, event: TallyEvent) -> bool {
    match requirement {
        FieldRequirement::Always => true,
        FieldRequirement::AtDispatch => event.rank() >= TallyEvent::Dispatched.rank(),
        FieldRequirement::AtStart => event.rank() >= TallyEvent::Started.rank(),
        FieldRequirement::AtCompleted => event == TallyEvent::Completed,
        FieldRequirement::AtCompletedOrFailed => {
            matches!(event, TallyEvent::Completed | TallyEvent::Failed)
        }
        FieldRequirement::AtEvidence => event.is_evidence(),
        FieldRequirement::Conditional => false,
    }
}

fn field_present(fields: &TallyFields, name: &str) -> bool {
    match name {
        "SYSLOG_IDENTIFIER" => !fields.syslog_identifier.is_empty(),
        "TALLY_EVENT" | "TALLY_CLASS" | "TALLY_SOURCE" => true,
        "TALLY_TASK_UUID" => !fields.task_uuid.is_empty(),
        "TALLY_TASK_REF" => fields.task_ref.is_some(),
        "TALLY_JOURNAL_SCOPE" => fields
            .journal_scope
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        "TALLY_PROJECTION_SCOPE" => fields.projection_scope.is_some(),
        "MESSAGE" => !fields.message.is_empty(),
        "TALLY_AGENT" => fields
            .agent
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        "TALLY_SESSION_REF" => fields
            .session_ref
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        "TALLY_UNIT" => fields
            .unit
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        "TALLY_EXIT_CODE" => fields.exit_code.is_some(),
        "TALLY_STDERR_TAIL" => fields.stderr_tail.is_some(),
        "TALLY_STDERR_TRUNCATED" => fields.stderr_truncated.is_some(),
        "TALLY_GPU_SECONDS" => fields.gpu_seconds.is_some(),
        "TALLY_CONTEXT_TOKENS" => fields.context_tokens.is_some(),
        "TALLY_CONTEXT_WINDOW" => fields.context_window.is_some(),
        "TALLY_ARTIFACT_HASH" => fields
            .artifact_hash
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        "TALLY_EVIDENCE" => fields
            .evidence
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        "TALLY_ATTEMPT" => fields.attempt.is_some(),
        "TALLY_LEASE_EPOCH" => fields.lease_epoch.is_some(),
        "TALLY_LABOR_CLASS" => fields.labor_class.is_some(),
        "TALLY_JOB_ID" => fields
            .job_id
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        "TALLY_PARENT" => fields
            .parent
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        "TALLY_POOL" => fields.pools.as_ref().is_some_and(|value| !value.is_empty()),
        "TALLY_EXECUTOR" => fields
            .executor
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        _ => false,
    }
}

pub fn validate_fields(fields: &TallyFields) -> Result<(), JournalError> {
    if fields.syslog_identifier != JOURNAL_IDENTIFIER {
        return Err(JournalError::Invalid(format!(
            "SYSLOG_IDENTIFIER must be {JOURNAL_IDENTIFIER:?}"
        )));
    }
    for (name, requirement) in TALLY_FIELD_MATRIX {
        if required(*requirement, fields.event) && !field_present(fields, name) {
            return Err(JournalError::Invalid(format!(
                "event {:?} requires {name}",
                fields.event.as_str()
            )));
        }
    }
    if let Some(pools) = &fields.pools {
        let mut canonical = pools.clone();
        crate::poolset::canonicalize(&mut canonical)
            .map_err(|error| JournalError::Invalid(error.to_string()))?;
        if &canonical != pools {
            return Err(JournalError::Invalid(
                "TALLY_POOL set must be in canonical order".to_owned(),
            ));
        }
    }
    if let Some(flow_run_id) = fields.journal_scope.as_deref() {
        uuid::Uuid::parse_str(flow_run_id).map_err(|_| {
            JournalError::Invalid("TALLY_JOURNAL_SCOPE must be a UUID string".to_owned())
        })?;
    }
    if fields.projection_scope.is_some() && fields.journal_scope.is_none() {
        return Err(JournalError::Invalid(
            "TALLY_PROJECTION_SCOPE requires TALLY_JOURNAL_SCOPE".to_owned(),
        ));
    }
    match fields.projection_scope {
        Some(JournalProjectionScope::Attempt)
            if fields.task_ref.is_none() || fields.attempt.is_none() =>
        {
            return Err(JournalError::Invalid(
                "attempt-scoped journal projections require TALLY_TASK_REF and TALLY_ATTEMPT"
                    .to_owned(),
            ));
        }
        Some(JournalProjectionScope::Task) if fields.task_ref.is_none() => {
            return Err(JournalError::Invalid(
                "task-scoped journal projections require TALLY_TASK_REF".to_owned(),
            ));
        }
        Some(JournalProjectionScope::Pass | JournalProjectionScope::Campaign)
            if fields.task_ref.is_some() =>
        {
            return Err(JournalError::Invalid(
                "pass- and campaign-scoped journal projections must not carry TALLY_TASK_REF"
                    .to_owned(),
            ));
        }
        _ => {}
    }
    for (name, value) in string_fields(fields) {
        if value.contains('\0') {
            return Err(JournalError::Invalid(format!("{name} contains a NUL byte")));
        }
        if !matches!(name, "MESSAGE" | "TALLY_STDERR_TAIL") && value.chars().any(char::is_control) {
            return Err(JournalError::Invalid(format!(
                "{name} contains a control character"
            )));
        }
    }
    if fields.exit_code.is_some_and(|code| code < 0) {
        return Err(JournalError::Invalid(
            "TALLY_EXIT_CODE must be non-negative".to_owned(),
        ));
    }
    match (&fields.stderr_tail, fields.stderr_truncated) {
        (Some(tail), Some(_)) => {
            if fields.event != TallyEvent::Failed {
                return Err(JournalError::Invalid(
                    "TALLY_STDERR_TAIL is valid only on failed events".to_owned(),
                ));
            }
            if tail.len() > crate::executor::CAPTURE_EXCERPT_MAX_BYTES {
                return Err(JournalError::Invalid(format!(
                    "TALLY_STDERR_TAIL exceeds {} bytes",
                    crate::executor::CAPTURE_EXCERPT_MAX_BYTES
                )));
            }
        }
        (None, None) => {}
        _ => {
            return Err(JournalError::Invalid(
                "TALLY_STDERR_TAIL and TALLY_STDERR_TRUNCATED must appear together".to_owned(),
            ));
        }
    }
    if fields
        .gpu_seconds
        .is_some_and(|seconds| !seconds.is_finite() || seconds < 0.0)
    {
        return Err(JournalError::Invalid(
            "TALLY_GPU_SECONDS must be finite and non-negative".to_owned(),
        ));
    }
    if fields.attempt == Some(0) {
        return Err(JournalError::Invalid(
            "TALLY_ATTEMPT must be positive".to_owned(),
        ));
    }
    if fields.lease_epoch == Some(0) {
        return Err(JournalError::Invalid(
            "TALLY_LEASE_EPOCH must be positive".to_owned(),
        ));
    }
    Ok(())
}

fn string_fields(fields: &TallyFields) -> Vec<(&'static str, &str)> {
    let mut values = vec![
        ("SYSLOG_IDENTIFIER", fields.syslog_identifier.as_str()),
        ("TALLY_TASK_UUID", fields.task_uuid.as_str()),
        ("MESSAGE", fields.message.as_str()),
    ];
    if let Some(task_ref) = &fields.task_ref {
        values.push(("TALLY_TASK_REF", task_ref.as_str()));
    }
    if let Some(journal_scope) = fields.journal_scope.as_deref() {
        values.push(("TALLY_JOURNAL_SCOPE", journal_scope));
    }
    for (name, value) in [
        ("TALLY_AGENT", fields.agent.as_deref()),
        ("TALLY_SESSION_REF", fields.session_ref.as_deref()),
        ("TALLY_UNIT", fields.unit.as_deref()),
        ("TALLY_STDERR_TAIL", fields.stderr_tail.as_deref()),
        ("TALLY_ARTIFACT_HASH", fields.artifact_hash.as_deref()),
        ("TALLY_EVIDENCE", fields.evidence.as_deref()),
        ("TALLY_JOB_ID", fields.job_id.as_deref()),
        ("TALLY_PARENT", fields.parent.as_deref()),
        ("TALLY_EXECUTOR", fields.executor.as_deref()),
    ] {
        if let Some(value) = value {
            values.push((name, value));
        }
    }
    if let Some(pools) = &fields.pools {
        values.extend(pools.iter().map(|pool| ("TALLY_POOL", pool.as_str())));
    }
    values
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalDestination {
    Stdout,
    Native,
}

#[derive(Debug, Clone)]
pub struct JournalEmitter {
    destination: JournalDestination,
    socket_path: PathBuf,
}

impl JournalEmitter {
    pub fn new(native: bool) -> Self {
        Self {
            destination: if native {
                JournalDestination::Native
            } else {
                JournalDestination::Stdout
            },
            socket_path: PathBuf::from(JOURNAL_SOCKET),
        }
    }

    pub fn from_config(config: &crate::config::JournaldConfig) -> Self {
        Self::new(config.native)
    }

    pub fn with_native_socket(mut self, socket_path: impl Into<PathBuf>) -> Self {
        self.socket_path = socket_path.into();
        self
    }

    pub const fn destination(&self) -> JournalDestination {
        self.destination
    }

    pub fn emit(&self, event: EmitEvent) -> Result<TallyFields, JournalError> {
        let stdout = std::io::stdout();
        self.emit_to(event, &mut stdout.lock())
    }

    pub fn emit_fields(&self, fields: &TallyFields) -> Result<(), JournalError> {
        let stdout = std::io::stdout();
        self.emit_fields_to(fields, &mut stdout.lock())
    }

    pub fn emit_to(
        &self,
        event: EmitEvent,
        stdout: &mut dyn Write,
    ) -> Result<TallyFields, JournalError> {
        let fields = event.into_fields()?;
        self.emit_fields_to(&fields, stdout)?;
        Ok(fields)
    }

    pub fn emit_fields_to(
        &self,
        fields: &TallyFields,
        stdout: &mut dyn Write,
    ) -> Result<(), JournalError> {
        match self.destination {
            JournalDestination::Stdout => write_stdout_record(stdout, fields),
            JournalDestination::Native => send_native_record(&self.socket_path, fields),
        }
    }
}

pub fn render_stdout_record(fields: &TallyFields) -> Result<Vec<u8>, JournalError> {
    validate_fields(fields)?;
    let mut bytes = serde_json::to_vec(fields)?;
    if bytes.contains(&b'\n') || bytes.contains(&b'\r') {
        return Err(JournalError::Invalid(
            "stdout journal JSON contains a literal line break".to_owned(),
        ));
    }
    bytes.push(b'\n');
    if bytes.len() > MAX_STDOUT_RECORD_BYTES {
        return Err(JournalError::TooLarge {
            size: bytes.len(),
            limit: MAX_STDOUT_RECORD_BYTES,
        });
    }
    Ok(bytes)
}

pub fn write_stdout_record(
    writer: &mut dyn Write,
    fields: &TallyFields,
) -> Result<(), JournalError> {
    let bytes = render_stdout_record(fields)?;
    writer.write_all(&bytes).map_err(|source| JournalError::Io {
        action: "stdout write",
        path: PathBuf::from("<stdout>"),
        source,
    })
}

pub fn encode_native_record(fields: &TallyFields) -> Result<Vec<u8>, JournalError> {
    validate_fields(fields)?;
    let mut packet = Vec::new();
    for (name, value) in native_fields(fields)? {
        if value.as_bytes().contains(&b'\n') {
            packet.extend_from_slice(name.as_bytes());
            packet.push(b'\n');
            packet.extend_from_slice(&(value.len() as u64).to_le_bytes());
            packet.extend_from_slice(value.as_bytes());
            packet.push(b'\n');
        } else {
            packet.extend_from_slice(name.as_bytes());
            packet.push(b'=');
            packet.extend_from_slice(value.as_bytes());
            packet.push(b'\n');
        }
    }
    if packet.len() > MAX_NATIVE_RECORD_BYTES {
        return Err(JournalError::TooLarge {
            size: packet.len(),
            limit: MAX_NATIVE_RECORD_BYTES,
        });
    }
    Ok(packet)
}

pub fn send_native_record(path: &Path, fields: &TallyFields) -> Result<(), JournalError> {
    let packet = encode_native_record(fields)?;
    let socket = UnixDatagram::unbound().map_err(|source| JournalError::Io {
        action: "socket creation",
        path: path.to_path_buf(),
        source,
    })?;
    socket.connect(path).map_err(|source| JournalError::Io {
        action: "socket connect",
        path: path.to_path_buf(),
        source,
    })?;
    let sent = socket.send(&packet).map_err(|source| JournalError::Io {
        action: "datagram send",
        path: path.to_path_buf(),
        source,
    })?;
    if sent != packet.len() {
        return Err(JournalError::ShortDatagram {
            sent,
            expected: packet.len(),
        });
    }
    Ok(())
}

fn enum_json_string<T: Serialize>(value: T) -> Result<String, JournalError> {
    let value = serde_json::to_value(value)?;
    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| JournalError::Invalid("enum did not serialize as a string".to_owned()))
}

fn native_fields(fields: &TallyFields) -> Result<Vec<(&'static str, String)>, JournalError> {
    let mut values = vec![
        ("SYSLOG_IDENTIFIER", fields.syslog_identifier.clone()),
        ("TALLY_EVENT", fields.event.to_string()),
        ("TALLY_TASK_UUID", fields.task_uuid.clone()),
        ("TALLY_CLASS", enum_json_string(fields.class)?),
        ("TALLY_SOURCE", enum_json_string(fields.source)?),
        ("MESSAGE", fields.message.clone()),
    ];
    if let Some(task_ref) = &fields.task_ref {
        values.push(("TALLY_TASK_REF", task_ref.to_string()));
    }
    push_optional(
        &mut values,
        "TALLY_JOURNAL_SCOPE",
        fields.journal_scope.as_deref(),
    );
    if let Some(scope) = fields.projection_scope {
        values.push(("TALLY_PROJECTION_SCOPE", enum_json_string(scope)?));
    }
    push_optional(&mut values, "TALLY_AGENT", fields.agent.as_deref());
    push_optional(
        &mut values,
        "TALLY_SESSION_REF",
        fields.session_ref.as_deref(),
    );
    push_optional(&mut values, "TALLY_UNIT", fields.unit.as_deref());
    if let Some(value) = fields.exit_code {
        values.push(("TALLY_EXIT_CODE", value.to_string()));
    }
    push_optional(
        &mut values,
        "TALLY_STDERR_TAIL",
        fields.stderr_tail.as_deref(),
    );
    if let Some(value) = fields.stderr_truncated {
        values.push(("TALLY_STDERR_TRUNCATED", value.to_string()));
    }
    if let Some(value) = fields.gpu_seconds {
        values.push(("TALLY_GPU_SECONDS", value.to_string()));
    }
    if let Some(value) = fields.context_tokens {
        values.push(("TALLY_CONTEXT_TOKENS", value.to_string()));
    }
    if let Some(value) = fields.context_window {
        values.push(("TALLY_CONTEXT_WINDOW", value.to_string()));
    }
    push_optional(
        &mut values,
        "TALLY_ARTIFACT_HASH",
        fields.artifact_hash.as_deref(),
    );
    push_optional(&mut values, "TALLY_EVIDENCE", fields.evidence.as_deref());
    if let Some(value) = fields.attempt {
        values.push(("TALLY_ATTEMPT", value.to_string()));
    }
    if let Some(value) = fields.lease_epoch {
        values.push(("TALLY_LEASE_EPOCH", value.to_string()));
    }
    if let Some(value) = fields.labor_class {
        values.push(("TALLY_LABOR_CLASS", enum_json_string(value)?));
    }
    push_optional(&mut values, "TALLY_JOB_ID", fields.job_id.as_deref());
    push_optional(&mut values, "TALLY_PARENT", fields.parent.as_deref());
    if let Some(pools) = &fields.pools {
        values.push(("TALLY_POOL", crate::poolset::encoded(pools)?));
    }
    push_optional(&mut values, "TALLY_EXECUTOR", fields.executor.as_deref());
    Ok(values)
}

fn push_optional(
    fields: &mut Vec<(&'static str, String)>,
    name: &'static str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        fields.push((name, value.to_owned()));
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JournalEntry {
    pub fields: TallyFields,
    pub realtime_us: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalFilter {
    pub task: Option<String>,
    pub session: Option<String>,
    pub event: Option<TallyEvent>,
    pub journal_scope: Option<String>,
    pub projection_scope: Option<JournalProjectionScope>,
    pub since_realtime_us: Option<u64>,
}

impl JournalFilter {
    pub fn matches(&self, entry: &JournalEntry) -> bool {
        if self
            .task
            .as_deref()
            .is_some_and(|task| entry.fields.task_uuid != task)
        {
            return false;
        }
        if self
            .session
            .as_deref()
            .is_some_and(|session| entry.fields.session_ref.as_deref() != Some(session))
        {
            return false;
        }
        if self.event.is_some_and(|event| entry.fields.event != event) {
            return false;
        }
        if self
            .journal_scope
            .as_deref()
            .is_some_and(|scope| entry.fields.journal_scope.as_deref() != Some(scope))
        {
            return false;
        }
        if self
            .projection_scope
            .is_some_and(|scope| entry.fields.projection_scope != Some(scope))
        {
            return false;
        }
        if self
            .since_realtime_us
            .is_some_and(|since| entry.realtime_us.is_some_and(|seen| seen < since))
        {
            return false;
        }
        true
    }
}

pub fn parse_journal_json_line(line: &str) -> Result<Option<JournalEntry>, JournalError> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let outer: Value = serde_json::from_str(trimmed)?;
    let outer = outer
        .as_object()
        .ok_or_else(|| JournalError::Invalid("journal JSON line is not an object".to_owned()))?;
    let realtime_us = outer
        .get("__REALTIME_TIMESTAMP")
        .map(parse_u64_value)
        .transpose()?;

    let payload = outer
        .get("MESSAGE")
        .and_then(Value::as_str)
        .and_then(|message| serde_json::from_str::<Value>(message).ok())
        .and_then(|value| value.as_object().cloned())
        .filter(|object| object.contains_key("TALLY_EVENT"));

    let source = if outer.contains_key("TALLY_EVENT") {
        outer
    } else if let Some(payload) = payload.as_ref() {
        payload
    } else {
        return Ok(None);
    };
    let fields = hydrate_fields(source)?;
    validate_fields(&fields)?;
    Ok(Some(JournalEntry {
        fields,
        realtime_us,
    }))
}

pub fn parse_journal_json_lines(
    input: &str,
    filter: &JournalFilter,
) -> Result<Vec<JournalEntry>, JournalError> {
    let mut entries = Vec::new();
    for line in input.lines() {
        if let Some(entry) = parse_journal_json_line(line)? {
            if filter.matches(&entry) {
                entries.push(entry);
            }
        }
    }
    entries.sort_by_key(|entry| entry.realtime_us);
    Ok(entries)
}

fn hydrate_fields(source: &Map<String, Value>) -> Result<TallyFields, JournalError> {
    let mut normalized = source.clone();
    for name in [
        "TALLY_EXIT_CODE",
        "TALLY_GPU_SECONDS",
        "TALLY_CONTEXT_TOKENS",
        "TALLY_CONTEXT_WINDOW",
        "TALLY_ATTEMPT",
        "TALLY_LEASE_EPOCH",
    ] {
        if let Some(value) = normalized.get(name).cloned() {
            if let Some(value) = value.as_str() {
                let number = value
                    .parse::<serde_json::Number>()
                    .map_err(|_| JournalError::Invalid(format!("{name} is not a JSON number")))?;
                normalized.insert(name.to_owned(), Value::Number(number));
            }
        }
    }
    if let Some(value) = normalized.get("TALLY_STDERR_TRUNCATED").cloned() {
        if let Some(value) = value.as_str() {
            let value = value.parse::<bool>().map_err(|_| {
                JournalError::Invalid("TALLY_STDERR_TRUNCATED is not a boolean".to_owned())
            })?;
            normalized.insert("TALLY_STDERR_TRUNCATED".to_owned(), Value::Bool(value));
        }
    }
    serde_json::from_value(Value::Object(normalized)).map_err(JournalError::Json)
}

fn parse_u64_value(value: &Value) -> Result<u64, JournalError> {
    if let Some(value) = value.as_u64() {
        return Ok(value);
    }
    value
        .as_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| {
            JournalError::Invalid("__REALTIME_TIMESTAMP is not a non-negative integer".to_owned())
        })
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixDatagram;
    use std::time::Duration;

    use proptest::prelude::*;
    use tempfile::tempdir;

    use super::*;

    fn full_event(event: TallyEvent) -> EmitEvent {
        EmitEvent {
            event,
            task_uuid: "task-abc".to_owned(),
            task_ref: None,
            journal_scope: None,
            projection_scope: None,
            class: Priority::High,
            source: EnqueueSource::Manual,
            message: None,
            agent: Some(tally_agent_label("claude-code").unwrap()),
            session_ref: Some("session-1".to_owned()),
            unit: Some("tally-job-task-abc.service".to_owned()),
            exit_code: Some(0),
            stderr_tail: (event == TallyEvent::Failed).then(|| "failure detail\n".to_owned()),
            stderr_truncated: (event == TallyEvent::Failed).then_some(false),
            gpu_seconds: Some(12.5),
            context_tokens: Some(1234),
            context_window: Some(1_000_000),
            artifact_hash: Some("sha256:deadbeef".to_owned()),
            evidence: Some("pass artifact:/out/result".to_owned()),
            attempt: Some(1),
            lease_epoch: Some(7),
            labor_class: Some(LaborClass::Fresh),
            job_id: Some("job-abc".to_owned()),
            parent: Some("parent-abc".to_owned()),
            pools: Some(vec!["gpu".to_owned()]),
            executor: None,
        }
    }

    #[test]
    fn event_vocabulary_and_always_fields_are_pinned() {
        assert_eq!(
            TALLY_EVENTS
                .iter()
                .copied()
                .map(TallyEvent::as_str)
                .collect::<Vec<_>>(),
            [
                "enqueued",
                "dispatched",
                "started",
                "heartbeat",
                "preempted",
                "resumed",
                "completed",
                "failed",
                "evidence_pass",
                "evidence_fail",
                "witness_emitted",
            ]
        );
        assert_eq!(
            TALLY_FIELD_MATRIX
                .iter()
                .filter_map(
                    |(name, requirement)| (*requirement == FieldRequirement::Always)
                        .then_some(*name)
                )
                .collect::<Vec<_>>(),
            [
                "SYSLOG_IDENTIFIER",
                "TALLY_EVENT",
                "TALLY_TASK_UUID",
                "TALLY_CLASS",
                "TALLY_SOURCE",
                "MESSAGE",
            ]
        );
    }

    #[test]
    fn every_event_accepts_a_complete_field_set() {
        for event in TALLY_EVENTS {
            full_event(*event).into_fields().unwrap();
        }
    }

    #[test]
    fn every_required_matrix_field_is_rejected_when_absent() {
        for event in TALLY_EVENTS {
            let fields = full_event(*event).into_fields().unwrap();
            for (name, requirement) in TALLY_FIELD_MATRIX {
                if !required(*requirement, *event) {
                    continue;
                }
                let mut value = serde_json::to_value(&fields).unwrap();
                value.as_object_mut().unwrap().remove(*name);
                let rejected = serde_json::from_value::<TallyFields>(value)
                    .map_or(true, |fields| validate_fields(&fields).is_err());
                assert!(rejected, "{event} accepted missing required field {name}");
            }
        }
    }

    #[test]
    fn optional_fields_stay_absent_and_agent_vocabulary_is_canonical() {
        let fields = EmitEvent::enqueued("task", Priority::Low, EnqueueSource::Manual)
            .into_fields()
            .unwrap();
        let value = serde_json::to_value(fields).unwrap();
        for optional in [
            "TALLY_AGENT",
            "TALLY_TASK_REF",
            "TALLY_JOURNAL_SCOPE",
            "TALLY_PROJECTION_SCOPE",
            "TALLY_SESSION_REF",
            "TALLY_CONTEXT_TOKENS",
            "TALLY_CONTEXT_WINDOW",
            "TALLY_JOB_ID",
            "TALLY_PARENT",
            "TALLY_POOL",
        ] {
            assert!(value.get(optional).is_none(), "unexpected {optional}");
        }
        assert_eq!(tally_agent_label("claude-code").unwrap(), "cc");
        assert_eq!(tally_agent_label("pi").unwrap(), "pi");
        assert_eq!(tally_agent_label("shell").unwrap(), "shell");
        assert_eq!(tally_agent_label("codex").unwrap(), "codex");
        assert_eq!(adapter_from_tally_agent("cc").unwrap(), "claude-code");
        assert!(tally_agent_label("bad\nlabel").is_err());
    }

    #[test]
    fn spec_build_roles_have_one_exhaustive_projection_scope() {
        use SpecBuildNodeRole::*;

        let expected = [
            (Agent, JournalProjectionScope::Attempt),
            (CheckpointRecord, JournalProjectionScope::Attempt),
            (Cleanup, JournalProjectionScope::Task),
            (Constraint, JournalProjectionScope::Attempt),
            (Continue, JournalProjectionScope::Pass),
            (Diagnosis, JournalProjectionScope::Attempt),
            (Escalate, JournalProjectionScope::Campaign),
            (Gate, JournalProjectionScope::Attempt),
            (Merge, JournalProjectionScope::Task),
            (Ownership, JournalProjectionScope::Attempt),
            (Prep, JournalProjectionScope::Task),
            (Publish, JournalProjectionScope::Task),
            (Rebase, JournalProjectionScope::Task),
            (Reconcile, JournalProjectionScope::Pass),
            (Retry, JournalProjectionScope::Attempt),
            (Steering, JournalProjectionScope::Attempt),
            (Sweep, JournalProjectionScope::Campaign),
        ];
        assert_eq!(expected.map(|(role, _)| role), SpecBuildNodeRole::ALL);
        assert_eq!(
            expected.map(|(_, scope)| scope),
            SpecBuildNodeRole::ALL.map(JournalProjectionScope::for_spec_build_role)
        );
        assert_eq!(
            JournalProjectionScope::ALL.map(|scope| enum_json_string(scope).unwrap()),
            ["attempt", "task", "pass", "campaign"]
        );
    }

    #[test]
    fn campaign_projection_scope_is_structured_native_and_filterable() {
        const FLOW_RUN: &str = "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321";
        let mut event = full_event(TallyEvent::Started);
        event.task_ref = Some(TaskRef::new("crm/t07").unwrap());
        event.journal_scope = Some(FLOW_RUN.to_owned());
        event.projection_scope = Some(JournalProjectionScope::Attempt);
        let fields = event.into_fields().unwrap();

        // Scope is additive structure. The outcome-first narration stays on
        // the exact pre-scope template.
        assert_eq!(
            fields.message,
            "Started task-abc taskRef=crm/t07 tally-job-task-abc.service attempt=1"
        );
        let structured = serde_json::to_value(&fields).unwrap();
        assert_eq!(structured["TALLY_JOURNAL_SCOPE"], FLOW_RUN);
        assert_eq!(structured["TALLY_PROJECTION_SCOPE"], "attempt");
        let native = String::from_utf8(encode_native_record(&fields).unwrap()).unwrap();
        assert!(native
            .lines()
            .any(|line| line == format!("TALLY_JOURNAL_SCOPE={FLOW_RUN}")));
        assert!(native
            .lines()
            .any(|line| line == "TALLY_PROJECTION_SCOPE=attempt"));

        let entry = JournalEntry {
            fields,
            realtime_us: Some(7),
        };
        assert!(JournalFilter {
            journal_scope: Some(FLOW_RUN.to_owned()),
            projection_scope: Some(JournalProjectionScope::Attempt),
            ..JournalFilter::default()
        }
        .matches(&entry));
        assert!(!JournalFilter {
            projection_scope: Some(JournalProjectionScope::Task),
            ..JournalFilter::default()
        }
        .matches(&entry));
    }

    #[test]
    fn projection_scope_identity_requirements_fail_closed() {
        const FLOW_RUN: &str = "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321";
        for scope in JournalProjectionScope::ALL {
            let mut event = full_event(TallyEvent::Started);
            event.journal_scope = Some(FLOW_RUN.to_owned());
            event.projection_scope = Some(scope);
            if matches!(
                scope,
                JournalProjectionScope::Attempt | JournalProjectionScope::Task
            ) {
                event.task_ref = Some(TaskRef::new("crm/t07").unwrap());
            }
            event.into_fields().unwrap();
        }

        let mut event = full_event(TallyEvent::Started);
        event.projection_scope = Some(JournalProjectionScope::Campaign);
        assert!(event.into_fields().is_err());

        let mut event = full_event(TallyEvent::Started);
        event.journal_scope = Some(FLOW_RUN.to_owned());
        event.projection_scope = Some(JournalProjectionScope::Attempt);
        assert!(event.into_fields().is_err());

        let mut event = full_event(TallyEvent::Started);
        event.task_ref = Some(TaskRef::new("crm/t07").unwrap());
        event.journal_scope = Some(FLOW_RUN.to_owned());
        event.projection_scope = Some(JournalProjectionScope::Campaign);
        assert!(event.into_fields().is_err());

        let mut event = full_event(TallyEvent::Started);
        event.journal_scope = Some("not-a-flow-run".to_owned());
        assert!(event.into_fields().is_err());
    }

    #[test]
    fn task_ref_is_present_in_structured_native_and_human_lifecycle_records() {
        let mut event = EmitEvent::enqueued("uuid-1", Priority::Low, EnqueueSource::Orchestrator);
        event.task_ref = Some(TaskRef::new("crm/t07").unwrap());
        let fields = event.into_fields().unwrap();

        assert_eq!(fields.message, "Enqueued uuid-1 taskRef=crm/t07");
        assert_eq!(
            serde_json::to_value(&fields).unwrap()["TALLY_TASK_REF"],
            "crm/t07"
        );
        let native = String::from_utf8(encode_native_record(&fields).unwrap()).unwrap();
        assert!(native.lines().any(|line| line == "TALLY_TASK_REF=crm/t07"));
    }

    #[test]
    fn daemon_authored_messages_lead_with_a_past_tense_outcome() {
        // #385 audits the daemon's own default `MESSAGE` templates against the
        // same outcome-first contract the narrate slot enforces at the publish
        // boundary: every one of TALLY_EVENTS' 11 kinds gets its own assertion
        // here, named explicitly, so a 12th event added later fails this test
        // instead of silently going unaudited.
        assert_eq!(TALLY_EVENTS.len(), 11, "audit every lifecycle event kind");
        let expectations: &[(TallyEvent, &str)] = &[
            (TallyEvent::Enqueued, "Enqueued task-abc"),
            (TallyEvent::Dispatched, "Dispatched task-abc"),
            (TallyEvent::Started, "Started task-abc"),
            (TallyEvent::Heartbeat, "Recorded a heartbeat for task-abc"),
            (TallyEvent::Preempted, "Preempted task-abc"),
            (TallyEvent::Resumed, "Resumed task-abc"),
            (TallyEvent::Completed, "Completed task-abc"),
            (TallyEvent::Failed, "Failed task-abc"),
            (
                TallyEvent::EvidencePass,
                "Passed the evidence check for task-abc",
            ),
            (
                TallyEvent::EvidenceFail,
                "Failed the evidence check for task-abc",
            ),
            (
                TallyEvent::WitnessEmitted,
                "Emitted the witness record for task-abc",
            ),
        ];
        assert_eq!(expectations.len(), TALLY_EVENTS.len());
        for &kind in TALLY_EVENTS {
            let (matched, expected_prefix) = expectations
                .iter()
                .find(|(event, _)| *event == kind)
                .unwrap();
            assert_eq!(*matched, kind);
            let event = full_event(kind);
            let fields = event.into_fields().unwrap();
            assert!(
                fields.message.starts_with(expected_prefix),
                "{kind:?} message {:?} does not open with {expected_prefix:?}",
                fields.message
            );
            let opening_word = fields.message.split(' ').next().unwrap();
            assert!(
                opening_word.ends_with("ed"),
                "{kind:?} message {:?} does not open with a past-tense verb",
                fields.message
            );
            assert!(
                !fields.message.contains('!'),
                "{kind:?} message {:?} contains an exclamation mark",
                fields.message
            );
        }
    }

    #[test]
    fn lifecycle_requirements_fail_closed() {
        let dispatched = EmitEvent {
            event: TallyEvent::Dispatched,
            ..EmitEvent::enqueued("task", Priority::Low, EnqueueSource::Manual)
        };
        assert!(dispatched.into_fields().is_err());

        let mut completed = full_event(TallyEvent::Completed);
        completed.artifact_hash = None;
        assert!(completed
            .into_fields()
            .unwrap_err()
            .to_string()
            .contains("TALLY_ARTIFACT_HASH"));

        let mut failed = full_event(TallyEvent::Failed);
        failed.artifact_hash = None;
        failed.into_fields().unwrap();

        let mut evidence = full_event(TallyEvent::EvidenceFail);
        evidence.evidence = None;
        assert!(evidence.into_fields().is_err());
    }

    #[test]
    fn invalid_values_fail_before_emission() {
        let mut event = full_event(TallyEvent::Completed);
        event.gpu_seconds = Some(f64::NAN);
        assert!(event.into_fields().is_err());

        let mut event = full_event(TallyEvent::Started);
        event.lease_epoch = Some(0);
        assert!(event.into_fields().is_err());

        let mut event = EmitEvent::enqueued("task", Priority::Low, EnqueueSource::Manual);
        event.pools = Some(vec!["bad\npool".to_owned()]);
        assert!(event.into_fields().is_err());

        let mut event = full_event(TallyEvent::Completed);
        event.stderr_tail = Some("only failures may carry this".to_owned());
        event.stderr_truncated = Some(false);
        assert!(event.into_fields().is_err());

        let mut event = full_event(TallyEvent::Failed);
        event.stderr_truncated = None;
        assert!(event.into_fields().is_err());

        let mut event = full_event(TallyEvent::Failed);
        event.stderr_tail = Some("x".repeat(crate::executor::CAPTURE_EXCERPT_MAX_BYTES + 1));
        assert!(event.into_fields().is_err());
    }

    #[test]
    fn stderr_tail_bound_reanchors_on_the_excerpt_derivation() {
        // The bound validated above is not a free number: it is the
        // executor's derivation over this file's envelope (vestige-sweep
        // V-4). Re-derive it here from the consumer side so a hand-retyped
        // numeral on either end diverges and fails.
        assert_eq!(
            crate::executor::CAPTURE_EXCERPT_MAX_BYTES
                + crate::executor::CAPTURE_EXCERPT_FRAMING_MARGIN,
            MAX_STDOUT_RECORD_BYTES
        );

        // A maximal excerpt is admitted, and a Failed record carrying it
        // still renders inside the envelope with every other field present:
        // the framing margin the derivation reserves is real headroom.
        let mut event = full_event(TallyEvent::Failed);
        event.stderr_tail = Some("x".repeat(crate::executor::CAPTURE_EXCERPT_MAX_BYTES));
        event.stderr_truncated = Some(true);
        let fields = event.into_fields().unwrap();
        let rendered = render_stdout_record(&fields).unwrap();
        assert!(rendered.len() <= MAX_STDOUT_RECORD_BYTES);
        assert!(encode_native_record(&fields).is_ok());
    }

    #[test]
    fn stdout_is_one_json_line_and_preserves_message_newlines() {
        let mut event = EmitEvent::enqueued("task", Priority::Low, EnqueueSource::Manual);
        event.message = Some("line one\nline two".to_owned());
        let fields = event.into_fields().unwrap();
        let bytes = render_stdout_record(&fields).unwrap();
        assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
        let parsed: TallyFields = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(parsed.message, "line one\nline two");
    }

    #[test]
    fn stdout_limit_cannot_cross_journalds_default_line_max() {
        assert_eq!(MAX_STDOUT_RECORD_BYTES, 48 * 1024);
        // The capture excerpt peephole is a derivation over this envelope
        // (vestige-sweep V-4); a hand-retyped CAPTURE_EXCERPT_MAX_BYTES
        // drifts from the envelope it must render inside and fails here.
        assert_eq!(
            crate::executor::CAPTURE_EXCERPT_MAX_BYTES
                + crate::executor::CAPTURE_EXCERPT_FRAMING_MARGIN,
            MAX_STDOUT_RECORD_BYTES
        );
        let mut event = EmitEvent::enqueued("task", Priority::Low, EnqueueSource::Manual);
        event.message = Some("x".repeat(MAX_STDOUT_RECORD_BYTES));
        let fields = event.into_fields().unwrap();
        assert!(matches!(
            render_stdout_record(&fields),
            Err(JournalError::TooLarge {
                limit: MAX_STDOUT_RECORD_BYTES,
                ..
            })
        ));
        assert!(encode_native_record(&fields).is_ok());
    }

    #[test]
    fn native_protocol_uses_binary_framing_for_newlines() {
        let mut event = EmitEvent::enqueued("task", Priority::Low, EnqueueSource::Manual);
        event.message = Some("line one\nline two".to_owned());
        let packet = encode_native_record(&event.into_fields().unwrap()).unwrap();
        let marker = b"MESSAGE\n";
        let index = packet
            .windows(marker.len())
            .position(|window| window == marker)
            .unwrap();
        let length_start = index + marker.len();
        let length = u64::from_le_bytes(packet[length_start..length_start + 8].try_into().unwrap());
        assert_eq!(length, "line one\nline two".len() as u64);

        let packet =
            encode_native_record(&full_event(TallyEvent::Failed).into_fields().unwrap()).unwrap();
        let marker = b"TALLY_STDERR_TAIL\n";
        let index = packet
            .windows(marker.len())
            .position(|window| window == marker)
            .unwrap();
        let length_start = index + marker.len();
        let length = u64::from_le_bytes(packet[length_start..length_start + 8].try_into().unwrap());
        assert_eq!(length, "failure detail\n".len() as u64);
    }

    #[test]
    fn toggle_selects_stdout_or_one_native_datagram() {
        let mut stdout = Vec::new();
        let fallback = JournalEmitter::new(false);
        assert_eq!(fallback.destination(), JournalDestination::Stdout);
        fallback
            .emit_to(
                EmitEvent::enqueued("stdout-task", Priority::Low, EnqueueSource::Manual),
                &mut stdout,
            )
            .unwrap();
        assert!(!stdout.is_empty());

        let directory = tempdir().unwrap();
        let socket_path = directory.path().join("journal.sock");
        let receiver = UnixDatagram::bind(&socket_path).unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let native = JournalEmitter::new(true).with_native_socket(&socket_path);
        assert_eq!(native.destination(), JournalDestination::Native);
        let mut ignored_stdout = Vec::new();
        native
            .emit_to(
                EmitEvent::enqueued("native-task", Priority::High, EnqueueSource::Manual),
                &mut ignored_stdout,
            )
            .unwrap();
        assert!(ignored_stdout.is_empty());
        let mut packet = vec![0_u8; MAX_NATIVE_RECORD_BYTES];
        let received = receiver.recv(&mut packet).unwrap();
        let packet = &packet[..received];
        assert!(packet
            .windows(b"TALLY_TASK_UUID=native-task".len())
            .any(|window| window == b"TALLY_TASK_UUID=native-task"));
    }

    #[test]
    fn native_failure_never_falls_back_to_stdout() {
        let directory = tempdir().unwrap();
        let missing = directory.path().join("absent.sock");
        let emitter = JournalEmitter::new(true).with_native_socket(missing);
        let mut stdout = Vec::new();
        assert!(emitter
            .emit_to(
                EmitEvent::enqueued("task", Priority::Low, EnqueueSource::Manual),
                &mut stdout,
            )
            .is_err());
        assert!(stdout.is_empty());
    }

    #[test]
    fn parser_rehydrates_stdout_and_native_shapes() {
        let fields = full_event(TallyEvent::Completed).into_fields().unwrap();
        let payload = String::from_utf8(render_stdout_record(&fields).unwrap()).unwrap();
        let stdout_line = serde_json::json!({
            "__REALTIME_TIMESTAMP": "1720526400000000",
            "SYSLOG_IDENTIFIER": "tally",
            "MESSAGE": payload.trim_end(),
        });
        let stdout_entry = parse_journal_json_line(&stdout_line.to_string())
            .unwrap()
            .unwrap();
        assert_eq!(stdout_entry.fields, fields);
        assert_eq!(stdout_entry.realtime_us, Some(1_720_526_400_000_000));

        let mut native = serde_json::to_value(&fields).unwrap();
        let native = native.as_object_mut().unwrap();
        native.insert(
            "__REALTIME_TIMESTAMP".to_owned(),
            Value::String("1720526400000001".to_owned()),
        );
        for name in [
            "TALLY_EXIT_CODE",
            "TALLY_GPU_SECONDS",
            "TALLY_CONTEXT_TOKENS",
            "TALLY_CONTEXT_WINDOW",
            "TALLY_ATTEMPT",
            "TALLY_LEASE_EPOCH",
        ] {
            let value = native.get(name).unwrap().to_string();
            native.insert(name.to_owned(), Value::String(value));
        }
        let native_entry = parse_journal_json_line(&Value::Object(native.clone()).to_string())
            .unwrap()
            .unwrap();
        assert_eq!(native_entry.fields, fields);
    }

    #[test]
    fn native_top_level_fields_win_over_a_json_looking_message() {
        let mut event = EmitEvent::enqueued("native-task", Priority::Low, EnqueueSource::Manual);
        event.message = Some(r#"{"TALLY_EVENT":"failed"}"#.to_owned());
        let fields = event.into_fields().unwrap();
        let native = serde_json::to_value(&fields).unwrap();
        let entry = parse_journal_json_line(&native.to_string())
            .unwrap()
            .unwrap();
        assert_eq!(entry.fields, fields);
    }

    #[test]
    fn parser_rejects_incomplete_tally_records_but_ignores_noise() {
        assert!(
            parse_journal_json_line(r#"{"MESSAGE":"ordinary daemon line"}"#)
                .unwrap()
                .is_none()
        );
        let incomplete =
            r#"{"SYSLOG_IDENTIFIER":"tally","TALLY_EVENT":"completed","TALLY_TASK_UUID":"task"}"#;
        assert!(parse_journal_json_line(incomplete).is_err());
        assert!(parse_journal_json_line("{torn").is_err());
    }

    proptest! {
        #[test]
        fn journal_json_line_parser_never_panics(line in any::<String>()) {
            let _ = parse_journal_json_line(&line);
        }
    }

    #[test]
    fn parser_filters_and_orders_entries() {
        let a = EmitEvent::enqueued("A", Priority::Low, EnqueueSource::Manual)
            .into_fields()
            .unwrap();
        let b = EmitEvent::enqueued("B", Priority::High, EnqueueSource::Gh)
            .into_fields()
            .unwrap();
        let lines = [
            serde_json::json!({
                "__REALTIME_TIMESTAMP": "20",
                "MESSAGE": serde_json::to_string(&b).unwrap(),
            })
            .to_string(),
            serde_json::json!({
                "__REALTIME_TIMESTAMP": "10",
                "MESSAGE": serde_json::to_string(&a).unwrap(),
            })
            .to_string(),
        ]
        .join("\n");
        let entries = parse_journal_json_lines(
            &lines,
            &JournalFilter {
                event: Some(TallyEvent::Enqueued),
                ..JournalFilter::default()
            },
        )
        .unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.fields.task_uuid.as_str())
                .collect::<Vec<_>>(),
            ["A", "B"]
        );
    }
}
