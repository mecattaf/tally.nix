//! Machine-readable outcomes from one campaign registry poll.
//!
//! A poll scans a fleet, but every emitted fact belongs to exactly one durable
//! registration. Keeping that attribution in the type prevents a caller from
//! recreating ambiguous fleet-wide counters when it serializes outcomes to a
//! journal.
//!
//! Schema 2 deleted the `stabilizing`/`rearm-required` pair. A changed graph is
//! not a refusal waiting for an operator verb: `forge:"local"` promises
//! REMOTE-AUTHORITY, so a push to the armed identity's worklist is itself the
//! arming act and the observing pass re-admits it.
//!
//! Schema 3 reports the campaign lease. `deferred` is a pass that found the
//! identity already leased and dispatched nothing, and `complete` carries the
//! lapse — the written completion fact — rather than a registration prune
//! standing in for one.

use std::fmt;

use serde::{Deserialize, Serialize};

pub const CAMPAIGN_POLL_EVENT_SCHEMA_VERSION: u32 = 3;

/// What one poll decided for one registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CampaignPollStatus {
    Unchanged,
    /// The pass observed a changed worklist at the identity's authority
    /// remote and admitted it as a fresh reconcile epoch. The event carries
    /// the superseded and newly admitted digests plus the arm serial that
    /// now owns attempts, so a straddling attempt stays attributable.
    Readmitted,
    Dispatched,
    /// Another pass already holds this identity's lease. This one admitted
    /// nothing and dispatched nothing; the holder owns the frontier.
    Deferred,
    Complete,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CampaignPollAction {
    /// The lease lapsed: the last admitted task went terminal under a
    /// gate-proven, published head and the pass wrote the completion fact.
    /// Nothing was pruned — the identity stays armed, and a push to its
    /// worklist reactivates it.
    Lapsed,
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
    /// The arm serial the registration holds after this event. Present on a
    /// re-admission because that is the moment attempt ownership moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arm_serial: Option<u64>,
    /// The one revision the lapse fact names: gate-proven and published.
    /// Present on `complete`, which is the only status that writes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_head: Option<String>,
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
            arm_serial: None,
            published_head: None,
            detail: None,
        }
    }

    /// The terminal event: this identity's lease lapsed on a published head.
    ///
    /// The revision travels on the event because the lapse fact is the thing
    /// a release reads, and a reader tailing the poll stream should never
    /// have to open the lease file to learn which revision finished.
    #[must_use]
    pub fn complete(
        registration_id: impl Into<String>,
        issue_url: impl Into<String>,
        registration: impl Into<String>,
        published_head: impl Into<String>,
        arm_serial: u64,
    ) -> Self {
        let mut event = Self::new(
            registration_id,
            issue_url,
            registration,
            CampaignPollStatus::Complete,
        );
        event.action = Some(CampaignPollAction::Lapsed);
        event.published_head = Some(published_head.into());
        event.arm_serial = Some(arm_serial);
        event
    }

    /// A pass that found the identity leased by another and stood down.
    #[must_use]
    pub fn deferred(
        registration_id: impl Into<String>,
        issue_url: impl Into<String>,
        registration: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        let mut event = Self::new(
            registration_id,
            issue_url,
            registration,
            CampaignPollStatus::Deferred,
        );
        event.detail = Some(detail.into());
        event
    }

    /// One epoch flip, named from both sides.
    ///
    /// `approved_graph_digest` is the epoch this pass superseded and
    /// `live_graph_digest` the one it admitted, so a reader holding either
    /// digest — an operator, or an attempt prepared before the push — can
    /// place itself without consulting a second record.
    #[must_use]
    pub fn readmitted(
        registration_id: impl Into<String>,
        issue_url: impl Into<String>,
        registration: impl Into<String>,
        superseded_graph_digest: impl Into<String>,
        admitted_graph_digest: impl Into<String>,
        arm_serial: u64,
    ) -> Self {
        let mut event = Self::new(
            registration_id,
            issue_url,
            registration,
            CampaignPollStatus::Readmitted,
        );
        event.approved_graph_digest = Some(superseded_graph_digest.into());
        event.live_graph_digest = Some(admitted_graph_digest.into());
        event.arm_serial = Some(arm_serial);
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

/// A pass, worktree, or receipt prepared under one admitted graph met a
/// different one.
///
/// This type exists because of a recorded incident (specs/eta/evidence
/// /run-log.md, the S4 attempt-3 postmortem): a re-arm landed while an
/// attempt was mid-flight, the attempt's committed worktree was judged
/// against the freshly admitted graph, and the campaign reported "agent
/// produced no commit relative to the prepared base" — a sentence about the
/// agent, naming neither digest, for a fault that belonged entirely to the
/// epoch. A straddle is a fact about two digests and must always be told as
/// one. Nothing here proposes an operator verb: re-admission is automatic,
/// so the remedy is never "go re-arm it".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CampaignDigestMismatch {
    /// What was refused, in the reader's vocabulary: a pass, an attempt, a
    /// worktree.
    pub subject: String,
    /// The digest the identity admits now.
    pub admitted_graph_digest: String,
    pub admitted_arm_serial: u64,
    /// The digest the refused subject was prepared under.
    pub prepared_graph_digest: String,
    /// The arm that prepared it, when the superseded epoch is still on disk
    /// to name it. Absent means only the digest could be recovered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_arm_serial: Option<u64>,
}

impl fmt::Display for CampaignDigestMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "digest-mismatch: {} was prepared under graph {}",
            self.subject, self.prepared_graph_digest
        )?;
        if let Some(arm_serial) = self.prepared_arm_serial {
            write!(formatter, " (arm {arm_serial})")?;
        }
        write!(
            formatter,
            ", but this campaign admits graph {} (arm {}); the work it carries belongs to the epoch it was prepared under, not to the agent",
            self.admitted_graph_digest, self.admitted_arm_serial
        )
    }
}

impl std::error::Error for CampaignDigestMismatch {}

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
            "e".repeat(40),
            6,
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
        assert_eq!(events[1]["action"], "lapsed");
        assert_eq!(events[1]["publishedHead"], "e".repeat(40));
        assert_eq!(events[1]["armSerial"], 6);
        assert!(events[1].get("detail").is_none());
    }

    /// The lease's two new reports, and the one they replaced.
    #[test]
    fn lease_poll_events_report_deferral_and_lapse_without_a_prune() {
        let deferred = serde_json::to_value(CampaignPollEvent::deferred(
            "0198f000-0000-7000-8000-000000000004",
            "local://acme/four/specs/four.json",
            "/state/campaigns/armed/four.json",
            "pid 4242 holds this identity's lease",
        ))
        .unwrap();
        assert_eq!(deferred["status"], "deferred");
        assert_eq!(
            deferred["schemaVersion"],
            CAMPAIGN_POLL_EVENT_SCHEMA_VERSION
        );
        assert!(deferred.get("action").is_none());
        assert!(deferred["detail"].as_str().unwrap().contains("4242"));

        // Completion is a written fact about a revision, so a prune standing in
        // for one must fail loudly on the wire.
        let complete = serde_json::to_string(&CampaignPollEvent::complete(
            "0198f000-0000-7000-8000-000000000005",
            "local://acme/five/specs/five.json",
            "/state/campaigns/armed/five.json",
            "f".repeat(40),
            2,
        ))
        .unwrap();
        assert!(serde_json::from_str::<CampaignPollEvent>(&complete).is_ok());
        assert!(serde_json::from_str::<CampaignPollEvent>(
            &complete.replace("\"lapsed\"", "\"pruned\""),
        )
        .is_err());
    }

    #[test]
    fn readmission_event_names_both_graphs_and_the_arm_that_now_owns_attempts() {
        let event = CampaignPollEvent::readmitted(
            "0198f000-0000-7000-8000-000000000003",
            "https://github.com/acme/three/issues/3",
            "/state/campaigns/armed/three.json",
            format!("sha256:{}", "a".repeat(64)),
            format!("sha256:{}", "b".repeat(64)),
            4,
        );
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["status"], "readmitted");
        assert_eq!(
            value["approvedGraphDigest"],
            format!("sha256:{}", "a".repeat(64))
        );
        assert_eq!(
            value["liveGraphDigest"],
            format!("sha256:{}", "b".repeat(64))
        );
        assert_eq!(value["armSerial"], 4);
        assert_eq!(value["schemaVersion"], CAMPAIGN_POLL_EVENT_SCHEMA_VERSION);

        // The refusal this status replaced is gone from the wire, not merely
        // unemitted: a reader that still expects it must fail loudly.
        assert!(serde_json::from_str::<CampaignPollEvent>(
            &serde_json::to_string(&event)
                .unwrap()
                .replace("\"readmitted\"", "\"rearm-required\""),
        )
        .is_err());
    }

    #[test]
    fn readmission_straddle_refusal_names_both_digests_and_blames_no_agent() {
        let mismatch = CampaignDigestMismatch {
            subject: "campaign pass acme/widgets specs/night/tasks.json".to_owned(),
            admitted_graph_digest: format!("sha256:{}", "b".repeat(64)),
            admitted_arm_serial: 5,
            prepared_graph_digest: format!("sha256:{}", "a".repeat(64)),
            prepared_arm_serial: Some(4),
        };
        let rendered = mismatch.to_string();
        assert!(rendered.starts_with("digest-mismatch: "), "{rendered}");
        assert!(
            rendered.contains(&mismatch.prepared_graph_digest),
            "{rendered}"
        );
        assert!(
            rendered.contains(&mismatch.admitted_graph_digest),
            "{rendered}"
        );
        assert!(
            rendered.contains("arm 4") && rendered.contains("arm 5"),
            "{rendered}"
        );
        // The two sentences a stale digest mismatch must not print, and the
        // verb it must not demand.
        for forbidden in ["produced no commit", "re-arm", "campaign arm"] {
            assert!(!rendered.contains(forbidden), "{forbidden:?} in {rendered}");
        }

        // An unknown prepared arm still names both digests; only the arm
        // clause drops out.
        let unnamed = CampaignDigestMismatch {
            prepared_arm_serial: None,
            ..mismatch
        };
        let rendered = unnamed.to_string();
        assert!(
            rendered.contains(&unnamed.prepared_graph_digest),
            "{rendered}"
        );
        assert!(
            rendered.contains(&unnamed.admitted_graph_digest),
            "{rendered}"
        );
        assert!(!rendered.contains("(arm 4)"), "{rendered}");
    }
}
