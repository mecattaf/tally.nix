//! The publish decision of the single-line integration model.
//!
//! One line of development carries the campaign: lanes branch from and merge
//! to the integration branch, worklist amendments are commits on that same
//! branch, and the base branch advances **only** by fast-forward of a head a
//! stage gate has proven. This module is the pure half of that act — it
//! consumes already-witnessed Git facts and returns the plan a caller must
//! execute, so the driver and any release surface decide identically.
//!
//! Two properties are structural rather than procedural. A plan publishes a
//! revision only when the gate proof names that exact revision, and a receipt
//! carries exactly one revision: [`PublishReceiptV1`] has a single `sha`, and
//! its constructor refuses a proof reference that does not name it. The pair
//! of shas whose divergence the record charges to publish (a proven head and
//! a different published head) has nowhere to live.

use std::collections::BTreeSet;

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PUBLISH_RECEIPT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PublishError {
    #[error("a published revision must be a full Git object ID")]
    InvalidPublishedRevision,
    #[error("a publish receipt names the gate proof of the revision it published")]
    UnprovenPublication,
    #[error("a publish receipt records the actor that moved the base branch")]
    InvalidActor,
    #[error("a publish receipt records when the base branch moved")]
    InvalidTimestamp,
    #[error("a publish receipt names the base branch it advanced")]
    InvalidBaseBranch,
}

/// What the machinery may do with the integration head it just witnessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PublishAction {
    /// The base branch advances to the proven head, and nothing else happens.
    FastForward,
    /// The proven head is already reachable from the base branch.
    AlreadyPublished,
    /// The two lines diverged over disjoint paths: rebase the integration
    /// line onto the base branch. The rebased head is a different commit, so
    /// it is unproven and the base branch does not move on this act.
    RebaseAndRegate,
    /// The base branch stays where it is.
    Withhold,
}

impl PublishAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FastForward => "fast-forward",
            Self::AlreadyPublished => "already-published",
            Self::RebaseAndRegate => "rebase-and-regate",
            Self::Withhold => "withhold",
        }
    }

    /// Whether this action leaves the base branch naming the proven head.
    #[must_use]
    pub const fn publishes(self) -> bool {
        matches!(self, Self::FastForward | Self::AlreadyPublished)
    }
}

/// Git facts witnessed for one publish decision.
///
/// Every field is an observation, never a prediction: the caller reads the two
/// branch heads, the durable gate proof, and the two sides of the divergence
/// before the fold runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishFacts {
    /// The published line — `main` under the default configuration.
    pub base_branch: String,
    /// The published line's current head.
    pub base_head: String,
    /// The campaign integration branch's current head.
    pub integration_head: String,
    /// The revision a stage or chapter gate proved, when a durable proof
    /// exists at all. `None` is the honest reading of "nothing has been
    /// proven": it withholds exactly like a proof of another revision.
    pub proven_head: Option<String>,
    /// Whether `base_head` is an ancestor of `integration_head`.
    pub base_is_ancestor: bool,
    /// Whether `integration_head` is an ancestor of `base_head`.
    pub integration_is_ancestor: bool,
    /// Paths the published line changed since the two lines forked.
    pub base_paths: BTreeSet<String>,
    /// Paths the integration line changed since the two lines forked.
    pub integration_paths: BTreeSet<String>,
}

/// The decided act, its single revision, and the sentence that explains it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishPlan {
    pub action: PublishAction,
    /// The one revision this plan publishes, present only when it publishes.
    pub sha: Option<String>,
    pub reason: String,
}

impl PublishPlan {
    fn new(action: PublishAction, sha: Option<&str>, reason: impl Into<String>) -> Self {
        Self {
            action,
            sha: sha.map(ToOwned::to_owned),
            reason: reason.into(),
        }
    }
}

/// The paths both lines changed since they forked.
#[must_use]
pub fn contended_paths(facts: &PublishFacts) -> Vec<String> {
    facts
        .base_paths
        .intersection(&facts.integration_paths)
        .cloned()
        .collect()
}

/// Decide what may happen to the base branch, from witnessed facts alone.
///
/// The rule the whole model rests on is the first one: an integration head
/// that is not the gate-proven revision never moves the base branch, whatever
/// else is true of the graph.
#[must_use]
pub fn publish_plan(facts: &PublishFacts) -> PublishPlan {
    let head = facts.integration_head.as_str();
    let base = facts.base_branch.as_str();
    let Some(proven) = facts.proven_head.as_deref() else {
        return PublishPlan::new(
            PublishAction::Withhold,
            None,
            format!("no stage gate has proven integration head {head}"),
        );
    };
    if proven != head {
        return PublishPlan::new(
            PublishAction::Withhold,
            None,
            format!("integration head {head} is not the gate-proven revision {proven}"),
        );
    }
    if facts.base_head == facts.integration_head {
        return PublishPlan::new(
            PublishAction::AlreadyPublished,
            Some(head),
            format!("{base} already names the gate-proven head {head}"),
        );
    }
    if facts.integration_is_ancestor {
        return PublishPlan::new(
            PublishAction::AlreadyPublished,
            Some(head),
            format!("the gate-proven head {head} is already reachable from {base}"),
        );
    }
    if facts.base_is_ancestor {
        return PublishPlan::new(
            PublishAction::FastForward,
            Some(head),
            format!("{base} fast-forwards to the gate-proven head {head}"),
        );
    }
    let contended = contended_paths(facts);
    if contended.is_empty() {
        return PublishPlan::new(
            PublishAction::RebaseAndRegate,
            None,
            format!(
                "{base} carries {} record commit(s) worth of paths the integration line never touched; rebase and re-gate before any fast-forward",
                facts.base_paths.len()
            ),
        );
    }
    PublishPlan::new(
        PublishAction::Withhold,
        None,
        format!(
            "{base} and the integration line both changed {}: {}",
            plural_paths(contended.len()),
            bounded_path_list(&contended)
        ),
    )
}

/// The gate proof a publication rests on: the checkpoint task and the durable
/// ref whose name ends in the revision it proved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PublishProof {
    pub task_id: String,
    pub reference: String,
}

/// The durable record of one machine fast-forward.
///
/// There is one revision here on purpose. The wedge this model deletes was a
/// receipt that could name a proven revision and a published revision and let
/// them differ; a receipt that cannot spell the second one cannot record the
/// divergence either.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PublishReceiptV1 {
    pub schema_version: u32,
    pub campaign: String,
    pub base_branch: String,
    pub sha: String,
    pub proven_by: PublishProof,
    pub actor: String,
    pub written_at: String,
}

impl PublishReceiptV1 {
    /// Build the receipt for one published revision.
    ///
    /// Fails unless the proof reference names that same revision, so no caller
    /// can record a publication the gate did not prove.
    pub fn new(
        campaign: impl Into<String>,
        base_branch: impl Into<String>,
        sha: impl Into<String>,
        proven_by: PublishProof,
        actor: impl Into<String>,
        written_at: impl Into<String>,
    ) -> Result<Self, PublishError> {
        let receipt = Self {
            schema_version: PUBLISH_RECEIPT_SCHEMA_VERSION,
            campaign: campaign.into(),
            base_branch: base_branch.into(),
            sha: sha.into(),
            proven_by,
            actor: actor.into(),
            written_at: written_at.into(),
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> Result<(), PublishError> {
        if !is_object_id(&self.sha) {
            return Err(PublishError::InvalidPublishedRevision);
        }
        if self.base_branch.is_empty() || self.base_branch.chars().any(char::is_whitespace) {
            return Err(PublishError::InvalidBaseBranch);
        }
        if !proof_names_revision(&self.proven_by.reference, &self.sha)
            || self.proven_by.task_id.is_empty()
        {
            return Err(PublishError::UnprovenPublication);
        }
        if self.actor.is_empty() || self.actor.chars().count() > 128 {
            return Err(PublishError::InvalidActor);
        }
        DateTime::parse_from_rfc3339(&self.written_at)
            .map_err(|_| PublishError::InvalidTimestamp)?;
        Ok(())
    }
}

/// The durable ref that records one published revision.
///
/// Named by the revision it published and pointing at it, the ref is
/// idempotent: re-publishing the same head writes the same fact.
pub fn publish_receipt_ref(state_prefix: &str, sha: &str) -> Result<String, PublishError> {
    if !is_object_id(sha) {
        return Err(PublishError::InvalidPublishedRevision);
    }
    Ok(format!("{state_prefix}/publish/{sha}"))
}

/// Whether a gate-proof ref names this revision as the revision it proved.
#[must_use]
pub fn proof_names_revision(reference: &str, sha: &str) -> bool {
    is_object_id(sha)
        && reference
            .strip_suffix(sha)
            .is_some_and(|prefix| prefix.ends_with('/'))
}

#[must_use]
pub fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn plural_paths(count: usize) -> String {
    if count == 1 {
        "1 path".to_owned()
    } else {
        format!("{count} paths")
    }
}

fn bounded_path_list(paths: &[String]) -> String {
    const SHOWN: usize = 4;
    let mut rendered = paths
        .iter()
        .take(SHOWN)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > SHOWN {
        rendered.push_str(&format!(" …and {} more", paths.len() - SHOWN));
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::{
        publish_plan, publish_receipt_ref, PublishAction, PublishError, PublishFacts, PublishProof,
        PublishReceiptV1, PUBLISH_RECEIPT_SCHEMA_VERSION,
    };
    use std::collections::BTreeSet;

    const PROVEN: &str = "1111111111111111111111111111111111111111";
    const OLD_MAIN: &str = "2222222222222222222222222222222222222222";
    const RECORD: &str = "3333333333333333333333333333333333333333";

    fn paths(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    /// `main` behind the integration branch, the head proven: the one case
    /// that moves `main`, and it moves by fast-forward to exactly that head.
    fn ff_facts() -> PublishFacts {
        PublishFacts {
            base_branch: "main".to_owned(),
            base_head: OLD_MAIN.to_owned(),
            integration_head: PROVEN.to_owned(),
            proven_head: Some(PROVEN.to_owned()),
            base_is_ancestor: true,
            integration_is_ancestor: false,
            base_paths: BTreeSet::new(),
            integration_paths: paths(&["crates/tally/src/cli/campaign.rs"]),
        }
    }

    #[test]
    fn a_gate_proven_head_publishes_main_by_fast_forward() {
        let plan = publish_plan(&ff_facts());
        assert_eq!(plan.action, PublishAction::FastForward);
        assert!(plan.action.publishes());
        assert_eq!(plan.sha.as_deref(), Some(PROVEN));
        assert!(plan.reason.contains("fast-forwards"), "{}", plan.reason);
    }

    #[test]
    fn an_unproven_head_never_publishes_main() {
        let mut unproven = ff_facts();
        unproven.proven_head = None;
        let plan = publish_plan(&unproven);
        assert_eq!(plan.action, PublishAction::Withhold);
        assert!(!plan.action.publishes());
        assert_eq!(plan.sha, None);

        // A proof of an earlier revision is not a proof of this head: the
        // integration branch moved after the gate ran.
        let mut stale = ff_facts();
        stale.proven_head = Some(OLD_MAIN.to_owned());
        let plan = publish_plan(&stale);
        assert_eq!(plan.action, PublishAction::Withhold);
        assert_eq!(plan.sha, None);
        assert!(
            plan.reason.contains("is not the gate-proven"),
            "{}",
            plan.reason
        );
    }

    #[test]
    fn a_published_head_stays_published_without_a_second_act() {
        let mut settled = ff_facts();
        settled.base_head = PROVEN.to_owned();
        settled.integration_is_ancestor = true;
        let plan = publish_plan(&settled);
        assert_eq!(plan.action, PublishAction::AlreadyPublished);
        assert!(plan.action.publishes());
        assert_eq!(plan.sha.as_deref(), Some(PROVEN));

        // `main` carrying later commits still contains the proven head.
        let mut ahead = ff_facts();
        ahead.base_head = RECORD.to_owned();
        ahead.base_is_ancestor = false;
        ahead.integration_is_ancestor = true;
        assert_eq!(publish_plan(&ahead).action, PublishAction::AlreadyPublished);
    }

    #[test]
    fn an_operator_record_commit_publishes_only_after_a_rebase_and_a_re_gate() {
        let mut diverged = ff_facts();
        diverged.base_head = RECORD.to_owned();
        diverged.base_is_ancestor = false;
        diverged.base_paths = paths(&["AUG17-RUN.md"]);
        let plan = publish_plan(&diverged);
        assert_eq!(plan.action, PublishAction::RebaseAndRegate);
        assert!(!plan.action.publishes(), "a rebase publishes nothing");
        // The rebased head is a different commit, so this act publishes no
        // revision at all: the re-gate decides the next one.
        assert_eq!(plan.sha, None);
        assert!(plan.reason.contains("re-gate"), "{}", plan.reason);
    }

    #[test]
    fn a_contended_divergence_publishes_nothing_and_names_the_paths() {
        let mut contended = ff_facts();
        contended.base_head = RECORD.to_owned();
        contended.base_is_ancestor = false;
        contended.base_paths = paths(&["crates/tally/src/cli/campaign.rs", "README.md"]);
        let plan = publish_plan(&contended);
        assert_eq!(plan.action, PublishAction::Withhold);
        assert_eq!(plan.sha, None);
        assert!(
            plan.reason.contains("crates/tally/src/cli/campaign.rs"),
            "{}",
            plan.reason
        );
    }

    #[test]
    fn a_publish_receipt_records_one_sha_and_the_proof_that_named_it() {
        let reference = format!("refs/tally/spec-build/v1/eta-1/checkpoint/gate-c2-abc/{PROVEN}");
        let receipt = PublishReceiptV1::new(
            "eta",
            "main",
            PROVEN,
            PublishProof {
                task_id: "chapter-gate-c2".to_owned(),
                reference: reference.clone(),
            },
            "spec-build-driver",
            "2026-08-17T12:00:00Z",
        )
        .unwrap();
        assert_eq!(receipt.schema_version, PUBLISH_RECEIPT_SCHEMA_VERSION);
        assert_eq!(receipt.sha, PROVEN);
        let encoded = serde_json::to_value(&receipt).unwrap();
        // One revision on the record: the proof and the publication are the
        // same commit or the receipt does not exist.
        assert_eq!(encoded["sha"], PROVEN);
        assert_eq!(
            encoded
                .as_object()
                .unwrap()
                .keys()
                .filter(|key| encoded[key.as_str()] == PROVEN)
                .count(),
            1
        );

        // A proof of another revision cannot be recorded as this one's.
        assert_eq!(
            PublishReceiptV1::new(
                "eta",
                "main",
                PROVEN,
                PublishProof {
                    task_id: "chapter-gate-c2".to_owned(),
                    reference: reference.replace(PROVEN, OLD_MAIN),
                },
                "spec-build-driver",
                "2026-08-17T12:00:00Z",
            ),
            Err(PublishError::UnprovenPublication)
        );
    }

    #[test]
    fn a_publish_receipt_ref_is_named_by_the_revision_it_published() {
        assert_eq!(
            publish_receipt_ref("refs/tally/spec-build/v1/eta-1", PROVEN).unwrap(),
            format!("refs/tally/spec-build/v1/eta-1/publish/{PROVEN}")
        );
        assert_eq!(
            publish_receipt_ref("refs/tally/spec-build/v1/eta-1", "not-a-revision"),
            Err(PublishError::InvalidPublishedRevision)
        );
    }
}
