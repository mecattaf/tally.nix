use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use uuid::Uuid;

pub use tally_client::{TaskRef, MAX_TASK_REF_COMPONENT_BYTES};

pub const DEFAULT_FLOW_MAX_NODES: u64 = 1_000;

/// Closed semantic vocabulary for nodes emitted by the spec-build flow.
///
/// Labels remain operator-facing diagnostics. This role is the stable field
/// campaign folds use when a node's meaning matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpecBuildNodeRole {
    Agent,
    CheckpointRecord,
    Cleanup,
    Constraint,
    Continue,
    Diagnosis,
    Escalate,
    Gate,
    Merge,
    Ownership,
    Prep,
    Publish,
    Rebase,
    Reconcile,
    Retry,
    Steering,
    Sweep,
}

impl SpecBuildNodeRole {
    pub const ALL: [Self; 17] = [
        Self::Agent,
        Self::CheckpointRecord,
        Self::Cleanup,
        Self::Constraint,
        Self::Continue,
        Self::Diagnosis,
        Self::Escalate,
        Self::Gate,
        Self::Merge,
        Self::Ownership,
        Self::Prep,
        Self::Publish,
        Self::Rebase,
        Self::Reconcile,
        Self::Retry,
        Self::Steering,
        Self::Sweep,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::CheckpointRecord => "checkpoint-record",
            Self::Cleanup => "cleanup",
            Self::Constraint => "constraint",
            Self::Continue => "continue",
            Self::Diagnosis => "diagnosis",
            Self::Escalate => "escalate",
            Self::Gate => "gate",
            Self::Merge => "merge",
            Self::Ownership => "ownership",
            Self::Prep => "prep",
            Self::Publish => "publish",
            Self::Rebase => "rebase",
            Self::Reconcile => "reconcile",
            Self::Retry => "retry",
            Self::Steering => "steering",
            Self::Sweep => "sweep",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SpecBuildNodeIdentity<'a> {
    role: SpecBuildNodeRole,
    subject_task_id: Option<&'a str>,
}

impl<'a> SpecBuildNodeIdentity<'a> {
    /// Decode the flow-local portion of
    /// `flow:<run>:k:spec-build:v1:<role>:<subject>:<diagnostic-key>`.
    fn parse(key: &'a str) -> Result<Option<Self>, String> {
        if !key.starts_with("spec-build:") {
            return Ok(None);
        }
        let mut fields = key.splitn(5, ':');
        let namespace = fields.next();
        let version = fields.next();
        let role = fields.next();
        let subject = fields.next();
        let diagnostic_key = fields.next();
        if namespace != Some("spec-build")
            || version != Some("v1")
            || role.is_none()
            || subject.is_none()
            || diagnostic_key.is_none_or(str::is_empty)
        {
            return Err("spec-build node key must match spec-build:v1:<role>:<subjectTaskId>:<diagnostic-key>".to_owned());
        }
        let role = SpecBuildNodeRole::parse(role.expect("checked above"))
            .ok_or_else(|| "spec-build node key carries an unknown node role".to_owned())?;
        let subject = subject.expect("checked above");
        let subject_task_id = (!subject.is_empty()).then_some(subject);
        if let Some(subject_task_id) = subject_task_id {
            validate_subject_task_id(subject_task_id)?;
        }
        Ok(Some(Self {
            role,
            subject_task_id,
        }))
    }
}

fn validate_subject_task_id(subject_task_id: &str) -> Result<(), String> {
    TaskRef::new(format!("spec-build/{subject_task_id}"))
        .map(|_| ())
        .map_err(|_| {
            "orchestration subjectTaskId must be a valid task-ref component when set".to_owned()
        })
}

/// An orchestration capsule is intentionally opaque apart from the admission
/// fields the kernel owns. Keeping the original `Value` preserves
/// object member order and every uninterpreted field unchanged across
/// row-to-witness serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orchestration(Value);

impl Orchestration {
    pub fn new(value: Value) -> Result<Self, String> {
        let capsule = Self(value);
        capsule.validate()?;
        Ok(capsule)
    }

    pub fn validate(&self) -> Result<(), String> {
        let object = self
            .0
            .as_object()
            .ok_or_else(|| "orchestration must be a JSON object".to_owned())?;
        if object.contains_key("iterationPath") {
            return Err("orchestration iterationPath does not exist in the flow model".to_owned());
        }
        let flow_run_id = object
            .get("flowRunId")
            .and_then(Value::as_str)
            .ok_or_else(|| "orchestration flowRunId must be a UUID string".to_owned())?;
        Uuid::parse_str(flow_run_id)
            .map_err(|_| "orchestration flowRunId must be a UUID string".to_owned())?;
        if let Some(max_nodes) = object.get("maxNodes") {
            if max_nodes.as_u64().is_none_or(|value| value == 0) {
                return Err("orchestration maxNodes must be a positive integer when set".to_owned());
            }
        }
        if object
            .get("nodeOrdinal")
            .is_some_and(|ordinal| ordinal.as_u64().is_none())
        {
            return Err(
                "orchestration nodeOrdinal must be a non-negative integer when set".to_owned(),
            );
        }
        if object.get("promptRevision").is_some_and(|revision| {
            revision.as_str().is_none_or(|revision| {
                revision.len() != 71
                    || !revision.starts_with("sha256:")
                    || !revision[7..]
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        }) {
            return Err(
                "orchestration promptRevision must be lowercase sha256 hex when set".to_owned(),
            );
        }
        if object.get("skillRevision").is_some_and(|revision| {
            revision.as_str().is_none_or(|revision| {
                revision.is_empty()
                    || revision.len() > 256
                    || revision.chars().any(char::is_control)
            })
        }) {
            return Err(
                "orchestration skillRevision must be non-empty, at most 256 bytes, and contain no control characters when set"
                    .to_owned(),
            );
        }
        let task_ref = if let Some(task_ref) = object.get("taskRef") {
            let task_ref = task_ref
                .as_str()
                .ok_or_else(|| "orchestration taskRef must be a string when set".to_owned())?;
            Some(
                TaskRef::new(task_ref.to_owned())
                    .map_err(|error| format!("orchestration {error}"))?,
            )
        } else {
            None
        };
        let node_role = object
            .get("nodeRole")
            .map(|role| {
                let role = role
                    .as_str()
                    .ok_or_else(|| "orchestration nodeRole must be a string when set".to_owned())?;
                SpecBuildNodeRole::parse(role)
                    .ok_or_else(|| "orchestration nodeRole is not a spec-build role".to_owned())
            })
            .transpose()?;
        let subject_task_id = object
            .get("subjectTaskId")
            .map(|subject| {
                let subject = subject.as_str().ok_or_else(|| {
                    "orchestration subjectTaskId must be a string when set".to_owned()
                })?;
                validate_subject_task_id(subject)?;
                Ok::<_, String>(subject)
            })
            .transpose()?;
        if subject_task_id.is_some() && node_role.is_none() {
            return Err("orchestration subjectTaskId requires nodeRole when set".to_owned());
        }
        if let Some(task_ref) = task_ref.as_ref() {
            if node_role.is_some() && subject_task_id.is_none() {
                return Err(
                    "orchestration nodeRole requires subjectTaskId when taskRef is set".to_owned(),
                );
            }
            if subject_task_id.is_some_and(|subject| subject != task_ref.task_id()) {
                return Err(
                    "orchestration subjectTaskId must match taskRef's task component".to_owned(),
                );
            }
        } else if subject_task_id.is_some() {
            return Err("orchestration subjectTaskId requires taskRef when set".to_owned());
        }
        Ok(())
    }

    /// Promote a versioned spec-build node key into explicit capsule fields.
    ///
    /// `tally-flow` already carries node keys and labels but has no generic
    /// extension map for orchestration metadata. The spec-build flow therefore
    /// emits a closed, versioned key. Admission decodes it once and persists
    /// the typed identity; read models never need to interpret the label.
    pub fn admit_spec_build_node_identity(&mut self, dedup_key: &str) -> Result<(), String> {
        if self
            .0
            .get("flowName")
            .and_then(Value::as_str)
            .is_none_or(|name| name != "spec-build")
        {
            return Ok(());
        }
        let flow_key_prefix = format!("flow:{}:k:", self.flow_run_id());
        let Some(flow_key) = dedup_key.strip_prefix(&flow_key_prefix) else {
            return Ok(());
        };
        let Some(identity) = SpecBuildNodeIdentity::parse(flow_key)? else {
            return Ok(());
        };
        let object = self
            .0
            .as_object_mut()
            .expect("validated orchestration is an object");
        match object.get("nodeRole") {
            Some(role) if role.as_str() != Some(identity.role.as_str()) => {
                return Err(
                    "orchestration nodeRole disagrees with the spec-build node key".to_owned(),
                );
            }
            None => {
                object.insert(
                    "nodeRole".to_owned(),
                    Value::String(identity.role.as_str().to_owned()),
                );
            }
            Some(_) => {}
        }
        match (object.get("subjectTaskId"), identity.subject_task_id) {
            (Some(subject), Some(expected)) if subject.as_str() != Some(expected) => {
                return Err(
                    "orchestration subjectTaskId disagrees with the spec-build node key".to_owned(),
                );
            }
            (Some(_), None) => {
                return Err(
                    "orchestration subjectTaskId disagrees with the spec-build node key".to_owned(),
                );
            }
            (None, Some(subject)) => {
                object.insert(
                    "subjectTaskId".to_owned(),
                    Value::String(subject.to_owned()),
                );
            }
            (None, None) | (Some(_), Some(_)) => {}
        }
        self.validate()
    }

    pub fn flow_run_id(&self) -> &str {
        self.0
            .get("flowRunId")
            .and_then(Value::as_str)
            .expect("validated orchestration always carries flowRunId")
    }

    pub fn max_nodes(&self) -> Option<u64> {
        self.0.get("maxNodes").and_then(Value::as_u64)
    }

    pub fn node_ordinal(&self) -> Option<u64> {
        self.0.get("nodeOrdinal").and_then(Value::as_u64)
    }

    pub fn task_ref(&self) -> Option<TaskRef> {
        self.0.get("taskRef").and_then(Value::as_str).map(|value| {
            TaskRef::new(value.to_owned())
                .expect("validated orchestration always carries a valid taskRef")
        })
    }

    #[must_use]
    pub fn node_role(&self) -> Option<SpecBuildNodeRole> {
        self.0
            .get("nodeRole")
            .and_then(Value::as_str)
            .and_then(SpecBuildNodeRole::parse)
    }

    #[must_use]
    pub fn subject_task_id(&self) -> Option<&str> {
        self.0.get("subjectTaskId").and_then(Value::as_str)
    }

    pub fn effective_max_nodes(&self) -> u64 {
        self.max_nodes().unwrap_or(DEFAULT_FLOW_MAX_NODES)
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

impl Serialize for Orchestration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Orchestration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn capsule_preserves_opaque_member_order_and_interprets_only_guardrail_fields() {
        let raw = r#"{"flowName":"nightly","flowRunId":"018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321","scriptHash":"sha256-x","nodeOrdinal":7,"maxNodes":12,"selection":{"members":["b","a"]}}"#;
        let capsule: Orchestration = serde_json::from_str(raw).unwrap();
        assert_eq!(
            capsule.flow_run_id(),
            "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321"
        );
        assert_eq!(capsule.effective_max_nodes(), 12);
        assert_eq!(capsule.node_ordinal(), Some(7));
        assert_eq!(serde_json::to_string(&capsule).unwrap(), raw);
    }

    #[test]
    fn capsule_rejects_missing_identity_invalid_caps_and_iteration_path() {
        for invalid in [
            serde_json::json!({}),
            serde_json::json!({"flowRunId": "not-a-uuid"}),
            serde_json::json!({
                "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
                "maxNodes": 0
            }),
            serde_json::json!({
                "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
                "nodeOrdinal": -1
            }),
            serde_json::json!({
                "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
                "iterationPath": [1]
            }),
            serde_json::json!({
                "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
                "promptRevision": "sha256:ABC"
            }),
            serde_json::json!({
                "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
                "skillRevision": ""
            }),
            serde_json::json!({
                "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
                "skillRevision": "bad\nrevision"
            }),
        ] {
            assert!(Orchestration::new(invalid).is_err());
        }
    }

    #[test]
    fn capsule_accepts_reserved_revision_keys_and_preserves_their_order() {
        let raw = concat!(
            r#"{"flowRunId":"018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321","promptRevision":"sha256:"#,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            r#"","skillRevision":"review-agent-v3"}"#,
        );
        let capsule: Orchestration = serde_json::from_str(raw).unwrap();
        assert_eq!(serde_json::to_string(&capsule).unwrap(), raw);
    }

    #[test]
    fn task_ref_is_a_validated_scalar_and_remains_opaque_provenance() {
        let raw = r#"{"flowRunId":"018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321","taskRef":"crm/t07"}"#;
        let capsule: Orchestration = serde_json::from_str(raw).unwrap();
        let task_ref = capsule.task_ref().unwrap();
        assert_eq!(task_ref.as_str(), "crm/t07");
        assert_eq!(task_ref.campaign(), "crm");
        assert_eq!(task_ref.task_id(), "t07");
        assert_eq!(serde_json::to_string(&capsule).unwrap(), raw);

        for invalid in [
            "crm",
            "crm/t07/extra",
            "crm/task id",
            "/t07",
            "crm/",
            ".crm/t07",
            "crm/-t07",
        ] {
            assert!(TaskRef::new(invalid).is_err(), "accepted {invalid:?}");
        }
        assert!(Orchestration::new(serde_json::json!({
            "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
            "taskRef": {"campaign": "crm", "id": "t07"}
        }))
        .is_err());
    }

    #[test]
    fn typed_spec_build_identity_is_admitted_from_the_versioned_flow_key() {
        let mut capsule = Orchestration::new(serde_json::json!({
            "flowName": "spec-build",
            "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
            "nodeLabel": "operator-facing text",
            "taskRef": "crm/t07"
        }))
        .unwrap();
        capsule
            .admit_spec_build_node_identity(
                "flow:018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321:k:spec-build:v1:merge:t07:merge-t07",
            )
            .unwrap();

        assert_eq!(capsule.node_role(), Some(SpecBuildNodeRole::Merge));
        assert_eq!(capsule.subject_task_id(), Some("t07"));
        assert_eq!(capsule.as_value()["nodeLabel"], "operator-facing text");
        assert_eq!(capsule.as_value()["nodeRole"], "merge");
        assert_eq!(capsule.as_value()["subjectTaskId"], "t07");
    }

    #[test]
    fn typed_identity_refuses_unknown_roles_and_subject_disagreement() {
        let capsule = || {
            Orchestration::new(serde_json::json!({
                "flowName": "spec-build",
                "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
                "taskRef": "crm/t07"
            }))
            .unwrap()
        };
        let mut unknown = capsule();
        assert!(unknown
            .admit_spec_build_node_identity(
                "flow:018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321:k:spec-build:v1:unknown:t07:x",
            )
            .unwrap_err()
            .contains("unknown node role"));
        let mut mismatched = capsule();
        assert!(mismatched
            .admit_spec_build_node_identity(
                "flow:018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321:k:spec-build:v1:agent:t08:x",
            )
            .unwrap_err()
            .contains("must match taskRef"));

        for invalid in [
            serde_json::json!({
                "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
                "nodeRole": "invented"
            }),
            serde_json::json!({
                "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
                "subjectTaskId": "t07",
                "taskRef": "crm/t07"
            }),
            serde_json::json!({
                "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
                "nodeRole": "agent",
                "subjectTaskId": "t08",
                "taskRef": "crm/t07"
            }),
        ] {
            assert!(Orchestration::new(invalid).is_err());
        }
    }

    #[test]
    fn rust_and_spec_build_flow_pin_the_same_role_vocabulary() {
        let flow = include_str!("../../../examples/flows/spec-build.js");
        let declaration = flow
            .split_once("const specBuildNodeRole = Object.freeze(")
            .expect("spec-build flow declares its node roles")
            .1
            .split_once(");")
            .expect("spec-build role declaration has a closing delimiter")
            .0;
        let flow_roles = serde_json::from_str::<BTreeMap<String, String>>(declaration)
            .expect("spec-build role declaration remains a JSON object");
        let flow_values = flow_roles.values().cloned().collect::<Vec<_>>();
        let flow_set = flow_values.iter().cloned().collect::<BTreeSet<_>>();
        let core_set = SpecBuildNodeRole::ALL
            .into_iter()
            .map(|role| role.as_str().to_owned())
            .collect::<BTreeSet<_>>();

        assert_eq!(
            flow_values.len(),
            flow_set.len(),
            "flow roles repeat a value"
        );
        assert_eq!(flow_set, core_set);
    }
}
