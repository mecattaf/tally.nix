use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use taskchampion::Uuid;

pub const DEFAULT_FLOW_MAX_NODES: u64 = 1_000;

/// An orchestration capsule is intentionally opaque apart from the two
/// admission fields the kernel owns. Keeping the original `Value` preserves
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
        Ok(())
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

    #[test]
    fn capsule_preserves_opaque_member_order_and_interprets_only_guardrail_fields() {
        let raw = r#"{"flowName":"nightly","flowRunId":"018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321","scriptHash":"sha256-x","nodeOrdinal":7,"maxNodes":12,"selection":{"members":["b","a"]}}"#;
        let capsule: Orchestration = serde_json::from_str(raw).unwrap();
        assert_eq!(
            capsule.flow_run_id(),
            "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321"
        );
        assert_eq!(capsule.effective_max_nodes(), 12);
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
                "iterationPath": [1]
            }),
        ] {
            assert!(Orchestration::new(invalid).is_err());
        }
    }
}
