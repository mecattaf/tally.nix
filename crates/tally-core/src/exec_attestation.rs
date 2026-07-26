//! Advisory execution attestations and comparison against the canonical witness chain.
//!
//! Each executing host owns an independent hash-chained ledger.  The chain is
//! deliberately unauthenticated: its purpose is to make independently observed
//! execution facts comparable with coordinator canon, not to introduce a second
//! canonical writer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::Instant;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::evidence::{parse_evidence_specs, run_evidence_gate, EvidenceError, RunOutcome};
use crate::witness::{
    append_attestation, current_host_id, read_verified_attestations, read_verified_records,
    AttestationRecord, AttestationVerifyReport, LaborClass, WitnessError, WitnessRecord,
};

pub const EXEC_ATTESTATION_SCHEMA_VERSION: u32 = 2;
pub const EXEC_ATTESTATION_LEDGER: &str = "exec-attestations.jsonl";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecAttestationContext {
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_hash: Option<String>,
    pub evidence: Vec<String>,
}

impl ExecAttestationContext {
    pub fn validate(&self) -> Result<(), String> {
        if !registry_component_shape(&self.adapter) {
            return Err("adapter is not a safe registry component".to_owned());
        }
        if self
            .executor
            .as_deref()
            .is_some_and(|executor| !registry_component_shape(executor))
        {
            return Err("executor is not a safe registry component".to_owned());
        }
        for (name, value) in [
            ("payloadHash", self.payload_hash.as_deref()),
            ("briefHash", self.brief_hash.as_deref()),
        ] {
            if value.is_some_and(|value| !sha256_shape(value)) {
                return Err(format!("{name} is not lowercase sha256 hex"));
            }
        }
        parse_evidence_specs(&self.evidence).map_err(|error| error.to_string())?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecAttestationPayload {
    pub schema_version: u32,
    pub kind: String,
    pub execution_id: String,
    pub task_uuid: String,
    pub attempt: u32,
    pub lease_epoch: u64,
    pub host_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    pub argv_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_hash: Option<String>,
    pub started_at: String,
    pub finished_at: String,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_paths: Option<Vec<String>>,
}

impl ExecAttestationPayload {
    fn validate(&self) -> Result<(), String> {
        if self.schema_version != EXEC_ATTESTATION_SCHEMA_VERSION {
            return Err(format!(
                "schemaVersion must be the integer {EXEC_ATTESTATION_SCHEMA_VERSION}"
            ));
        }
        if self.kind != "exec" {
            return Err("kind must be exec".to_owned());
        }
        if !sha256_shape(&self.execution_id) {
            return Err("executionId is not lowercase sha256 hex".to_owned());
        }
        Uuid::parse_str(&self.task_uuid).map_err(|_| "taskUuid is not a UUID".to_owned())?;
        if self.attempt == 0 {
            return Err("attempt must be positive".to_owned());
        }
        if self.lease_epoch == 0 {
            return Err("leaseEpoch must be positive".to_owned());
        }
        if self.execution_id != execution_id(&self.task_uuid, self.attempt, self.lease_epoch) {
            return Err("executionId does not match taskUuid, attempt, and leaseEpoch".to_owned());
        }
        crate::witness::validate_host_id(&self.host_id)
            .map_err(|error| format!("hostId is invalid: {error}"))?;
        for (name, value) in [
            ("adapter", self.adapter.as_deref()),
            ("executor", self.executor.as_deref()),
        ] {
            if value.is_some_and(|value| !registry_component_shape(value)) {
                return Err(format!("{name} is not a safe registry component"));
            }
        }
        for (name, value) in [
            ("argvHash", Some(self.argv_hash.as_str())),
            ("payloadHash", self.payload_hash.as_deref()),
            ("briefHash", self.brief_hash.as_deref()),
            ("outputHash", self.output_hash.as_deref()),
        ] {
            if value.is_some_and(|value| !sha256_shape(value)) {
                return Err(format!("{name} is not lowercase sha256 hex"));
            }
        }
        let started = canonical_timestamp(&self.started_at, "startedAt")?;
        let finished = canonical_timestamp(&self.finished_at, "finishedAt")?;
        if finished < started {
            return Err("finishedAt precedes startedAt".to_owned());
        }
        if !(0..=255).contains(&self.exit_code) {
            return Err("exitCode must be in 0..=255".to_owned());
        }
        if let Some(paths) = &self.store_paths {
            if paths.is_empty() {
                return Err("storePaths must be non-empty when present".to_owned());
            }
            if paths
                .iter()
                .any(|path| !crate::witness::is_nix_store_path(path))
            {
                return Err("storePaths contains an invalid Nix store path".to_owned());
            }
            if paths.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err("storePaths must be byte-ascending sorted and unique".to_owned());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedExecAttestation {
    pub record: AttestationRecord,
    pub payload: ExecAttestationPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecRunRequest {
    pub ledger: PathBuf,
    pub task_uuid: String,
    pub attempt: u32,
    pub lease_epoch: u64,
    pub payload_hash: Option<String>,
    pub brief_hash: Option<String>,
    pub adapter: Option<String>,
    pub executor: Option<String>,
    pub evidence: Vec<String>,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecRunOutcome {
    pub exit_code: i32,
    pub attestation_appended: bool,
}

#[derive(Debug, Error)]
pub enum ExecRunError {
    #[error("invalid execution attestation request: {0}")]
    Invalid(String),
    #[error("invalid execution evidence: {0}")]
    Evidence(#[from] EvidenceError),
    #[error("cannot spawn child {program}: {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
}

/// Run a child with inherited stdio, environment, and cwd, then append exactly
/// one advisory terminal attestation.  A ledger/hostname failure is logged but
/// never changes the child's propagated exit code.
pub fn run_exec(request: ExecRunRequest) -> Result<ExecRunOutcome, ExecRunError> {
    validate_run_request(&request)?;
    let evidence = parse_evidence_specs(&request.evidence)?;
    let host_id = current_host_id();
    let started_at = now_timestamp();
    let started = Instant::now();
    let status = Command::new(&request.argv[0])
        .args(&request.argv[1..])
        .status()
        .map_err(|source| ExecRunError::Spawn {
            program: request.argv[0].clone(),
            source,
        })?;
    let elapsed = started.elapsed();
    let finished_at = now_timestamp();
    let exit_code = mapped_exit_code(status);
    let gate = run_evidence_gate(RunOutcome {
        exit_code,
        wall_clock_seconds: elapsed.as_secs_f64(),
        evidence: &evidence,
    });
    let task_uuid = request.task_uuid.clone();
    let payload = host_id.map(|host_id| ExecAttestationPayload {
        schema_version: EXEC_ATTESTATION_SCHEMA_VERSION,
        kind: "exec".to_owned(),
        execution_id: execution_id(&request.task_uuid, request.attempt, request.lease_epoch),
        task_uuid: task_uuid.clone(),
        attempt: request.attempt,
        lease_epoch: request.lease_epoch,
        host_id,
        adapter: request.adapter,
        executor: request.executor,
        argv_hash: argv_hash(&request.argv),
        payload_hash: request.payload_hash,
        brief_hash: request.brief_hash,
        started_at,
        finished_at,
        exit_code,
        output_hash: gate.artifact_hash,
        store_paths: gate.store_paths,
    });

    let appended = match payload {
        Ok(payload) => match serde_json::to_value(payload)
            .map_err(WitnessError::from)
            .and_then(|payload| append_attestation(&request.ledger, payload))
        {
            Ok(_) => true,
            Err(error) => {
                eprintln!(
                    "tally: execution attestation append failed for {}: {error}",
                    task_uuid
                );
                false
            }
        },
        Err(error) => {
            eprintln!(
                "tally: execution attestation append failed for {}: {error}",
                task_uuid
            );
            false
        }
    };
    Ok(ExecRunOutcome {
        exit_code,
        attestation_appended: appended,
    })
}

fn validate_run_request(request: &ExecRunRequest) -> Result<(), ExecRunError> {
    Uuid::parse_str(&request.task_uuid)
        .map_err(|_| ExecRunError::Invalid("taskUuid is not a UUID".to_owned()))?;
    if request.attempt == 0 {
        return Err(ExecRunError::Invalid("attempt must be positive".to_owned()));
    }
    if request.lease_epoch == 0 {
        return Err(ExecRunError::Invalid(
            "leaseEpoch must be positive".to_owned(),
        ));
    }
    if request.ledger.as_os_str().is_empty() {
        return Err(ExecRunError::Invalid(
            "ledger path must not be empty".to_owned(),
        ));
    }
    if request.argv.is_empty() || request.argv[0].is_empty() {
        return Err(ExecRunError::Invalid(
            "argv must contain a non-empty executable".to_owned(),
        ));
    }
    if request.argv.iter().any(|argument| argument.contains('\0')) {
        return Err(ExecRunError::Invalid(
            "argv must not contain NUL bytes".to_owned(),
        ));
    }
    for (name, value) in [
        ("adapter", request.adapter.as_deref()),
        ("executor", request.executor.as_deref()),
    ] {
        if value.is_some_and(|value| !registry_component_shape(value)) {
            return Err(ExecRunError::Invalid(format!(
                "{name} is not a safe registry component"
            )));
        }
    }
    for (name, value) in [
        ("payloadHash", request.payload_hash.as_deref()),
        ("briefHash", request.brief_hash.as_deref()),
    ] {
        if value.is_some_and(|value| !sha256_shape(value)) {
            return Err(ExecRunError::Invalid(format!(
                "{name} is not lowercase sha256 hex"
            )));
        }
    }
    Ok(())
}

pub fn execution_id(task_uuid: &str, attempt: u32, lease_epoch: u64) -> String {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Identity<'a> {
        task_uuid: &'a str,
        attempt: u32,
        lease_epoch: u64,
    }
    let bytes = serde_json::to_vec(&Identity {
        task_uuid,
        attempt,
        lease_epoch,
    })
    .expect("execution identity contains only serializable scalar values");
    sha256(&bytes)
}

pub fn argv_hash(argv: &[String]) -> String {
    let bytes = serde_json::to_vec(argv).expect("argv strings are always serializable");
    sha256(&bytes)
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn mapped_exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(0)
    }
    #[cfg(not(unix))]
    {
        1
    }
}

pub fn read_verified_exec_attestations(
    path: &Path,
) -> Result<(AttestationVerifyReport, Vec<VerifiedExecAttestation>), WitnessError> {
    let (mut report, records) = read_verified_attestations(path)?;
    if !report.ok {
        return Ok((report, Vec::new()));
    }
    let mut verified = Vec::with_capacity(records.len());
    for record in records {
        let payload: ExecAttestationPayload = match serde_json::from_value(record.payload.clone()) {
            Ok(payload) => payload,
            Err(error) => {
                report.ok = false;
                report.problem = Some(format!(
                    "execution attestation seq {} has an invalid payload: {error}",
                    record.seq
                ));
                return Ok((report, Vec::new()));
            }
        };
        if let Err(error) = payload.validate() {
            report.ok = false;
            report.problem = Some(format!(
                "execution attestation seq {} has an invalid payload: {error}",
                record.seq
            ));
            return Ok((report, Vec::new()));
        }
        verified.push(VerifiedExecAttestation { record, payload });
    }
    Ok((report, verified))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agreement {
    Unanimous,
    Diverged,
    Unattested,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonComparison {
    pub host_id: Option<String>,
    pub exit_code: i32,
    pub output_hash: Option<String>,
    pub store_paths: Option<Vec<String>>,
    pub payload_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationComparison {
    pub ledger: PathBuf,
    pub host_id: String,
    pub exit_code: i32,
    pub output_hash: Option<String>,
    pub store_paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionComparison {
    pub execution_id: String,
    pub task_uuid: String,
    pub attempt: u32,
    pub witness_ref: String,
    pub canon: CanonComparison,
    pub attestations: Vec<AttestationComparison>,
    pub agreement: Agreement,
    pub diffs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareSummary {
    pub compared: usize,
    pub unanimous: usize,
    pub diverged: usize,
    pub unattested: usize,
    pub orphans: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareReport {
    pub schema_version: u32,
    pub ok: bool,
    pub executions: Vec<ExecutionComparison>,
    pub summary: CompareSummary,
}

#[derive(Debug, Error)]
pub enum CompareError {
    #[error(transparent)]
    Witness(#[from] WitnessError),
    #[error("canonical witness chain is invalid: {0}")]
    CanonInvalid(String),
    #[error("execution attestation ledger {path} is invalid: {problem}")]
    AttestationInvalid { path: PathBuf, problem: String },
}

#[derive(Debug, Clone)]
struct LocatedAttestation {
    ledger: PathBuf,
    payload: ExecAttestationPayload,
}

pub fn compare(
    canon_path: &Path,
    attestation_paths: &[PathBuf],
    strict: bool,
) -> Result<CompareReport, CompareError> {
    let (canon_report, canon_records) = read_verified_records(canon_path)?;
    if !canon_report.ok {
        let problem = canon_report
            .problems
            .iter()
            .map(|problem| format!("line {}: {}", problem.line, problem.reason))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(CompareError::CanonInvalid(problem));
    }

    let mut by_execution = BTreeMap::<String, Vec<LocatedAttestation>>::new();
    for path in attestation_paths {
        let (report, records) = read_verified_exec_attestations(path)?;
        if !report.ok {
            return Err(CompareError::AttestationInvalid {
                path: path.clone(),
                problem: report
                    .problem
                    .unwrap_or_else(|| "chain verification failed".to_owned()),
            });
        }
        for record in records {
            by_execution
                .entry(record.payload.execution_id.clone())
                .or_default()
                .push(LocatedAttestation {
                    ledger: path.clone(),
                    payload: record.payload,
                });
        }
    }

    let canon = canon_records
        .iter()
        .filter(|record| {
            matches!(
                record.labor_class,
                LaborClass::Fresh | LaborClass::Recovered
            )
        })
        .filter_map(|record| {
            record
                .task_uuid
                .as_deref()
                .map(|task_uuid| (record, task_uuid))
        })
        .collect::<Vec<_>>();
    let canon_ids = canon
        .iter()
        .map(|(record, task_uuid)| execution_id(task_uuid, record.attempt, record.lease_epoch))
        .collect::<BTreeSet<_>>();
    let orphans = by_execution
        .iter()
        .filter(|(execution_id, _)| !canon_ids.contains(*execution_id))
        .map(|(_, records)| records.len())
        .sum();

    let mut executions = Vec::with_capacity(canon.len());
    let mut unanimous = 0;
    let mut diverged = 0;
    let mut unattested = 0;
    for (record, task_uuid) in canon {
        let identity = execution_id(task_uuid, record.attempt, record.lease_epoch);
        let located = by_execution.remove(&identity).unwrap_or_default();
        let mut diffs = Vec::new();
        let mut attestations = Vec::with_capacity(located.len());
        for attestation in located {
            compare_field(
                &mut diffs,
                &attestation,
                "taskUuid",
                &Value::String(task_uuid.to_owned()),
                &Value::String(attestation.payload.task_uuid.clone()),
            );
            compare_field(
                &mut diffs,
                &attestation,
                "attempt",
                &Value::from(record.attempt),
                &Value::from(attestation.payload.attempt),
            );
            compare_field(
                &mut diffs,
                &attestation,
                "leaseEpoch",
                &Value::from(record.lease_epoch),
                &Value::from(attestation.payload.lease_epoch),
            );
            compare_field(
                &mut diffs,
                &attestation,
                "exitCode",
                &Value::from(record.exit_code),
                &Value::from(attestation.payload.exit_code),
            );
            compare_field(
                &mut diffs,
                &attestation,
                "outputHash",
                &option_value(record.artifact_content_hash.as_deref()),
                &option_value(attestation.payload.output_hash.as_deref()),
            );
            compare_field(
                &mut diffs,
                &attestation,
                "storePaths",
                &store_paths_value(record.store_paths.as_deref()),
                &store_paths_value(attestation.payload.store_paths.as_deref()),
            );
            compare_field(
                &mut diffs,
                &attestation,
                "payloadHash",
                &option_value(record.payload_hash.as_deref()),
                &option_value(attestation.payload.payload_hash.as_deref()),
            );
            attestations.push(AttestationComparison {
                ledger: attestation.ledger,
                host_id: attestation.payload.host_id,
                exit_code: attestation.payload.exit_code,
                output_hash: attestation.payload.output_hash,
                store_paths: attestation.payload.store_paths,
            });
        }
        let agreement = if attestations.is_empty() {
            unattested += 1;
            Agreement::Unattested
        } else if diffs.is_empty() {
            unanimous += 1;
            Agreement::Unanimous
        } else {
            diverged += 1;
            Agreement::Diverged
        };
        executions.push(ExecutionComparison {
            execution_id: identity,
            task_uuid: task_uuid.to_owned(),
            attempt: record.attempt,
            witness_ref: format!("witness:{}", record.seq),
            canon: canonical_projection(record),
            attestations,
            agreement,
            diffs,
        });
    }
    let summary = CompareSummary {
        compared: executions.len(),
        unanimous,
        diverged,
        unattested,
        orphans,
    };
    let ok = diverged == 0 && (!strict || (unattested == 0 && orphans == 0));
    Ok(CompareReport {
        schema_version: EXEC_ATTESTATION_SCHEMA_VERSION,
        ok,
        executions,
        summary,
    })
}

fn canonical_projection(record: &WitnessRecord) -> CanonComparison {
    CanonComparison {
        host_id: record.host_id.clone(),
        exit_code: record.exit_code,
        output_hash: record.artifact_content_hash.clone(),
        store_paths: record.store_paths.clone(),
        payload_hash: record.payload_hash.clone(),
    }
}

fn compare_field(
    diffs: &mut Vec<String>,
    attestation: &LocatedAttestation,
    field: &str,
    canon: &Value,
    observed: &Value,
) {
    if canon != observed {
        diffs.push(format!(
            "{} host {} {field}: canon={} attestation={}",
            attestation.ledger.display(),
            attestation.payload.host_id,
            compact_value(canon),
            compact_value(observed)
        ));
    }
}

fn compact_value(value: &Value) -> String {
    serde_json::to_string(value).expect("JSON values always serialize")
}

fn option_value(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |value| Value::String(value.to_owned()))
}

fn store_paths_value(paths: Option<&[String]>) -> Value {
    let paths = paths.unwrap_or_default();
    Value::Array(paths.iter().cloned().map(Value::String).collect())
}

fn now_timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn canonical_timestamp(value: &str, field: &str) -> Result<DateTime<Utc>, String> {
    let parsed = DateTime::parse_from_rfc3339(value)
        .map_err(|_| format!("{field} is not RFC3339 UTC millis"))?
        .with_timezone(&Utc);
    if parsed.to_rfc3339_opts(SecondsFormat::Millis, true) != value {
        return Err(format!("{field} is not canonical RFC3339 UTC millis"));
    }
    Ok(parsed)
}

fn sha256_shape(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn registry_component_shape(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@' | b'+' | b':')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_identity_and_argv_hash_pin_canonical_bytes() {
        assert_eq!(
            execution_id("00000000-0000-4000-8000-000000000084", 2, 7),
            sha256(br#"{"taskUuid":"00000000-0000-4000-8000-000000000084","attempt":2,"leaseEpoch":7}"#)
        );
        assert_eq!(
            argv_hash(&["printf".to_owned(), "two words".to_owned()]),
            sha256(br#"["printf","two words"]"#)
        );
    }

    #[test]
    fn store_path_comparison_is_set_shaped_for_absence() {
        assert_eq!(store_paths_value(None), Value::Array(Vec::new()));
        assert_eq!(store_paths_value(Some(&[])), Value::Array(Vec::new()));
    }
}
