//! Pure projections shared by campaign reconciliation and release rendering.
//!
//! These folds intentionally mirror the deterministic implementations in the
//! spec-build driver. They accept already-witnessed facts and perform no I/O,
//! so both the Rust driver and release surfaces can share one implementation.
//! Their digest and rendering values are derived projections; the registration,
//! attempt/steering receipts, and repository receipts they consume are
//! canonical.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

pub const CAMPAIGN_DIGEST_SCHEMA_VERSION: u32 = 1;
pub const MAX_SUMMARY_ROWS: usize = 40;
pub const TALLY_TASK_PREFIX: &str = "Tally-Task:";
pub const TALLY_REVISION_PREFIX: &str = "Tally-Revision:";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CampaignFoldError {
    #[error("completion trailer task must be a safe task ID")]
    UnsafeCompletionTaskId,
    #[error("completion trailer revision must be a lowercase SHA-256 identity")]
    InvalidCompletionRevision,
    #[error("campaign summary scope must be a lowercase SHA-256 identity")]
    InvalidSummaryScope,
    #[error("campaign summary outcome must be complete or quiescent")]
    InvalidSummaryOutcome,
}

/// Worklist identity carried through a reconciliation and its digest.
///
/// The named fields are the driver's current contract. Flattening additional
/// fields keeps the fold's copy-through behavior if the source record grows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CampaignSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub sha256: String,
    pub revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciledTask {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergedFact {
    pub task_id: String,
    pub pull_request: String,
    pub merge_commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointFact {
    pub task_id: String,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedFact {
    pub task_id: String,
    pub blocked_by: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosisFact {
    pub task_id: String,
    pub attempt: u64,
    pub diagnosis: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryFact {
    pub task_id: String,
    pub attempt: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferralFact {
    pub task_id: String,
}

/// Already-reconciled campaign facts consumed by [`campaign_digest`].
///
/// Deserialization deliberately permits unrelated reconciliation fields. The
/// Python input also contains scheduler state such as `frontier` and
/// `quiescent`; this fold projects only the terminal receipt surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CampaignReconciliation {
    pub campaign: String,
    pub repository: String,
    pub source: CampaignSource,
    pub base_revision: String,
    pub tasks: Vec<ReconciledTask>,
    pub merged: Vec<MergedFact>,
    pub checkpoints: Vec<CheckpointFact>,
    pub remaining: Vec<String>,
    pub diagnoses: Vec<DiagnosisFact>,
    pub retries: Vec<RetryFact>,
    pub deferrals: Vec<DeferralFact>,
    pub blocked: Vec<BlockedFact>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DigestMergedTask {
    pub task_id: String,
    pub title: String,
    pub pull_request: String,
    pub merge_commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DigestCheckpoint {
    pub task_id: String,
    pub title: String,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DigestBlockedTask {
    pub task_id: String,
    pub title: String,
    pub blocked_by: Vec<String>,
    pub attempts: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DigestAttemptSummary {
    pub task_id: String,
    pub attempt: u64,
    pub summary: String,
}

/// Stable, serializable projection of a campaign's terminal reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CampaignDigest {
    pub schema_version: u32,
    pub campaign: String,
    pub repository: String,
    pub outcome: String,
    pub source: CampaignSource,
    pub base_revision: String,
    pub task_count: usize,
    pub merged: Vec<DigestMergedTask>,
    pub checkpoints: Vec<DigestCheckpoint>,
    pub blocked: Vec<DigestBlockedTask>,
    pub outstanding: Vec<String>,
    pub steering: Vec<DigestAttemptSummary>,
    pub retries: Vec<DigestAttemptSummary>,
    pub deferrals: Vec<String>,
    pub warnings: Vec<String>,
}

/// Return the stable local publication branch for one campaign task.
pub fn stable_publish_branch(
    campaign: &str,
    campaign_id: &str,
    task_id: &str,
    revision: Option<&str>,
) -> String {
    let campaign = safe_slug(campaign, 32);
    let campaign_id = safe_slug(campaign_id, 64);
    let suffix = revision.map_or_else(String::new, |revision| {
        let revision = revision.strip_prefix("sha256:").unwrap_or(revision);
        format!("-{}", revision.chars().take(16).collect::<String>())
    });
    format!("tally/{campaign}-campaign-{campaign_id}/{task_id}{suffix}")
}

/// Return the durable closing-summary ref for one admitted campaign graph.
///
/// The campaign/issue prefix deliberately remains stable for merges,
/// checkpoints, and receipts. Inserting the admitted digest immediately before
/// `summary` gives each graph its own terminal-outcome namespace while keeping
/// the longstanding `/summary/{complete,quiescent}` suffix.
pub fn stage_scoped_summary_ref(
    state_prefix: &str,
    admitted_digest: &str,
    outcome: &str,
) -> Result<String, CampaignFoldError> {
    if !is_sha256_identity(admitted_digest) {
        return Err(CampaignFoldError::InvalidSummaryScope);
    }
    if !matches!(outcome, "complete" | "quiescent") {
        return Err(CampaignFoldError::InvalidSummaryOutcome);
    }
    let digest = admitted_digest
        .strip_prefix("sha256:")
        .expect("validated SHA-256 identity has its prefix");
    Ok(format!("{state_prefix}/{digest}/summary/{outcome}"))
}

/// Render the node-owned completion trailers for a campaign task commit.
pub fn completion_trailer_block(
    task_id: &str,
    revision: &str,
) -> Result<String, CampaignFoldError> {
    if !is_safe_task_id(task_id) {
        return Err(CampaignFoldError::UnsafeCompletionTaskId);
    }
    if !is_sha256_identity(revision) {
        return Err(CampaignFoldError::InvalidCompletionRevision);
    }
    Ok(format!(
        "{TALLY_TASK_PREFIX} {task_id}\n{TALLY_REVISION_PREFIX} {revision}"
    ))
}

/// Fold witnessed reconciliation facts into the stable campaign digest.
pub fn campaign_digest(
    reconciliation: &CampaignReconciliation,
    outcome: impl Into<String>,
) -> CampaignDigest {
    let titles: BTreeMap<&str, &str> = reconciliation
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task.title.as_str()))
        .collect();
    let merged_ids: BTreeSet<&str> = reconciliation
        .merged
        .iter()
        .map(|fact| fact.task_id.as_str())
        .collect();
    let checkpoint_ids: BTreeSet<&str> = reconciliation
        .checkpoints
        .iter()
        .map(|fact| fact.task_id.as_str())
        .collect();
    let blocked: BTreeMap<&str, &[String]> = reconciliation
        .blocked
        .iter()
        .map(|fact| (fact.task_id.as_str(), fact.blocked_by.as_slice()))
        .collect();
    let mut attempts = BTreeMap::<&str, u64>::new();
    for diagnosis in &reconciliation.diagnoses {
        attempts
            .entry(diagnosis.task_id.as_str())
            .and_modify(|attempt| *attempt = (*attempt).max(diagnosis.attempt))
            .or_insert(diagnosis.attempt);
    }
    let title = |task_id: &str| titles.get(task_id).copied().unwrap_or(task_id).to_owned();

    CampaignDigest {
        schema_version: CAMPAIGN_DIGEST_SCHEMA_VERSION,
        campaign: reconciliation.campaign.clone(),
        repository: reconciliation.repository.clone(),
        outcome: outcome.into(),
        source: reconciliation.source.clone(),
        base_revision: reconciliation.base_revision.clone(),
        task_count: reconciliation.tasks.len(),
        merged: reconciliation
            .merged
            .iter()
            .map(|fact| DigestMergedTask {
                task_id: fact.task_id.clone(),
                title: title(&fact.task_id),
                pull_request: fact.pull_request.clone(),
                merge_commit: fact.merge_commit.clone(),
            })
            .collect(),
        checkpoints: reconciliation
            .checkpoints
            .iter()
            .map(|fact| DigestCheckpoint {
                task_id: fact.task_id.clone(),
                title: title(&fact.task_id),
                revision: fact.revision.clone(),
            })
            .collect(),
        blocked: reconciliation
            .remaining
            .iter()
            .filter_map(|task_id| {
                blocked
                    .get(task_id.as_str())
                    .map(|blocked_by| DigestBlockedTask {
                        task_id: task_id.clone(),
                        title: title(task_id),
                        blocked_by: blocked_by.to_vec(),
                        attempts: attempts.get(task_id.as_str()).copied().unwrap_or(0),
                    })
            })
            .collect(),
        outstanding: reconciliation
            .remaining
            .iter()
            .filter(|task_id| {
                !blocked.contains_key(task_id.as_str())
                    && !merged_ids.contains(task_id.as_str())
                    && !checkpoint_ids.contains(task_id.as_str())
            })
            .cloned()
            .collect(),
        steering: reconciliation
            .diagnoses
            .iter()
            .map(|diagnosis| DigestAttemptSummary {
                task_id: diagnosis.task_id.clone(),
                attempt: diagnosis.attempt,
                summary: compact_summary(&diagnosis.diagnosis, 160),
            })
            .collect(),
        retries: reconciliation
            .retries
            .iter()
            .map(|retry| DigestAttemptSummary {
                task_id: retry.task_id.clone(),
                attempt: retry.attempt,
                summary: compact_summary(&retry.reason, 160),
            })
            .collect(),
        deferrals: reconciliation
            .deferrals
            .iter()
            .map(|deferral| deferral.task_id.clone())
            .collect(),
        warnings: reconciliation.warnings.clone(),
    }
}

/// Render the bounded Markdown closing summary for a campaign digest.
pub fn render_campaign_summary(digest: &CampaignDigest) -> String {
    let complete = digest.outcome == "complete";
    let heading = if complete {
        "### Campaign complete"
    } else {
        "### Campaign closed at frontier quiescence"
    };
    let settled = digest.merged.len() + digest.checkpoints.len();
    let outcome_sentence = format!(
        "Settled {settled} of {} task(s) against durable merge/checkpoint facts.",
        digest.task_count
    );
    let provenance_sentence = match digest.source.repository.as_deref() {
        None => format!(
            "Worklist `{}` at `{}`.",
            digest.source.sha256, digest.source.revision
        ),
        Some(repository) => format!(
            "Worklist `{}` at `{}` in `{repository}`; code base `{}` in `{}`.",
            digest.source.sha256, digest.source.revision, digest.base_revision, digest.repository
        ),
    };
    let mut lines = vec![
        heading.to_owned(),
        String::new(),
        outcome_sentence,
        provenance_sentence,
        String::new(),
    ];

    if !complete {
        lines.extend([
            format!(
                "Blocked: {} · Outstanding: {} · Steering notes issued: {} · Machinery retries: {}",
                digest.blocked.len(),
                digest.outstanding.len(),
                digest.steering.len(),
                digest.retries.len()
            ),
            String::new(),
        ]);
    }
    if !digest.merged.is_empty() {
        lines.extend(["#### Merged".to_owned(), String::new()]);
        extend_summary_rows(&mut lines, &digest.merged, MAX_SUMMARY_ROWS, |fact| {
            format!(
                "- `{}` — {} ({})",
                fact.task_id,
                compact_summary(&fact.title, 80),
                fact.pull_request
            )
        });
        lines.push(String::new());
    }
    if !digest.checkpoints.is_empty() {
        lines.extend(["#### Checkpoints passed".to_owned(), String::new()]);
        extend_summary_rows(&mut lines, &digest.checkpoints, MAX_SUMMARY_ROWS, |fact| {
            format!(
                "- `{}` — {} at `{}`",
                fact.task_id,
                compact_summary(&fact.title, 80),
                fact.revision
            )
        });
        lines.push(String::new());
    }
    if !digest.blocked.is_empty() {
        lines.extend(["#### Blocked".to_owned(), String::new()]);
        extend_summary_rows(&mut lines, &digest.blocked, MAX_SUMMARY_ROWS, |fact| {
            let blocked_by = fact
                .blocked_by
                .iter()
                .map(|item| format!("`{item}`"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "- `{}` — {}; blocked by {}; {} steered attempt(s)",
                fact.task_id,
                compact_summary(&fact.title, 80),
                blocked_by,
                fact.attempts
            )
        });
        lines.push(String::new());
    }
    if !digest.outstanding.is_empty() {
        lines.extend(["#### Not attempted".to_owned(), String::new()]);
        extend_summary_rows(
            &mut lines,
            &digest.outstanding,
            MAX_SUMMARY_ROWS,
            |task_id| format!("- `{task_id}`"),
        );
        lines.push(String::new());
    }
    if !digest.deferrals.is_empty() {
        lines.extend([
            "#### Checkpoints deferred by outstanding work".to_owned(),
            String::new(),
        ]);
        extend_summary_rows(&mut lines, &digest.deferrals, MAX_SUMMARY_ROWS, |task_id| {
            format!("- `{task_id}`")
        });
        lines.push(String::new());
    }
    if !digest.steering.is_empty() {
        lines.extend(["#### Steering notes issued".to_owned(), String::new()]);
        extend_summary_rows(&mut lines, &digest.steering, MAX_SUMMARY_ROWS, |note| {
            format!(
                "- `{}` attempt {}: {}",
                note.task_id, note.attempt, note.summary
            )
        });
        lines.push(String::new());
    }
    if !digest.retries.is_empty() {
        lines.extend(["#### Campaign machinery faults".to_owned(), String::new()]);
        extend_summary_rows(&mut lines, &digest.retries, MAX_SUMMARY_ROWS, |retry| {
            format!(
                "- `{}` fault {}: {}",
                retry.task_id, retry.attempt, retry.summary
            )
        });
        lines.push(String::new());
    }
    if !digest.warnings.is_empty() {
        lines.extend(["#### Reconciler warnings".to_owned(), String::new()]);
        extend_summary_rows(&mut lines, &digest.warnings, 12, |warning| {
            format!("- {}", compact_summary(warning, 200))
        });
        lines.push(String::new());
    }

    let mut rendered = lines.join("\n").trim_end().to_owned();
    rendered.push('\n');
    rendered
}

fn safe_slug(value: &str, maximum: usize) -> String {
    let mut slug = String::new();
    let mut in_invalid_run = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            slug.push(character);
            in_invalid_run = false;
        } else if !in_invalid_run {
            slug.push('-');
            in_invalid_run = true;
        }
    }
    let slug = slug.trim_matches(|character| matches!(character, '.' | '-'));
    let slug = if slug.is_empty() { "campaign" } else { slug };
    slug.chars().take(maximum).collect()
}

fn is_safe_task_id(task_id: &str) -> bool {
    let bytes = task_id.as_bytes();
    let safe_edge = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    !bytes.is_empty()
        && safe_edge(bytes[0])
        && safe_edge(bytes[bytes.len() - 1])
        && bytes.iter().all(|byte| safe_edge(*byte) || *byte == b'-')
}

fn is_sha256_identity(revision: &str) -> bool {
    revision.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn compact_summary(value: &str, maximum: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= maximum {
        compact
    } else {
        let mut truncated = compact
            .chars()
            .take(maximum.saturating_sub(3))
            .collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

fn extend_summary_rows<T>(
    lines: &mut Vec<String>,
    rows: &[T],
    limit: usize,
    mut render: impl FnMut(&T) -> String,
) {
    lines.extend(rows.iter().take(limit).map(&mut render));
    if rows.len() > limit {
        lines.push(format!("- …and {} more", rows.len() - limit));
    }
}

#[cfg(test)]
mod tests {
    use super::{stage_scoped_summary_ref, CampaignFoldError};

    #[test]
    fn stage_scoped_summary_ref_uses_the_full_admitted_digest() {
        let digest = format!("sha256:{}", "a".repeat(64));
        assert_eq!(
            stage_scoped_summary_ref("refs/tally/spec-build/v1/campaign", &digest, "complete")
                .unwrap(),
            format!(
                "refs/tally/spec-build/v1/campaign/{}/summary/complete",
                "a".repeat(64)
            )
        );
        assert_eq!(
            stage_scoped_summary_ref(
                "refs/tally/spec-build/v1/campaign",
                "sha256:not-a-digest",
                "complete"
            ),
            Err(CampaignFoldError::InvalidSummaryScope)
        );
        assert_eq!(
            stage_scoped_summary_ref("refs/tally/spec-build/v1/campaign", &digest, "archived"),
            Err(CampaignFoldError::InvalidSummaryOutcome)
        );
    }
}
