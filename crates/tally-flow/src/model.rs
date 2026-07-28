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
    Substituted,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Pass,
    Substituted,
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
        matches!(self, Self::Pass | Self::Substituted)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DerivationOutput {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Derivation {
    pub drv_path: String,
    pub outputs: Vec<DerivationOutput>,
}

impl Derivation {
    pub(crate) fn canonicalize(&mut self) -> Result<(), String> {
        self.outputs
            .sort_by(|left, right| left.name.cmp(&right.name));
        self.validate()
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
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

    #[must_use]
    pub(crate) fn output_paths(&self) -> Vec<String> {
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

pub(crate) fn is_nix_store_path(value: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn node_spec_contract_covers_the_struct_and_pins_both_input_surfaces() {
        let spec = NodeSpec {
            argv: Some(vec!["true".to_owned()]),
            adapter: Some("shell".to_owned()),
            prompt: Some("mission".to_owned()),
            pools: vec!["slot".to_owned()],
            executor: Some("worker".to_owned()),
            priority: Some("high".to_owned()),
            runtime_max_sec: Some(30),
            evidence: vec!["exit:0".to_owned()],
            drv: Some(Derivation {
                drv_path: "/nix/store/00000000000000000000000000000000-node.drv".to_owned(),
                outputs: vec![DerivationOutput {
                    name: "out".to_owned(),
                    path: "/nix/store/11111111111111111111111111111111-node".to_owned(),
                }],
            }),
            evidence_class: Some(json!({"kind": "contract"})),
            manifest_hash: Some("sha256:manifest".to_owned()),
            workspace: Some(json!({"repo": "mecattaf/tally.nix"})),
            brief: Some(json!({"mission": "test"})),
            key: Some("node".to_owned()),
            dedup_key: Some("dedup".to_owned()),
            label: Some("label".to_owned()),
            env: BTreeMap::from([("SAFE".to_owned(), "yes".to_owned())]),
            result_schema: Some(json!({"type": "object"})),
            adapter_options: Some(json!({"model": "provider/model"})),
            selection: Some(SelectionProvenance {
                selector: "pooled".to_owned(),
                catalog_hash: "sha256:catalog".to_owned(),
                member_id: "worker-a".to_owned(),
                members: vec!["worker-a".to_owned()],
            }),
        };
        let serialized = serde_json::to_value(spec).unwrap();
        let serialized_fields = serialized
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            serialized_fields,
            NODE_SPEC_FIELD_CONTRACT
                .iter()
                .map(|field| field.json_name)
                .collect::<Vec<_>>()
        );

        assert_eq!(
            node_spec_fields(NodeSpecSurface::Job).collect::<Vec<_>>(),
            [
                "argv",
                "adapter",
                "prompt",
                "pools",
                "executor",
                "priority",
                "runtimeMaxSec",
                "evidence",
                "evidenceClass",
                "manifestHash",
                "workspace",
                "brief",
                "key",
                "dedupKey",
                "label",
                "env",
                "resultSchema",
            ]
        );
        assert_eq!(
            node_spec_fields(NodeSpecSurface::Sugar).collect::<Vec<_>>(),
            [
                "argv",
                "adapter",
                "pools",
                "executor",
                "priority",
                "runtimeMaxSec",
                "evidence",
                "evidenceClass",
                "manifestHash",
                "workspace",
                "brief",
                "key",
                "dedupKey",
                "label",
                "env",
                "resultSchema",
            ]
        );
        assert_eq!(
            flow_canonical_payload_fields(),
            [
                "argv",
                "pool",
                "executor",
                "adapter",
                "workspace",
                "adapterOptions",
                "evidence",
                "drv",
                "evidenceClass",
                "manifestHash",
                "runtimeMaxSec",
                "noEnqueue",
                "credentials",
                "briefHash",
            ]
        );

        for field in NODE_SPEC_FIELD_CONTRACT {
            match field.wire {
                NodeWireProjection::Field(field) => assert!(!field.is_empty()),
                NodeWireProjection::NormalizedInto(target) => assert!(
                    NODE_SPEC_FIELD_CONTRACT
                        .iter()
                        .any(|candidate| candidate.json_name == target),
                    "{} normalizes into missing wire field {target}",
                    field.json_name
                ),
                NodeWireProjection::Excluded(reason) => assert!(!reason.is_empty()),
            }
            match field.canonical {
                NodeCanonicalProjection::Hashed { field, .. } => assert!(!field.is_empty()),
                NodeCanonicalProjection::NormalizedInto(target) => assert!(
                    NODE_SPEC_FIELD_CONTRACT
                        .iter()
                        .any(|candidate| candidate.json_name == target),
                    "{} normalizes into missing NodeSpec field {target}",
                    field.json_name
                ),
                NodeCanonicalProjection::Excluded(reason) => assert!(!reason.is_empty()),
            }
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeSpecSurface {
    Job,
    Sugar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeCanonicalProjection {
    Hashed { field: &'static str, order: u8 },
    NormalizedInto(&'static str),
    Excluded(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeWireProjection {
    Field(&'static str),
    NormalizedInto(&'static str),
    Excluded(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeSpecFieldContract {
    pub rust_name: &'static str,
    pub json_name: &'static str,
    pub job: bool,
    pub sugar: bool,
    pub wire: NodeWireProjection,
    pub canonical: NodeCanonicalProjection,
}

macro_rules! define_node_spec {
    (
        $(
            $(#[$field_attribute:meta])*
            $field:ident: $field_type:ty => {
                json: $json_name:literal,
                job: $job:literal,
                sugar: $sugar:literal,
                wire: $wire:expr,
                canonical: $canonical:expr
            }
        ),+ $(,)?
    ) => {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        pub struct NodeSpec {
            $(
                $(#[$field_attribute])*
                pub $field: $field_type,
            )+
        }

        pub const NODE_SPEC_FIELD_CONTRACT: &[NodeSpecFieldContract] = &[
            $(
                NodeSpecFieldContract {
                    rust_name: stringify!($field),
                    json_name: $json_name,
                    job: $job,
                    sugar: $sugar,
                    wire: $wire,
                    canonical: $canonical,
                },
            )+
        ];
    };
}

define_node_spec! {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    argv: Option<Vec<String>> => {
        json: "argv", job: true, sugar: true,
        wire: NodeWireProjection::Field("argv"),
        canonical: NodeCanonicalProjection::Hashed { field: "argv", order: 10 }
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adapter: Option<String> => {
        json: "adapter", job: true, sugar: true,
        wire: NodeWireProjection::Field("adapter"),
        canonical: NodeCanonicalProjection::Hashed { field: "adapter", order: 40 }
    },
    // `job()` accepts prompt as a field. Agent sugars accept it positionally and
    // deliberately exclude it from their option objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<String> => {
        json: "prompt", job: true, sugar: false,
        wire: NodeWireProjection::NormalizedInto("brief"),
        canonical: NodeCanonicalProjection::NormalizedInto("brief")
    },
    pools: Vec<String> => {
        json: "pools", job: true, sugar: true,
        wire: NodeWireProjection::Field("pool"),
        canonical: NodeCanonicalProjection::Hashed { field: "pool", order: 20 }
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    executor: Option<String> => {
        json: "executor", job: true, sugar: true,
        wire: NodeWireProjection::Field("executor"),
        canonical: NodeCanonicalProjection::Hashed { field: "executor", order: 30 }
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority: Option<String> => {
        json: "priority", job: true, sugar: true,
        wire: NodeWireProjection::Field("priority"),
        canonical: NodeCanonicalProjection::Excluded("priority is admission scheduling metadata")
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_max_sec: Option<u64> => {
        json: "runtimeMaxSec", job: true, sugar: true,
        wire: NodeWireProjection::Field("runtimeMaxSec"),
        canonical: NodeCanonicalProjection::Hashed { field: "runtimeMaxSec", order: 110 }
    },
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    evidence: Vec<String> => {
        json: "evidence", job: true, sugar: true,
        wire: NodeWireProjection::Field("evidence"),
        canonical: NodeCanonicalProjection::Hashed { field: "evidence", order: 70 }
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    drv: Option<Derivation> => {
        json: "drv", job: false, sugar: false,
        wire: NodeWireProjection::Field("drv"),
        canonical: NodeCanonicalProjection::Hashed { field: "drv", order: 80 }
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence_class: Option<Value> => {
        json: "evidenceClass", job: true, sugar: true,
        wire: NodeWireProjection::Field("evidenceClass"),
        canonical: NodeCanonicalProjection::Hashed { field: "evidenceClass", order: 90 }
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    manifest_hash: Option<String> => {
        json: "manifestHash", job: true, sugar: true,
        wire: NodeWireProjection::Field("manifestHash"),
        canonical: NodeCanonicalProjection::Hashed { field: "manifestHash", order: 100 }
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace: Option<Value> => {
        json: "workspace", job: true, sugar: true,
        wire: NodeWireProjection::Field("workspace"),
        canonical: NodeCanonicalProjection::Hashed { field: "workspace", order: 50 }
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    brief: Option<Value> => {
        json: "brief", job: true, sugar: true,
        wire: NodeWireProjection::Field("brief"),
        canonical: NodeCanonicalProjection::Hashed { field: "briefHash", order: 140 }
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<String> => {
        json: "key", job: true, sugar: true,
        wire: NodeWireProjection::Excluded("key is resolved into FlowSubmission.dedupKey"),
        canonical: NodeCanonicalProjection::Excluded("key selects the admission dedup identity")
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dedup_key: Option<String> => {
        json: "dedupKey", job: true, sugar: true,
        wire: NodeWireProjection::Excluded("dedupKey is carried by FlowSubmission"),
        canonical: NodeCanonicalProjection::Excluded("dedupKey is admission identity metadata")
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String> => {
        json: "label", job: true, sugar: true,
        wire: NodeWireProjection::Excluded("label is carried by orchestration.nodeLabel"),
        canonical: NodeCanonicalProjection::Excluded("label is orchestration diagnostic metadata")
    },
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String> => {
        json: "env", job: true, sugar: true,
        wire: NodeWireProjection::NormalizedInto("adapterOptions"),
        canonical: NodeCanonicalProjection::NormalizedInto("adapterOptions")
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result_schema: Option<Value> => {
        json: "resultSchema", job: true, sugar: true,
        wire: NodeWireProjection::Excluded("resultSchema stays in the live runner"),
        canonical: NodeCanonicalProjection::Excluded("resultSchema is runner-side result validation")
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adapter_options: Option<Value> => {
        json: "adapterOptions", job: false, sugar: false,
        wire: NodeWireProjection::Field("adapterOptions"),
        canonical: NodeCanonicalProjection::Hashed { field: "adapterOptions", order: 60 }
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selection: Option<SelectionProvenance> => {
        json: "selection", job: false, sugar: false,
        wire: NodeWireProjection::Excluded("selection is carried by orchestration.selection"),
        canonical: NodeCanonicalProjection::Excluded("selection is recorded in orchestration provenance")
    }
}

pub fn node_spec_fields(surface: NodeSpecSurface) -> impl Iterator<Item = &'static str> {
    NODE_SPEC_FIELD_CONTRACT
        .iter()
        .filter(move |field| match surface {
            NodeSpecSurface::Job => field.job,
            NodeSpecSurface::Sugar => field.sugar,
        })
        .map(|field| field.json_name)
}

#[must_use]
pub fn flow_canonical_payload_fields() -> Vec<&'static str> {
    let mut fields = NODE_SPEC_FIELD_CONTRACT
        .iter()
        .filter_map(|field| match field.canonical {
            NodeCanonicalProjection::Hashed { field, order } => Some((order, field)),
            NodeCanonicalProjection::NormalizedInto(_) | NodeCanonicalProjection::Excluded(_) => {
                None
            }
        })
        .chain([(120, "noEnqueue"), (130, "credentials")])
        .collect::<Vec<_>>();
    fields.sort_unstable_by_key(|(order, _)| *order);
    fields.into_iter().map(|(_, field)| field).collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowSubmission {
    pub mode: String,
    pub dedup_key: String,
    pub payload_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_uuid: Option<String>,
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
