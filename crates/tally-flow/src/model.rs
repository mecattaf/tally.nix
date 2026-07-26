use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{FlowError, SourceLocation};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Disposition {
    Created,
    Attached,
    Reused,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Pass,
    CleanExitNoArtifact,
    Failed,
    Skipped,
    Cancelled,
    PoolVanished,
    Preempted,
    RuntimeExceeded,
}

impl Verdict {
    #[must_use]
    pub const fn is_pass(self) -> bool {
        matches!(self, Self::Pass)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeFailure {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeResult {
    pub task_uuid: String,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub witness_seq: u64,
    pub disposition: Disposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gates: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<NodeFailure>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Admission {
    pub schema_version: u32,
    pub disposition: Disposition,
    pub task_uuid: String,
    pub payload_hash: String,
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<NodeResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reused_rejected: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl ClientError {
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunInspection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionProvenance {
    pub selector: String,
    pub catalog_hash: String,
    pub member_id: String,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Orchestration {
    pub flow_name: String,
    pub flow_run_id: String,
    pub script_hash: String,
    pub node_ordinal: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_label: Option<String>,
    pub max_nodes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<SelectionProvenance>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub pools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_max_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_class: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_options: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<SelectionProvenance>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowSubmission {
    pub mode: String,
    pub dedup_key: String,
    pub payload_hash: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub credentials: BTreeMap<String, PathBuf>,
    pub spec: NodeSpec,
    pub orchestration: Orchestration,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    pub flow_run_id: String,
    pub flow_name: String,
    pub script_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_hash: Option<String>,
    pub ordinal_keys: Vec<String>,
    pub observation_order: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_value: Option<Value>,
}

#[derive(Debug, Clone)]
pub(crate) struct SubmissionPlan {
    pub submission: FlowSubmission,
    pub settle: bool,
    pub result_schema: Option<Value>,
    pub location: SourceLocation,
    pub ordinal: u64,
}

impl ClientError {
    pub(crate) fn into_flow(self, location: SourceLocation, ordinal: u64) -> FlowError {
        let name = match self.code.as_str() {
            "dedup-key-conflict" => "FlowDedupKeyConflict",
            "admission-denied" => "FlowAdmissionDenied",
            "flow-node-cap" => "FlowNodeCapError",
            "replay-divergence" | "script-history-conflict" | "script-changed-mid-run" => {
                "FlowReplayError"
            }
            _ => "FlowClientError",
        };
        let mut error = FlowError::new(name, self.code, self.message)
            .at(location)
            .with_ordinal(ordinal);
        if let Some(details) = self.details {
            if matches!(
                error.code.as_str(),
                "replay-divergence" | "script-changed-mid-run"
            ) {
                if let Value::Object(details) = details {
                    error.details.extend(details);
                } else {
                    error = error.detail("client", details);
                }
            } else {
                error = error.detail("client", details);
            }
        }
        error
    }
}
