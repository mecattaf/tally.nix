use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const GATE_MANIFEST_SCHEMA_VERSION: u32 = 1;
const MAX_GATE_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_GATE_ID_BYTES: usize = 96;
const MAX_GATE_TEXT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AcceptancePolicy {
    #[default]
    Manual,
    ExecutionAndGates,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GateManifestSpec {
    pub path: PathBuf,
    pub required_gate_ids: Vec<String>,
    #[serde(default)]
    pub acceptance_policy: AcceptancePolicy,
}

impl GateManifestSpec {
    pub fn validate(&self) -> Result<(), CompletionError> {
        if !self.path.is_absolute() {
            return Err(CompletionError::InvalidSpec(
                "gate manifest path must be absolute".to_owned(),
            ));
        }
        let path = self.path.to_str().ok_or_else(|| {
            CompletionError::InvalidSpec("gate manifest path must be valid UTF-8".to_owned())
        })?;
        if path.contains('\0') || path.chars().any(char::is_control) {
            return Err(CompletionError::InvalidSpec(
                "gate manifest path must contain no control characters".to_owned(),
            ));
        }
        let mut unique = BTreeSet::new();
        for gate_id in &self.required_gate_ids {
            validate_gate_id(gate_id).map_err(CompletionError::InvalidSpec)?;
            if !unique.insert(gate_id) {
                return Err(CompletionError::InvalidSpec(format!(
                    "requiredGateIds repeats {gate_id:?}"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionStatus {
    Success,
    Failure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutionFact {
    pub status: ExecutionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub reason: String,
}

impl ExecutionFact {
    pub fn exited(exit_code: i32) -> Self {
        Self {
            status: if exit_code == 0 {
                ExecutionStatus::Success
            } else {
                ExecutionStatus::Failure
            },
            exit_code: Some(exit_code),
            reason: format!("process exited with code {exit_code}"),
        }
    }

    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            status: ExecutionStatus::Failure,
            exit_code: None,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeclaredGateStatus {
    Pass,
    Fail,
    NotRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DeclaredGate {
    pub id: String,
    pub status: DeclaredGateStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateSummaryStatus {
    Pass,
    Fail,
    NotRun,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GateSummary {
    pub status: GateSummaryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<Value>,
    pub gates: Vec<DeclaredGate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_required_gate_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AcceptanceStatus {
    Pending,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AcceptanceFact {
    pub status: AcceptanceStatus,
    pub policy: AcceptancePolicy,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SemanticCompletion {
    pub schema_version: u32,
    pub execution: ExecutionFact,
    pub gates: GateSummary,
    pub acceptance: AcceptanceFact,
}

#[derive(Debug, Error)]
pub enum CompletionError {
    #[error("invalid gate manifest specification: {0}")]
    InvalidSpec(String),
    #[error("cannot read gate manifest {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("gate manifest {0} is not a regular file")]
    NotRegular(String),
    #[error("gate manifest {path} exceeds the {limit}-byte limit")]
    TooLarge { path: String, limit: u64 },
    #[error("gate manifest {0} changed while it was read")]
    Changed(String),
    #[error("gate manifest {0} is not valid UTF-8 JSON")]
    InvalidJson(String),
    #[error("gate manifest is invalid: {0}")]
    InvalidManifest(String),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct GateManifest {
    schema_version: u32,
    artifact: Value,
    gates: Vec<DeclaredGate>,
}

pub fn evaluate_completion(
    execution: ExecutionFact,
    spec: &GateManifestSpec,
) -> SemanticCompletion {
    let gates = match read_gate_manifest(spec) {
        Ok(summary) => summary,
        Err(ref error @ CompletionError::Read { ref source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            GateSummary {
                status: GateSummaryStatus::NotRun,
                artifact: None,
                gates: Vec::new(),
                missing_required_gate_ids: spec.required_gate_ids.clone(),
                manifest_error: Some(error.to_string()),
            }
        }
        Err(error) => GateSummary {
            status: GateSummaryStatus::Fail,
            artifact: None,
            gates: Vec::new(),
            missing_required_gate_ids: spec.required_gate_ids.clone(),
            manifest_error: Some(error.to_string()),
        },
    };
    let acceptance = acceptance(&execution, &gates, spec.acceptance_policy);
    SemanticCompletion {
        schema_version: GATE_MANIFEST_SCHEMA_VERSION,
        execution,
        gates,
        acceptance,
    }
}

fn read_gate_manifest(spec: &GateManifestSpec) -> Result<GateSummary, CompletionError> {
    spec.validate()?;
    let bytes = read_bounded_regular(&spec.path)?;
    let manifest: GateManifest = serde_json::from_slice(&bytes)
        .map_err(|_| CompletionError::InvalidJson(spec.path.display().to_string()))?;
    if manifest.schema_version != GATE_MANIFEST_SCHEMA_VERSION {
        return Err(CompletionError::InvalidManifest(format!(
            "schemaVersion {} is unsupported; expected {GATE_MANIFEST_SCHEMA_VERSION}",
            manifest.schema_version
        )));
    }
    let mut declared = BTreeSet::new();
    for gate in &manifest.gates {
        validate_gate(gate)?;
        if !declared.insert(gate.id.as_str()) {
            return Err(CompletionError::InvalidManifest(format!(
                "gate ID {:?} is repeated",
                gate.id
            )));
        }
    }
    let missing_required_gate_ids = spec
        .required_gate_ids
        .iter()
        .filter(|required| !declared.contains(required.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let status = if !missing_required_gate_ids.is_empty()
        || manifest
            .gates
            .iter()
            .any(|gate| gate.status == DeclaredGateStatus::Fail)
    {
        GateSummaryStatus::Fail
    } else if manifest
        .gates
        .iter()
        .any(|gate| gate.status == DeclaredGateStatus::NotRun)
    {
        GateSummaryStatus::NotRun
    } else {
        GateSummaryStatus::Pass
    };
    Ok(GateSummary {
        status,
        artifact: Some(manifest.artifact),
        gates: manifest.gates,
        missing_required_gate_ids,
        manifest_error: None,
    })
}

fn acceptance(
    execution: &ExecutionFact,
    gates: &GateSummary,
    policy: AcceptancePolicy,
) -> AcceptanceFact {
    let (status, reason) = match policy {
        AcceptancePolicy::Manual => (
            AcceptanceStatus::Pending,
            "manual acceptance has not been recorded".to_owned(),
        ),
        AcceptancePolicy::ExecutionAndGates => {
            if execution.status == ExecutionStatus::Failure
                || gates.status == GateSummaryStatus::Fail
            {
                (
                    AcceptanceStatus::Rejected,
                    "execution or a declared gate failed".to_owned(),
                )
            } else if gates.status == GateSummaryStatus::NotRun {
                (
                    AcceptanceStatus::Pending,
                    "at least one declared gate was explicitly not run".to_owned(),
                )
            } else {
                (
                    AcceptanceStatus::Accepted,
                    "execution succeeded and every declared gate passed".to_owned(),
                )
            }
        }
    };
    AcceptanceFact {
        status,
        policy,
        reason,
    }
}

fn validate_gate(gate: &DeclaredGate) -> Result<(), CompletionError> {
    validate_gate_id(&gate.id).map_err(CompletionError::InvalidManifest)?;
    for (label, value) in [("command", &gate.command), ("reason", &gate.reason)] {
        if value.as_ref().is_some_and(|value| {
            value.is_empty()
                || value.len() > MAX_GATE_TEXT_BYTES
                || value.contains('\0')
                || value.chars().any(|character| character == '\r')
        }) {
            return Err(CompletionError::InvalidManifest(format!(
                "gate {:?} {label} must be non-empty, bounded, and contain no carriage return",
                gate.id
            )));
        }
    }
    if gate.status == DeclaredGateStatus::NotRun
        && gate
            .reason
            .as_ref()
            .is_none_or(|reason| reason.trim().is_empty())
    {
        return Err(CompletionError::InvalidManifest(format!(
            "not-run gate {:?} requires a reason",
            gate.id
        )));
    }
    Ok(())
}

fn validate_gate_id(gate_id: &str) -> Result<(), String> {
    if gate_id.is_empty()
        || gate_id.len() > MAX_GATE_ID_BYTES
        || !gate_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        || !gate_id
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        return Err(format!(
            "gate ID {gate_id:?} must start with an alphanumeric character and contain only ASCII alphanumerics, '_', '.', or '-'"
        ));
    }
    Ok(())
}

fn read_bounded_regular(path: &Path) -> Result<Vec<u8>, CompletionError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|source| CompletionError::Read {
            path: path.display().to_string(),
            source,
        })?;
    let before = file.metadata().map_err(|source| CompletionError::Read {
        path: path.display().to_string(),
        source,
    })?;
    if !before.file_type().is_file() {
        return Err(CompletionError::NotRegular(path.display().to_string()));
    }
    if before.len() > MAX_GATE_MANIFEST_BYTES {
        return Err(CompletionError::TooLarge {
            path: path.display().to_string(),
            limit: MAX_GATE_MANIFEST_BYTES,
        });
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_GATE_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| CompletionError::Read {
            path: path.display().to_string(),
            source,
        })?;
    if bytes.len() as u64 > MAX_GATE_MANIFEST_BYTES {
        return Err(CompletionError::TooLarge {
            path: path.display().to_string(),
            limit: MAX_GATE_MANIFEST_BYTES,
        });
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|source| CompletionError::Read {
            path: path.display().to_string(),
            source,
        })?;
    let after = file.metadata().map_err(|source| CompletionError::Read {
        path: path.display().to_string(),
        source,
    })?;
    if before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || before.ino() != after.ino()
    {
        return Err(CompletionError::Changed(path.display().to_string()));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    fn spec(path: PathBuf, policy: AcceptancePolicy) -> GateManifestSpec {
        GateManifestSpec {
            path,
            required_gate_ids: vec!["static".to_owned(), "live".to_owned()],
            acceptance_policy: policy,
        }
    }

    #[test]
    fn zero_exit_failed_or_missing_gate_remains_separate_and_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gates.json");
        fs::write(
            &path,
            r#"{"schemaVersion":1,"artifact":{"commit":"abc"},"gates":[{"id":"static","status":"fail","command":"cargo test","reason":"one test failed"}]}"#,
        )
        .unwrap();
        let completion = evaluate_completion(
            ExecutionFact::exited(0),
            &spec(path, AcceptancePolicy::ExecutionAndGates),
        );
        assert_eq!(completion.execution.status, ExecutionStatus::Success);
        assert_eq!(completion.gates.status, GateSummaryStatus::Fail);
        assert_eq!(completion.gates.missing_required_gate_ids, ["live"]);
        assert_eq!(completion.acceptance.status, AcceptanceStatus::Rejected);
    }

    #[test]
    fn explicit_not_run_with_reason_is_pending_not_passed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gates.json");
        fs::write(
            &path,
            r#"{"schemaVersion":1,"artifact":{"commit":"abc"},"gates":[{"id":"static","status":"pass","command":"cargo test"},{"id":"live","status":"not-run","reason":"requires activation"}]}"#,
        )
        .unwrap();
        let completion = evaluate_completion(
            ExecutionFact::exited(0),
            &spec(path, AcceptancePolicy::ExecutionAndGates),
        );
        assert_eq!(completion.gates.status, GateSummaryStatus::NotRun);
        assert_eq!(completion.acceptance.status, AcceptanceStatus::Pending);
    }

    #[test]
    fn all_declared_gates_can_be_accepted_by_the_explicit_policy() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gates.json");
        fs::write(
            &path,
            r#"{"schemaVersion":1,"artifact":{"commit":"abc"},"gates":[{"id":"static","status":"pass"},{"id":"live","status":"pass"}]}"#,
        )
        .unwrap();
        let completion = evaluate_completion(
            ExecutionFact::exited(0),
            &spec(path, AcceptancePolicy::ExecutionAndGates),
        );
        assert_eq!(completion.gates.status, GateSummaryStatus::Pass);
        assert_eq!(completion.acceptance.status, AcceptanceStatus::Accepted);
    }

    #[test]
    fn absent_manifest_is_visible_not_run_without_failing_execution() {
        let temp = tempfile::tempdir().unwrap();
        let completion = evaluate_completion(
            ExecutionFact::exited(0),
            &GateManifestSpec {
                path: temp.path().join("absent-default.json"),
                required_gate_ids: Vec::new(),
                acceptance_policy: AcceptancePolicy::Manual,
            },
        );
        assert_eq!(completion.execution.status, ExecutionStatus::Success);
        assert_eq!(completion.gates.status, GateSummaryStatus::NotRun);
        assert!(completion.gates.gates.is_empty());
        assert!(completion.gates.manifest_error.is_some());
        assert_eq!(completion.acceptance.status, AcceptanceStatus::Pending);

        let required = evaluate_completion(
            ExecutionFact::exited(0),
            &spec(
                temp.path().join("absent-required.json"),
                AcceptancePolicy::ExecutionAndGates,
            ),
        );
        assert_eq!(required.gates.status, GateSummaryStatus::NotRun);
        assert_eq!(required.gates.missing_required_gate_ids, ["static", "live"]);
        assert_eq!(required.acceptance.status, AcceptanceStatus::Pending);
    }

    #[test]
    fn malformed_not_run_and_symlinked_manifest_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.json");
        fs::write(
            &target,
            r#"{"schemaVersion":1,"artifact":null,"gates":[{"id":"static","status":"pass"},{"id":"live","status":"not-run"}]}"#,
        )
        .unwrap();
        let malformed = evaluate_completion(
            ExecutionFact::exited(0),
            &spec(target.clone(), AcceptancePolicy::Manual),
        );
        assert_eq!(malformed.gates.status, GateSummaryStatus::Fail);
        assert!(malformed.gates.manifest_error.is_some());

        let link = temp.path().join("link.json");
        symlink(target, &link).unwrap();
        let linked = evaluate_completion(
            ExecutionFact::exited(0),
            &spec(link, AcceptancePolicy::Manual),
        );
        assert_eq!(linked.gates.status, GateSummaryStatus::Fail);
        assert!(linked.gates.manifest_error.is_some());
    }
}
