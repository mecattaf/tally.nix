//! Rendering for the ledger-backed `Assisted-by:` commit trailer.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AssistedBy {
    pub adapter: String,
    pub model: String,
    pub task_uuid: String,
    pub witness_seq: u64,
}

impl AssistedBy {
    pub fn trailer(&self) -> String {
        format!(
            "Assisted-by: {}:{} (tally:{} witness:{})",
            self.adapter, self.model, self.task_uuid, self.witness_seq
        )
    }
}

pub fn assisted_by_from_evidence(evidence: &Value) -> Option<AssistedBy> {
    let adapter = evidence.get("adapter")?.as_str()?;
    let model = evidence.get("model")?.as_str()?;
    let task_uuid = evidence.get("taskUuid")?.as_str()?;
    let witness_seq = evidence
        .get("witnessSeq")?
        .as_u64()
        .filter(|seq| *seq > 0)?;
    if adapter.is_empty()
        || model.is_empty()
        || adapter.chars().any(char::is_control)
        || model.chars().any(char::is_control)
        || Uuid::parse_str(task_uuid).is_err()
    {
        return None;
    }
    Some(AssistedBy {
        adapter: adapter.to_owned(),
        model: model.to_owned(),
        task_uuid: task_uuid.to_owned(),
        witness_seq,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_renders_the_ledger_backed_trailer() {
        let evidence = serde_json::json!({
            "adapter": "codex",
            "model": "gpt-5.6-codex",
            "taskUuid": "00000000-0000-4000-8000-000000000049",
            "witnessSeq": 5,
        });

        assert_eq!(
            assisted_by_from_evidence(&evidence).unwrap().trailer(),
            "Assisted-by: codex:gpt-5.6-codex (tally:00000000-0000-4000-8000-000000000049 witness:5)"
        );
    }

    #[test]
    fn invalid_evidence_does_not_render_a_trailer() {
        for evidence in [
            serde_json::json!({
                "adapter": "",
                "model": "gpt-5.6-codex",
                "taskUuid": "00000000-0000-4000-8000-000000000049",
                "witnessSeq": 5,
            }),
            serde_json::json!({
                "adapter": "codex",
                "model": "gpt-5.6-codex\n",
                "taskUuid": "00000000-0000-4000-8000-000000000049",
                "witnessSeq": 5,
            }),
            serde_json::json!({
                "adapter": "codex",
                "model": "gpt-5.6-codex",
                "taskUuid": "not-a-uuid",
                "witnessSeq": 5,
            }),
            serde_json::json!({
                "adapter": "codex",
                "model": "gpt-5.6-codex",
                "taskUuid": "00000000-0000-4000-8000-000000000049",
                "witnessSeq": 0,
            }),
        ] {
            assert_eq!(assisted_by_from_evidence(&evidence), None);
        }
    }
}
