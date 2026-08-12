//! Machine-readable outcomes from one campaign registry poll.
//!
//! A poll scans a fleet, but every emitted fact belongs to exactly one durable
//! registration. Keeping that attribution in the type prevents a caller from
//! recreating the old ambiguous fleet counters when it serializes outcomes to
//! a journal.

use serde::{Deserialize, Serialize};

pub const CAMPAIGN_POLL_EVENT_SCHEMA_VERSION: u32 = 1;

/// What one poll decided for one registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CampaignPollStatus {
    Unchanged,
    /// A changed executable graph has been seen once and is awaiting a stable
    /// second observation before it becomes an operator-facing refusal.
    Stabilizing,
    Dispatched,
    Complete,
    RearmRequired,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CampaignPollAction {
    Pruned,
}

/// A single JSON-lines record written by `tally campaign poll --once`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CampaignPollEvent {
    pub schema_version: u32,
    pub registration_id: String,
    pub issue_url: String,
    pub registration: String,
    pub status: CampaignPollStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<CampaignPollAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_graph_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_graph_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CampaignPollEvent {
    #[must_use]
    pub fn new(
        registration_id: impl Into<String>,
        issue_url: impl Into<String>,
        registration: impl Into<String>,
        status: CampaignPollStatus,
    ) -> Self {
        Self {
            schema_version: CAMPAIGN_POLL_EVENT_SCHEMA_VERSION,
            registration_id: registration_id.into(),
            issue_url: issue_url.into(),
            registration: registration.into(),
            status,
            action: None,
            approved_graph_digest: None,
            live_graph_digest: None,
            detail: None,
        }
    }

    #[must_use]
    pub fn complete(
        registration_id: impl Into<String>,
        issue_url: impl Into<String>,
        registration: impl Into<String>,
    ) -> Self {
        let mut event = Self::new(
            registration_id,
            issue_url,
            registration,
            CampaignPollStatus::Complete,
        );
        event.action = Some(CampaignPollAction::Pruned);
        event
    }

    #[must_use]
    pub fn graph_change(
        registration_id: impl Into<String>,
        issue_url: impl Into<String>,
        registration: impl Into<String>,
        status: CampaignPollStatus,
        approved_graph_digest: impl Into<String>,
        live_graph_digest: impl Into<String>,
    ) -> Self {
        debug_assert!(matches!(
            status,
            CampaignPollStatus::Stabilizing | CampaignPollStatus::RearmRequired
        ));
        let mut event = Self::new(registration_id, issue_url, registration, status);
        event.approved_graph_digest = Some(approved_graph_digest.into());
        event.live_graph_digest = Some(live_graph_digest.into());
        event
    }

    #[must_use]
    pub fn failed(
        registration_id: impl Into<String>,
        issue_url: impl Into<String>,
        registration: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        let mut event = Self::new(
            registration_id,
            issue_url,
            registration,
            CampaignPollStatus::Failed,
        );
        event.detail = Some(detail.into());
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_poll_events_keep_registration_and_issue_attribution() {
        let first = CampaignPollEvent::new(
            "0198f000-0000-7000-8000-000000000001",
            "https://github.com/acme/one/issues/1",
            "/state/campaigns/armed/one.json",
            CampaignPollStatus::Dispatched,
        );
        let second = CampaignPollEvent::complete(
            "0198f000-0000-7000-8000-000000000002",
            "https://github.com/acme/two/issues/2",
            "/state/campaigns/armed/two.json",
        );

        let jsonl = [first, second]
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let events = jsonl
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0]["registrationId"],
            "0198f000-0000-7000-8000-000000000001"
        );
        assert_eq!(
            events[0]["issueUrl"],
            "https://github.com/acme/one/issues/1"
        );
        assert_eq!(
            events[1]["registrationId"],
            "0198f000-0000-7000-8000-000000000002"
        );
        assert_eq!(
            events[1]["issueUrl"],
            "https://github.com/acme/two/issues/2"
        );
        assert_eq!(events[1]["status"], "complete");
        assert_eq!(events[1]["action"], "pruned");
        assert!(events[1].get("detail").is_none());
    }

    #[test]
    fn rearm_refusal_names_both_graphs_on_the_attributed_event() {
        let event = CampaignPollEvent::graph_change(
            "0198f000-0000-7000-8000-000000000003",
            "https://github.com/acme/three/issues/3",
            "/state/campaigns/armed/three.json",
            CampaignPollStatus::RearmRequired,
            format!("sha256:{}", "a".repeat(64)),
            format!("sha256:{}", "b".repeat(64)),
        );
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["status"], "rearm-required");
        assert_eq!(
            value["approvedGraphDigest"],
            format!("sha256:{}", "a".repeat(64))
        );
        assert_eq!(
            value["liveGraphDigest"],
            format!("sha256:{}", "b".repeat(64))
        );
    }
}
