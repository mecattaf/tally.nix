use std::collections::{BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::completion::SemanticCompletion;
use crate::provenance::Orchestration;
use crate::taskdb::AdmissionOrigin;

pub const GENESIS_PREV_HASH: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";
pub const WITNESS_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecordType {
    Verdict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Pass,
    CleanExitNoArtifact,
    Failed,
    Cancelled,
    Reused,
    PoolVanished,
    Preempted,
    RuntimeExceeded,
    Substituted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaborClass {
    Fresh,
    Recovered,
    Reused,
    Substituted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Charge {
    pub unit: String,
    pub amount: f64,
    #[serde(rename = "class")]
    pub class_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DerivationOutput {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Derivation {
    pub drv_path: String,
    pub outputs: Vec<DerivationOutput>,
}

impl Derivation {
    pub fn canonicalize(&mut self) -> Result<(), String> {
        self.outputs
            .sort_by(|left, right| left.name.cmp(&right.name));
        self.validate()
    }

    pub fn validate(&self) -> Result<(), String> {
        if !is_nix_store_path(&self.drv_path) || !self.drv_path.ends_with(".drv") {
            return Err("drvPath must be a Nix store path ending in .drv".to_owned());
        }
        if self.outputs.is_empty() {
            return Err("drv outputs must be non-empty".to_owned());
        }
        if self
            .outputs
            .iter()
            .any(|output| !is_nix_store_path(&output.path))
        {
            return Err("drv output path is not a Nix store path".to_owned());
        }
        if self
            .outputs
            .windows(2)
            .any(|pair| pair[0].name >= pair[1].name)
        {
            return Err("drv outputs must be sorted by name and unique".to_owned());
        }
        Ok(())
    }

    pub fn output_paths(&self) -> Vec<String> {
        let mut paths = self
            .outputs
            .iter()
            .map(|output| output.path.clone())
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthorshipStatus {
    Bound,
    Unavailable,
    MissingNote,
    Mismatch,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Authorship {
    pub provider: String,
    pub provider_version: String,
    pub note_ref: String,
    pub status: AuthorshipStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes_ref_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_content_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthorshipSession {
    pub tool: String,
    pub id: String,
    pub model: String,
}

impl AuthorshipSession {
    pub(crate) fn validate(&self) -> Result<(), String> {
        for (name, value, maximum) in [
            ("tool", self.tool.as_str(), 64),
            ("id", self.id.as_str(), 512),
            ("model", self.model.as_str(), 256),
        ] {
            if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
                return Err(format!(
                    "authorshipSessions {name} must be non-empty, at most {maximum} bytes, and contain no control characters"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WitnessRecord {
    pub schema_version: u32,
    pub record_type: RecordType,
    pub transition_timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_uuid: Option<String>,
    pub verdict: Verdict,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_paths: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drv: Option<Derivation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_seconds: Option<f64>,
    pub wall_clock: f64,
    pub attempt: u32,
    pub lease_epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_hash: Option<String>,
    pub origin: AdmissionOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration: Option<Orchestration>,
    pub labor_class: LaborClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_ref: Option<String>,
    pub pools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charge: Option<Charge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_class: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<SemanticCompletion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorship: Option<Authorship>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorship_sessions: Option<Vec<AuthorshipSession>>,
    #[serde(flatten, default)]
    pub extensions: Map<String, Value>,
    pub seq: u64,
    pub prev_hash: String,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WitnessBody {
    pub task_uuid: Option<String>,
    pub transition_timestamp: String,
    pub verdict: Verdict,
    pub exit_code: i32,
    pub artifact_content_hash: Option<String>,
    pub store_paths: Option<Vec<String>>,
    pub drv: Option<Derivation>,
    pub gpu_seconds: Option<f64>,
    pub wall_clock: f64,
    pub attempt: u32,
    pub lease_epoch: u64,
    pub dedup_key: Option<String>,
    pub payload_hash: Option<String>,
    pub brief_hash: Option<String>,
    pub origin: AdmissionOrigin,
    pub orchestration: Option<Orchestration>,
    pub labor_class: LaborClass,
    pub trace_ref: Option<String>,
    pub pools: Vec<String>,
    pub executor: Option<String>,
    pub host_id: Option<String>,
    pub charge: Option<Charge>,
    pub model: Option<String>,
    pub evidence_class: Option<Value>,
    pub manifest_hash: Option<Value>,
    pub completion: Option<SemanticCompletion>,
    pub result_revision: Option<String>,
    pub authorship: Option<Authorship>,
    pub authorship_sessions: Option<Vec<AuthorshipSession>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainHead {
    pub seq: u64,
    pub hash: String,
}

impl Default for ChainHead {
    fn default() -> Self {
        Self {
            seq: 0,
            hash: GENESIS_PREV_HASH.to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerifyProblemKind {
    ParseError,
    InvalidRecord,
    SchemaVersionInvalid,
    RecordTypeInvalid,
    HashMismatch,
    PrevHashMismatch,
    SeqOrder,
    SeqGap,
    SeqDuplicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyProblem {
    pub seq: Option<u64>,
    pub line: usize,
    pub kind: VerifyProblemKind,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyReport {
    pub ok: bool,
    pub records: usize,
    #[serde(rename = "firstSeq")]
    pub first_seq: Option<u64>,
    #[serde(rename = "lastSeq")]
    pub last_seq: Option<u64>,
    pub problems: Vec<VerifyProblem>,
}

#[derive(Debug, Error)]
pub enum WitnessError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("ledger is corrupt: {0}")]
    Corrupt(String),
    #[error(
        "old-format witness ledger at {path}; archive it aside before first boot: mv -- {path} {archive}"
    )]
    OldFormat { path: PathBuf, archive: PathBuf },
    #[error("invalid host ID: {0}")]
    InvalidHostId(String),
    #[error("cannot serialize ledger record: {0}")]
    Json(#[from] serde_json::Error),
}

fn io_error(path: &Path, source: std::io::Error) -> WitnessError {
    WitnessError::Io {
        path: path.to_owned(),
        source,
    }
}

pub fn canonical_hash_input(raw: &Value) -> Result<String, WitnessError> {
    let mut cleared = raw.clone();
    let object = cleared
        .as_object_mut()
        .ok_or_else(|| WitnessError::Corrupt("record is not a JSON object".to_owned()))?;
    if !object.contains_key("hash") {
        return Err(WitnessError::Corrupt("record has no hash field".to_owned()));
    }
    object.insert("hash".to_owned(), Value::String(String::new()));
    Ok(serde_json::to_string(&cleared)?)
}

pub fn compute_hash_value(raw: &Value) -> Result<String, WitnessError> {
    let input = canonical_hash_input(raw)?;
    let digest = Sha256::digest(input.as_bytes());
    Ok(format!("sha256:{digest:x}"))
}

pub fn compute_hash(record: &WitnessRecord) -> Result<String, WitnessError> {
    compute_hash_value(&stable_canonical_value(record)?)
}

// Normalize producer-side numbers through the same Value parse used by verification.
// serde_json's default float parser can otherwise shorten a duration-derived f64
// differently when the persisted bytes are read back.
fn stable_canonical_value(value: &impl Serialize) -> Result<Value, WitnessError> {
    let encoded = serde_json::to_vec(value)?;
    let normalized: Value = serde_json::from_slice(&encoded)?;
    let canonical = serde_json::to_vec(&normalized)?;
    let reparsed: Value = serde_json::from_slice(&canonical)?;
    if serde_json::to_vec(&reparsed)? != canonical {
        return Err(WitnessError::Corrupt(
            "record cannot be encoded as stable compact canonical JSON".to_owned(),
        ));
    }
    Ok(normalized)
}

fn canonical_record_bytes(raw: &Value) -> Result<Vec<u8>, WitnessError> {
    let encoded = serde_json::to_vec(raw)?;
    let normalized: Value = serde_json::from_slice(&encoded)?;
    let canonical = serde_json::to_vec(&normalized)?;
    if canonical != encoded {
        return Err(WitnessError::Corrupt(
            "record producer generated non-canonical JSON".to_owned(),
        ));
    }
    Ok(encoded)
}

fn build_record_with_raw(
    body: WitnessBody,
    head: &ChainHead,
) -> Result<(WitnessRecord, Value), WitnessError> {
    let mut pools = body.pools;
    crate::poolset::canonicalize(&mut pools)
        .map_err(|error| WitnessError::Corrupt(error.to_string()))?;
    let mut store_paths = body.store_paths;
    if let Some(paths) = &mut store_paths {
        paths.sort();
        if paths.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(WitnessError::Corrupt(
                "storePaths contains a duplicate path".to_owned(),
            ));
        }
    }
    let mut drv = body.drv;
    if let Some(drv) = &mut drv {
        drv.canonicalize().map_err(WitnessError::Corrupt)?;
    }
    let mut authorship_sessions = body.authorship_sessions;
    if let Some(sessions) = &mut authorship_sessions {
        sessions.sort();
        if sessions.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(WitnessError::Corrupt(
                "authorshipSessions contains a duplicate observation".to_owned(),
            ));
        }
    }
    let mut record = WitnessRecord {
        schema_version: WITNESS_SCHEMA_VERSION,
        record_type: RecordType::Verdict,
        transition_timestamp: body.transition_timestamp,
        task_uuid: body.task_uuid,
        verdict: body.verdict,
        exit_code: body.exit_code,
        artifact_content_hash: body.artifact_content_hash,
        store_paths,
        drv,
        gpu_seconds: body.gpu_seconds,
        wall_clock: body.wall_clock,
        attempt: body.attempt,
        lease_epoch: body.lease_epoch,
        dedup_key: body.dedup_key,
        payload_hash: body.payload_hash,
        brief_hash: body.brief_hash,
        origin: body.origin,
        orchestration: body.orchestration,
        labor_class: body.labor_class,
        trace_ref: body.trace_ref,
        pools,
        executor: body.executor,
        host_id: body.host_id,
        charge: body.charge,
        model: body.model,
        evidence_class: body.evidence_class,
        manifest_hash: body.manifest_hash,
        completion: body.completion,
        result_revision: body.result_revision,
        authorship: body.authorship,
        authorship_sessions,
        extensions: Map::new(),
        seq: head.seq + 1,
        prev_hash: head.hash.clone(),
        hash: String::new(),
    };
    let mut raw = stable_canonical_value(&record)?;
    let hash = compute_hash_value(&raw)?;
    record.hash.clone_from(&hash);
    raw.as_object_mut()
        .expect("serialized witness record is an object")
        .insert("hash".to_owned(), Value::String(hash));
    validate_record(&raw).map_err(|failure| WitnessError::Corrupt(failure.reason))?;
    canonical_record_bytes(&raw)?;
    Ok((record, raw))
}

pub fn build_record(body: WitnessBody, head: &ChainHead) -> Result<WitnessRecord, WitnessError> {
    build_record_with_raw(body, head).map(|(record, _)| record)
}

fn sha256_shape(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn git_oid_shape(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn is_nix_store_path(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("/nix/store/") else {
        return false;
    };
    let Some((hash, name)) = rest.split_once('-') else {
        return false;
    };
    hash.len() == 32
        && hash.bytes().all(|byte| {
            byte.is_ascii_digit()
                || matches!(byte, b'a'..=b'd' | b'f'..=b'n' | b'p'..=b's' | b'v'..=b'z')
        })
        && !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'_' | b'?' | b'=' | b'-')
        })
}

fn registry_component_shape(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && !matches!(value, "." | "..")
        && value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

pub fn validate_host_id(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("hostId must be non-empty".to_owned());
    }
    if value.len() > 96 {
        return Err("hostId must be at most 96 bytes".to_owned());
    }
    if value.chars().any(char::is_control) {
        return Err("hostId must contain no control characters".to_owned());
    }
    Ok(())
}

pub fn current_host_id() -> Result<String, WitnessError> {
    let host_id = gethostname::gethostname()
        .to_string_lossy()
        .trim()
        .to_owned();
    validate_host_id(&host_id).map_err(WitnessError::InvalidHostId)?;
    Ok(host_id)
}

#[derive(Debug)]
struct ValidationFailure {
    kind: VerifyProblemKind,
    reason: String,
}

impl ValidationFailure {
    fn new(kind: VerifyProblemKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            reason: reason.into(),
        }
    }

    fn invalid(reason: impl Into<String>) -> Self {
        Self::new(VerifyProblemKind::InvalidRecord, reason)
    }
}

fn canonical_field_index(field: &str) -> Option<usize> {
    [
        "schemaVersion",
        "recordType",
        "transitionTimestamp",
        "taskUuid",
        "verdict",
        "exitCode",
        "artifactContentHash",
        "storePaths",
        "drv",
        "gpuSeconds",
        "wallClock",
        "attempt",
        "leaseEpoch",
        "dedupKey",
        "payloadHash",
        "briefHash",
        "origin",
        "orchestration",
        "laborClass",
        "traceRef",
        "pools",
        "executor",
        "hostId",
        "charge",
        "model",
        "evidenceClass",
        "manifestHash",
        "completion",
        "resultRevision",
        "authorship",
        "authorshipSessions",
        "seq",
        "prevHash",
        "hash",
    ]
    .iter()
    .position(|candidate| *candidate == field)
}

fn validate_canonical_field_value(
    object: &Map<String, Value>,
    field: &str,
    canonical: &impl Serialize,
) -> Result<(), ValidationFailure> {
    let raw = object
        .get(field)
        .ok_or_else(|| ValidationFailure::invalid(format!("missing field {field}")))?;
    let canonical = serde_json::to_value(canonical)
        .map_err(|error| ValidationFailure::invalid(error.to_string()))?;
    if serde_json::to_string(raw).map_err(|error| ValidationFailure::invalid(error.to_string()))?
        != serde_json::to_string(&canonical)
            .map_err(|error| ValidationFailure::invalid(error.to_string()))?
    {
        return Err(ValidationFailure::invalid(format!(
            "field {field} is not in canonical serialized form"
        )));
    }
    Ok(())
}

fn validate_record(raw: &Value) -> Result<WitnessRecord, ValidationFailure> {
    let object = raw
        .as_object()
        .ok_or_else(|| ValidationFailure::invalid("line is not a JSON object"))?;
    for forbidden in ["parent", "parent_uuid", "parentUuid", "kind"] {
        if object.contains_key(forbidden) {
            return Err(ValidationFailure::invalid(format!(
                "{forbidden} is not a canonical witness field"
            )));
        }
    }
    if let Some((field, _)) = object.iter().find(|(_, value)| value.is_null()) {
        return Err(ValidationFailure::invalid(format!(
            "top-level field {field} must be omitted instead of null"
        )));
    }
    if object.get("schemaVersion").and_then(Value::as_u64)
        != Some(u64::from(WITNESS_SCHEMA_VERSION))
    {
        return Err(ValidationFailure::new(
            VerifyProblemKind::SchemaVersionInvalid,
            format!("schemaVersion must be the integer {WITNESS_SCHEMA_VERSION}"),
        ));
    }
    if object.get("recordType").and_then(Value::as_str) != Some("verdict") {
        return Err(ValidationFailure::new(
            VerifyProblemKind::RecordTypeInvalid,
            "recordType must be verdict",
        ));
    }
    let mut last_known = None;
    for field in object.keys() {
        let Some(index) = canonical_field_index(field) else {
            continue;
        };
        if last_known.is_some_and(|previous| index <= previous) {
            return Err(ValidationFailure::invalid(format!(
                "field {field} is not in canonical witness order"
            )));
        }
        last_known = Some(index);
    }
    let record: WitnessRecord = serde_json::from_value(raw.clone())
        .map_err(|error| ValidationFailure::invalid(error.to_string()))?;
    if record.schema_version != WITNESS_SCHEMA_VERSION {
        return Err(ValidationFailure::new(
            VerifyProblemKind::SchemaVersionInvalid,
            format!("schemaVersion must be the integer {WITNESS_SCHEMA_VERSION}"),
        ));
    }
    if record.record_type != RecordType::Verdict {
        return Err(ValidationFailure::new(
            VerifyProblemKind::RecordTypeInvalid,
            "recordType must be verdict",
        ));
    }
    validate_canonical_field_value(object, "wallClock", &record.wall_clock)?;
    if let Some(gpu_seconds) = record.gpu_seconds {
        validate_canonical_field_value(object, "gpuSeconds", &gpu_seconds)?;
    }
    validate_canonical_field_value(object, "origin", &record.origin)?;
    if let Some(drv) = &record.drv {
        validate_canonical_field_value(object, "drv", drv)?;
    }
    if let Some(orchestration) = &record.orchestration {
        validate_canonical_field_value(object, "orchestration", orchestration)?;
    }
    if let Some(charge) = &record.charge {
        validate_canonical_field_value(object, "charge", charge)?;
    }
    if let Some(authorship) = &record.authorship {
        validate_canonical_field_value(object, "authorship", authorship)?;
    }
    if let Some(authorship_sessions) = &record.authorship_sessions {
        validate_canonical_field_value(object, "authorshipSessions", authorship_sessions)?;
    }
    let parsed_timestamp = DateTime::parse_from_rfc3339(&record.transition_timestamp)
        .map_err(|_| ValidationFailure::invalid("transitionTimestamp is not RFC3339 UTC millis"))?;
    if parsed_timestamp
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Millis, true)
        != record.transition_timestamp
    {
        return Err(ValidationFailure::invalid(
            "transitionTimestamp is not canonical RFC3339 UTC millis",
        ));
    }
    if record
        .task_uuid
        .as_ref()
        .is_some_and(|task_uuid| Uuid::parse_str(task_uuid).is_err())
    {
        return Err(ValidationFailure::invalid("taskUuid is not a UUID"));
    }
    for (field, hash) in [
        ("artifactContentHash", record.artifact_content_hash.as_ref()),
        ("payloadHash", record.payload_hash.as_ref()),
        ("briefHash", record.brief_hash.as_ref()),
    ] {
        if hash.is_some_and(|hash| !sha256_shape(hash)) {
            return Err(ValidationFailure::invalid(format!(
                "{field} is not lowercase sha256 hex"
            )));
        }
    }
    if let Some(store_paths) = &record.store_paths {
        if store_paths.is_empty() {
            return Err(ValidationFailure::invalid(
                "storePaths must be non-empty when present",
            ));
        }
        if store_paths.iter().any(|path| !is_nix_store_path(path)) {
            return Err(ValidationFailure::invalid(
                "storePaths contains an invalid Nix store path",
            ));
        }
        if store_paths.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(ValidationFailure::invalid(
                "storePaths must be byte-ascending sorted and unique",
            ));
        }
    }
    if let Some(drv) = &record.drv {
        drv.validate().map_err(ValidationFailure::invalid)?;
    }
    if record
        .executor
        .as_ref()
        .is_some_and(|executor| !registry_component_shape(executor))
    {
        return Err(ValidationFailure::invalid(
            "executor is not a safe registry component",
        ));
    }
    if let Some(host_id) = &record.host_id {
        validate_host_id(host_id).map_err(ValidationFailure::invalid)?;
    }
    if let Some(orchestration) = &record.orchestration {
        orchestration
            .validate()
            .map_err(ValidationFailure::invalid)?;
    }
    record
        .origin
        .validate()
        .map_err(|error| ValidationFailure::invalid(error.to_string()))?;
    if (record.verdict == Verdict::Reused) != (record.labor_class == LaborClass::Reused) {
        return Err(ValidationFailure::invalid(
            "verdict reused and laborClass reused must appear together",
        ));
    }
    if (record.verdict == Verdict::Substituted) != (record.labor_class == LaborClass::Substituted) {
        return Err(ValidationFailure::invalid(
            "verdict substituted and laborClass substituted must appear together",
        ));
    }
    if matches!(record.verdict, Verdict::Reused | Verdict::Substituted) && record.exit_code != 0 {
        return Err(ValidationFailure::invalid(
            "reused and substituted verdicts require exitCode 0",
        ));
    }
    if record.verdict == Verdict::Substituted {
        let drv = record
            .drv
            .as_ref()
            .ok_or_else(|| ValidationFailure::invalid("substituted verdict requires drv"))?;
        let expected_dedup_key = format!("drv:{}", drv.drv_path);
        if record.task_uuid.is_none()
            || record.store_paths.is_none()
            || record.artifact_content_hash.is_some()
            || record.gpu_seconds.is_some()
            || record.charge.is_some()
            || record.wall_clock != 0.0
            || record.attempt != 1
            || record.lease_epoch != 1
            || record.pools != ["build"]
            || record.dedup_key.as_deref() != Some(expected_dedup_key.as_str())
        {
            return Err(ValidationFailure::invalid(
                "substituted verdict does not satisfy the cheap drv witness invariants",
            ));
        }
    }
    if record.seq == 0 {
        return Err(ValidationFailure::invalid(
            "seq missing or not a positive integer",
        ));
    }
    if !sha256_shape(&record.prev_hash) {
        return Err(ValidationFailure::invalid(
            "prevHash missing or not a lowercase sha256 hash",
        ));
    }
    if !sha256_shape(&record.hash) {
        return Err(ValidationFailure::invalid(
            "hash missing or not a lowercase sha256 hash",
        ));
    }
    if !record.wall_clock.is_finite()
        || record.wall_clock < 0.0
        || record.gpu_seconds.is_some_and(|value| !value.is_finite())
        || record
            .charge
            .as_ref()
            .is_some_and(|charge| !charge.amount.is_finite())
    {
        return Err(ValidationFailure::invalid(
            "numeric fields must be finite and wallClock must be non-negative",
        ));
    }
    if record.attempt == 0 {
        return Err(ValidationFailure::invalid("attempt must be at least 1"));
    }
    if record.lease_epoch == 0 {
        return Err(ValidationFailure::invalid("leaseEpoch must be at least 1"));
    }
    let mut canonical_pools = record.pools.clone();
    crate::poolset::canonicalize(&mut canonical_pools)
        .map_err(|error| ValidationFailure::invalid(error.to_string()))?;
    if canonical_pools != record.pools {
        return Err(ValidationFailure::invalid(
            "pools is not in canonical order",
        ));
    }
    if record
        .result_revision
        .as_ref()
        .is_some_and(|revision| !git_oid_shape(revision))
    {
        return Err(ValidationFailure::invalid(
            "resultRevision must be a 40- or 64-character lowercase Git object ID",
        ));
    }
    if let Some(authorship) = &record.authorship {
        if record.result_revision.is_none() {
            return Err(ValidationFailure::invalid(
                "authorship requires resultRevision",
            ));
        }
        if authorship.provider != "git-ai" || authorship.note_ref != "refs/notes/ai" {
            return Err(ValidationFailure::invalid(
                "authorship provider and noteRef must identify git-ai refs/notes/ai",
            ));
        }
        if authorship
            .notes_ref_target
            .as_ref()
            .is_some_and(|target| !git_oid_shape(target))
        {
            return Err(ValidationFailure::invalid(
                "authorship notesRefTarget must be a lowercase Git object ID",
            ));
        }
        if authorship
            .note_content_sha256
            .as_ref()
            .is_some_and(|hash| !sha256_shape(hash))
        {
            return Err(ValidationFailure::invalid(
                "authorship noteContentSha256 must be lowercase sha256 hex",
            ));
        }
        if authorship.status == AuthorshipStatus::Bound
            && (authorship.notes_ref_target.is_none() || authorship.note_content_sha256.is_none())
        {
            return Err(ValidationFailure::invalid(
                "bound authorship requires notesRefTarget and noteContentSha256",
            ));
        }
    }
    if let Some(sessions) = &record.authorship_sessions {
        let Some(authorship) = &record.authorship else {
            return Err(ValidationFailure::invalid(
                "authorshipSessions requires authorship",
            ));
        };
        if !matches!(
            authorship.status,
            AuthorshipStatus::Bound | AuthorshipStatus::Mismatch
        ) {
            return Err(ValidationFailure::invalid(
                "authorshipSessions requires bound or mismatch authorship",
            ));
        }
        if sessions.is_empty() || sessions.len() > 16 {
            return Err(ValidationFailure::invalid(
                "authorshipSessions must contain 1..=16 observations",
            ));
        }
        for session in sessions {
            session.validate().map_err(ValidationFailure::invalid)?;
        }
        if sessions.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(ValidationFailure::invalid(
                "authorshipSessions must be sorted and unique",
            ));
        }
    }
    Ok(record)
}

#[derive(Debug)]
struct ParsedRecord {
    record: WitnessRecord,
    raw: Value,
    line: usize,
}

pub fn verify_reader(mut reader: impl BufRead) -> VerifyReport {
    let mut problems = Vec::new();
    let mut valid = Vec::new();
    let mut line_number = 0;

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => line_number += 1,
            Err(error) => {
                problems.push(VerifyProblem {
                    seq: None,
                    line: line_number + 1,
                    kind: VerifyProblemKind::ParseError,
                    reason: format!("cannot read line: {error}"),
                });
                break;
            }
        }
        if !line.ends_with('\n') {
            problems.push(VerifyProblem {
                seq: None,
                line: line_number,
                kind: VerifyProblemKind::ParseError,
                reason: "record is not LF-terminated".to_owned(),
            });
            continue;
        }
        line.pop();
        if line.trim().is_empty() {
            problems.push(VerifyProblem {
                seq: None,
                line: line_number,
                kind: VerifyProblemKind::ParseError,
                reason: "blank lines are not canonical witness records".to_owned(),
            });
            continue;
        }
        let raw: Value = match serde_json::from_str(&line) {
            Ok(raw) => raw,
            Err(_) => {
                problems.push(VerifyProblem {
                    seq: None,
                    line: line_number,
                    kind: VerifyProblemKind::ParseError,
                    reason: "line is not valid JSON".to_owned(),
                });
                continue;
            }
        };
        let canonical_line = match serde_json::to_string(&raw) {
            Ok(canonical) => canonical,
            Err(error) => {
                problems.push(VerifyProblem {
                    seq: raw.get("seq").and_then(Value::as_u64),
                    line: line_number,
                    kind: VerifyProblemKind::InvalidRecord,
                    reason: format!("record cannot be canonically serialized: {error}"),
                });
                continue;
            }
        };
        if canonical_line != line {
            let offset = line
                .bytes()
                .zip(canonical_line.bytes())
                .position(|(actual, canonical)| actual != canonical)
                .unwrap_or_else(|| line.len().min(canonical_line.len()));
            problems.push(VerifyProblem {
                seq: raw.get("seq").and_then(Value::as_u64),
                line: line_number,
                kind: VerifyProblemKind::InvalidRecord,
                reason: format!("record bytes are not compact canonical JSON at byte {offset}"),
            });
            continue;
        }
        match validate_record(&raw) {
            Ok(record) => valid.push(ParsedRecord {
                record,
                raw,
                line: line_number,
            }),
            Err(failure) => problems.push(VerifyProblem {
                seq: raw.get("seq").and_then(Value::as_u64),
                line: line_number,
                kind: failure.kind,
                reason: failure.reason,
            }),
        }
    }

    for parsed in &valid {
        match compute_hash_value(&parsed.raw) {
            Ok(recomputed) if recomputed != parsed.record.hash => problems.push(VerifyProblem {
                seq: Some(parsed.record.seq),
                line: parsed.line,
                kind: VerifyProblemKind::HashMismatch,
                reason: format!(
                    "stored hash {} != recomputed {} (line tampered)",
                    parsed.record.hash, recomputed
                ),
            }),
            Err(error) => problems.push(VerifyProblem {
                seq: Some(parsed.record.seq),
                line: parsed.line,
                kind: VerifyProblemKind::InvalidRecord,
                reason: error.to_string(),
            }),
            _ => {}
        }
    }

    let mut previous_hash = GENESIS_PREV_HASH;
    for parsed in &valid {
        if parsed.record.prev_hash != previous_hash {
            problems.push(VerifyProblem {
                seq: Some(parsed.record.seq),
                line: parsed.line,
                kind: VerifyProblemKind::PrevHashMismatch,
                reason: format!(
                    "prev_hash {} != predecessor hash {} (chain broken)",
                    parsed.record.prev_hash, previous_hash
                ),
            });
        }
        previous_hash = &parsed.record.hash;
    }

    let mut previous_seq = 0;
    for parsed in &valid {
        if parsed.record.seq <= previous_seq {
            problems.push(VerifyProblem {
                seq: Some(parsed.record.seq),
                line: parsed.line,
                kind: VerifyProblemKind::SeqOrder,
                reason: format!(
                    "seq {} does not strictly follow {} (reordered or duplicate)",
                    parsed.record.seq, previous_seq
                ),
            });
        }
        previous_seq = parsed.record.seq;
    }

    let mut sequence_lines = HashMap::new();
    let mut sequences = BTreeSet::new();
    for parsed in &valid {
        if !sequences.insert(parsed.record.seq) {
            problems.push(VerifyProblem {
                seq: Some(parsed.record.seq),
                line: parsed.line,
                kind: VerifyProblemKind::SeqDuplicate,
                reason: format!("seq {} appears more than once", parsed.record.seq),
            });
        }
        sequence_lines.insert(parsed.record.seq, parsed.line);
    }
    for (offset, sequence) in sequences.iter().enumerate() {
        let expected = offset as u64 + 1;
        if *sequence != expected {
            problems.push(VerifyProblem {
                seq: Some(*sequence),
                line: sequence_lines[sequence],
                kind: VerifyProblemKind::SeqGap,
                reason: format!("expected seq {expected} but found {sequence} (missing line)"),
            });
            break;
        }
    }

    VerifyReport {
        ok: problems.is_empty(),
        records: valid.len(),
        first_seq: valid.first().map(|parsed| parsed.record.seq),
        last_seq: valid.last().map(|parsed| parsed.record.seq),
        problems,
    }
}

pub fn verify_file(path: &Path) -> Result<VerifyReport, WitnessError> {
    if !path.exists() {
        return Ok(VerifyReport {
            ok: true,
            records: 0,
            first_seq: None,
            last_seq: None,
            problems: Vec::new(),
        });
    }
    let file = File::open(path).map_err(|source| io_error(path, source))?;
    Ok(verify_reader(BufReader::new(file)))
}

pub fn read_verified_records(
    path: &Path,
) -> Result<(VerifyReport, Vec<WitnessRecord>), WitnessError> {
    if !path.exists() {
        return Ok((
            VerifyReport {
                ok: true,
                records: 0,
                first_seq: None,
                last_seq: None,
                problems: Vec::new(),
            },
            Vec::new(),
        ));
    }
    let mut file = File::open(path).map_err(|source| io_error(path, source))?;
    file.lock_shared()
        .map_err(|source| io_error(path, source))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    let report = verify_reader(Cursor::new(bytes.as_slice()));
    if !report.ok {
        return Ok((report, Vec::new()));
    }
    let records = BufReader::new(Cursor::new(bytes))
        .lines()
        .filter_map(|line| match line {
            Ok(line) if line.trim().is_empty() => None,
            other => Some(other),
        })
        .map(|line| {
            let line = line.map_err(|source| io_error(path, source))?;
            let raw = serde_json::from_str(&line)?;
            validate_record(&raw).map_err(|failure| WitnessError::Corrupt(failure.reason))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((report, records))
}

pub fn read_records(path: &Path) -> Result<Vec<WitnessRecord>, WitnessError> {
    let file = File::open(path).map_err(|source| io_error(path, source))?;
    BufReader::new(file)
        .lines()
        .filter_map(|line| match line {
            Ok(line) if line.trim().is_empty() => None,
            other => Some(other),
        })
        .map(|line| {
            let line = line.map_err(|source| io_error(path, source))?;
            let raw = serde_json::from_str(&line)?;
            validate_record(&raw).map_err(|failure| WitnessError::Corrupt(failure.reason))
        })
        .collect()
}

pub fn counts_toward_canonical_gpu_seconds(record: &WitnessRecord) -> bool {
    record.labor_class == LaborClass::Fresh
        && !matches!(
            record.verdict,
            Verdict::CleanExitNoArtifact
                | Verdict::PoolVanished
                | Verdict::Preempted
                | Verdict::Cancelled
                | Verdict::Substituted
        )
}

pub fn canonical_gpu_seconds(records: impl IntoIterator<Item = WitnessRecord>) -> f64 {
    records
        .into_iter()
        .filter(counts_toward_canonical_gpu_seconds)
        .filter_map(|record| record.gpu_seconds)
        .sum()
}

fn looks_like_old_format(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .any(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .is_some_and(|object| {
                    !object.contains_key("schemaVersion")
                        && ["task_uuid", "transition_timestamp", "pool", "prev_hash"]
                            .iter()
                            .any(|field| object.contains_key(*field))
                })
        })
}

fn old_format_error(path: &Path) -> WitnessError {
    WitnessError::OldFormat {
        path: path.to_owned(),
        archive: PathBuf::from(format!(
            "{}.pre-{}",
            path.display(),
            Utc::now().format("%Y-%m-%d")
        )),
    }
}

fn scan_head(file: &mut File, path: &Path) -> Result<ChainHead, WitnessError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_error(path, source))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.is_empty() {
        return Ok(ChainHead::default());
    }
    if looks_like_old_format(&bytes) {
        return Err(old_format_error(path));
    }
    let complete_len = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position + 1);
    if complete_len != bytes.len() {
        file.set_len(complete_len as u64)
            .map_err(|source| io_error(path, source))?;
        bytes.truncate(complete_len);
    }
    let report = verify_reader(BufReader::new(bytes.as_slice()));
    if !report.ok {
        return Err(WitnessError::Corrupt(
            serde_json::to_string(&report.problems)
                .unwrap_or_else(|_| "verification failed".to_owned()),
        ));
    }
    if report.records == 0 {
        return Ok(ChainHead::default());
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| WitnessError::Corrupt(format!("ledger is not UTF-8: {error}")))?;
    let last = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| WitnessError::Corrupt("ledger has no complete record".to_owned()))?;
    let raw: Value = serde_json::from_str(last)?;
    let record = validate_record(&raw).map_err(|failure| WitnessError::Corrupt(failure.reason))?;
    Ok(ChainHead {
        seq: record.seq,
        hash: record.hash,
    })
}

pub struct WitnessLedger {
    path: PathBuf,
    file: File,
    head: ChainHead,
}

impl WitnessLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WitnessError> {
        let path = path.as_ref().to_owned();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        }
        let created = !path.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .map_err(|source| io_error(&path, source))?;
        file.lock_exclusive()
            .map_err(|source| io_error(&path, source))?;
        let head = scan_head(&mut file, &path);
        let unlock = FileExt::unlock(&file).map_err(|source| io_error(&path, source));
        let head = head?;
        unlock?;
        if created {
            if let Some(parent) = path.parent() {
                File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|source| io_error(parent, source))?;
            }
        }
        Ok(Self { path, file, head })
    }

    pub fn head(&self) -> &ChainHead {
        &self.head
    }

    pub fn append(&mut self, body: WitnessBody) -> Result<WitnessRecord, WitnessError> {
        self.file
            .lock_exclusive()
            .map_err(|source| io_error(&self.path, source))?;
        let result = (|| {
            self.head = scan_head(&mut self.file, &self.path)?;
            let (record, raw) = build_record_with_raw(body, &self.head)?;
            let mut line = canonical_record_bytes(&raw)?;
            line.push(b'\n');
            self.file
                .write_all(&line)
                .map_err(|source| io_error(&self.path, source))?;
            self.file
                .sync_all()
                .map_err(|source| io_error(&self.path, source))?;
            self.head = ChainHead {
                seq: record.seq,
                hash: record.hash.clone(),
            };
            Ok(record)
        })();
        let unlock = FileExt::unlock(&self.file).map_err(|source| io_error(&self.path, source));
        match (result, unlock) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(record), Ok(())) => Ok(record),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttestationRecord {
    pub observed_at: String,
    pub payload: Value,
    pub seq: u64,
    pub prev_hash: String,
    pub hash: String,
}

pub fn append_attestation(path: &Path, payload: Value) -> Result<AttestationRecord, WitnessError> {
    let parent = durable_parent(path);
    std::fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    file.lock_exclusive()
        .map_err(|source| io_error(path, source))?;
    truncate_incomplete_attestation_tail(&mut file, path)?;
    let (seq, previous_hash) = scan_attestation_head(&mut file, path)?;
    let mut record = AttestationRecord {
        observed_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        payload,
        seq: seq + 1,
        prev_hash: previous_hash,
        hash: String::new(),
    };
    record.hash = compute_hash_value(&serde_json::to_value(&record)?)?;
    let mut line = serde_json::to_vec(&record)?;
    line.push(b'\n');
    file.write_all(&line)
        .map_err(|source| io_error(path, source))?;
    file.sync_all().map_err(|source| io_error(path, source))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(parent, source))?;
    Ok(record)
}

fn durable_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub fn repair_attestation_tail(path: &Path) -> Result<(), WitnessError> {
    if !path.exists() {
        return Ok(());
    }
    let bytes = std::fs::read(path).map_err(|source| io_error(path, source))?;
    if bytes.is_empty() || bytes.last() == Some(&b'\n') {
        return Ok(());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    file.lock_exclusive()
        .map_err(|source| io_error(path, source))?;
    if truncate_incomplete_attestation_tail(&mut file, path)? {
        file.sync_all().map_err(|source| io_error(path, source))?;
    }
    Ok(())
}

fn truncate_incomplete_attestation_tail(
    file: &mut File,
    path: &Path,
) -> Result<bool, WitnessError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_error(path, source))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.is_empty() || bytes.last() == Some(&b'\n') {
        return Ok(false);
    }
    let complete_len = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position + 1);
    if serde_json::from_slice::<Value>(&bytes[complete_len..]).is_ok() {
        scan_attestation_head(file, path)?;
        file.seek(SeekFrom::End(0))
            .map_err(|source| io_error(path, source))?;
        file.write_all(b"\n")
            .map_err(|source| io_error(path, source))?;
        return Ok(true);
    }
    file.set_len(complete_len as u64)
        .map_err(|source| io_error(path, source))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_error(path, source))?;
    Ok(true)
}

fn scan_attestation_head(file: &mut File, path: &Path) -> Result<(u64, String), WitnessError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_error(path, source))?;
    let reader = BufReader::new(file.try_clone().map_err(|source| io_error(path, source))?);
    let mut expected_seq = 1;
    let mut previous_hash = GENESIS_PREV_HASH.to_owned();
    for line in reader.lines() {
        let line = line.map_err(|source| io_error(path, source))?;
        if line.trim().is_empty() {
            continue;
        }
        let raw: Value = serde_json::from_str(&line)?;
        let record: AttestationRecord = serde_json::from_value(raw.clone())?;
        if record.seq != expected_seq
            || record.prev_hash != previous_hash
            || compute_hash_value(&raw)? != record.hash
        {
            return Err(WitnessError::Corrupt(format!(
                "attestation chain breaks at seq {}",
                record.seq
            )));
        }
        expected_seq += 1;
        previous_hash = record.hash;
    }
    Ok((expected_seq - 1, previous_hash))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttestationVerifyReport {
    pub ok: bool,
    pub records: usize,
    pub authentication: &'static str,
    pub problem: Option<String>,
}

pub fn verify_attestations(path: &Path) -> Result<AttestationVerifyReport, WitnessError> {
    if !path.exists() {
        return Ok(AttestationVerifyReport {
            ok: true,
            records: 0,
            authentication: "unauthenticated-by-construction",
            problem: None,
        });
    }
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    match scan_attestation_head(&mut file, path) {
        Ok((records, _)) => Ok(AttestationVerifyReport {
            ok: true,
            records: records as usize,
            authentication: "unauthenticated-by-construction",
            problem: None,
        }),
        Err(WitnessError::Corrupt(problem)) => Ok(AttestationVerifyReport {
            ok: false,
            records: 0,
            authentication: "unauthenticated-by-construction",
            problem: Some(problem),
        }),
        Err(error) => Err(error),
    }
}

pub fn read_verified_attestations(
    path: &Path,
) -> Result<(AttestationVerifyReport, Vec<AttestationRecord>), WitnessError> {
    if !path.exists() {
        return Ok((
            AttestationVerifyReport {
                ok: true,
                records: 0,
                authentication: "unauthenticated-by-construction",
                problem: None,
            },
            Vec::new(),
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    file.lock_shared()
        .map_err(|source| io_error(path, source))?;
    let report = match scan_attestation_head(&mut file, path) {
        Ok((records, _)) => AttestationVerifyReport {
            ok: true,
            records: records as usize,
            authentication: "unauthenticated-by-construction",
            problem: None,
        },
        Err(WitnessError::Corrupt(problem)) => {
            return Ok((
                AttestationVerifyReport {
                    ok: false,
                    records: 0,
                    authentication: "unauthenticated-by-construction",
                    problem: Some(problem),
                },
                Vec::new(),
            ))
        }
        Err(error) => return Err(error),
    };
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_error(path, source))?;
    let records = BufReader::new(file)
        .lines()
        .filter_map(|line| match line {
            Ok(line) if line.trim().is_empty() => None,
            other => Some(other),
        })
        .map(|line| {
            let line = line.map_err(|source| io_error(path, source))?;
            serde_json::from_str(&line).map_err(WitnessError::Json)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((report, records))
}

pub fn parse_rfc3339(value: &str) -> bool {
    DateTime::parse_from_rfc3339(value).is_ok()
}

#[cfg(test)]
mod tests {
    use proptest::collection::vec;
    use proptest::prelude::*;

    use super::*;
    use crate::taskdb::EnqueueSource;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test/fixtures/ledger")
            .join(name)
    }

    fn body() -> WitnessBody {
        WitnessBody {
            task_uuid: Some("b2c40001-0000-4000-8000-000000000001".to_owned()),
            transition_timestamp: "2026-07-09T10:00:01.100Z".to_owned(),
            verdict: Verdict::Pass,
            exit_code: 0,
            artifact_content_hash: Some(format!("sha256:{}", "a".repeat(64))),
            store_paths: None,
            drv: None,
            gpu_seconds: Some(42.5),
            wall_clock: 44.0,
            attempt: 1,
            lease_epoch: 42,
            dedup_key: Some("ocr:paper-0001".to_owned()),
            payload_hash: None,
            brief_hash: None,
            origin: AdmissionOrigin::direct(EnqueueSource::Manual),
            orchestration: None,
            labor_class: LaborClass::Fresh,
            trace_ref: None,
            pools: vec!["worker-gpu".to_owned()],
            executor: None,
            host_id: None,
            charge: Some(Charge {
                unit: "gpu-seconds".to_owned(),
                amount: 42.5,
                class_name: "verifiable".to_owned(),
            }),
            model: Some("vllm/qwen2-vl-ocr".to_owned()),
            evidence_class: None,
            manifest_hash: None,
            completion: None,
            result_revision: None,
            authorship: None,
            authorship_sessions: None,
        }
    }

    fn fully_populated_body() -> WitnessBody {
        WitnessBody {
            task_uuid: Some("b2c40001-0000-4000-8000-000000000002".to_owned()),
            transition_timestamp: "2026-07-26T12:34:56.789Z".to_owned(),
            verdict: Verdict::Pass,
            exit_code: 0,
            artifact_content_hash: Some(format!("sha256:{}", "a".repeat(64))),
            store_paths: Some(vec![
                format!("/nix/store/{}-dev", "2".repeat(32)),
                format!("/nix/store/{}-out", "3".repeat(32)),
            ]),
            drv: Some(Derivation {
                drv_path: format!("/nix/store/{}-package.drv", "1".repeat(32)),
                outputs: vec![
                    DerivationOutput {
                        name: "out".to_owned(),
                        path: format!("/nix/store/{}-out", "3".repeat(32)),
                    },
                    DerivationOutput {
                        name: "dev".to_owned(),
                        path: format!("/nix/store/{}-dev", "2".repeat(32)),
                    },
                ],
            }),
            gpu_seconds: Some(42.5),
            wall_clock: 44.0,
            attempt: 2,
            lease_epoch: 42,
            dedup_key: Some("drv:package".to_owned()),
            payload_hash: Some(format!("sha256:{}", "b".repeat(64))),
            brief_hash: Some(format!("sha256:{}", "c".repeat(64))),
            origin: AdmissionOrigin::producer("calendar", EnqueueSource::Calendar),
            orchestration: Some(
                serde_json::from_value(serde_json::json!({
                    "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
                    "maxNodes": 12,
                    "promptRevision": format!("sha256:{}", "d".repeat(64)),
                    "skillRevision": "review-agent-v3",
                    "selection": {"members": ["b", "a"]}
                }))
                .unwrap(),
            ),
            labor_class: LaborClass::Fresh,
            trace_ref: Some("trace:session-2".to_owned()),
            pools: vec!["worker-gpu".to_owned(), "worker-cpu".to_owned()],
            executor: Some("ssh-worker-1".to_owned()),
            host_id: Some("worker-1".to_owned()),
            charge: Some(Charge {
                unit: "gpu-seconds".to_owned(),
                amount: 42.5,
                class_name: "verifiable".to_owned(),
            }),
            model: Some("vllm/qwen2-vl-ocr".to_owned()),
            evidence_class: Some(serde_json::json!({"kind": "artifact", "rank": 1})),
            manifest_hash: Some(serde_json::json!(format!("sha256:{}", "e".repeat(64)))),
            completion: Some(
                serde_json::from_value(serde_json::json!({
                    "schemaVersion": 1,
                    "execution": {"status": "success", "exitCode": 0, "reason": "process exited with code 0"},
                    "gates": {"status": "pass", "artifact": {"commit": "abc"}, "gates": []},
                    "acceptance": {"status": "accepted", "policy": "execution-and-gates", "reason": "all gates passed"}
                }))
                .unwrap(),
            ),
            result_revision: Some("f".repeat(40)),
            authorship: Some(Authorship {
                provider: "git-ai".to_owned(),
                provider_version: "1.2.3".to_owned(),
                note_ref: "refs/notes/ai".to_owned(),
                status: AuthorshipStatus::Bound,
                notes_ref_target: Some("a".repeat(40)),
                note_content_sha256: Some(format!("sha256:{}", "f".repeat(64))),
                reason: Some("matched".to_owned()),
            }),
            authorship_sessions: Some(vec![AuthorshipSession {
                tool: "codex".to_owned(),
                id: "session-42".to_owned(),
                model: "gpt-5".to_owned(),
            }]),
        }
    }

    fn fixture_records() -> Vec<WitnessRecord> {
        let first = build_record(body(), &ChainHead::default()).unwrap();
        let mut head = ChainHead {
            seq: first.seq,
            hash: first.hash.clone(),
        };

        let drv_path = format!("/nix/store/{}-fixture.drv", "1".repeat(32));
        let output_path = format!("/nix/store/{}-fixture", "2".repeat(32));
        let mut with_store = body();
        with_store.task_uuid = Some("b2c40001-0000-4000-8000-000000000003".to_owned());
        with_store.transition_timestamp = "2026-07-26T12:00:02.000Z".to_owned();
        with_store.store_paths = Some(vec![output_path.clone()]);
        with_store.drv = Some(Derivation {
            drv_path: drv_path.clone(),
            outputs: vec![DerivationOutput {
                name: "out".to_owned(),
                path: output_path.clone(),
            }],
        });
        with_store.dedup_key = Some(format!("drv:{drv_path}"));
        with_store.pools = vec!["build".to_owned()];
        let second = build_record(with_store, &head).unwrap();
        head = ChainHead {
            seq: second.seq,
            hash: second.hash.clone(),
        };

        let mut substituted = body();
        substituted.task_uuid = Some("b2c40001-0000-4000-8000-000000000004".to_owned());
        substituted.transition_timestamp = "2026-07-26T12:00:03.000Z".to_owned();
        substituted.verdict = Verdict::Substituted;
        substituted.exit_code = 0;
        substituted.artifact_content_hash = None;
        substituted.store_paths = Some(vec![output_path.clone()]);
        substituted.drv = Some(Derivation {
            drv_path: drv_path.clone(),
            outputs: vec![DerivationOutput {
                name: "out".to_owned(),
                path: output_path,
            }],
        });
        substituted.gpu_seconds = None;
        substituted.wall_clock = 0.0;
        substituted.attempt = 1;
        substituted.lease_epoch = 1;
        substituted.dedup_key = Some(format!("drv:{drv_path}"));
        substituted.labor_class = LaborClass::Substituted;
        substituted.pools = vec!["build".to_owned()];
        substituted.charge = None;
        let third = build_record(substituted, &head).unwrap();
        head = ChainHead {
            seq: third.seq,
            hash: third.hash.clone(),
        };

        let fourth = build_record(fully_populated_body(), &head).unwrap();
        vec![first, second, third, fourth]
    }

    fn generated_witness_chain(values: &[u64]) -> Vec<u8> {
        let mut head = ChainHead::default();
        let mut bytes = Vec::new();
        for (ordinal, value) in values.iter().enumerate() {
            let mut next = body();
            next.task_uuid = Some(format!("b2c40001-0000-4000-8000-{ordinal:012x}"));
            next.dedup_key = Some(format!("proptest:{ordinal}:{value}"));
            let record = build_record(next, &head).unwrap();
            bytes.extend(serde_json::to_vec(&record).unwrap());
            bytes.push(b'\n');
            head = ChainHead {
                seq: record.seq,
                hash: record.hash,
            };
        }
        bytes
    }

    fn generated_attestation_chain(values: &[u64]) -> Vec<u8> {
        let mut previous_hash = GENESIS_PREV_HASH.to_owned();
        let mut bytes = Vec::new();
        for (ordinal, value) in values.iter().enumerate() {
            let mut record = AttestationRecord {
                observed_at: "2026-07-27T00:00:00.000Z".to_owned(),
                payload: serde_json::json!({"ordinal": ordinal, "value": value}),
                seq: ordinal as u64 + 1,
                prev_hash: previous_hash,
                hash: String::new(),
            };
            record.hash = compute_hash_value(&serde_json::to_value(&record).unwrap()).unwrap();
            bytes.extend(serde_json::to_vec(&record).unwrap());
            bytes.push(b'\n');
            previous_hash = record.hash;
        }
        bytes
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic_chain_verification(
            input in vec(any::<u8>(), 0..16_384),
        ) {
            let report = verify_reader(Cursor::new(input.as_slice()));
            prop_assert!(report.records <= input.iter().filter(|byte| **byte == b'\n').count());

            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("attestations.jsonl");
            std::fs::write(&path, &input).unwrap();
            let _ = verify_attestations(&path);
        }

        #[test]
        fn every_single_byte_witness_mutation_is_detected(
            values in vec(any::<u64>(), 1..=8),
            mutation_offset in any::<usize>(),
            mutation_mask in 1_u8..=u8::MAX,
        ) {
            let mut chain = generated_witness_chain(&values);
            let original = verify_reader(Cursor::new(chain.as_slice()));
            prop_assert!(original.ok, "generated chain was invalid: {:?}", original.problems);

            // The final LF frames the last record rather than carrying any of
            // it. Replacing it leaves every record byte-identical and the chain
            // legitimately valid, so it is not a mutation this property covers.
            // Interior LFs stay in the domain: corrupting one merges records and
            // must still be caught.
            let index = mutation_offset % (chain.len() - 1);
            chain[index] ^= mutation_mask;
            let mutated = verify_reader(Cursor::new(chain.as_slice()));
            prop_assert!(!mutated.ok, "mutation at byte {} was accepted", index);
        }

        #[test]
        fn every_single_byte_attestation_mutation_is_detected(
            values in vec(any::<u64>(), 1..=8),
            mutation_offset in any::<usize>(),
            mutation_mask in 1_u8..=u8::MAX,
        ) {
            let mut chain = generated_attestation_chain(&values);
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("attestations.jsonl");
            std::fs::write(&path, &chain).unwrap();
            let original = verify_attestations(&path).unwrap();
            prop_assert!(original.ok, "generated attestation chain was invalid");

            // See the witness-chain property above: the trailing LF is framing,
            // not content.
            let index = mutation_offset % (chain.len() - 1);
            chain[index] ^= mutation_mask;
            std::fs::write(&path, &chain).unwrap();
            let detected = match verify_attestations(&path) {
                Ok(report) => !report.ok,
                Err(_) => true,
            };
            prop_assert!(detected, "mutation at byte {} was accepted", index);
        }
    }

    #[test]
    fn ledger_fixture_is_green_and_tamper_is_red() {
        let valid = verify_file(&fixture("valid.jsonl")).unwrap();
        assert!(valid.ok, "{:?}", valid.problems);
        assert_eq!(valid.records, 4);

        let tampered = verify_file(&fixture("tampered.jsonl")).unwrap();
        assert!(!tampered.ok);
        assert!(tampered
            .problems
            .iter()
            .any(|problem| problem.kind == VerifyProblemKind::HashMismatch));
    }

    #[test]
    fn valid_fixture_is_the_exact_builder_output() {
        let actual = fixture_records()
            .iter()
            .map(|record| serde_json::to_string(record).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        assert_eq!(
            std::fs::read_to_string(fixture("valid.jsonl")).unwrap(),
            actual
        );
    }

    #[test]
    fn duration_derived_wall_clock_is_written_as_canonical_json() {
        let mut observed = body();
        observed.wall_clock = std::time::Duration::from_nanos(1_212_416_383).as_secs_f64();
        assert_eq!(
            serde_json::to_string(&observed.wall_clock).unwrap(),
            "1.2124163829999999"
        );

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("witness.jsonl");
        let record = WitnessLedger::open(&path)
            .unwrap()
            .append(observed)
            .unwrap();
        assert_eq!(compute_hash(&record).unwrap(), record.hash);

        let report = verify_file(&path).unwrap();
        assert!(report.ok, "{:?}", report.problems);
        assert!(std::fs::read_to_string(path)
            .unwrap()
            .contains(r#""wallClock":1.212416383,"#));
    }

    #[test]
    fn old_format_is_red_and_open_returns_an_actionable_archive_error() {
        let report = verify_file(&fixture("old-format.jsonl")).unwrap();
        assert!(!report.ok);
        assert!(report
            .problems
            .iter()
            .any(|problem| problem.kind == VerifyProblemKind::SchemaVersionInvalid));

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("witness.jsonl");
        std::fs::copy(fixture("old-format.jsonl"), &path).unwrap();
        let error = match WitnessLedger::open(&path) {
            Err(error) => error,
            Ok(_) => panic!("old-format ledger unexpectedly opened"),
        };
        assert!(matches!(error, WitnessError::OldFormat { .. }));
        let message = error.to_string();
        assert!(message.contains("archive it aside before first boot"));
        assert!(message.contains("mv --"));
        assert!(message.contains(".pre-"));
    }

    #[test]
    fn fully_populated_record_pins_canonical_hash_input_bytes() {
        let record = build_record(fully_populated_body(), &ChainHead::default()).unwrap();
        let raw = serde_json::to_value(&record).unwrap();
        assert_eq!(
            canonical_hash_input(&raw).unwrap(),
            r#"{"schemaVersion":2,"recordType":"verdict","transitionTimestamp":"2026-07-26T12:34:56.789Z","taskUuid":"b2c40001-0000-4000-8000-000000000002","verdict":"pass","exitCode":0,"artifactContentHash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","storePaths":["/nix/store/22222222222222222222222222222222-dev","/nix/store/33333333333333333333333333333333-out"],"drv":{"drvPath":"/nix/store/11111111111111111111111111111111-package.drv","outputs":[{"name":"dev","path":"/nix/store/22222222222222222222222222222222-dev"},{"name":"out","path":"/nix/store/33333333333333333333333333333333-out"}]},"gpuSeconds":42.5,"wallClock":44.0,"attempt":2,"leaseEpoch":42,"dedupKey":"drv:package","payloadHash":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","briefHash":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","origin":{"schemaVersion":1,"source":"calendar","producer":{"name":"calendar","kind":"calendar"}},"orchestration":{"flowRunId":"018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321","maxNodes":12,"promptRevision":"sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","skillRevision":"review-agent-v3","selection":{"members":["b","a"]}},"laborClass":"fresh","traceRef":"trace:session-2","pools":["worker-cpu","worker-gpu"],"executor":"ssh-worker-1","hostId":"worker-1","charge":{"unit":"gpu-seconds","amount":42.5,"class":"verifiable"},"model":"vllm/qwen2-vl-ocr","evidenceClass":{"kind":"artifact","rank":1},"manifestHash":"sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","completion":{"schemaVersion":1,"execution":{"status":"success","exitCode":0,"reason":"process exited with code 0"},"gates":{"status":"pass","artifact":{"commit":"abc"},"gates":[]},"acceptance":{"status":"accepted","policy":"execution-and-gates","reason":"all gates passed"}},"resultRevision":"ffffffffffffffffffffffffffffffffffffffff","authorship":{"provider":"git-ai","providerVersion":"1.2.3","noteRef":"refs/notes/ai","status":"bound","notesRefTarget":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","noteContentSha256":"sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff","reason":"matched"},"authorshipSessions":[{"tool":"codex","id":"session-42","model":"gpt-5"}],"seq":1,"prevHash":"sha256:0000000000000000000000000000000000000000000000000000000000000000","hash":""}"#
        );
    }

    #[test]
    fn optional_metadata_is_ordered_before_seq_and_absent_stays_absent() {
        let absent = build_record(body(), &ChainHead::default()).unwrap();
        let absent_json = serde_json::to_string(&absent).unwrap();
        assert!(!absent_json.contains("evidenceClass"));
        assert!(!absent_json.contains("manifestHash"));
        assert!(!absent_json.contains("payloadHash"));
        assert!(!absent_json.contains("briefHash"));
        assert!(!absent_json.contains("orchestration"));

        let mut present_body = body();
        present_body.payload_hash = Some(format!("sha256:{}", "b".repeat(64)));
        present_body.brief_hash = Some(format!("sha256:{}", "c".repeat(64)));
        present_body.orchestration = Some(
            serde_json::from_value(serde_json::json!({
                "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
                "scriptHash": "sha256-script"
            }))
            .unwrap(),
        );
        present_body.evidence_class = Some(Value::String("opaque/class".to_owned()));
        present_body.manifest_hash = Some(Value::String("urn:manifest:anything".to_owned()));
        let present = build_record(present_body, &ChainHead::default()).unwrap();
        let json = serde_json::to_string(&present).unwrap();
        assert!(json.find("payloadHash").unwrap() < json.find("laborClass").unwrap());
        assert!(json.find("briefHash").unwrap() < json.find("orchestration").unwrap());
        assert!(json.find("orchestration").unwrap() < json.find("laborClass").unwrap());
        assert!(json.find("evidenceClass").unwrap() < json.find("manifestHash").unwrap());
        assert!(json.find("manifestHash").unwrap() < json.find("\"seq\"").unwrap());
        let report = verify_reader(BufReader::new(format!("{json}\n").as_bytes()));
        assert!(report.ok, "{:?}", report.problems);
    }

    #[test]
    fn optional_revision_keys_are_absent_unless_supplied_and_change_the_hash() {
        let mut baseline_body = body();
        baseline_body.orchestration = Some(
            serde_json::from_value(serde_json::json!({
                "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
                "scriptHash": "sha256:legacy"
            }))
            .unwrap(),
        );
        let baseline = build_record(baseline_body, &ChainHead::default()).unwrap();
        let baseline_json = serde_json::to_string(&baseline).unwrap();
        assert!(!baseline_json.contains("promptRevision"));
        assert!(!baseline_json.contains("skillRevision"));

        let mut revised_body = body();
        revised_body.orchestration = Some(
            serde_json::from_value(serde_json::json!({
                "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
                "scriptHash": "sha256:legacy",
                "promptRevision": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "skillRevision": "review-agent-v3"
            }))
            .unwrap(),
        );
        let revised = build_record(revised_body, &ChainHead::default()).unwrap();
        assert_ne!(revised.hash, baseline.hash);
    }

    #[test]
    fn authorship_sessions_are_bounded_canonical_and_hash_covered() {
        let mut reordered = fully_populated_body();
        reordered.authorship_sessions = Some(vec![
            AuthorshipSession {
                tool: "codex".to_owned(),
                id: "session-z".to_owned(),
                model: "gpt-5".to_owned(),
            },
            AuthorshipSession {
                tool: "claude".to_owned(),
                id: "session-a".to_owned(),
                model: "opus".to_owned(),
            },
        ]);
        let canonical = build_record(reordered, &ChainHead::default()).unwrap();
        assert_eq!(
            canonical
                .authorship_sessions
                .as_ref()
                .unwrap()
                .iter()
                .map(|session| session.tool.as_str())
                .collect::<Vec<_>>(),
            ["claude", "codex"]
        );

        let mut duplicate = fully_populated_body();
        duplicate.authorship_sessions = Some(vec![
            AuthorshipSession {
                tool: "codex".to_owned(),
                id: "session-42".to_owned(),
                model: "gpt-5".to_owned(),
            },
            AuthorshipSession {
                tool: "codex".to_owned(),
                id: "session-42".to_owned(),
                model: "gpt-5".to_owned(),
            },
        ]);
        assert!(build_record(duplicate, &ChainHead::default())
            .unwrap_err()
            .to_string()
            .contains("duplicate observation"));

        let baseline = build_record(fully_populated_body(), &ChainHead::default()).unwrap();
        let mut changed = fully_populated_body();
        changed.authorship_sessions.as_mut().unwrap()[0].id = "session-43".to_owned();
        let changed = build_record(changed, &ChainHead::default()).unwrap();
        assert_ne!(baseline.hash, changed.hash);

        let mut invalid = baseline.clone();
        invalid.authorship_sessions.as_mut().unwrap()[0].id = "bad\nsession".to_owned();
        assert!(validation_failure(invalid)
            .reason
            .contains("control characters"));

        let mut invalid = baseline;
        invalid.authorship.as_mut().unwrap().status = AuthorshipStatus::Error;
        assert!(validation_failure(invalid)
            .reason
            .contains("bound or mismatch"));
    }

    fn validation_failure(record: WitnessRecord) -> ValidationFailure {
        validate_record(&serde_json::to_value(record).unwrap()).unwrap_err()
    }

    #[test]
    fn final_schema_rejects_invalid_envelope_and_field_shapes() {
        let valid = build_record(body(), &ChainHead::default()).unwrap();

        let mut wrong_version = serde_json::to_value(&valid).unwrap();
        wrong_version["schemaVersion"] = Value::from(1);
        assert_eq!(
            validate_record(&wrong_version).unwrap_err().kind,
            VerifyProblemKind::SchemaVersionInvalid
        );

        let mut wrong_type = serde_json::to_value(&valid).unwrap();
        wrong_type["recordType"] = Value::String("boundary".to_owned());
        assert_eq!(
            validate_record(&wrong_type).unwrap_err().kind,
            VerifyProblemKind::RecordTypeInvalid
        );

        let mut forbidden = serde_json::to_value(&valid).unwrap();
        forbidden["parentUuid"] = Value::String(Uuid::nil().to_string());
        assert!(validate_record(&forbidden)
            .unwrap_err()
            .reason
            .contains("not a canonical witness field"));

        let mut top_level_null = serde_json::to_value(&valid).unwrap();
        top_level_null["futureProof"] = Value::Null;
        assert!(validate_record(&top_level_null)
            .unwrap_err()
            .reason
            .contains("must be omitted instead of null"));

        let mut integer_float = serde_json::to_value(&valid).unwrap();
        integer_float["wallClock"] = Value::from(44);
        assert!(validate_record(&integer_float)
            .unwrap_err()
            .reason
            .contains("canonical serialized form"));

        let mut reordered_charge = serde_json::to_value(&valid).unwrap();
        reordered_charge["charge"] =
            serde_json::from_str(r#"{"amount":42.5,"unit":"gpu-seconds","class":"verifiable"}"#)
                .unwrap();
        assert!(validate_record(&reordered_charge)
            .unwrap_err()
            .reason
            .contains("field charge"));

        let mut invalid = valid.clone();
        invalid.transition_timestamp = "2026-07-26T12:00:00Z".to_owned();
        assert!(validation_failure(invalid)
            .reason
            .contains("canonical RFC3339 UTC millis"));

        let mut invalid = valid.clone();
        invalid.attempt = 0;
        assert!(validation_failure(invalid).reason.contains("attempt"));

        let mut invalid = valid.clone();
        invalid.pools.clear();
        assert!(validation_failure(invalid).reason.contains("pool set"));

        let mut invalid = valid.clone();
        invalid.store_paths = Some(vec!["/tmp/not-store".to_owned()]);
        assert!(validation_failure(invalid)
            .reason
            .contains("Nix store path"));

        let mut invalid = valid.clone();
        invalid.host_id = Some("bad\nhost".to_owned());
        assert!(validation_failure(invalid)
            .reason
            .contains("control characters"));

        let mut invalid = valid.clone();
        invalid.result_revision = Some("ABC".to_owned());
        assert!(validation_failure(invalid)
            .reason
            .contains("resultRevision"));

        let mut invalid = valid;
        invalid.authorship = Some(Authorship {
            provider: "git-ai".to_owned(),
            provider_version: "1.2.3".to_owned(),
            note_ref: "refs/notes/ai".to_owned(),
            status: AuthorshipStatus::Bound,
            notes_ref_target: None,
            note_content_sha256: None,
            reason: None,
        });
        assert!(validation_failure(invalid)
            .reason
            .contains("requires resultRevision"));
    }

    #[test]
    fn substituted_is_a_strict_non_metered_drv_witness() {
        let record = fixture_records()[2].clone();
        assert_eq!(record.verdict, Verdict::Substituted);
        assert_eq!(record.labor_class, LaborClass::Substituted);
        assert!(!counts_toward_canonical_gpu_seconds(&record));

        let mut invalid = record.clone();
        invalid.wall_clock = 1.0;
        assert!(validation_failure(invalid)
            .reason
            .contains("cheap drv witness invariants"));

        let mut invalid = record;
        invalid.drv = None;
        assert!(validation_failure(invalid).reason.contains("requires drv"));
    }

    #[test]
    fn unknown_additive_fields_verify_and_round_trip_in_raw_order() {
        let record = build_record(body(), &ChainHead::default()).unwrap();
        let line = serde_json::to_string(&record)
            .unwrap()
            .replace(",\"seq\":", ",\"futureProof\":{\"z\":1,\"a\":2},\"seq\":");
        let mut raw: Value = serde_json::from_str(&line).unwrap();
        raw["hash"] = Value::String(compute_hash_value(&raw).unwrap());
        let line = serde_json::to_string(&raw).unwrap();
        let report = verify_reader(BufReader::new(format!("{line}\n").as_bytes()));
        assert!(report.ok, "{:?}", report.problems);

        let parsed = validate_record(&raw).unwrap();
        assert_eq!(
            parsed.extensions["futureProof"],
            serde_json::json!({"z": 1, "a": 2})
        );
        assert_eq!(serde_json::to_string(&parsed).unwrap(), line);

        let padded = format!(" {line}\n");
        let report = verify_reader(BufReader::new(padded.as_bytes()));
        assert!(report
            .problems
            .iter()
            .any(|problem| problem.reason.contains("compact canonical JSON")));
    }

    #[test]
    fn hostname_mechanism_trims_and_validates_the_process_hostname() {
        let host_id = current_host_id().unwrap();
        assert_eq!(host_id, host_id.trim());
        validate_host_id(&host_id).unwrap();
        assert!(validate_host_id("").is_err());
        assert!(validate_host_id("bad\nhost").is_err());
        assert!(validate_host_id(&"x".repeat(97)).is_err());
    }

    #[test]
    fn witness_pool_encoding_is_always_an_array_and_canonicalizes() {
        let singleton = build_record(body(), &ChainHead::default()).unwrap();
        let singleton_json = serde_json::to_string(&singleton).unwrap();
        assert!(singleton_json.contains(r#""pools":["worker-gpu"]"#));

        let mut multi_body = body();
        multi_body.pools = vec!["zeta".to_owned(), "alpha".to_owned()];
        let multi = build_record(multi_body, &ChainHead::default()).unwrap();
        assert_eq!(multi.pools, ["alpha".to_owned(), "zeta".to_owned()]);
        let multi_json = serde_json::to_string(&multi).unwrap();
        assert!(multi_json.contains(r#""pools":["alpha","zeta"]"#));
        let report = verify_reader(BufReader::new(format!("{multi_json}\n").as_bytes()));
        assert!(report.ok, "{:?}", report.problems);
    }

    #[test]
    fn append_reopens_one_unbroken_fsynced_chain() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("witness.jsonl");
        {
            let mut ledger = WitnessLedger::open(&path).unwrap();
            assert_eq!(ledger.append(body()).unwrap().seq, 1);
        }
        {
            let mut ledger = WitnessLedger::open(&path).unwrap();
            assert_eq!(ledger.append(body()).unwrap().seq, 2);
        }
        assert!(verify_file(&path).unwrap().ok);
    }

    #[test]
    fn open_ledgers_lock_each_append_without_blocking_readers_or_each_other() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("witness.jsonl");
        let mut first = WitnessLedger::open(&path).unwrap();
        let mut second = WitnessLedger::open(&path).unwrap();

        assert_eq!(first.append(body()).unwrap().seq, 1);
        let (report, records) = read_verified_records(&path).unwrap();
        assert!(report.ok);
        assert_eq!(records.len(), 1);
        assert_eq!(second.append(body()).unwrap().seq, 2);
        assert_eq!(first.append(body()).unwrap().seq, 3);

        let (report, records) = read_verified_records(&path).unwrap();
        assert!(report.ok, "{:?}", report.problems);
        assert_eq!(
            records.iter().map(|record| record.seq).collect::<Vec<_>>(),
            [1, 2, 3]
        );
    }

    #[test]
    fn attestation_chain_is_independent_and_advisory() {
        assert_eq!(
            durable_parent(Path::new("attestations.jsonl")),
            Path::new(".")
        );
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("attestations.jsonl");
        append_attestation(&path, serde_json::json!({"capture": "session-1"})).unwrap();
        append_attestation(&path, serde_json::json!({"scraped_actual": 12})).unwrap();
        let report = verify_attestations(&path).unwrap();
        assert!(report.ok);
        assert_eq!(report.records, 2);
        assert_eq!(report.authentication, "unauthenticated-by-construction");

        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"observed_at\":")
            .unwrap();
        repair_attestation_tail(&path).unwrap();
        let report = verify_attestations(&path).unwrap();
        assert!(report.ok);
        assert_eq!(report.records, 2);
        assert_eq!(
            append_attestation(&path, serde_json::json!({"afterRepair": true}))
                .unwrap()
                .seq,
            3
        );

        let length = std::fs::metadata(&path).unwrap().len();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(length - 1)
            .unwrap();
        repair_attestation_tail(&path).unwrap();
        assert!(std::fs::read(&path).unwrap().ends_with(b"\n"));
        assert_eq!(verify_attestations(&path).unwrap().records, 3);

        let malformed = temp.path().join("complete-but-invalid.jsonl");
        std::fs::write(&malformed, b"{\"payload\":{}}").unwrap();
        assert!(repair_attestation_tail(&malformed).is_err());
        assert_eq!(std::fs::read(&malformed).unwrap(), b"{\"payload\":{}}");
    }

    #[test]
    fn non_metered_verdicts_never_count() {
        for verdict in [
            Verdict::PoolVanished,
            Verdict::Preempted,
            Verdict::Cancelled,
        ] {
            let mut value = body();
            value.verdict = verdict;
            let record = build_record(value, &ChainHead::default()).unwrap();
            assert!(!counts_toward_canonical_gpu_seconds(&record));
        }
        let mut runtime_limited = body();
        runtime_limited.verdict = Verdict::RuntimeExceeded;
        let record = build_record(runtime_limited, &ChainHead::default()).unwrap();
        assert!(counts_toward_canonical_gpu_seconds(&record));
    }
}
