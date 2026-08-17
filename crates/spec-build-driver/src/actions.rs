use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, Duration as ChronoDuration, FixedOffset, SecondsFormat, Utc};
use regex::Regex;
use tally_core::adapters::AdapterConfig;
use tally_core::attempt_receipts::{
    is_sha256_identity, validate_attempt_receipt_stamp, AttemptReceiptAuthorityV1,
    ATTEMPT_RECEIPT_AUTHORITY_FILE, ATTEMPT_RECEIPT_MACHINE_ACTOR, ATTEMPT_RECEIPT_SCHEMA_VERSION,
    LEGACY_ATTEMPT_RECEIPT_SCHEMA_VERSION, MAX_TASK_LIFETIME_ATTEMPTS,
};
use tally_core::campaign_contract::task_input_epoch;
use tally_core::campaign_folds::{
    campaign_digest as fold_campaign_digest, render_campaign_summary, stable_publish_branch,
    stage_scoped_summary_ref, CampaignReconciliation,
};
use uuid::Uuid;

use crate::adapter_outcome::{self, LaneOutcome};
use crate::error::{DriverError, Result};
use crate::git::{git, git_with_input};
use crate::json::{self, Json};
use crate::path::{is_symlink, resolve};
use crate::sha256;
use crate::worktrees::{self, Identity};

const COMMIT_SUBJECT_MAX: usize = 200;
const COMMIT_BODY_MAX: usize = 4_000;
const COMMIT_HEADER_MAX: usize = 72;
const COMMIT_BODY_LINE_MAX: usize = 100;
const COMMIT_REASON_MAX: usize = 200;
const OUTCOME_FIRST_LEAD_MAX: usize = 240;
const MAX_CAMPAIGN_TASKS: usize = 128;
const MAX_DIFF_CHARS: usize = 128 * 1024;
const MAX_ATTEMPT_RECEIPTS_LOG_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ATTEMPT_RECEIPT_AUTHORITY_BYTES: u64 = 64 * 1024;
const MAX_DIAGNOSIS_CHARS: usize = 12_000;
const MAX_RETRY_CHARS: usize = 2_000;
const MAX_MACHINE_RETRIES: usize = 2;
// The diagnosis slot's peepholes, each a derivation over the slot it feeds
// (vestige-sweep V-5) — the bare 8 KiB / 10-line / 8 KiB numerals these
// replaced sized the verdict that gates attempt 2 through a 10-line window
// of up-to-3-hour gate runs.
//
// One worker's findings may fill at most two thirds of the diagnosis slot
// the findings inform; the rest of the slot stays room for the diagnosis
// prose itself. The slot counts chars and this bound counts bytes, so any
// multibyte text surviving redaction fits with room to spare.
const MAX_WORKER_FINDINGS_BYTES: usize = MAX_DIAGNOSIS_CHARS * 2 / 3;
const MAX_CONTINUATION_EVENT_BYTES: usize = 1024 * 1024;
// One stored checkpoint-capture stream may fill the diagnosis slot it
// feeds: the capture is the evidence one diagnosis reads.
const CHECKPOINT_CAPTURE_MAX_BYTES: usize = MAX_DIAGNOSIS_CHARS;
// The checkpoint note reserves one third of the diagnosis slot for the
// capture's error-aware stderr excerpt and leaves the rest to the
// diagnosis prose the note is appended to.
const CHECKPOINT_STDERR_WINDOW_CHARS: usize = MAX_DIAGNOSIS_CHARS / 3;
const ATTEMPT_RECEIPTS_FILE: &str = "attempt-receipts-v1.jsonl";
const PUBLIC_REDACTION: &str = "conservative-v2";
const PUBLIC_DIAGNOSIS_TRUNCATION: &str = "\n[... diagnosis truncated after redaction ...]";
const WORKER_FINDINGS_TRUNCATION: &str = "\n[... worker findings truncated after redaction ...]";
const CHECKPOINT_CAPTURE_FILE: &str = "checkpoint.json";
const TALLY_TASK_PREFIX: &str = "Tally-Task:";
const TALLY_REVISION_PREFIX: &str = "Tally-Revision:";
const ASSISTED_BY_PREFIX: &str = "Assisted-by:";
const BRIEF_SENTINEL: &str = "Read the file whose path is in the TALLY_BRIEF environment variable and execute the mission it contains. That brief is your complete instruction set.";
const LIVE_JOB_STATES: [&str; 3] = ["paused", "queued", "running"];

#[cfg(target_os = "linux")]
const O_CLOEXEC: i32 = 0o2000000;
#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0o400000;
#[cfg(target_os = "linux")]
const O_DIRECTORY: i32 = 0o200000;
#[cfg(target_os = "linux")]
const O_NONBLOCK: i32 = 0o4000;
const LOCK_SH: i32 = 1;
const LOCK_EX: i32 = 2;
const LOCK_UN: i32 = 8;

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[derive(Clone, Debug)]
struct RepoConfig {
    checkout: PathBuf,
    base_branch: String,
    remote: String,
}

#[derive(Clone, Debug)]
struct CampaignCoordinate {
    repository: String,
    config: RepoConfig,
}

#[derive(Clone, Debug)]
struct Workspace {
    task_id: String,
    base_rev: String,
    branch: String,
    publish_branch: String,
    worktree: PathBuf,
}

#[derive(Clone, Debug)]
struct Ownership {
    task_id: String,
    domains_required: bool,
    conflict_domains: Option<Vec<String>>,
    owned_paths: Vec<String>,
    base_rev: String,
    head: String,
}

#[derive(Clone, Debug)]
struct Publication {
    task_id: String,
    branch: String,
    head: String,
    pull_request: String,
    narration: Json,
    ownership: Ownership,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommitMessage {
    subject: String,
    body: String,
}

#[derive(Clone, Debug)]
struct Constraint {
    gate_id: String,
    patterns: Vec<String>,
    checked_paths: usize,
    base_rev: String,
    head: String,
}

pub(crate) fn load_brief() -> Result<Json> {
    let path =
        env::var_os("TALLY_BRIEF").ok_or_else(|| DriverError::new("TALLY_BRIEF is required"))?;
    let path = PathBuf::from(path);
    if !path.is_absolute() || !path.is_file() {
        return Err(DriverError::new(
            "TALLY_BRIEF must name an absolute regular file",
        ));
    }
    let text = fs::read_to_string(&path)
        .map_err(|error| DriverError::new(format!("cannot read TALLY_BRIEF: {error}")))?;
    let value = json::parse(&text)
        .map_err(|error| DriverError::new(format!("cannot read TALLY_BRIEF: {error}")))?;
    if value.as_object().is_none() {
        return Err(DriverError::new("TALLY_BRIEF must contain an object"));
    }
    Ok(value)
}

pub(crate) fn dispatch(action: &str, brief: &Json) -> Result<Json> {
    match action {
        "worklist" => action_worklist(brief),
        "sweep" => action_sweep(brief),
        "reconcile" => action_reconcile(brief),
        "diff" => action_diff(brief),
        "outcome" => action_worker_outcome(brief),
        "steeringRecheck" => action_steering_recheck(brief),
        "steer" => action_steer(brief),
        "retry" => action_retry(brief),
        "escalate" => action_escalate(brief),
        "continue" => action_continue(brief),
        "preflight" => action_preflight(brief),
        "prep" => action_prep(brief),
        "ownership" => action_ownership(brief),
        "treeDelta" => action_tree_delta(brief),
        "constraint" => action_constraint(brief),
        "checkpoint" => action_checkpoint(brief),
        "publish" => action_publish(brief),
        "rebase" => action_rebase(brief),
        "merge" => action_merge(brief),
        "cleanup" => action_cleanup(brief),
        _ => Err(DriverError::new(format!(
            "action {action:?} has no native handler"
        ))),
    }
}

fn object_exact<'a>(
    value: &'a Json,
    fields: &[&str],
    context: &str,
) -> Result<&'a BTreeMap<String, Json>> {
    let object = value
        .as_object()
        .ok_or_else(|| DriverError::new(format!("{context} must be an object")))?;
    let allowed: BTreeSet<_> = fields.iter().copied().collect();
    let unknown: Vec<_> = object
        .keys()
        .filter(|field| !allowed.contains(field.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(DriverError::new(format!(
            "{context} has unknown fields: {}",
            unknown.join(", ")
        )));
    }
    Ok(object)
}

fn object_complete<'a>(
    value: &'a Json,
    fields: &[&str],
    context: &str,
) -> Result<&'a BTreeMap<String, Json>> {
    let object = object_exact(value, fields, context)?;
    let missing: Vec<_> = fields
        .iter()
        .filter(|field| !object.contains_key(**field))
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(DriverError::new(format!(
            "{context} is missing canonical fields: {}",
            missing.join(", ")
        )));
    }
    Ok(object)
}

fn member<'a>(object: &'a BTreeMap<String, Json>, key: &str) -> Option<&'a Json> {
    object.get(key)
}

fn required_string(value: Option<&Json>, context: &str, maximum: Option<usize>) -> Result<String> {
    let value = value.and_then(Json::as_str).ok_or_else(|| {
        DriverError::new(format!(
            "{context} must be a non-empty string without control characters"
        ))
    })?;
    if value.is_empty() || value.chars().any(|character| (character as u32) < 32) {
        return Err(DriverError::new(format!(
            "{context} must be a non-empty string without control characters"
        )));
    }
    if let Some(maximum) = maximum {
        if value.chars().count() > maximum {
            return Err(DriverError::new(format!(
                "{context} exceeds {maximum} characters"
            )));
        }
    }
    Ok(value.to_owned())
}

fn required_bool(value: Option<&Json>, context: &str) -> Result<bool> {
    value
        .and_then(Json::as_bool)
        .ok_or_else(|| DriverError::new(format!("{context} must be boolean")))
}

fn is_full_oid(value: &str) -> bool {
    (40..=64).contains(&value.len())
        && value
            .bytes()
            .all(|character| character.is_ascii_digit() || (b'a'..=b'f').contains(&character))
}

fn full_oid(value: Option<&Json>, context: &str) -> Result<String> {
    let value = required_string(value, context, None)?;
    if !is_full_oid(&value) {
        return Err(DriverError::new(format!(
            "{context} must be a full Git object ID"
        )));
    }
    Ok(value)
}

fn string_list(value: Option<&Json>, context: &str, nonempty: bool) -> Result<Vec<String>> {
    let values = value.and_then(Json::as_array).ok_or_else(|| {
        DriverError::new(format!(
            "{context} must be {} array",
            if nonempty { "a non-empty" } else { "an" }
        ))
    })?;
    if nonempty && values.is_empty() {
        return Err(DriverError::new(format!(
            "{context} must be a non-empty array"
        )));
    }
    values
        .iter()
        .enumerate()
        .map(|(index, value)| required_string(Some(value), &format!("{context}[{index}]"), None))
        .collect()
}

fn is_task_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes[0].is_ascii_lowercase_or_digit()
        && bytes[bytes.len() - 1].is_ascii_lowercase_or_digit()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase_or_digit() || *byte == b'-')
}

trait AsciiTaskCharacter {
    fn is_ascii_lowercase_or_digit(&self) -> bool;
}

impl AsciiTaskCharacter for u8 {
    fn is_ascii_lowercase_or_digit(&self) -> bool {
        self.is_ascii_lowercase() || self.is_ascii_digit()
    }
}

fn is_component(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && (bytes[0].is_ascii_alphanumeric() || bytes[0] == b'_')
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'.' | b'-'))
}

fn is_repository(value: &str) -> bool {
    let mut pieces = value.split('/');
    let owner = pieces.next().unwrap_or_default();
    let name = pieces.next().unwrap_or_default();
    !owner.is_empty()
        && !name.is_empty()
        && pieces.next().is_none()
        && !owner.bytes().any(|byte| matches!(byte, b' ' | b'\t'))
        && !name.bytes().any(|byte| matches!(byte, b' ' | b'\t'))
}

fn normalize_paths(
    value: Option<&Json>,
    context: &str,
    required: bool,
) -> Result<Option<Vec<String>>> {
    let Some(value) = value else {
        if required {
            return Err(DriverError::new(format!(
                "{context} must be a non-empty array"
            )));
        }
        return Ok(None);
    };
    let paths = string_list(Some(value), context, required)?;
    let unique: BTreeSet<_> = paths.iter().collect();
    if unique.len() != paths.len() {
        return Err(DriverError::new(format!("{context} contains duplicates")));
    }
    for (index, candidate) in paths.iter().enumerate() {
        let pieces: Vec<_> = candidate.split('/').collect();
        if candidate.starts_with('/')
            || candidate.ends_with('/')
            || candidate.is_empty()
            || candidate == "."
            || pieces
                .iter()
                .any(|piece| piece.is_empty() || *piece == "." || *piece == "..")
        {
            let suffix =
                if candidate.starts_with('/') || candidate.ends_with('/') || pieces.contains(&"..")
                {
                    " without '..'"
                } else {
                    ""
                };
            return Err(DriverError::new(format!(
                "{context}[{index}] must be a normalized relative path{suffix}"
            )));
        }
    }
    Ok(Some(paths))
}

fn repo_config(value: Option<&Json>) -> Result<RepoConfig> {
    let value = value.ok_or_else(|| DriverError::new("repositoryConfig must be an object"))?;
    let object = object_exact(
        value,
        &["checkout", "baseBranch", "remote", "forge"],
        "repositoryConfig",
    )?;
    let checkout = PathBuf::from(required_string(
        member(object, "checkout"),
        "repositoryConfig.checkout",
        None,
    )?);
    if !checkout.is_absolute() || !checkout.is_dir() {
        return Err(DriverError::new(
            "repositoryConfig.checkout must be an absolute directory",
        ));
    }
    let base_branch = required_string(
        member(object, "baseBranch"),
        "repositoryConfig.baseBranch",
        None,
    )?;
    let remote = required_string(member(object, "remote"), "repositoryConfig.remote", None)?;
    if member(object, "forge").and_then(Json::as_str) != Some("local") {
        return Err(DriverError::new("repositoryConfig.forge must be local"));
    }
    git(&checkout, ["rev-parse", "--git-dir"], true)?;
    Ok(RepoConfig {
        checkout,
        base_branch,
        remote,
    })
}

fn campaign_issue(value: Option<&Json>) -> Result<(String, String)> {
    let value = value.ok_or_else(|| DriverError::new("issue must be an object"))?;
    let issue = object_exact(value, &["number", "url"], "issue")?;
    let number = required_string(member(issue, "number"), "issue.number", None)?;
    if number.starts_with('0') || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(DriverError::new(
            "issue.number must be a positive decimal string",
        ));
    }
    let url = required_string(member(issue, "url"), "issue.url", None)?;
    Ok((number, url))
}

fn safe_slug(value: &str, maximum: usize) -> String {
    let mut slug = String::new();
    let mut invalid = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-') {
            if invalid && !slug.is_empty() {
                slug.push('-');
            }
            invalid = false;
            slug.push(character);
        } else {
            invalid = true;
        }
    }
    let slug = slug.trim_matches(|character| matches!(character, '.' | '-'));
    let slug = if slug.is_empty() { "campaign" } else { slug };
    slug.chars().take(maximum).collect()
}

fn campaign_identity(data: &BTreeMap<String, Json>, campaign: &str) -> Result<String> {
    if let Some(value) = data.get("campaignIdentity") {
        return required_string(Some(value), "campaignIdentity", Some(128));
    }
    if data.contains_key("issue") {
        return Ok(campaign_issue(data.get("issue"))?.0);
    }
    required_string(
        Some(&Json::String(campaign.to_owned())),
        "campaign",
        Some(128),
    )
}

fn repository_name(value: Option<&Json>, context: &str) -> Result<String> {
    let repository = required_string(value, context, None)?;
    if !is_repository(&repository) {
        return Err(DriverError::new(format!(
            "{context} must use owner/name form"
        )));
    }
    Ok(repository)
}

fn campaign_coordinate(value: Option<&Json>, context: &str) -> Result<CampaignCoordinate> {
    let value = value.ok_or_else(|| DriverError::new(format!("{context} must be an object")))?;
    let object = object_exact(value, &["repository", "repositoryConfig"], context)?;
    Ok(CampaignCoordinate {
        repository: repository_name(object.get("repository"), &format!("{context}.repository"))?,
        config: repo_config(object.get("repositoryConfig"))?,
    })
}

fn campaign_coordinates(
    data: &BTreeMap<String, Json>,
    repository: String,
    config: RepoConfig,
) -> Result<(CampaignCoordinate, CampaignCoordinate, CampaignCoordinate)> {
    let code = CampaignCoordinate { repository, config };
    let spec = match data.get("specRepository") {
        Some(Json::Null) | None => code.clone(),
        value => campaign_coordinate(value, "specRepository")?,
    };
    let issue = match data.get("issueRepository") {
        Some(Json::Null) | None => spec.clone(),
        value => campaign_coordinate(value, "issueRepository")?,
    };
    Ok((code, spec, issue))
}

fn same_repository(left: &CampaignCoordinate, right: &CampaignCoordinate) -> bool {
    left.repository == right.repository && left.config.checkout == right.config.checkout
}

fn positive_u64(value: Option<&Json>, context: &str) -> Result<u64> {
    value
        .and_then(Json::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| DriverError::new(format!("{context} must be a positive integer")))
}

fn argv_list(value: Option<&Json>, context: &str) -> Result<Vec<String>> {
    let arguments = string_list(value, context, true)?;
    if arguments.first().is_none_or(String::is_empty) {
        return Err(DriverError::new(format!(
            "{context} requires a non-empty executable"
        )));
    }
    Ok(arguments)
}

fn normalize_dependencies(
    value: Option<&Json>,
    context: &str,
    prior_ids: &BTreeSet<String>,
) -> Result<Vec<String>> {
    let dependencies = string_list(value, &format!("{context}.dependencies"), false)?;
    if dependencies.iter().collect::<BTreeSet<_>>().len() != dependencies.len() {
        return Err(DriverError::new(format!(
            "{context}.dependencies contains duplicates"
        )));
    }
    let missing: Vec<_> = dependencies
        .iter()
        .filter(|dependency| !prior_ids.contains(*dependency))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(DriverError::new(format!(
            "{context}.dependencies must reference earlier tasks; unavailable: {}",
            missing.join(", ")
        )));
    }
    Ok(dependencies)
}

fn normalize_acceptance(value: Option<&Json>, context: &str) -> Result<Json> {
    let values = value
        .and_then(Json::as_array)
        .filter(|values| !values.is_empty())
        .ok_or_else(|| DriverError::new(format!("{context} must be a non-empty array")))?;
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let item_context = format!("{context}[{index}]");
        let item = object_exact(value, &["id", "description", "argv"], &item_context)?;
        let identifier = required_string(item.get("id"), &format!("{item_context}.id"), Some(80))?;
        if !is_component(&identifier) {
            return Err(DriverError::new(format!(
                "{item_context}.id is not a safe component"
            )));
        }
        if !seen.insert(identifier.clone()) {
            return Err(DriverError::new(format!(
                "{context} repeats id {identifier:?}"
            )));
        }
        normalized.push(Json::object([
            ("id", Json::from(identifier)),
            (
                "description",
                Json::from(required_string(
                    item.get("description"),
                    &format!("{item_context}.description"),
                    Some(4_000),
                )?),
            ),
            (
                "argv",
                Json::Array(
                    argv_list(item.get("argv"), &format!("{item_context}.argv"))?
                        .into_iter()
                        .map(Json::from)
                        .collect(),
                ),
            ),
        ]));
    }
    Ok(Json::Array(normalized))
}

fn normalize_task(
    value: &Json,
    index: usize,
    prior_ids: &BTreeSet<String>,
    require_conflict_domains: bool,
) -> Result<Json> {
    let context = format!("tasks[{index}]");
    let object = value
        .as_object()
        .ok_or_else(|| DriverError::new(format!("{context} must be an object")))?;
    let kind = object.get("kind").and_then(Json::as_str);
    if kind == Some("checkpoint") {
        let task = object_exact(
            value,
            &[
                "id",
                "kind",
                "title",
                "argv",
                "runtimeMaxSec",
                "dependencies",
            ],
            &context,
        )?;
        let identifier = required_string(task.get("id"), &format!("{context}.id"), Some(80))?;
        if !is_task_id(&identifier) {
            return Err(DriverError::new(format!(
                "{context}.id must match ^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$"
            )));
        }
        return Ok(Json::object([
            ("id", Json::from(identifier)),
            ("kind", Json::from("checkpoint")),
            (
                "title",
                Json::from(required_string(
                    task.get("title"),
                    &format!("{context}.title"),
                    Some(300),
                )?),
            ),
            (
                "argv",
                Json::Array(
                    argv_list(task.get("argv"), &format!("{context}.argv"))?
                        .into_iter()
                        .map(Json::from)
                        .collect(),
                ),
            ),
            (
                "runtimeMaxSec",
                Json::Number(
                    positive_u64(
                        task.get("runtimeMaxSec"),
                        &format!("{context}.runtimeMaxSec"),
                    )?
                    .to_string(),
                ),
            ),
            (
                "dependencies",
                Json::Array(
                    normalize_dependencies(task.get("dependencies"), &context, prior_ids)?
                        .into_iter()
                        .map(Json::from)
                        .collect(),
                ),
            ),
        ]));
    }
    if kind != Some("implementation") {
        return Err(DriverError::new(format!(
            "{context}.kind must equal implementation or checkpoint"
        )));
    }
    let task = object_exact(
        value,
        &[
            "id",
            "kind",
            "title",
            "goal",
            "deliveredBehaviors",
            "readFirst",
            "acceptanceCriteria",
            "dependencies",
            "conflictDomains",
        ],
        &context,
    )?;
    let identifier = required_string(task.get("id"), &format!("{context}.id"), Some(80))?;
    if !is_task_id(&identifier) {
        return Err(DriverError::new(format!(
            "{context}.id must match ^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$"
        )));
    }
    let read_first_value = task
        .get("readFirst")
        .ok_or_else(|| DriverError::new(format!("{context}.readFirst must be an object")))?;
    let read_first = object_exact(
        read_first_value,
        &["specSections", "styleReferences"],
        &format!("{context}.readFirst"),
    )?;
    let mut normalized = BTreeMap::from([
        ("id".to_owned(), Json::from(identifier)),
        ("kind".to_owned(), Json::from("implementation")),
        (
            "title".to_owned(),
            Json::from(required_string(
                task.get("title"),
                &format!("{context}.title"),
                Some(300),
            )?),
        ),
        (
            "goal".to_owned(),
            Json::from(required_string(
                task.get("goal"),
                &format!("{context}.goal"),
                Some(12_000),
            )?),
        ),
        (
            "deliveredBehaviors".to_owned(),
            Json::Array(
                string_list(
                    task.get("deliveredBehaviors"),
                    &format!("{context}.deliveredBehaviors"),
                    true,
                )?
                .into_iter()
                .map(Json::from)
                .collect(),
            ),
        ),
        (
            "readFirst".to_owned(),
            Json::object([
                (
                    "specSections",
                    Json::Array(
                        string_list(
                            read_first.get("specSections"),
                            &format!("{context}.readFirst.specSections"),
                            true,
                        )?
                        .into_iter()
                        .map(Json::from)
                        .collect(),
                    ),
                ),
                (
                    "styleReferences",
                    Json::Array(
                        string_list(
                            read_first.get("styleReferences"),
                            &format!("{context}.readFirst.styleReferences"),
                            false,
                        )?
                        .into_iter()
                        .map(Json::from)
                        .collect(),
                    ),
                ),
            ]),
        ),
        (
            "acceptanceCriteria".to_owned(),
            normalize_acceptance(
                task.get("acceptanceCriteria"),
                &format!("{context}.acceptanceCriteria"),
            )?,
        ),
        (
            "dependencies".to_owned(),
            Json::Array(
                normalize_dependencies(task.get("dependencies"), &context, prior_ids)?
                    .into_iter()
                    .map(Json::from)
                    .collect(),
            ),
        ),
    ]);
    if let Some(domains) = normalize_paths(
        task.get("conflictDomains"),
        &format!("{context}.conflictDomains"),
        require_conflict_domains,
    )? {
        normalized.insert(
            "conflictDomains".to_owned(),
            Json::Array(domains.into_iter().map(Json::from).collect()),
        );
    }
    Ok(Json::Object(normalized))
}

fn campaign_string(value: Option<&Json>, context: &str, maximum: Option<usize>) -> Result<String> {
    let value = value.and_then(Json::as_str).ok_or_else(|| {
        DriverError::new(format!(
            "{context} must be a non-empty string without control characters"
        ))
    })?;
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(DriverError::new(format!(
            "{context} must be a non-empty string without control characters"
        )));
    }
    if maximum.is_some_and(|maximum| value.chars().count() > maximum) {
        return Err(DriverError::new(format!(
            "{context} exceeds {} characters",
            maximum.expect("checked above")
        )));
    }
    Ok(value.to_owned())
}

fn campaign_string_list(
    value: Option<&Json>,
    context: &str,
    nonempty: bool,
) -> Result<Vec<String>> {
    let values = value.and_then(Json::as_array).ok_or_else(|| {
        DriverError::new(format!(
            "{context} must be {} array",
            if nonempty { "a non-empty" } else { "an" }
        ))
    })?;
    if nonempty && values.is_empty() {
        return Err(DriverError::new(format!(
            "{context} must be a non-empty array"
        )));
    }
    values
        .iter()
        .enumerate()
        .map(|(index, value)| campaign_string(Some(value), &format!("{context}[{index}]"), None))
        .collect()
}

fn campaign_u64(value: Option<&Json>, context: &str) -> Result<u64> {
    value
        .and_then(Json::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            DriverError::new(format!(
                "{context} must be a positive unsigned 64-bit integer"
            ))
        })
}

fn validate_campaign_agent(value: Option<&Json>, context: &str) -> Result<()> {
    let empty = Json::object(std::iter::empty::<(&str, Json)>());
    let agent = object_exact(
        value.unwrap_or(&empty),
        &[
            "adapter",
            "argv",
            "model",
            "priority",
            "approvalPolicy",
            "sandboxPolicy",
            "diagnosisSandboxPolicy",
            "runtimeMaxSec",
        ],
        context,
    )?;
    campaign_string(
        agent
            .get("adapter")
            .or(Some(&Json::String("codex".to_owned()))),
        &format!("{context}.adapter"),
        None,
    )?;
    let default_argv = Json::Array(vec![Json::from(BRIEF_SENTINEL)]);
    campaign_string_list(
        agent.get("argv").or(Some(&default_argv)),
        &format!("{context}.argv"),
        true,
    )?;
    let default_priority = Json::from("low");
    let priority = campaign_string(
        agent.get("priority").or(Some(&default_priority)),
        &format!("{context}.priority"),
        None,
    )?;
    if !matches!(priority.as_str(), "interrupt" | "high" | "medium" | "low") {
        return Err(DriverError::new(format!("{context}.priority is invalid")));
    }
    // No policy default: the three policy names are adapter vocabulary, and the
    // adapter answers for a worklist that names none. Absent is admissible and
    // renders nothing.
    for (name, default) in [
        ("approvalPolicy", None::<&str>),
        ("sandboxPolicy", None),
        ("diagnosisSandboxPolicy", None),
        ("model", None),
    ] {
        let temporary = default.map(Json::from);
        let candidate = agent.get(name).or(temporary.as_ref());
        match candidate {
            None | Some(Json::Null) => {}
            Some(value) => {
                let maximum = (name == "model").then_some(128);
                campaign_string(Some(value), &format!("{context}.{name}"), maximum)?;
            }
        }
    }
    let default_runtime = Json::Number("14400".to_owned());
    let runtime = agent.get("runtimeMaxSec").or(Some(&default_runtime));
    if !runtime.is_some_and(|value| matches!(value, Json::Null)) {
        campaign_u64(runtime, &format!("{context}.runtimeMaxSec"))?;
    }
    Ok(())
}

fn validate_campaign_gate(value: &Json, context: &str) -> Result<String> {
    let object = value
        .as_object()
        .ok_or_else(|| DriverError::new(format!("{context} must be an object")))?;
    match object.get("kind").and_then(Json::as_str) {
        Some("command") => {
            let gate = object_exact(
                value,
                &["kind", "id", "preflightArgv", "argv", "runtimeMaxSec"],
                context,
            )?;
            let identifier = required_string(gate.get("id"), &format!("{context}.id"), Some(80))?;
            if !is_component(&identifier) {
                return Err(DriverError::new(format!(
                    "{context}.id is not a safe component"
                )));
            }
            campaign_string_list(
                gate.get("preflightArgv"),
                &format!("{context}.preflightArgv"),
                true,
            )?;
            campaign_string_list(gate.get("argv"), &format!("{context}.argv"), true)?;
            let default_runtime = Json::Number("900".to_owned());
            campaign_u64(
                gate.get("runtimeMaxSec").or(Some(&default_runtime)),
                &format!("{context}.runtimeMaxSec"),
            )?;
            Ok(identifier)
        }
        Some("forbidPaths") => {
            let gate = object_exact(
                value,
                &["kind", "id", "forbidPaths", "runtimeMaxSec"],
                context,
            )?;
            let identifier = required_string(gate.get("id"), &format!("{context}.id"), Some(80))?;
            if !is_component(&identifier) {
                return Err(DriverError::new(format!(
                    "{context}.id is not a safe component"
                )));
            }
            let patterns = gate
                .get("forbidPaths")
                .and_then(Json::as_array)
                .filter(|patterns| !patterns.is_empty())
                .ok_or_else(|| {
                    DriverError::new(format!("{context}.forbidPaths must be a non-empty array"))
                })?;
            if patterns.len() > 128 {
                return Err(DriverError::new(format!(
                    "{context}.forbidPaths exceeds 128 entries"
                )));
            }
            let mut seen = BTreeSet::new();
            for (index, pattern) in patterns.iter().enumerate() {
                let Some(pattern) = pattern.as_str() else {
                    return Err(DriverError::new(format!(
                        "{context}.forbidPaths[{index}] is invalid"
                    )));
                };
                let components: Vec<_> = pattern.split('/').collect();
                let invalid = pattern.is_empty()
                    || pattern.chars().count() > 1024
                    || pattern.starts_with('/')
                    || pattern.ends_with('/')
                    || pattern.contains('\0')
                    || components.contains(&"..")
                    || components
                        .iter()
                        .any(|component| component.contains("**") && *component != "**")
                    || !seen.insert(pattern.to_owned());
                if invalid {
                    return Err(DriverError::new(format!(
                        "{context}.forbidPaths[{index}] is invalid"
                    )));
                }
            }
            let default_runtime = Json::Number("900".to_owned());
            campaign_u64(
                gate.get("runtimeMaxSec").or(Some(&default_runtime)),
                &format!("{context}.runtimeMaxSec"),
            )?;
            Ok(identifier)
        }
        _ => Err(DriverError::new(format!(
            "{context}.kind must be command or forbidPaths"
        ))),
    }
}

fn validate_worklist_campaign(
    value: &Json,
    source_path: &str,
    witnessed_max_tasks: usize,
    witnessed_max_parallel: usize,
) -> Result<()> {
    let campaign = object_exact(
        value,
        &[
            "name",
            "maxTasks",
            "maxParallel",
            "mergeMethod",
            "driverRuntimeMaxSec",
            "runtimeMaxSec",
            "agent",
            "steward",
            "stewardArgv",
            "stewardRuntimeMaxSec",
            "gates",
        ],
        "worklist.campaign",
    )?;
    let default_name = Path::new(source_path)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let default_name = Json::from(default_name);
    let name = required_string(
        campaign.get("name").or(Some(&default_name)),
        "worklist.campaign.name",
        Some(80),
    )?;
    if !is_component(&name) {
        return Err(DriverError::new(
            "worklist.campaign.name must be a safe path component",
        ));
    }
    let default_tasks = Json::Number("64".to_owned());
    let max_tasks = campaign
        .get("maxTasks")
        .or(Some(&default_tasks))
        .and_then(Json::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| (1..=MAX_CAMPAIGN_TASKS).contains(value))
        .ok_or_else(|| {
            DriverError::new(format!(
                "worklist.campaign.maxTasks must be in 1..={MAX_CAMPAIGN_TASKS}"
            ))
        })?;
    let default_parallel = Json::Number("1".to_owned());
    let max_parallel = campaign
        .get("maxParallel")
        .or(Some(&default_parallel))
        .and_then(Json::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| (1..=MAX_CAMPAIGN_TASKS).contains(value) && *value <= max_tasks)
        .ok_or_else(|| {
            DriverError::new(format!(
                "worklist.campaign.maxParallel must be in 1..={MAX_CAMPAIGN_TASKS} and not exceed maxTasks"
            ))
        })?;
    let default_method = Json::from("squash");
    let method = campaign_string(
        campaign.get("mergeMethod").or(Some(&default_method)),
        "worklist.campaign.mergeMethod",
        None,
    )?;
    if !matches!(method.as_str(), "merge" | "squash") {
        return Err(DriverError::new(
            "worklist.campaign.mergeMethod must be merge or squash",
        ));
    }
    let default_driver_runtime = Json::Number("900".to_owned());
    campaign_u64(
        campaign
            .get("driverRuntimeMaxSec")
            .or(Some(&default_driver_runtime)),
        "worklist.campaign.driverRuntimeMaxSec",
    )?;
    if let Some(runtime) = campaign.get("runtimeMaxSec") {
        if !matches!(runtime, Json::Null) {
            campaign_u64(Some(runtime), "worklist.campaign.runtimeMaxSec")?;
        }
    }
    validate_campaign_agent(campaign.get("agent"), "worklist.campaign.agent")?;
    let steward = campaign.get("steward");
    let steward_present = match steward {
        Some(Json::Null) | None => false,
        Some(value) => {
            let steward = required_string(Some(value), "worklist.campaign.steward", Some(80))?;
            if !is_component(&steward) {
                return Err(DriverError::new(
                    "worklist.campaign.steward must be null or a safe adapter name",
                ));
            }
            true
        }
    };
    let empty_argv = Json::Array(Vec::new());
    let steward_argv = campaign_string_list(
        campaign.get("stewardArgv").or(Some(&empty_argv)),
        "worklist.campaign.stewardArgv",
        false,
    )?;
    if !steward_present && !steward_argv.is_empty() {
        return Err(DriverError::new(
            "worklist.campaign.stewardArgv requires a steward adapter",
        ));
    }
    let default_steward_runtime = Json::Number("120".to_owned());
    campaign_u64(
        campaign
            .get("stewardRuntimeMaxSec")
            .or(Some(&default_steward_runtime)),
        "worklist.campaign.stewardRuntimeMaxSec",
    )?;
    let gates = campaign
        .get("gates")
        .and_then(Json::as_array)
        .filter(|gates| (1..=16).contains(&gates.len()))
        .ok_or_else(|| DriverError::new("worklist.campaign.gates must contain 1..=16 entries"))?;
    let mut gate_ids = BTreeSet::new();
    for (index, gate) in gates.iter().enumerate() {
        let identifier =
            validate_campaign_gate(gate, &format!("worklist.campaign.gates[{index}]"))?;
        if !gate_ids.insert(identifier) {
            return Err(DriverError::new(
                "worklist.campaign gate ids must be unique",
            ));
        }
    }
    if max_tasks != witnessed_max_tasks {
        return Err(DriverError::new(format!(
            "worklist campaign maxTasks disagrees with the witnessed brief: campaign={max_tasks} brief={witnessed_max_tasks}"
        )));
    }
    if max_parallel != witnessed_max_parallel {
        return Err(DriverError::new(format!(
            "worklist campaign maxParallel disagrees with the witnessed brief: campaign={max_parallel} brief={witnessed_max_parallel}"
        )));
    }
    Ok(())
}

fn case_sensitive_component_glob(text: &str, pattern: &str) -> bool {
    let text: Vec<char> = text.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    let mut memo = BTreeMap::new();
    fn matches(
        text: &[char],
        pattern: &[char],
        text_index: usize,
        pattern_index: usize,
        memo: &mut BTreeMap<(usize, usize), bool>,
    ) -> bool {
        if let Some(value) = memo.get(&(text_index, pattern_index)) {
            return *value;
        }
        let result = if pattern_index == pattern.len() {
            text_index == text.len()
        } else {
            match pattern[pattern_index] {
                '*' => {
                    matches(text, pattern, text_index, pattern_index + 1, memo)
                        || (text_index < text.len()
                            && matches(text, pattern, text_index + 1, pattern_index, memo))
                }
                '?' => {
                    text_index < text.len()
                        && matches(text, pattern, text_index + 1, pattern_index + 1, memo)
                }
                '[' => {
                    let mut closing = pattern_index + 1;
                    if closing < pattern.len() && matches!(pattern[closing], '!' | '^') {
                        closing += 1;
                    }
                    if closing < pattern.len() && pattern[closing] == ']' {
                        closing += 1;
                    }
                    while closing < pattern.len() && pattern[closing] != ']' {
                        closing += 1;
                    }
                    if closing == pattern.len() {
                        text_index < text.len()
                            && text[text_index] == '['
                            && matches(text, pattern, text_index + 1, pattern_index + 1, memo)
                    } else if text_index >= text.len() {
                        false
                    } else {
                        let mut cursor = pattern_index + 1;
                        let negated = cursor < closing && matches!(pattern[cursor], '!' | '^');
                        if negated {
                            cursor += 1;
                        }
                        let mut included = false;
                        while cursor < closing {
                            if cursor + 2 < closing && pattern[cursor + 1] == '-' {
                                included |= pattern[cursor] <= text[text_index]
                                    && text[text_index] <= pattern[cursor + 2];
                                cursor += 3;
                            } else {
                                included |= pattern[cursor] == text[text_index];
                                cursor += 1;
                            }
                        }
                        (included != negated)
                            && matches(text, pattern, text_index + 1, closing + 1, memo)
                    }
                }
                literal => {
                    text_index < text.len()
                        && text[text_index] == literal
                        && matches(text, pattern, text_index + 1, pattern_index + 1, memo)
                }
            }
        };
        memo.insert((text_index, pattern_index), result);
        result
    }
    matches(&text, &pattern, 0, 0, &mut memo)
}

fn canonical_sha256(value: &Json) -> String {
    format!("sha256:{}", sha256::digest(value.stringify().as_bytes()))
}

fn current_task_input_epochs(
    tasks: &[Json],
    gate_set: Option<&Json>,
    steering_high_water: Option<&Json>,
    admitted_hashes: Option<&Json>,
) -> Result<BTreeMap<String, String>> {
    if gate_set.is_none() && steering_high_water.is_none() && admitted_hashes.is_none() {
        // Compatibility action briefs predate epoch derivation. Their stamped
        // and unstamped receipts remain conservative until a current flow
        // supplies the complete input tuple.
        return Ok(BTreeMap::new());
    }
    let gates = gate_set
        .and_then(Json::as_array)
        .ok_or_else(|| DriverError::new("gateSet must be an array for epoch derivation"))?;
    let mut gate_ids = BTreeSet::new();
    for (index, gate) in gates.iter().enumerate() {
        let id = validate_campaign_gate(gate, &format!("gateSet[{index}]"))?;
        if !gate_ids.insert(id.clone()) {
            return Err(DriverError::new(format!("gateSet repeats gate id {id:?}")));
        }
    }

    let high_water = object_complete(
        steering_high_water.ok_or_else(|| {
            DriverError::new("steeringHighWater is required for epoch derivation")
        })?,
        &["campaign", "tasks"],
        "steeringHighWater",
    )?;
    let campaign_high_water = high_water
        .get("campaign")
        .and_then(Json::as_u64)
        .ok_or_else(|| DriverError::new("steeringHighWater.campaign must be an integer"))?;
    let task_high_waters = high_water
        .get("tasks")
        .and_then(Json::as_object)
        .ok_or_else(|| DriverError::new("steeringHighWater.tasks must be an object"))?;
    if task_high_waters.len() > MAX_CAMPAIGN_TASKS
        || task_high_waters.keys().any(|task_id| !is_task_id(task_id))
        || task_high_waters
            .values()
            .any(|value| value.as_u64().is_none())
    {
        return Err(DriverError::new(
            "steeringHighWater.tasks must map at most 128 safe task IDs to integers",
        ));
    }

    let admitted_hashes = admitted_hashes.map(|value| {
        let hashes = value
            .as_object()
            .ok_or_else(|| DriverError::new("taskInputHashes must be an object"))?;
        if hashes.len() > MAX_CAMPAIGN_TASKS {
            return Err(DriverError::new("taskInputHashes exceeds 128 tasks"));
        }
        hashes
            .iter()
            .map(|(task_id, value)| {
                if !is_task_id(task_id) {
                    return Err(DriverError::new(
                        "taskInputHashes contains an unsafe task ID",
                    ));
                }
                let hash =
                    required_string(Some(value), &format!("taskInputHashes.{task_id}"), Some(71))?;
                if !is_sha256_identity(&hash) {
                    return Err(DriverError::new(format!(
                        "taskInputHashes.{task_id} must be a lowercase SHA-256 identity"
                    )));
                }
                Ok((task_id.clone(), hash))
            })
            .collect::<Result<BTreeMap<_, _>>>()
    });
    let admitted_hashes = admitted_hashes.transpose()?;

    let task_ids = tasks
        .iter()
        .map(|task| task_id(task).map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?;
    if let Some(hashes) = &admitted_hashes {
        if hashes.keys().cloned().collect::<BTreeSet<_>>() != task_ids {
            return Err(DriverError::new(
                "taskInputHashes must name exactly the reconciled tasks",
            ));
        }
    }
    tasks
        .iter()
        .map(|task| {
            let task_id = task_id(task)?.to_owned();
            let input_hash = if let Some(hashes) = &admitted_hashes {
                hashes
                    .get(&task_id)
                    .expect("the admitted hash key set was checked")
                    .clone()
            } else {
                let mut authored_task = task_object(task, "epoch task")?.clone();
                authored_task.remove("revision");
                canonical_sha256(&Json::object([
                    ("contractVersion", Json::Number("1".to_owned())),
                    ("task", Json::Object(authored_task)),
                    ("gates", Json::Array(gates.to_vec())),
                ]))
            };
            let addressed = task_high_waters
                .get(&task_id)
                .and_then(Json::as_u64)
                .unwrap_or(campaign_high_water)
                .max(campaign_high_water);
            let epoch = task_input_epoch(&input_hash, addressed)
                .map_err(|error| DriverError::new(error.to_string()))?;
            Ok((task_id, epoch))
        })
        .collect()
}

fn file_task_completion_revision(
    repository: &str,
    source: &BTreeMap<String, Json>,
    task: &Json,
) -> Result<String> {
    let source_repository = source
        .get("repository")
        .and_then(Json::as_str)
        .unwrap_or(repository);
    let source_path = required_string(source.get("path"), "worklist source path", None)?;
    Ok(canonical_sha256(&Json::object([
        ("contractVersion", Json::Number("1".to_owned())),
        ("repository", Json::from(repository)),
        (
            "source",
            Json::object([
                ("repository", Json::from(source_repository)),
                ("path", Json::from(source_path)),
            ]),
        ),
        ("task", task.clone()),
    ])))
}

fn action_worklist(brief: &Json) -> Result<Json> {
    let mut fields = vec![
        "repository",
        "repositoryConfig",
        "worklist",
        "maxTasks",
        "maxParallel",
    ];
    if brief
        .as_object()
        .is_some_and(|object| object.contains_key("specRepository"))
    {
        fields.push("specRepository");
    }
    if brief
        .as_object()
        .is_some_and(|object| object.contains_key("issueRepository"))
    {
        fields.push("issueRepository");
    }
    let data = object_exact(brief, &fields, "worklist brief")?;
    let repository = repository_name(data.get("repository"), "repository")?;
    let config = repo_config(data.get("repositoryConfig"))?;
    let (code, spec, _issue) = campaign_coordinates(data, repository.clone(), config)?;
    let pattern = required_string(data.get("worklist"), "worklist", None)?;
    let pattern_path = Path::new(&pattern);
    if pattern_path.is_absolute()
        || pattern_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(DriverError::new(
            "worklist must be a relative pattern without '..'",
        ));
    }
    let max_tasks = data
        .get("maxTasks")
        .and_then(Json::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| (1..=MAX_CAMPAIGN_TASKS).contains(value))
        .ok_or_else(|| DriverError::new("maxTasks must be an integer from 1 through 128"))?;
    let default_parallel = Json::Number("1".to_owned());
    let max_parallel = data
        .get("maxParallel")
        .or(Some(&default_parallel))
        .and_then(Json::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| (1..=MAX_CAMPAIGN_TASKS).contains(value))
        .ok_or_else(|| DriverError::new("maxParallel must be an integer from 1 through 128"))?;
    if max_parallel > max_tasks {
        return Err(DriverError::new("maxParallel must not exceed maxTasks"));
    }

    git(
        &spec.config.checkout,
        ["fetch", "--prune", "--no-tags", &spec.config.remote],
        true,
    )?;
    let base_ref = format!("{}/{}", spec.config.remote, spec.config.base_branch);
    let base_rev = git(
        &spec.config.checkout,
        ["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
        true,
    )?
    .stdout_trimmed();
    let pattern_parts: Vec<_> = pattern_path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => part.to_str(),
            std::path::Component::CurDir => None,
            _ => None,
        })
        .collect();
    let literal_prefix: Vec<_> = pattern_parts
        .iter()
        .take_while(|part| {
            !part
                .chars()
                .any(|character| matches!(character, '*' | '?' | '['))
        })
        .copied()
        .collect();
    let mut tree_arguments = vec![
        "ls-tree".to_owned(),
        "-r".to_owned(),
        "-z".to_owned(),
        "--full-tree".to_owned(),
        base_rev.clone(),
    ];
    if !literal_prefix.is_empty() {
        tree_arguments.push("--".to_owned());
        tree_arguments.push(literal_prefix.join("/"));
    }
    let tree = git(&spec.config.checkout, &tree_arguments, true)?.stdout;
    let mut matches = Vec::new();
    for entry in tree
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let separator = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| {
                DriverError::new("remote base tree contains a malformed worklist candidate")
            })?;
        let metadata = std::str::from_utf8(&entry[..separator]).map_err(|_| {
            DriverError::new("remote base tree contains a malformed worklist candidate")
        })?;
        let path = std::str::from_utf8(&entry[separator + 1..]).map_err(|_| {
            DriverError::new("remote base tree contains a malformed worklist candidate")
        })?;
        let metadata: Vec<_> = metadata.split(' ').collect();
        if metadata.len() != 3 {
            return Err(DriverError::new(
                "remote base tree contains a malformed worklist candidate",
            ));
        }
        let path_parts: Vec<_> = path.split('/').collect();
        let matched = path_parts.len() == pattern_parts.len()
            && pattern_parts
                .iter()
                .zip(&path_parts)
                .all(|(part, candidate)| {
                    (!part.starts_with('.') || candidate.starts_with('.'))
                        && case_sensitive_component_glob(candidate, part)
                });
        if matched && metadata[1] == "blob" && matches!(metadata[0], "100644" | "100755") {
            matches.push((path.to_owned(), metadata[2].to_owned()));
        }
    }
    if matches.len() != 1 {
        return Err(DriverError::new(format!(
            "worklist pattern {pattern:?} matched {} regular files; expected exactly one",
            matches.len()
        )));
    }
    let (source_path, source_object) = &matches[0];
    let raw = git(
        &spec.config.checkout,
        ["cat-file", "blob", source_object],
        true,
    )?
    .stdout;
    let text = std::str::from_utf8(&raw)
        .map_err(|error| DriverError::new(format!("worklist is not valid JSON: {error}")))?;
    let document = json::parse(text)
        .map_err(|error| DriverError::new(format!("worklist is not valid JSON: {error}")))?;
    let document = object_exact(
        &document,
        &["schemaVersion", "tasks", "campaign"],
        "worklist",
    )?;
    if document.get("schemaVersion").and_then(Json::as_u64) != Some(1) {
        return Err(DriverError::new("worklist.schemaVersion must equal 1"));
    }
    if let Some(campaign) = document.get("campaign") {
        validate_worklist_campaign(campaign, source_path, max_tasks, max_parallel)?;
    }
    let candidates = document
        .get("tasks")
        .and_then(Json::as_array)
        .filter(|tasks| !tasks.is_empty())
        .ok_or_else(|| DriverError::new("worklist.tasks must be a non-empty array"))?;
    if candidates.len() > max_tasks {
        return Err(DriverError::new(format!(
            "worklist has {} tasks, exceeding maxTasks {max_tasks}",
            candidates.len()
        )));
    }
    let mut tasks = Vec::new();
    let mut prior_ids = BTreeSet::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let task = normalize_task(candidate, index, &prior_ids, max_parallel > 1)?;
        let task_id = task
            .as_object()
            .and_then(|task| task.get("id"))
            .and_then(Json::as_str)
            .expect("normalized task has an ID")
            .to_owned();
        if !prior_ids.insert(task_id.clone()) {
            return Err(DriverError::new(format!(
                "worklist repeats task id {task_id:?}"
            )));
        }
        tasks.push(task);
    }
    let mut source = BTreeMap::from([
        ("path".to_owned(), Json::from(source_path.clone())),
        (
            "sha256".to_owned(),
            Json::from(format!("sha256:{}", sha256::digest(&raw))),
        ),
        ("revision".to_owned(), Json::from(base_rev)),
    ]);
    if !same_repository(&spec, &code) {
        source.insert("repository".to_owned(), Json::from(spec.repository));
    }
    for task in &mut tasks {
        let revision = file_task_completion_revision(&repository, &source, task)?;
        task.as_object_mut()
            .expect("normalized task is an object")
            .insert("revision".to_owned(), Json::from(revision));
    }
    Ok(Json::object([
        ("schemaVersion", Json::Number("1".to_owned())),
        ("repository", Json::from(repository)),
        ("source", Json::Object(source)),
        ("tasks", Json::Array(tasks)),
    ]))
}

fn truncate_chars(value: &str, maximum: usize, marker: &str) -> (String, bool) {
    if value.chars().count() <= maximum {
        return (value.to_owned(), false);
    }
    let mut truncated: String = value.chars().take(maximum).collect();
    truncated.push_str(marker);
    (truncated, true)
}

fn action_diff(brief: &Json) -> Result<Json> {
    let data = object_exact(brief, &["repositoryConfig", "workspace"], "diff brief")?;
    repo_config(data.get("repositoryConfig"))?;
    let workspace = prepared_workspace(data.get("workspace"), "workspace")?;
    if !is_task_id(&workspace.task_id) {
        return Err(DriverError::new("workspace.taskId is not safe"));
    }
    if !is_full_oid(&workspace.base_rev) {
        return Err(DriverError::new(
            "workspace.baseRev must be a full Git object ID",
        ));
    }
    if !workspace.worktree.is_absolute() {
        return Err(DriverError::new("workspace.worktreePath must be absolute"));
    }
    if !workspace.worktree.is_dir() {
        return Ok(Json::object([
            ("taskId", Json::from(workspace.task_id)),
            ("available", Json::from(false)),
            ("baseRev", Json::from(workspace.base_rev)),
            ("head", Json::Null),
            ("status", Json::from("")),
            ("patch", Json::from("")),
            ("truncated", Json::from(false)),
            (
                "reason",
                Json::from("prepared worktree is no longer available"),
            ),
        ]));
    }
    git(&workspace.worktree, ["rev-parse", "--git-dir"], true)?;
    let actual_branch =
        git(&workspace.worktree, ["branch", "--show-current"], true)?.stdout_trimmed();
    if actual_branch != workspace.branch {
        return Err(DriverError::new(format!(
            "diff worktree is on branch {actual_branch:?}, expected {:?}",
            workspace.branch
        )));
    }
    let head = git(&workspace.worktree, ["rev-parse", "HEAD"], true)?.stdout_trimmed();
    let status = git(
        &workspace.worktree,
        ["status", "--short", "--untracked-files=all"],
        true,
    )?
    .stdout_text();
    let mut patch = git(
        &workspace.worktree,
        [
            "diff",
            "--binary",
            "--no-ext-diff",
            &workspace.base_rev,
            "--",
        ],
        true,
    )?
    .stdout_text();
    let untracked = git(
        &workspace.worktree,
        ["ls-files", "--others", "--exclude-standard", "-z"],
        true,
    )?
    .stdout;
    for raw in untracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let relative = std::str::from_utf8(raw).map_err(|error| {
            DriverError::new(format!("cannot capture non-UTF-8 untracked path: {error}"))
        })?;
        let rendered = format!("./{relative}");
        let captured = git(
            &workspace.worktree,
            [
                "diff",
                "--no-index",
                "--binary",
                "--",
                "/dev/null",
                &rendered,
            ],
            false,
        )?;
        if !matches!(captured.status, 0 | 1) {
            return Err(DriverError::new(format!(
                "cannot capture untracked diff for {relative:?}: {}",
                captured.detail()
            )));
        }
        patch.push_str(&captured.stdout_text());
    }
    let (patch, truncated) = truncate_chars(&patch, MAX_DIFF_CHARS, "\n[... diff truncated ...]\n");
    let (status, _) = truncate_chars(&status, 16_000, "\n[... status truncated ...]\n");
    Ok(Json::object([
        ("taskId", Json::from(workspace.task_id)),
        ("available", Json::from(true)),
        ("baseRev", Json::from(workspace.base_rev)),
        ("head", Json::from(head)),
        ("status", Json::from(status)),
        ("patch", Json::from(patch)),
        ("truncated", Json::from(truncated)),
        ("reason", Json::Null),
    ]))
}

fn local_actor(value: Option<&Json>, context: &str) -> Result<String> {
    let actor = value
        .and_then(Json::as_str)
        .ok_or_else(|| DriverError::new(format!("{context} is not a valid local actor")))?;
    if actor.is_empty()
        || actor.chars().count() > 128
        || actor.contains(['\0', '/', '\\'])
        || actor.chars().any(char::is_whitespace)
    {
        return Err(DriverError::new(format!(
            "{context} is not a valid local actor"
        )));
    }
    Ok(actor.to_owned())
}

fn steering_comment(value: &Json, context: &str) -> Result<Json> {
    let comment = object_complete(
        value,
        &["id", "url", "author", "body", "createdAt", "updatedAt"],
        context,
    )?;
    let id = comment
        .get("id")
        .and_then(Json::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(|| DriverError::new(format!("{context}.id must be a positive integer")))?;
    let body = comment
        .get("body")
        .and_then(Json::as_str)
        .filter(|body| !body.contains('\0'))
        .ok_or_else(|| {
            DriverError::new(format!("{context}.body must be text without NUL bytes"))
        })?;
    if body.chars().count() > 64_000 {
        return Err(DriverError::new(format!(
            "{context}.body exceeds 64000 characters"
        )));
    }
    Ok(Json::object([
        ("id", Json::Number(id.to_string())),
        (
            "url",
            Json::from(required_string(
                comment.get("url"),
                &format!("{context}.url"),
                None,
            )?),
        ),
        (
            "author",
            Json::from(local_actor(
                comment.get("author"),
                &format!("{context}.author"),
            )?),
        ),
        ("body", Json::from(body)),
        (
            "createdAt",
            Json::from(required_string(
                comment.get("createdAt"),
                &format!("{context}.createdAt"),
                None,
            )?),
        ),
        (
            "updatedAt",
            Json::from(required_string(
                comment.get("updatedAt"),
                &format!("{context}.updatedAt"),
                None,
            )?),
        ),
    ]))
}

fn steering_timestamp(value: Option<&Json>, context: &str) -> Result<DateTime<FixedOffset>> {
    let text = required_string(value, context, None)?;
    DateTime::parse_from_rfc3339(&text)
        .map_err(|_| DriverError::new(format!("{context} is not an RFC 3339 timestamp")))
}

#[derive(Debug)]
struct SteeringSource {
    registration_id: String,
    local_actor: String,
    log_path: PathBuf,
    lock_path: PathBuf,
    prepared_cursor: u64,
}

fn steering_source(value: Option<&Json>, actor: &str) -> Result<SteeringSource> {
    let value = value.ok_or_else(|| DriverError::new("steeringSource must be an object"))?;
    let source = object_complete(
        value,
        &[
            "schemaVersion",
            "kind",
            "registrationId",
            "localActor",
            "logPath",
            "lockPath",
            "preparedCursor",
        ],
        "steeringSource",
    )?;
    if source.get("schemaVersion").and_then(Json::as_u64) != Some(1)
        || source.get("kind").and_then(Json::as_str) != Some("local-jsonl")
    {
        return Err(DriverError::new(
            "steeringSource must use local-jsonl schema version 1",
        ));
    }
    let registration_id = required_string(
        source.get("registrationId"),
        "steeringSource.registrationId",
        Some(128),
    )?;
    let parsed = Uuid::parse_str(&registration_id)
        .map_err(|_| DriverError::new("steeringSource.registrationId must be a canonical UUID"))?;
    if parsed.to_string() != registration_id {
        return Err(DriverError::new(
            "steeringSource.registrationId must be a canonical UUID",
        ));
    }
    let source_actor = local_actor(source.get("localActor"), "steeringSource.localActor")?;
    if source_actor != actor {
        return Err(DriverError::new(
            "steeringSource.localActor does not match localActor",
        ));
    }
    let log_path = PathBuf::from(required_string(
        source.get("logPath"),
        "steeringSource.logPath",
        None,
    )?);
    let lock_path = PathBuf::from(required_string(
        source.get("lockPath"),
        "steeringSource.lockPath",
        None,
    )?);
    if !log_path.is_absolute() || !lock_path.is_absolute() {
        return Err(DriverError::new("steeringSource paths must be absolute"));
    }
    let parent = log_path.parent();
    let valid_paths = log_path
        .file_name()
        .is_some_and(|name| name == "steering-v1.jsonl")
        && lock_path
            .file_name()
            .is_some_and(|name| name == "steering.lock")
        && parent == lock_path.parent()
        && parent
            .and_then(Path::file_name)
            .is_some_and(|name| name == registration_id.as_str())
        && parent
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|name| name == "steering")
        && parent
            .and_then(Path::parent)
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|name| name == "campaigns");
    if !valid_paths {
        return Err(DriverError::new(
            "steeringSource paths do not identify one campaign steering source",
        ));
    }
    let prepared_cursor = source
        .get("preparedCursor")
        .and_then(Json::as_u64)
        .ok_or_else(|| {
            DriverError::new("steeringSource.preparedCursor must be a non-negative integer")
        })?;
    Ok(SteeringSource {
        registration_id,
        local_actor: source_actor,
        log_path,
        lock_path,
        prepared_cursor,
    })
}

fn open_regular_nofollow(path: &Path, write: bool, context: &str) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(write)
        .custom_flags(O_CLOEXEC | O_NOFOLLOW);
    let file = options.open(path).map_err(|error| {
        DriverError::new(format!("cannot open {context} {}: {error}", path.display()))
    })?;
    if !file.metadata()?.is_file() {
        return Err(DriverError::new(format!(
            "{context} {} is not a regular file",
            path.display()
        )));
    }
    Ok(file)
}

fn local_steering_comments(source: &SteeringSource, task_id: &str) -> Result<(Vec<Json>, u64)> {
    let lock = open_regular_nofollow(&source.lock_path, true, "campaign steering lock")?;
    if unsafe { flock(lock.as_raw_fd(), LOCK_SH) } != 0 {
        return Err(DriverError::new(format!(
            "cannot lock campaign steering source: {}",
            std::io::Error::last_os_error()
        )));
    }
    let read = (|| {
        let mut log = open_regular_nofollow(&source.log_path, false, "campaign steering log")?;
        let metadata = log.metadata()?;
        if metadata.len() > 128 * 1024 * 1024 {
            return Err(DriverError::new("campaign steering log exceeds 128 MiB"));
        }
        let mut raw = Vec::with_capacity(metadata.len() as usize);
        log.read_to_end(&mut raw)?;
        if raw.len() > 128 * 1024 * 1024 {
            return Err(DriverError::new("campaign steering log exceeds 128 MiB"));
        }
        if !raw.is_empty() && !raw.ends_with(b"\n") {
            return Err(DriverError::new(
                "campaign steering log has an incomplete final record",
            ));
        }
        let text = std::str::from_utf8(&raw).map_err(|error| {
            DriverError::new(format!("campaign steering log record is invalid: {error}"))
        })?;
        let mut comments = Vec::new();
        let mut target_counts = BTreeMap::<Option<String>, usize>::new();
        let mut prior_embargo = None;
        for (index, line) in text.lines().enumerate() {
            let sequence = index as u64 + 1;
            if line.is_empty() {
                return Err(DriverError::new(format!(
                    "campaign steering log has an empty record at line {sequence}"
                )));
            }
            let value = json::parse(line).map_err(|error| {
                DriverError::new(format!(
                    "campaign steering log record {sequence} is invalid: {error}"
                ))
            })?;
            let record = object_complete(
                &value,
                &[
                    "schemaVersion",
                    "sequence",
                    "registrationId",
                    "taskId",
                    "doNotDispatchBefore",
                    "comment",
                ],
                &format!("campaign steering record {sequence}"),
            )?;
            let target = match record.get("taskId") {
                None | Some(Json::Null) => None,
                Some(Json::String(target))
                    if target.chars().count() <= 80 && is_task_id(target) =>
                {
                    Some(target.clone())
                }
                _ => {
                    return Err(DriverError::new(format!(
                        "campaign steering record {sequence}.taskId is invalid"
                    )))
                }
            };
            let comment = steering_comment(
                record.get("comment").unwrap_or(&Json::Null),
                &format!("campaign steering record {sequence}.comment"),
            )?;
            let comment_object = comment.as_object().expect("comment object");
            let expected_url = format!(
                "local://campaign/{}/steering/{sequence}",
                source.registration_id
            );
            let valid = record.get("schemaVersion").and_then(Json::as_u64) == Some(1)
                && record.get("sequence").and_then(Json::as_u64) == Some(sequence)
                && record.get("registrationId").and_then(Json::as_str)
                    == Some(source.registration_id.as_str())
                && comment_object.get("id").and_then(Json::as_u64) == Some(sequence)
                && comment_object.get("url").and_then(Json::as_str) == Some(expected_url.as_str())
                && comment_object.get("author").and_then(Json::as_str)
                    == Some(source.local_actor.as_str());
            if !valid {
                return Err(DriverError::new(format!(
                    "campaign steering record {sequence} violates steering-v1 invariants"
                )));
            }
            let created = steering_timestamp(
                comment_object.get("createdAt"),
                &format!("campaign steering record {sequence}.comment.createdAt"),
            )?;
            let updated = steering_timestamp(
                comment_object.get("updatedAt"),
                &format!("campaign steering record {sequence}.comment.updatedAt"),
            )?;
            let embargo = steering_timestamp(
                record.get("doNotDispatchBefore"),
                &format!("campaign steering record {sequence}.doNotDispatchBefore"),
            )?;
            if updated != created || embargo != created + ChronoDuration::milliseconds(1_000) {
                return Err(DriverError::new(format!(
                    "campaign steering record {sequence} has inconsistent append-only timestamps"
                )));
            }
            if prior_embargo.is_some_and(|prior| embargo <= prior) {
                return Err(DriverError::new(format!(
                    "campaign steering record {sequence} does not advance doNotDispatchBefore"
                )));
            }
            prior_embargo = Some(embargo);
            let count = target_counts.entry(target.clone()).or_default();
            *count += 1;
            if *count > 1_000 {
                return Err(DriverError::new(format!(
                    "campaign steering target {target:?} has more than 1000 records"
                )));
            }
            if target.as_deref().is_none_or(|target| target == task_id) {
                comments.push(comment);
            }
        }
        Ok((comments, text.lines().count() as u64))
    })();
    unsafe {
        flock(lock.as_raw_fd(), LOCK_UN);
    }
    read
}

fn action_steering_recheck(brief: &Json) -> Result<Json> {
    let data = object_complete(
        brief,
        &[
            "campaign",
            "campaignIdentity",
            "taskId",
            "localActor",
            "steeringSource",
            "preparedComments",
        ],
        "steering re-check brief",
    )?;
    let campaign = required_string(data.get("campaign"), "campaign", None)?;
    if !is_component(&campaign) {
        return Err(DriverError::new("campaign is not a safe component"));
    }
    let campaign_identity =
        required_string(data.get("campaignIdentity"), "campaignIdentity", Some(128))?;
    let task_id = required_string(data.get("taskId"), "taskId", None)?;
    if task_id.chars().count() > 80 || !is_task_id(&task_id) {
        return Err(DriverError::new("taskId is not safe"));
    }
    let actor = local_actor(data.get("localActor"), "localActor")?;
    let source = steering_source(data.get("steeringSource"), &actor)?;
    if source.registration_id != campaign_identity {
        return Err(DriverError::new(
            "steeringSource.registrationId does not match campaignIdentity",
        ));
    }
    let prepared_values = data
        .get("preparedComments")
        .and_then(Json::as_array)
        .ok_or_else(|| DriverError::new("preparedComments must be an array"))?;
    if prepared_values.len() > 2_000 {
        return Err(DriverError::new(
            "preparedComments has more than 2000 local steering records",
        ));
    }
    let mut prepared = Vec::new();
    let mut prepared_ids = BTreeSet::new();
    for (index, value) in prepared_values.iter().enumerate() {
        let comment = steering_comment(value, &format!("preparedComments[{index}]"))?;
        let object = comment.as_object().expect("comment object");
        let id = object.get("id").and_then(Json::as_u64).expect("comment id");
        if object.get("author").and_then(Json::as_str) != Some(actor.as_str()) {
            return Err(DriverError::new(
                "preparedComments contains an actor outside local authority",
            ));
        }
        if id > source.prepared_cursor {
            return Err(DriverError::new(
                "preparedComments contains an ID beyond the prepared cursor",
            ));
        }
        let expected = format!("local://campaign/{}/steering/{id}", source.registration_id);
        if object.get("url").and_then(Json::as_str) != Some(expected.as_str()) {
            return Err(DriverError::new(
                "preparedComments contains a record outside the local source",
            ));
        }
        if !prepared_ids.insert(id) {
            return Err(DriverError::new(format!(
                "preparedComments repeated comment id {id}"
            )));
        }
        prepared.push(comment);
    }
    let (rechecked, rechecked_cursor) = local_steering_comments(&source, &task_id)?;
    if rechecked_cursor < source.prepared_cursor {
        return Err(DriverError::new(
            "campaign steering log is behind the prepared cursor",
        ));
    }
    if rechecked.len() > 2_000 {
        return Err(DriverError::new(
            "task steering re-check comments has more than 2000 approved steering comments",
        ));
    }
    let mut merged = prepared.clone();
    let mut positions: BTreeMap<u64, usize> = merged
        .iter()
        .enumerate()
        .map(|(index, comment)| {
            (
                comment
                    .as_object()
                    .and_then(|comment| comment.get("id"))
                    .and_then(Json::as_u64)
                    .expect("comment id"),
                index,
            )
        })
        .collect();
    let mut late_ids = Vec::new();
    for comment in rechecked {
        let id = comment
            .as_object()
            .and_then(|comment| comment.get("id"))
            .and_then(Json::as_u64)
            .expect("comment id");
        if let Some(position) = positions.get(&id).copied() {
            if merged[position] != comment {
                merged[position] = comment;
                late_ids.push(Json::Number(id.to_string()));
            }
        } else {
            positions.insert(id, merged.len());
            merged.push(comment);
            late_ids.push(Json::Number(id.to_string()));
        }
    }
    Ok(Json::object([
        ("taskId", Json::from(task_id)),
        ("authorizedComments", Json::Array(merged)),
        (
            "receipt",
            Json::object([
                (
                    "source",
                    Json::object([
                        ("kind", Json::from("local-jsonl")),
                        ("registrationId", Json::from(source.registration_id)),
                        (
                            "path",
                            Json::from(source.log_path.to_string_lossy().into_owned()),
                        ),
                        (
                            "preparedCursor",
                            Json::Number(source.prepared_cursor.to_string()),
                        ),
                        (
                            "recheckedCursor",
                            Json::Number(rechecked_cursor.to_string()),
                        ),
                    ]),
                ),
                ("rechecked", Json::from(true)),
                ("recheckTruncated", Json::from(false)),
                (
                    "preparedCommentIds",
                    Json::Array(
                        prepared
                            .iter()
                            .map(|comment| {
                                Json::Number(
                                    comment
                                        .as_object()
                                        .and_then(|comment| comment.get("id"))
                                        .and_then(Json::as_u64)
                                        .expect("comment id")
                                        .to_string(),
                                )
                            })
                            .collect(),
                    ),
                ),
                ("lateRecheckCommentIds", Json::Array(late_ids)),
            ]),
        ),
    ]))
}

fn gate_evidence_requirements(value: Option<&Json>) -> (Option<String>, Option<String>) {
    let Some(evidence) = value.and_then(Json::as_object) else {
        return (None, None);
    };
    let id = evidence
        .get("id")
        .and_then(Json::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    let path = evidence
        .get("detail")
        .and_then(Json::as_str)
        .and_then(|detail| {
            Regex::new(
                r#"forbidPaths gate \S+ rejected \d+ path\(s\) touched in lane history \(a later removal does not clear this; the path must never appear in any lane commit\): \"((?:[^\"\\]|\\.)*)\""#,
            )
            .expect("static forbidPaths evidence regex")
            .captures(detail)
            .and_then(|capture| capture.get(1))
            .map(|path| path.as_str().to_owned())
        });
    (id, path)
}

fn collapse_whitespace(value: &str) -> String {
    let mut output = String::new();
    let mut pending = false;
    for character in value.chars() {
        if character.is_whitespace() {
            pending = !output.is_empty();
        } else {
            if pending {
                output.push(' ');
                pending = false;
            }
            output.push(character);
        }
    }
    output
}

fn python_single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn diagnosis_fallback_note(
    reason: &str,
    gate_id: Option<&str>,
    path: Option<&str>,
    rejected: Option<&str>,
) -> String {
    let mut note = format!(
        "Recorded a grammar-rejected steward diagnosis. Validation rejected the proposal because {reason}."
    );
    if let Some(gate_id) = gate_id {
        note.push_str(&format!(
            " Required literal check id: {}.",
            python_single_quoted(gate_id)
        ));
    }
    if let Some(path) = path {
        note.push_str(&format!(
            " Required literal offending path: {}.",
            python_single_quoted(path)
        ));
    }
    if let Some(rejected) = rejected {
        let excerpt = collapse_whitespace(rejected);
        let excerpt = take_chars(&excerpt, 2_000);
        let excerpt = trim_end_punctuation(&excerpt);
        let excerpt = replace_bare_exclamation_marks(excerpt, ".");
        if !excerpt.is_empty() {
            note.push_str(&format!(" Redacted proposal excerpt: {excerpt}."));
        }
    }
    if note.chars().count() > MAX_DIAGNOSIS_CHARS {
        note = format!("{}…", take_chars(&note, MAX_DIAGNOSIS_CHARS - 1).trim_end());
    }
    note
}

fn diagnosis_rejection_reason(
    diagnosis: &str,
    gate_evidence: Option<&Json>,
) -> (Option<String>, Option<String>, Option<String>) {
    let (required_id, required_path) = gate_evidence_requirements(gate_evidence);
    let mut reason = validate_outcome_first(diagnosis, MAX_DIAGNOSIS_CHARS, "diagnosis");
    if reason.is_none()
        && required_id
            .as_ref()
            .is_some_and(|required| !diagnosis.contains(required))
    {
        reason = Some(format!(
            "diagnosis omits the failing check id {}",
            python_single_quoted(required_id.as_deref().expect("checked above"))
        ));
    }
    if reason.is_none()
        && required_path
            .as_ref()
            .is_some_and(|required| !diagnosis.contains(required))
    {
        reason = Some(format!(
            "diagnosis omits the offending path {}",
            python_single_quoted(required_path.as_deref().expect("checked above"))
        ));
    }
    (reason, required_id, required_path)
}

fn validated_diagnosis(diagnosis: &str, evidence: Option<&Json>) -> String {
    let (reason, required_id, required_path) = diagnosis_rejection_reason(diagnosis, evidence);
    reason.map_or_else(
        || diagnosis.to_owned(),
        |reason| {
            diagnosis_fallback_note(
                &reason,
                required_id.as_deref(),
                required_path.as_deref(),
                Some(diagnosis),
            )
        },
    )
}

fn normalize_diagnosis_proposal(value: Option<&Json>, context: &str) -> Result<Option<Json>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if matches!(value, Json::Null) {
        return Ok(None);
    }
    let proposal = object_exact(
        value,
        &[
            "kind",
            "paths",
            "goal",
            "acceptanceCriteria",
            "dependencies",
        ],
        context,
    )?;
    let kind = required_string(proposal.get("kind"), &format!("{context}.kind"), None)?;
    if !matches!(kind.as_str(), "amendment-task" | "gate-set-fix") {
        return Err(DriverError::new(format!(
            "{context}.kind must be amendment-task or gate-set-fix"
        )));
    }
    let paths = normalize_paths(proposal.get("paths"), &format!("{context}.paths"), true)?
        .expect("proposal paths are required");
    if paths.len() > 128 || paths.iter().any(|path| path.chars().count() > 4_096) {
        return Err(DriverError::new(format!(
            "{context}.paths exceeds the proposal bound"
        )));
    }
    let goal = required_text(proposal.get("goal"), &format!("{context}.goal"), 12_000)?;
    let acceptance = normalize_acceptance(
        proposal.get("acceptanceCriteria"),
        &format!("{context}.acceptanceCriteria"),
    )?;
    let criteria = acceptance
        .as_array()
        .expect("normalized acceptance criteria are an array");
    if criteria.len() > 16 {
        return Err(DriverError::new(format!(
            "{context}.acceptanceCriteria exceeds 16 entries"
        )));
    }
    for (index, criterion) in criteria.iter().enumerate() {
        let argv = criterion
            .as_object()
            .and_then(|criterion| criterion.get("argv"))
            .and_then(Json::as_array)
            .expect("normalized criterion argv is an array");
        if argv.len() > 32
            || argv.iter().any(|argument| {
                argument
                    .as_str()
                    .is_some_and(|argument| argument.chars().count() > 4_096)
            })
        {
            return Err(DriverError::new(format!(
                "{context}.acceptanceCriteria[{index}].argv exceeds the proposal bound"
            )));
        }
    }
    let dependencies = string_list(
        proposal.get("dependencies"),
        &format!("{context}.dependencies"),
        false,
    )?;
    if dependencies.len() > 128
        || dependencies
            .iter()
            .any(|dependency| dependency.chars().count() > 80 || !is_task_id(dependency))
        || dependencies.iter().collect::<BTreeSet<_>>().len() != dependencies.len()
    {
        return Err(DriverError::new(format!(
            "{context}.dependencies must contain at most 128 unique stable task IDs"
        )));
    }
    Ok(Some(Json::object([
        ("kind", Json::from(kind)),
        (
            "paths",
            Json::Array(paths.into_iter().map(Json::from).collect()),
        ),
        ("goal", Json::from(goal)),
        ("acceptanceCriteria", acceptance),
        (
            "dependencies",
            Json::Array(dependencies.into_iter().map(Json::from).collect()),
        ),
    ])))
}

fn redact_json_strings(value: &Json) -> (Json, bool) {
    match value {
        Json::String(value) => {
            let (value, redacted) = redact_public_text(value);
            (Json::from(value), redacted)
        }
        Json::Array(values) => {
            let mut redacted = false;
            let values = values
                .iter()
                .map(|value| {
                    let (value, item_redacted) = redact_json_strings(value);
                    redacted |= item_redacted;
                    value
                })
                .collect();
            (Json::Array(values), redacted)
        }
        Json::Object(values) => {
            let mut redacted = false;
            let values = values
                .iter()
                .map(|(key, value)| {
                    let (value, item_redacted) = redact_json_strings(value);
                    redacted |= item_redacted;
                    (key.clone(), value)
                })
                .collect();
            (Json::Object(values), redacted)
        }
        value => (value.clone(), false),
    }
}

fn public_diagnosis_proposal(value: Option<&Json>) -> Result<(Option<Json>, bool)> {
    let proposal = normalize_diagnosis_proposal(value, "proposal")?;
    let Some(proposal) = proposal else {
        return Ok((None, false));
    };
    let (redacted_proposal, redacted) = redact_json_strings(&proposal);
    if !redacted {
        return Ok((Some(proposal), false));
    }
    // Redaction may make a path or stable ID cease to satisfy its structural
    // grammar. Secrets still never reach the public ledger; in that rare case
    // retain the diagnosis and omit the now-unactionable proposal.
    Ok((
        normalize_diagnosis_proposal(Some(&redacted_proposal), "redacted proposal").unwrap_or(None),
        true,
    ))
}

fn bound_public_diagnosis(value: &str) -> String {
    if value.chars().count() <= MAX_DIAGNOSIS_CHARS {
        return value.to_owned();
    }
    let width = MAX_DIAGNOSIS_CHARS - PUBLIC_DIAGNOSIS_TRUNCATION.chars().count();
    format!(
        "{}{}",
        take_chars(value, width).trim_end(),
        PUBLIC_DIAGNOSIS_TRUNCATION
    )
}

fn abort_reason_text(reason: &str) -> Option<&'static str> {
    match reason {
        "tree-delta-breach" => Some(
            "Aborted the lane: a tree-delta permission breach found out-of-allowlist change(s), so this task will not be retried.",
        ),
        "tree-delta-ungated" => Some(
            "Aborted the lane: the tree-delta permission gate could not judge this pass -- the agent node failed, so the ownership node never ran and certified no paths, and this task declares no conflictDomains, leaving no allowlist. No out-of-allowlist change has been established. Declare conflictDomains for this task and re-arm; this task will not be retried until then.",
        ),
        _ => None,
    }
}

fn breach_note(diagnosis: &str, detail: &str, reason: &str) -> String {
    let mut parts = vec![abort_reason_text(reason).expect("validated abort reason")];
    if !diagnosis.is_empty() {
        parts.push(diagnosis);
    }
    let evidence;
    if !detail.is_empty() {
        evidence = format!("Witnessed evidence: {detail}");
        parts.push(&evidence);
    }
    parts.join("\n\n")
}

fn bounded_breach_note(diagnosis: &str, detail: &str, reason: &str) -> String {
    let mut composed = breach_note(diagnosis, detail, reason);
    let overflow = composed.chars().count().saturating_sub(MAX_DIAGNOSIS_CHARS);
    if overflow != 0 {
        let kept = diagnosis.chars().count().saturating_sub(overflow);
        composed = breach_note(take_chars(diagnosis, kept).trim_end(), detail, reason);
    }
    bound_public_diagnosis(&composed)
}

fn json_truthy(value: Option<&Json>) -> bool {
    match value {
        None | Some(Json::Null) | Some(Json::Bool(false)) => false,
        Some(Json::Number(number)) => number != "0" && number != "0.0",
        Some(Json::String(value)) => !value.is_empty(),
        Some(Json::Array(value)) => !value.is_empty(),
        Some(Json::Object(value)) => !value.is_empty(),
        Some(Json::Bool(true)) => true,
    }
}

fn classify_worker_outcome(value: Option<&Json>) -> Result<Option<WorkerOutcome>> {
    let Some(object) = value.and_then(Json::as_object) else {
        return Ok(None);
    };
    match object.get("outcome").and_then(Json::as_str) {
        Some("needs-authority") => {
            let object = object_complete(
                value.expect("an object was matched above"),
                &["outcome", "paths"],
                "needs-authority worker outcome",
            )?;
            let paths = normalize_paths(
                object.get("paths"),
                "needs-authority worker outcome.paths",
                true,
            )?
            .expect("required paths are present");
            if paths.len() > 128 {
                return Err(DriverError::new(
                    "needs-authority worker outcome.paths exceeds 128 entries",
                ));
            }
            if paths.iter().any(|path| path.chars().count() > 4_096) {
                return Err(DriverError::new(
                    "needs-authority worker outcome path exceeds 4096 characters",
                ));
            }
            Ok(Some(WorkerOutcome::NeedsAuthority { paths }))
        }
        Some("impossible") => {
            let object = object_complete(
                value.expect("an object was matched above"),
                &["outcome", "reason"],
                "impossible worker outcome",
            )?;
            Ok(Some(WorkerOutcome::Impossible {
                reason: required_text(
                    object.get("reason"),
                    "impossible worker outcome.reason",
                    12_000,
                )?,
            }))
        }
        _ => Ok(None),
    }
}

fn action_worker_outcome(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &[
            "campaign",
            "issue",
            "task",
            "taskUuid",
            "message",
            "attemptReceipts",
        ],
        "worker outcome brief",
    )?;
    let campaign = required_string(data.get("campaign"), "campaign", None)?;
    if !is_component(&campaign) {
        return Err(DriverError::new("campaign is not a safe component"));
    }
    let issue_number = campaign_issue(data.get("issue"))?.0;
    let task = data
        .get("task")
        .ok_or_else(|| DriverError::new("worker outcome brief.task is required"))?;
    let task_id = task_id(task)?.to_owned();
    if !is_task_id(&task_id) {
        return Err(DriverError::new("taskId is not safe"));
    }
    let task_revision = task_revision(task_object(task, "worker outcome brief.task")?)?
        .ok_or_else(|| DriverError::new("worker outcome task requires a completion revision"))?;
    let task_uuid = required_string(data.get("taskUuid"), "taskUuid", Some(36))?;
    let parsed_uuid = Uuid::parse_str(&task_uuid)
        .map_err(|_| DriverError::new("taskUuid must be a canonical UUID"))?;
    if parsed_uuid.to_string() != task_uuid {
        return Err(DriverError::new(
            "taskUuid must use canonical UUID spelling",
        ));
    }
    let outcome = classify_worker_outcome(data.get("message"))?.ok_or_else(|| {
        DriverError::new("worker final message has no structured outcome envelope")
    })?;
    let (recorded, receipt) = append_attempt_receipt(
        data.get("attemptReceipts"),
        &campaign,
        &issue_number,
        Json::object([
            ("kind", Json::from("worker-outcome")),
            ("taskId", Json::from(task_id.clone())),
            ("taskRevision", Json::from(task_revision.clone())),
            ("taskUuid", Json::from(task_uuid.clone())),
            ("outcome", Json::from(outcome.class())),
            ("paths", outcome.paths_json()),
            ("reason", outcome.reason_json()),
        ]),
    )?;
    let comment = receipt
        .as_object()
        .and_then(|record| record.get("comment"))
        .and_then(Json::as_str)
        .expect("append receipt returns a comment")
        .to_owned();
    Ok(Json::object([
        ("taskId", Json::from(task_id)),
        ("taskRevision", Json::from(task_revision)),
        ("taskUuid", Json::from(task_uuid)),
        ("outcome", Json::from(outcome.class())),
        ("comment", Json::from(comment)),
        ("paths", outcome.paths_json()),
        ("reason", outcome.reason_json()),
        ("recorded", Json::from(recorded)),
        ("attemptCost", Json::Number("0".to_owned())),
    ]))
}

#[allow(clippy::too_many_arguments)]
fn diagnosis_steering_result(
    task_id: &str,
    attempt: u64,
    comment: String,
    verdict: DiagnosisVerdict,
    proposal: Option<&Json>,
    posted: bool,
    redacted: bool,
    retry: Option<Json>,
) -> Json {
    let mut result = BTreeMap::from([
        ("kind".to_owned(), Json::from("diagnosis")),
        ("taskId".to_owned(), Json::from(task_id)),
        ("attempt".to_owned(), Json::Number(attempt.to_string())),
        ("comment".to_owned(), Json::from(comment)),
        ("verdict".to_owned(), Json::from(verdict.as_str())),
        (
            "blocked".to_owned(),
            Json::from(attempt == 2 || verdict == DiagnosisVerdict::Blocked),
        ),
        ("posted".to_owned(), Json::from(posted)),
        ("redacted".to_owned(), Json::from(redacted)),
        ("retry".to_owned(), retry.unwrap_or(Json::Null)),
    ]);
    if let Some(proposal) = proposal {
        result.insert("proposal".to_owned(), proposal.clone());
    }
    Json::Object(result)
}

fn transient_steering_result(retry: &Json) -> Json {
    let retry = retry
        .as_object()
        .expect("record_machine_retry returns an object");
    Json::object([
        ("kind", Json::from("retry")),
        (
            "taskId",
            retry
                .get("taskId")
                .expect("record_machine_retry returns taskId")
                .clone(),
        ),
        (
            "attempt",
            retry
                .get("attempt")
                .expect("record_machine_retry returns attempt")
                .clone(),
        ),
        (
            "comment",
            retry
                .get("comment")
                .expect("a posted machinery retry returns comment")
                .clone(),
        ),
        ("verdict", Json::from("transient")),
        ("blocked", Json::from(false)),
        (
            "posted",
            retry
                .get("posted")
                .expect("record_machine_retry returns posted")
                .clone(),
        ),
        (
            "redacted",
            retry
                .get("redacted")
                .expect("record_machine_retry returns redacted")
                .clone(),
        ),
        (
            "exhausted",
            retry
                .get("exhausted")
                .expect("record_machine_retry returns exhausted")
                .clone(),
        ),
        ("retry", Json::Null),
    ])
}

fn action_steer(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &[
            "campaign",
            "repository",
            "repositoryConfig",
            "issue",
            "taskId",
            "taskKind",
            "stage",
            "attempt",
            "diagnosis",
            "verdict",
            "proposal",
            "attemptReceipts",
            "checkpointCapture",
            "laneCapture",
            "gateEvidence",
            "breach",
            "breachDetail",
            "abortReason",
            "specRepository",
            "issueRepository",
        ],
        "steer brief",
    )?;
    let campaign = required_string(data.get("campaign"), "campaign", None)?;
    if !is_component(&campaign) {
        return Err(DriverError::new("campaign is not a safe component"));
    }
    let repository = repository_name(data.get("repository"), "repository")?;
    let config = repo_config(data.get("repositoryConfig"))?;
    campaign_coordinates(data, repository, config)?;
    let issue_number = campaign_issue(data.get("issue"))?.0;
    let task_id = required_string(data.get("taskId"), "taskId", None)?;
    if !is_task_id(&task_id) {
        return Err(DriverError::new("taskId is not safe"));
    }
    let attempt = data.get("attempt").and_then(Json::as_u64);
    if !matches!(attempt, Some(1 | 2)) {
        return Err(DriverError::new("attempt must equal 1 or 2"));
    }
    let attempt = attempt.expect("validated attempt");
    let task_kind = data
        .get("taskKind")
        .and_then(Json::as_str)
        .unwrap_or("implementation");
    if !matches!(task_kind, "implementation" | "checkpoint") {
        return Err(DriverError::new(
            "taskKind must equal implementation or checkpoint",
        ));
    }
    let requested_verdict =
        DiagnosisVerdict::parse(data.get("verdict"), "verdict")?.unwrap_or(if attempt == 2 {
            DiagnosisVerdict::Blocked
        } else {
            DiagnosisVerdict::Retry
        });
    let (proposal, proposal_redacted) = public_diagnosis_proposal(data.get("proposal"))?;
    if proposal.is_some() && requested_verdict != DiagnosisVerdict::Blocked {
        return Err(DriverError::new(
            "proposal is allowed only with a blocked diagnosis verdict",
        ));
    }
    let breach = json_truthy(data.get("breach"));
    let abort_reason = data
        .get("abortReason")
        .and_then(Json::as_str)
        .unwrap_or("tree-delta-breach");
    if abort_reason_text(abort_reason).is_none() {
        return Err(DriverError::new(
            "abortReason is not a declared lane-abort reason",
        ));
    }
    let state = campaign_attempt_state_all(data.get("attemptReceipts"), &campaign, &issue_number)?;
    let task_receipts: Vec<_> = state
        .diagnoses
        .iter()
        .filter(|receipt| receipt.task_id == task_id)
        .collect();
    let spent_machinery_retries = state
        .retries
        .iter()
        .filter(|receipt| receipt.task_id == task_id)
        .count();
    // The judge proposes; deterministic rails decide what can execute. A
    // checkpoint never receives a second dispatch, attempt two is the hard
    // per-input ceiling, and transient consumes (rather than bypasses) the
    // machinery retry budget.
    let transient_budget_exhausted = requested_verdict == DiagnosisVerdict::Transient
        && spent_machinery_retries >= MAX_MACHINE_RETRIES;
    // An adapter that stated its own terminal condition has settled the
    // verdict before the judge was asked (vestige-sweep V-16). This is kin to
    // the judge's `blocked`, but it needs no judgment slot: the rail is
    // deterministic because the adapter said so in its own stream, and
    // whatever the judge proposed -- retry, transient -- cannot outrank a
    // dated wall that named itself.
    let lane_outcome = lane_capture_outcome(data.get("laneCapture"))?;
    let adapter_terminal = lane_outcome
        .as_ref()
        .is_some_and(LaneOutcome::is_adapter_terminal);
    let mut verdict = if breach
        || task_kind == "checkpoint"
        || attempt == 2
        || transient_budget_exhausted
        || adapter_terminal
    {
        DiagnosisVerdict::Blocked
    } else {
        requested_verdict
    };
    let proposal = (verdict == DiagnosisVerdict::Blocked)
        .then_some(proposal)
        .flatten();

    if breach {
        if let Some(existing) = task_receipts.iter().find(|receipt| receipt.attempt == 2) {
            return Ok(diagnosis_steering_result(
                &task_id,
                2,
                existing.comment.clone(),
                DiagnosisVerdict::Blocked,
                existing.proposal.as_ref(),
                false,
                false,
                None,
            ));
        }
        let (capture_note, capture_redacted) = if data.contains_key("checkpointCapture") {
            let note = checkpoint_capture_note(data.get("checkpointCapture"), &campaign, &task_id)?;
            let (note, redacted) = redact_public_text(&note);
            (note, redacted)
        } else {
            (String::new(), false)
        };
        let diagnosis = required_text(data.get("diagnosis"), "diagnosis", MAX_DIAGNOSIS_CHARS)?;
        let (diagnosis, redacted_diagnosis) = redact_public_text(&diagnosis);
        let diagnosis = bound_public_diagnosis(&diagnosis);
        let diagnosis = validated_diagnosis(&diagnosis, data.get("gateEvidence"));
        let (detail, redacted_detail) = match data.get("breachDetail").and_then(Json::as_str) {
            Some(detail) if !detail.trim().is_empty() => {
                let (detail, redacted) = redact_public_text(detail);
                (bound_public_diagnosis(&detail), redacted)
            }
            _ => (String::new(), false),
        };
        let mut composed = bounded_breach_note(&diagnosis, &detail, abort_reason);
        composed = append_checkpoint_capture_note(&composed, &capture_note, MAX_DIAGNOSIS_CHARS);
        let mut posted_comment = None;
        for post_attempt in [1_u64, 2] {
            if task_receipts
                .iter()
                .any(|receipt| receipt.attempt == post_attempt)
            {
                continue;
            }
            posted_comment = Some(record_diagnosis(
                data.get("attemptReceipts"),
                &campaign,
                &issue_number,
                &task_id,
                post_attempt,
                &composed,
                DiagnosisVerdict::Blocked,
                (post_attempt == 1).then_some(proposal.as_ref()).flatten(),
            )?);
        }
        return Ok(diagnosis_steering_result(
            &task_id,
            2,
            posted_comment.expect("a missing breach attempt was recorded"),
            DiagnosisVerdict::Blocked,
            proposal.as_ref(),
            true,
            redacted_diagnosis || redacted_detail || capture_redacted || proposal_redacted,
            None,
        ));
    }

    if let Some(existing) = task_receipts
        .iter()
        .find(|receipt| receipt.attempt == attempt)
    {
        return Ok(diagnosis_steering_result(
            &task_id,
            attempt,
            existing.comment.clone(),
            existing.effective_verdict(),
            existing.proposal.as_ref(),
            false,
            false,
            None,
        ));
    }
    let expected_attempt = task_receipts.len() as u64 + 1;
    if attempt != expected_attempt {
        return Err(DriverError::new(format!(
            "task {task_id:?} diagnosis attempt {attempt} is not next after {} durable receipts",
            task_receipts.len()
        )));
    }
    let (capture_note, capture_redacted) = if data.contains_key("checkpointCapture") {
        let note = checkpoint_capture_note(data.get("checkpointCapture"), &campaign, &task_id)?;
        let (note, redacted) = redact_public_text(&note);
        (note, redacted)
    } else {
        (String::new(), false)
    };
    let diagnosis = required_text(data.get("diagnosis"), "diagnosis", MAX_DIAGNOSIS_CHARS)?;
    let (diagnosis, redacted) = redact_public_text(&diagnosis);
    let diagnosis = bound_public_diagnosis(&diagnosis);
    let diagnosis = validated_diagnosis(&diagnosis, data.get("gateEvidence"));
    let diagnosis = append_checkpoint_capture_note(&diagnosis, &capture_note, MAX_DIAGNOSIS_CHARS);
    // The adapter's own account of how the lane ended, and what the lane
    // spent saying it, land in the durable receipt (eta R1 1.6 and 1.8)
    // rather than in a session ledger somebody reconstructs by hand. This is
    // the half of the fix an operator actually reads: a wall that names its
    // reset hour is only useful if the hour survives into the record.
    let (lane_note, lane_note_redacted) = match lane_outcome.as_ref().map(LaneOutcome::note) {
        Some(note) if !note.is_empty() => redact_public_text(&note),
        _ => (String::new(), false),
    };
    let diagnosis = append_machine_note(
        &diagnosis,
        &lane_note,
        MAX_DIAGNOSIS_CHARS,
        "\n[... earlier machine detail shortened for the lane outcome envelope ...]",
    );
    // A transient diagnosis is a reason for the existing free machinery
    // retry, not a task-attempt receipt. Keeping it out of the diagnosis
    // ledger means the redispatch remains attempt 1 while the independent
    // machinery budget is charged.
    if verdict == DiagnosisVerdict::Transient {
        let stage = data
            .get("stage")
            .and_then(Json::as_str)
            .unwrap_or("diagnosis");
        if !Regex::new(r"^[a-z][a-z0-9:._-]{0,63}$")
            .expect("static stage regex")
            .is_match(stage)
        {
            return Err(DriverError::new("stage is not a safe campaign stage name"));
        }
        let retry = record_machine_retry(
            data.get("attemptReceipts"),
            &campaign,
            &issue_number,
            &task_id,
            stage,
            &format!("The judge classified this failure as transient.\n\n{diagnosis}"),
            "",
        )?;
        if retry
            .as_object()
            .and_then(|retry| retry.get("posted"))
            .and_then(Json::as_bool)
            == Some(true)
        {
            return Ok(transient_steering_result(&retry));
        }
        verdict = DiagnosisVerdict::Blocked;
    }
    let comment = record_diagnosis(
        data.get("attemptReceipts"),
        &campaign,
        &issue_number,
        &task_id,
        attempt,
        &diagnosis,
        verdict,
        proposal.as_ref(),
    )?;
    Ok(diagnosis_steering_result(
        &task_id,
        attempt,
        comment,
        verdict,
        proposal.as_ref(),
        true,
        redacted || capture_redacted || proposal_redacted || lane_note_redacted,
        None,
    ))
}

/// The lane's own capture, as a brief names it, classified into an outcome.
///
/// The adapter's declarations travel in the brief rather than being resolved
/// from ambient configuration. The driver runs as an ordinary job and holds
/// no adapter catalog, and a classification that stops a retry ladder has to
/// be reproducible from the brief alone -- the same rule every other
/// deterministic decision here follows.
///
/// Absent is admissible and means only that this pass named no capture: a
/// lane whose stream was never handed over is classified exactly as it was
/// before, by the machinery's own code.
fn lane_capture_outcome(value: Option<&Json>) -> Result<Option<LaneOutcome>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if matches!(value, Json::Null) {
        return Ok(None);
    }
    let capture = object_exact(
        value,
        &[
            "adapter",
            "adapterConfig",
            "stdoutPath",
            "stderrPath",
            "failureCode",
        ],
        "laneCapture",
    )?;
    let adapter = required_string(capture.get("adapter"), "laneCapture.adapter", Some(128))?;
    if !is_component(&adapter) {
        return Err(DriverError::new("laneCapture.adapter is not a safe name"));
    }
    let declarations = capture
        .get("adapterConfig")
        .filter(|value| value.as_object().is_some())
        .ok_or_else(|| DriverError::new("laneCapture.adapterConfig must be an object"))?;
    let declarations: AdapterConfig =
        serde_json::from_str(&declarations.stringify()).map_err(|error| {
            DriverError::new(format!(
                "laneCapture.adapterConfig is not an adapter declaration: {error}"
            ))
        })?;
    let stdout = lane_capture_path(capture.get("stdoutPath"), "laneCapture.stdoutPath")?
        .ok_or_else(|| DriverError::new("laneCapture.stdoutPath is required"))?;
    let stderr = lane_capture_path(capture.get("stderrPath"), "laneCapture.stderrPath")?;
    // A capture the retention horizon has already reaped states nothing, and
    // a re-run of a settled pass must stay idempotent rather than becoming a
    // hard driver failure the second time it is asked. So an absent file
    // degrades to the classification this pass would have reached without it;
    // an unreadable one still refuses, because that is a capture claiming to
    // exist and failing to be read.
    if !stdout.is_file() {
        return Ok(None);
    }
    let stderr = stderr.filter(|path| path.is_file());
    let failure_code = match capture.get("failureCode") {
        None | Some(Json::Null) => None,
        Some(value) => Some(required_string(
            Some(value),
            "laneCapture.failureCode",
            Some(128),
        )?),
    };
    adapter_outcome::classify_paths(
        &adapter,
        &declarations,
        &stdout,
        stderr.as_deref(),
        failure_code.as_deref(),
    )
    .map(Some)
}

fn lane_capture_path(value: Option<&Json>, context: &str) -> Result<Option<PathBuf>> {
    match value {
        None | Some(Json::Null) => Ok(None),
        Some(value) => {
            let text = required_string(Some(value), context, Some(700))?;
            let path = PathBuf::from(text);
            if !path.is_absolute() {
                return Err(DriverError::new(format!("{context} must be absolute")));
            }
            Ok(Some(path))
        }
    }
}

/// The retry result a stopped ladder produces.
///
/// It is deliberately the same shape a spent budget returns -- nothing
/// appended, `posted: false` -- because the flow's reading of that shape is
/// already exactly right: no dispatch happened, so the failure is steered
/// rather than re-run. What differs is the reason, and the reason is a fact
/// the adapter stated; it reaches the durable ledger through the steering
/// diagnosis that follows, which carries the message and the token spend
/// (see [`adapter_outcome::LaneOutcome::note`]).
fn no_machinery_retry(
    source: Option<&Json>,
    campaign: &str,
    issue_number: &str,
    task_id: &str,
) -> Result<Json> {
    let state = campaign_attempt_state_all(source, campaign, issue_number)?;
    let spent = state
        .retries
        .iter()
        .filter(|receipt| receipt.task_id == task_id)
        .count();
    Ok(Json::object([
        ("taskId", Json::from(task_id)),
        ("attempt", Json::from(spent)),
        ("comment", Json::Null),
        ("exhausted", Json::from(true)),
        ("posted", Json::from(false)),
        ("redacted", Json::from(false)),
    ]))
}

fn record_machine_retry(
    source: Option<&Json>,
    campaign: &str,
    issue_number: &str,
    task_id: &str,
    stage: &str,
    detail: &str,
    capture_note: &str,
) -> Result<Json> {
    let state = campaign_attempt_state_all(source, campaign, issue_number)?;
    let spent = state
        .retries
        .iter()
        .filter(|receipt| receipt.task_id == task_id)
        .count();
    if spent >= MAX_MACHINE_RETRIES {
        return Ok(Json::object([
            ("taskId", Json::from(task_id)),
            ("attempt", Json::from(spent)),
            ("comment", Json::Null),
            ("exhausted", Json::from(true)),
            ("posted", Json::from(false)),
            ("redacted", Json::from(false)),
        ]));
    }
    let attempt = spent + 1;
    let mut raw_reason = format!("Stage `{stage}` faulted.");
    if !capture_note.is_empty() {
        raw_reason.push_str(&format!("\n\n{capture_note}"));
    }
    raw_reason.push_str(&format!("\n\n{detail}"));
    let (mut reason, redacted) = redact_public_text(&raw_reason);
    if reason.chars().count() > MAX_RETRY_CHARS {
        reason = format!("{}...", take_chars(&reason, MAX_RETRY_CHARS - 3).trim_end());
    }
    let reason = required_text(Some(&Json::from(reason)), "retry reason", MAX_RETRY_CHARS)?;
    let (created, receipt) = append_attempt_receipt(
        source,
        campaign,
        issue_number,
        Json::object([
            ("kind", Json::from("retry")),
            ("taskId", Json::from(task_id)),
            ("attempt", Json::from(attempt)),
            ("reason", Json::from(reason)),
            ("redaction", Json::from(PUBLIC_REDACTION)),
        ]),
    )?;
    let comment = receipt
        .as_object()
        .and_then(|receipt| receipt.get("comment"))
        .and_then(Json::as_str)
        .expect("append receipt returns comment")
        .to_owned();
    Ok(Json::object([
        ("taskId", Json::from(task_id)),
        ("attempt", Json::from(attempt)),
        ("comment", Json::from(comment)),
        ("exhausted", Json::from(attempt == MAX_MACHINE_RETRIES)),
        ("posted", Json::from(created)),
        ("redacted", Json::from(redacted)),
    ]))
}

fn action_retry(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &[
            "campaign",
            "repository",
            "repositoryConfig",
            "issue",
            "taskId",
            "stage",
            "detail",
            "attemptReceipts",
            "checkpointCapture",
            "laneCapture",
            "specRepository",
            "issueRepository",
        ],
        "retry brief",
    )?;
    let campaign = required_string(data.get("campaign"), "campaign", None)?;
    if !is_component(&campaign) {
        return Err(DriverError::new("campaign is not a safe component"));
    }
    let repository = repository_name(data.get("repository"), "repository")?;
    let config = repo_config(data.get("repositoryConfig"))?;
    campaign_coordinates(data, repository, config)?;
    let issue_number = campaign_issue(data.get("issue"))?.0;
    let task_id = required_string(data.get("taskId"), "taskId", None)?;
    if !is_task_id(&task_id) {
        return Err(DriverError::new("taskId is not safe"));
    }
    let stage = required_string(data.get("stage"), "stage", None)?;
    if !Regex::new(r"^[a-z][a-z0-9:._-]{0,63}$")
        .expect("static stage regex")
        .is_match(&stage)
    {
        return Err(DriverError::new("stage is not a safe campaign stage name"));
    }
    let detail = required_text(data.get("detail"), "detail", MAX_RETRY_CHARS)?;
    // The lane's own stream is read before the budget is charged, because an
    // adapter that stated its own terminal condition has already answered the
    // question a retry would ask (vestige-sweep V-16). A quota wall is dated
    // and non-retryable; re-dispatching against it is how five hours went
    // last time, and the message naming the reset hour was in this capture
    // the whole time.
    if lane_capture_outcome(data.get("laneCapture"))?
        .is_some_and(|outcome| !outcome.dispatches_retry())
    {
        return no_machinery_retry(
            data.get("attemptReceipts"),
            &campaign,
            &issue_number,
            &task_id,
        );
    }
    let capture_note = if data.contains_key("checkpointCapture") {
        checkpoint_capture_note(data.get("checkpointCapture"), &campaign, &task_id)?
    } else {
        String::new()
    };
    record_machine_retry(
        data.get("attemptReceipts"),
        &campaign,
        &issue_number,
        &task_id,
        &stage,
        &detail,
        &capture_note,
    )
}

fn compact_summary(value: &str, maximum: usize) -> String {
    let compact = collapse_whitespace(value);
    if compact.chars().count() <= maximum {
        compact
    } else {
        format!("{}...", take_chars(&compact, maximum - 3))
    }
}

fn rendered_proposal_diff(proposal: &Json) -> Result<Vec<String>> {
    let value: serde_json::Value =
        serde_json::from_str(&proposal.stringify()).map_err(|error| {
            DriverError::new(format!(
                "cannot render structured diagnosis proposal: {error}"
            ))
        })?;
    let rendered = serde_json::to_string_pretty(&value).map_err(|error| {
        DriverError::new(format!(
            "cannot render structured diagnosis proposal: {error}"
        ))
    })?;
    Ok(std::iter::once("```diff".to_owned())
        .chain(rendered.lines().map(|line| format!("+ {line}")))
        .chain(std::iter::once("```".to_owned()))
        .collect())
}

fn append_diagnosis_report(
    lines: &mut Vec<String>,
    diagnoses: &[Json],
    public_values: &mut Vec<String>,
) -> Result<()> {
    if !diagnoses.is_empty() {
        lines.extend([String::new(), "Accumulated machine diagnoses:".to_owned()]);
    }
    let mut proposals = Vec::new();
    for diagnosis in diagnoses {
        let diagnosis = diagnosis
            .as_object()
            .ok_or_else(|| DriverError::new("reconciliation diagnosis must be an object"))?;
        let task_id = diagnosis
            .get("taskId")
            .and_then(Json::as_str)
            .unwrap_or_default();
        let attempt = diagnosis
            .get("attempt")
            .and_then(Json::as_u64)
            .unwrap_or_default();
        let text = diagnosis
            .get("diagnosis")
            .and_then(Json::as_str)
            .unwrap_or_default();
        let verdict = diagnosis
            .get("verdict")
            .and_then(Json::as_str)
            .unwrap_or(if attempt == 2 { "blocked" } else { "retry" });
        lines.push(format!(
            "- `{task_id}` attempt {attempt} ({verdict}): {}",
            compact_summary(text, 64)
        ));
        public_values.push(text.to_owned());
        if let Some(proposal) = diagnosis.get("proposal") {
            proposals.push((task_id.to_owned(), attempt, proposal.clone()));
            public_values.push(proposal.stringify());
        }
    }
    if !proposals.is_empty() {
        lines.extend([
            String::new(),
            "Prepared worklist proposals (ready diffs):".to_owned(),
        ]);
        for (task_id, attempt, proposal) in proposals {
            lines.extend([String::new(), format!("`{task_id}` attempt {attempt}:")]);
            lines.extend(rendered_proposal_diff(&proposal)?);
        }
    }
    Ok(())
}

fn checkpoint_capture_paths(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let prefix = "Checkpoint capture: ";
    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    for value in values {
        for line in value.lines() {
            if let Some(path) = line.strip_prefix(prefix) {
                if !path.is_empty() && seen.insert(path.to_owned()) {
                    paths.push(path.to_owned());
                }
            }
        }
    }
    paths
}

fn escalation_snapshot(value: &Json) -> Result<&BTreeMap<String, Json>> {
    value
        .as_object()
        .ok_or_else(|| DriverError::new("reconciliation must be an object"))
}

fn action_escalate(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &[
            "campaign",
            "campaignIdentity",
            "repository",
            "repositoryConfig",
            "issue",
            "worklist",
            "maxTasks",
            "maxParallel",
            "gateSet",
            "steeringHighWater",
            "taskInputHashes",
            "attemptReceipts",
            "specRepository",
            "issueRepository",
        ],
        "escalate brief",
    )?;
    let first = action_reconcile(brief)?;
    let first_object = escalation_snapshot(&first)?;
    if first_object.get("complete").and_then(Json::as_bool) == Some(true)
        || first_object.get("quiescent").and_then(Json::as_bool) != Some(true)
    {
        return Err(DriverError::new(
            "campaign escalation requires an incomplete empty frontier",
        ));
    }
    let first_diagnoses = first_object
        .get("diagnoses")
        .and_then(Json::as_array)
        .unwrap_or_default();
    let first_retries = first_object
        .get("retries")
        .and_then(Json::as_array)
        .unwrap_or_default();
    if let Some(existing) = first_object.get("escalation").and_then(Json::as_str) {
        return Ok(Json::object([
            ("posted", Json::from(false)),
            ("comment", Json::from(existing)),
            ("summary", Json::Null),
            ("diagnosisCount", Json::from(first_diagnoses.len())),
            ("retryCount", Json::from(first_retries.len())),
        ]));
    }
    let reconciliation = action_reconcile(brief)?;
    let object = escalation_snapshot(&reconciliation)?;
    if object.get("complete").and_then(Json::as_bool) == Some(true)
        || object.get("quiescent").and_then(Json::as_bool) != Some(true)
    {
        return Err(DriverError::new(
            "campaign quiescence changed during the pre-post durable refresh; refusing to post outcome=quiescent",
        ));
    }
    let diagnoses = object
        .get("diagnoses")
        .and_then(Json::as_array)
        .ok_or_else(|| DriverError::new("reconciliation.diagnoses must be an array"))?;
    let retries = object
        .get("retries")
        .and_then(Json::as_array)
        .ok_or_else(|| DriverError::new("reconciliation.retries must be an array"))?;
    let outcomes = object
        .get("outcomes")
        .and_then(Json::as_array)
        .ok_or_else(|| DriverError::new("reconciliation.outcomes must be an array"))?;
    if let Some(existing) = object.get("escalation").and_then(Json::as_str) {
        return Ok(Json::object([
            ("posted", Json::from(false)),
            ("comment", Json::from(existing)),
            ("summary", Json::Null),
            ("diagnosisCount", Json::from(diagnoses.len())),
            ("retryCount", Json::from(retries.len())),
        ]));
    }
    let campaign = required_string(object.get("campaign"), "campaign", None)?;
    let repository = repository_name(object.get("repository"), "repository")?;
    let code_config = repo_config(data.get("repositoryConfig"))?;
    let (_, _, target) = campaign_coordinates(data, repository, code_config)?;
    let issue_number = campaign_issue(data.get("issue"))?.0;
    let blocked = object
        .get("blocked")
        .and_then(Json::as_array)
        .ok_or_else(|| DriverError::new("reconciliation.blocked must be an array"))?;
    let direct: Vec<_> = blocked
        .iter()
        .filter_map(Json::as_object)
        .filter_map(|fact| {
            let task_id = fact.get("taskId")?.as_str()?;
            let blocked_by = fact.get("blockedBy")?.as_array()?;
            blocked_by
                .iter()
                .any(|root| root.as_str() == Some(task_id))
                .then(|| task_id.to_owned())
        })
        .collect();
    let has_authority_request = outcomes.iter().any(|outcome| {
        outcome
            .as_object()
            .and_then(|outcome| outcome.get("outcome"))
            .and_then(Json::as_str)
            == Some("needs-authority")
    });
    let blocking_rule = if has_authority_request {
        "Tally stopped because each directly blocked task received a blocked judge verdict, reached the hard attempt cap, or requested authority-surface paths; authority requests spend no attempt."
    } else {
        "Tally stopped because each directly blocked task received a blocked judge verdict or reached the hard attempt cap."
    };
    let mut lines = vec![
        "### Spec-build escalation: frontier quiescent".to_owned(),
        String::new(),
        "The worklist is incomplete and no unblocked task is dispatchable.".to_owned(),
        blocking_rule.to_owned(),
        String::new(),
        format!(
            "Directly blocked tasks: {}",
            direct
                .iter()
                .map(|task_id| format!("`{task_id}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        format!(
            "Blocked worklist tasks (including descendants): {}",
            blocked.len()
        ),
    ];
    let mut public_values = Vec::new();
    if !outcomes.is_empty() {
        lines.extend([String::new(), "Structured worker outcomes:".to_owned()]);
        for outcome in outcomes {
            let outcome = outcome
                .as_object()
                .ok_or_else(|| DriverError::new("reconciliation outcome must be an object"))?;
            let task_id = outcome
                .get("taskId")
                .and_then(Json::as_str)
                .unwrap_or_default();
            match outcome.get("outcome").and_then(Json::as_str) {
                Some("needs-authority") => {
                    let paths = outcome
                        .get("paths")
                        .and_then(Json::as_array)
                        .unwrap_or_default()
                        .iter()
                        .filter_map(Json::as_str)
                        .map(|path| format!("`{path}`"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    lines.push(format!(
                        "- `{task_id}` needs authority (attempt cost 0): {paths}"
                    ));
                }
                Some("impossible") => {
                    let reason = outcome
                        .get("reason")
                        .and_then(Json::as_str)
                        .unwrap_or_default();
                    lines.push(format!(
                        "- `{task_id}` worker impossibility claim (not a verdict): {}",
                        compact_summary(reason, 160)
                    ));
                    public_values.push(reason.to_owned());
                }
                _ => {}
            }
        }
    }
    append_diagnosis_report(&mut lines, diagnoses, &mut public_values)?;
    if !retries.is_empty() {
        lines.extend([
            String::new(),
            "Campaign machinery faults that bought a retry:".to_owned(),
        ]);
        for retry in retries {
            let retry = retry
                .as_object()
                .ok_or_else(|| DriverError::new("reconciliation retry must be an object"))?;
            let task_id = retry
                .get("taskId")
                .and_then(Json::as_str)
                .unwrap_or_default();
            let attempt = retry
                .get("attempt")
                .and_then(Json::as_u64)
                .unwrap_or_default();
            let text = retry
                .get("reason")
                .and_then(Json::as_str)
                .unwrap_or_default();
            lines.push(format!(
                "- `{task_id}` fault {attempt}: {}",
                compact_summary(text, 64)
            ));
            public_values.push(text.to_owned());
        }
    }
    let capture_paths = checkpoint_capture_paths(public_values);
    if !capture_paths.is_empty() {
        lines.extend([String::new(), "Checkpoint captures:".to_owned()]);
        lines.extend(capture_paths.into_iter().map(|path| format!("- {path}")));
    }
    let warnings = object
        .get("warnings")
        .and_then(Json::as_array)
        .unwrap_or_default();
    if !warnings.is_empty() {
        lines.extend([String::new(), "Reconciler warnings:".to_owned()]);
        lines.extend(
            warnings
                .iter()
                .take(12)
                .filter_map(Json::as_str)
                .map(|warning| format!("- {}", compact_summary(warning, 200))),
        );
    }
    let body = lines.join("\n");
    if body.chars().count() > 60_000 {
        return Err(DriverError::new(
            "machine escalation exceeds 60,000 characters",
        ));
    }
    let projected: CampaignReconciliation = serde_json::from_str(&reconciliation.stringify())
        .map_err(|error| {
            DriverError::new(format!(
                "cannot project campaign reconciliation through tally-core: {error}"
            ))
        })?;
    let digest = fold_campaign_digest(&projected, "quiescent");
    let summary = publish_closing_summary(
        &target.repository,
        &target.config,
        &campaign,
        &issue_number,
        &digest,
    )?;
    let (created, receipt) = append_attempt_receipt(
        data.get("attemptReceipts"),
        &campaign,
        &issue_number,
        Json::object([
            ("kind", Json::from("escalation")),
            ("body", Json::from(body)),
        ]),
    )?;
    let comment = receipt
        .as_object()
        .and_then(|receipt| receipt.get("comment"))
        .and_then(Json::as_str)
        .expect("append receipt returns comment")
        .to_owned();
    Ok(Json::object([
        ("posted", Json::from(created)),
        ("comment", Json::from(comment)),
        ("summary", Json::from(summary)),
        ("diagnosisCount", Json::from(diagnoses.len())),
        ("retryCount", Json::from(retries.len())),
    ]))
}

#[derive(Debug)]
struct ContinuationSpec {
    argv: Vec<String>,
    pool: Vec<String>,
    priority: String,
    runtime_seconds: Option<u64>,
    events_dir: PathBuf,
}

fn continuation_spec(value: Option<&Json>) -> Result<ContinuationSpec> {
    let value = value.ok_or_else(|| DriverError::new("continuation must be an object"))?;
    let spec = object_exact(
        value,
        &["argv", "pool", "priority", "runtimeMaxSec", "eventsDir"],
        "continuation",
    )?;
    let events_dir = PathBuf::from(required_string(
        spec.get("eventsDir"),
        "continuation.eventsDir",
        Some(4_096),
    )?);
    if !events_dir.is_absolute() {
        return Err(DriverError::new("continuation.eventsDir must be absolute"));
    }
    let priority = spec
        .get("priority")
        .and_then(Json::as_str)
        .filter(|priority| matches!(*priority, "interrupt" | "high" | "medium" | "low"))
        .ok_or_else(|| DriverError::new("continuation.priority is not a declared priority"))?
        .to_owned();
    let runtime_seconds = match spec.get("runtimeMaxSec") {
        None | Some(Json::Null) => None,
        value => Some(positive_u64(value, "continuation.runtimeMaxSec")?),
    };
    let pool = string_list(spec.get("pool"), "continuation.pool", true)?;
    if pool.iter().collect::<BTreeSet<_>>().len() != pool.len() {
        return Err(DriverError::new("continuation.pool must not repeat a pool"));
    }
    Ok(ContinuationSpec {
        argv: argv_list(spec.get("argv"), "continuation.argv")?,
        pool,
        priority,
        runtime_seconds,
        events_dir,
    })
}

fn continuation_run_id(
    campaign: &str,
    repository: &str,
    issue_number: &str,
    run_id: &str,
) -> String {
    let material = [
        "spec-build-continuation:v1",
        campaign,
        repository,
        issue_number,
        run_id,
    ]
    .join("\n");
    format!(
        "continuation-{}",
        &sha256::digest(material.as_bytes())[..32]
    )
}

fn write_continuation_event(
    spec: &ContinuationSpec,
    dedup_key: &str,
    brief: Option<Json>,
) -> Result<(bool, PathBuf)> {
    let mut payload = BTreeMap::from([
        (
            "argv".to_owned(),
            Json::Array(spec.argv.iter().cloned().map(Json::from).collect()),
        ),
        ("adapter".to_owned(), Json::from("shell")),
        (
            "pool".to_owned(),
            Json::Array(spec.pool.iter().cloned().map(Json::from).collect()),
        ),
        ("priority".to_owned(), Json::from(spec.priority.clone())),
        ("source".to_owned(), Json::from("events-dir")),
        ("dedupKey".to_owned(), Json::from(dedup_key)),
        (
            "submission".to_owned(),
            Json::object([("mode", Json::from("full"))]),
        ),
        (
            "evidence".to_owned(),
            Json::Array(vec![Json::from("exit:0")]),
        ),
        ("noEnqueue".to_owned(), Json::from(false)),
    ]);
    if let Some(runtime) = spec.runtime_seconds {
        payload.insert(
            "runtimeMaxSec".to_owned(),
            Json::Number(runtime.to_string()),
        );
    }
    if let Some(brief) = brief {
        payload.insert("brief".to_owned(), brief);
    }
    let name = format!(
        "campaign-continuation-{}",
        &sha256::digest(dedup_key.as_bytes())[..32]
    );
    let path = spec.events_dir.join(format!("{name}.json"));
    let rendered = format!("{}\n", Json::Object(payload).stringify());
    if rendered.len() > MAX_CONTINUATION_EVENT_BYTES {
        return Err(DriverError::new(
            "continuation payload exceeds the bounded event size",
        ));
    }
    let temporary = spec
        .events_dir
        .join(format!(".{name}.{}.tmp", Uuid::new_v4()));
    let result = (|| {
        fs::create_dir_all(&spec.events_dir).map_err(|error| {
            DriverError::new(format!(
                "cannot write the continuation event {}: {error}",
                path.display()
            ))
        })?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| {
                DriverError::new(format!(
                    "cannot write the continuation event {}: {error}",
                    path.display()
                ))
            })?;
        file.write_all(rendered.as_bytes()).map_err(|error| {
            DriverError::new(format!(
                "cannot write the continuation event {}: {error}",
                path.display()
            ))
        })?;
        file.sync_all().map_err(|error| {
            DriverError::new(format!(
                "cannot write the continuation event {}: {error}",
                path.display()
            ))
        })?;
        let created = match fs::hard_link(&temporary, &path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => {
                return Err(DriverError::new(format!(
                    "cannot write the continuation event {}: {error}",
                    path.display()
                )))
            }
        };
        Ok((created, path.clone()))
    })();
    let _ = fs::remove_file(&temporary);
    result
}

fn action_continue(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &[
            "campaign",
            "repository",
            "repositoryConfig",
            "issue",
            "runId",
            "continuation",
            "brief",
            "specRepository",
            "issueRepository",
        ],
        "continue brief",
    )?;
    let campaign = required_string(data.get("campaign"), "campaign", None)?;
    if !is_component(&campaign) {
        return Err(DriverError::new("campaign is not a safe component"));
    }
    let repository = repository_name(data.get("repository"), "repository")?;
    let config = repo_config(data.get("repositoryConfig"))?;
    let (_, _, target) = campaign_coordinates(data, repository.clone(), config)?;
    let issue_number = campaign_issue(data.get("issue"))?.0;
    let run_id = required_string(data.get("runId"), "runId", Some(512))?;
    let spec = continuation_spec(data.get("continuation"))?;
    let next_run_id = continuation_run_id(&campaign, &repository, &issue_number, &run_id);
    let dedup_key = format!("campaign-continuation:{repository}:{issue_number}:{next_run_id}");
    let next_brief = match data.get("brief") {
        None | Some(Json::Null) => None,
        Some(Json::Object(brief)) => {
            let mut brief = brief.clone();
            brief.insert("runId".to_owned(), Json::from(next_run_id.clone()));
            Some(Json::Object(brief))
        }
        _ => {
            return Err(DriverError::new(
                "continue brief.brief must be an object or null",
            ))
        }
    };
    let (created, path) = write_continuation_event(&spec, &dedup_key, next_brief)?;
    let reference = format!(
        "{}/continuation/{next_run_id}",
        local_state_prefix(&campaign, &issue_number)
    );
    let expected = Json::object([
        ("schemaVersion", Json::Number("1".to_owned())),
        ("kind", Json::from("continuation")),
        ("campaign", Json::from(campaign)),
        ("issueNumber", Json::from(issue_number)),
        ("runId", Json::from(run_id)),
        ("dedupKey", Json::from(dedup_key.clone())),
    ]);
    let (_, observed) = write_local_blob(&target.config, &reference, &expected)?;
    if observed != expected {
        return Err(DriverError::new(format!(
            "local campaign continuation {reference:?} disagrees with this pass"
        )));
    }
    Ok(Json::object([
        ("event", Json::from(path.to_string_lossy().into_owned())),
        ("dedupKey", Json::from(dedup_key)),
        ("runId", Json::from(next_run_id)),
        ("created", Json::from(created)),
        (
            "receipt",
            Json::from(format!("local://{}/{reference}", target.repository)),
        ),
    ]))
}

#[derive(Debug)]
struct CheckpointCapture {
    path: PathBuf,
    stdout_truncated: bool,
    stderr_truncated: bool,
    verdict: String,
}

fn read_capture_tail(path: &Path) -> Result<(String, bool)> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            DriverError::new(format!(
                "cannot open checkpoint capture stream {}: {error}",
                path.display()
            ))
        })?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(DriverError::new(format!(
            "checkpoint capture stream is not a private regular file: {}",
            path.display()
        )));
    }
    let window = CHECKPOINT_CAPTURE_MAX_BYTES as u64;
    if metadata.len() <= window {
        let mut captured = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut captured)?;
        let text = String::from_utf8_lossy(&captured)
            .replace('\0', "�")
            .to_owned();
        return Ok((text, false));
    }
    // Error-aware, like the executor's failure excerpt (vestige-sweep V-5):
    // a probe of the capture's head, where the first error block sits, plus
    // the tail — composed by the same derivation-aware renderer the executor
    // uses, so both homes speak one vocabulary. Both reads are bounded by
    // the window no matter how long the gate ran.
    let probe_len = window.min(metadata.len() - window);
    let probe = read_capture_window(&mut file, 0, probe_len)?;
    let tail = read_capture_window(&mut file, metadata.len() - window, window)?;
    let excerpt = tally_core::executor::compose_capture_excerpt(
        &String::from_utf8_lossy(&probe).replace('\0', "�"),
        &String::from_utf8_lossy(&tail).replace('\0', "�"),
        probe_len == metadata.len() - window,
        CHECKPOINT_CAPTURE_MAX_BYTES,
    );
    Ok((excerpt.text, true))
}

fn read_capture_window(file: &mut File, start: u64, len: u64) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(start))?;
    let mut captured = Vec::with_capacity(len as usize);
    file.take(len).read_to_end(&mut captured)?;
    if start != 0 {
        let prefix = captured
            .iter()
            .take_while(|byte| (**byte & 0b1100_0000) == 0b1000_0000)
            .count();
        captured.drain(..prefix);
    }
    Ok(captured)
}

/// An error-aware excerpt of an in-memory capture text within `window`,
/// mirroring the executor's windowed read: a probe of the head plus the
/// tail, composed by the same derivation-aware renderer, so a causal error
/// far above the tail survives here exactly as it does in the failure fact.
fn excerpt_text_window(text: &str, window: usize) -> tally_core::executor::CaptureExcerpt {
    if text.len() <= window {
        return tally_core::executor::CaptureExcerpt {
            text: text.to_owned(),
            truncated: false,
        };
    }
    let probe_len = window.min(text.len() - window);
    let mut probe_end = probe_len;
    while !text.is_char_boundary(probe_end) {
        probe_end -= 1;
    }
    let mut tail_start = text.len() - window;
    while !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    tally_core::executor::compose_capture_excerpt(
        &text[..probe_end],
        &text[tail_start..],
        probe_end == tail_start,
        window,
    )
}

fn write_private_json(path: &Path, value: &Json) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| DriverError::new("checkpoint capture path has no parent"))?;
    fs::create_dir_all(parent)?;
    let metadata = fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(DriverError::new(format!(
            "checkpoint capture parent must be a real directory: {}",
            parent.display()
        )));
    }
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let temporary = path.with_file_name(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(CHECKPOINT_CAPTURE_FILE),
        std::process::id(),
        Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(O_CLOEXEC | O_NOFOLLOW)
            .open(&temporary)?;
        file.write_all(value.stringify().as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(O_CLOEXEC | O_DIRECTORY)
            .open(parent)?;
        directory.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(|error: std::io::Error| {
        DriverError::new(format!(
            "cannot persist checkpoint capture {}: {error}",
            path.display()
        ))
    })
}

fn json_integer_or_null(value: Option<&Json>, context: &str) -> Result<Json> {
    match value {
        Some(Json::Null) | None => Ok(Json::Null),
        Some(Json::Number(number))
            if number
                .strip_prefix('-')
                .unwrap_or(number)
                .bytes()
                .all(|byte| byte.is_ascii_digit())
                && number != "-" =>
        {
            Ok(Json::Number(number.clone()))
        }
        _ => Err(DriverError::new(format!(
            "{context} must be an integer or null"
        ))),
    }
}

fn persist_checkpoint_capture(
    capture_root_value: Option<&Json>,
    execution_value: Option<&Json>,
    campaign: &str,
    issue_number: &str,
    task_id: &str,
) -> Result<CheckpointCapture> {
    let capture_root = PathBuf::from(required_string(
        capture_root_value,
        "captureRoot",
        Some(4_096),
    )?);
    let valid_root = capture_root.is_absolute()
        && capture_root
            .file_name()
            .is_some_and(|name| name == "archive")
        && capture_root
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "capture");
    if !valid_root {
        return Err(DriverError::new(
            "captureRoot must name an absolute capture/archive directory",
        ));
    }
    if capture_root.exists() && (is_symlink(&capture_root) || !capture_root.is_dir()) {
        return Err(DriverError::new("captureRoot must be a real directory"));
    }
    let execution_value = execution_value
        .ok_or_else(|| DriverError::new("checkpoint execution must be an object"))?;
    let execution = object_exact(
        execution_value,
        &["taskUuid", "verdict", "exitCode"],
        "checkpoint execution",
    )?;
    let task_uuid = required_string(
        execution.get("taskUuid"),
        "checkpoint execution.taskUuid",
        None,
    )?;
    let parsed = Uuid::parse_str(&task_uuid)
        .map_err(|_| DriverError::new("checkpoint execution.taskUuid must be a UUID"))?;
    if parsed.to_string() != task_uuid {
        return Err(DriverError::new(
            "checkpoint execution.taskUuid must be a canonical UUID",
        ));
    }
    let verdict = required_string(
        execution.get("verdict"),
        "checkpoint execution.verdict",
        None,
    )?;
    if !matches!(
        verdict.as_str(),
        "pass"
            | "substituted"
            | "failed"
            | "skipped"
            | "cancelled"
            | "pool-vanished"
            | "preempted"
            | "runtime-exceeded"
            | "clean-exit-no-artifact"
    ) {
        return Err(DriverError::new(
            "checkpoint execution.verdict is not a terminal verdict",
        ));
    }
    let exit_code =
        json_integer_or_null(execution.get("exitCode"), "checkpoint execution.exitCode")?;
    let capture_stem = format!("{task_uuid}.{task_id}");
    let current_root = capture_root.parent().expect("validated capture root");
    let (stdout, stdout_truncated) =
        read_capture_tail(&current_root.join(format!("{capture_stem}.out")))?;
    let (stderr, stderr_truncated) =
        read_capture_tail(&current_root.join(format!("{capture_stem}.adapter.err")))?;
    let path = capture_root
        .join(&capture_stem)
        .join(CHECKPOINT_CAPTURE_FILE);
    write_private_json(
        &path,
        &Json::object([
            ("schemaVersion", Json::Number("1".to_owned())),
            ("campaign", Json::from(campaign)),
            ("issueNumber", Json::from(issue_number)),
            ("taskId", Json::from(task_id)),
            ("taskUuid", Json::from(task_uuid)),
            ("verdict", Json::from(verdict.clone())),
            ("exitCode", exit_code),
            ("stdout", Json::from(stdout)),
            ("stdoutTruncated", Json::from(stdout_truncated)),
            ("stderr", Json::from(stderr)),
            ("stderrTruncated", Json::from(stderr_truncated)),
        ]),
    )?;
    Ok(CheckpointCapture {
        path,
        stdout_truncated,
        stderr_truncated,
        verdict,
    })
}

fn read_checkpoint_capture(path: &Path, campaign: &str, task_id: &str) -> Result<Json> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            DriverError::new(format!(
                "cannot open checkpoint capture {}: {error}",
                path.display()
            ))
        })?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > 128 * 1024 {
        return Err(DriverError::new(format!(
            "checkpoint capture is not a bounded private regular file: {}",
            path.display()
        )));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    let value = json::parse(&text).map_err(|error| {
        DriverError::new(format!(
            "checkpoint capture is not valid JSON: {}: {error}",
            path.display()
        ))
    })?;
    let capture = object_complete(
        &value,
        &[
            "schemaVersion",
            "campaign",
            "issueNumber",
            "taskId",
            "taskUuid",
            "verdict",
            "exitCode",
            "stdout",
            "stdoutTruncated",
            "stderr",
            "stderrTruncated",
        ],
        "checkpoint capture",
    )?;
    if capture.get("schemaVersion").and_then(Json::as_u64) != Some(1)
        || capture.get("campaign").and_then(Json::as_str) != Some(campaign)
        || capture.get("taskId").and_then(Json::as_str) != Some(task_id)
    {
        return Err(DriverError::new(
            "checkpoint capture identity does not match the machine receipt",
        ));
    }
    for stream in ["stdout", "stderr"] {
        if capture
            .get(stream)
            .and_then(Json::as_str)
            .is_none_or(|content| content.len() > CHECKPOINT_CAPTURE_MAX_BYTES)
        {
            return Err(DriverError::new(format!(
                "checkpoint capture {stream} exceeds its {CHECKPOINT_CAPTURE_MAX_BYTES} byte bound"
            )));
        }
    }
    required_bool(
        capture.get("stdoutTruncated"),
        "checkpoint capture stdoutTruncated",
    )?;
    required_bool(
        capture.get("stderrTruncated"),
        "checkpoint capture stderrTruncated",
    )?;
    Ok(value)
}

fn checkpoint_capture_note(value: Option<&Json>, campaign: &str, task_id: &str) -> Result<String> {
    let value = value.ok_or_else(|| DriverError::new("checkpointCapture must be an object"))?;
    let publication = object_exact(
        value,
        &["path", "postFailureEvidence", "postFailureStderr"],
        "checkpointCapture",
    )?;
    let path_text = required_string(publication.get("path"), "checkpointCapture.path", Some(700))?;
    let path = PathBuf::from(&path_text);
    if !path.is_absolute() {
        return Err(DriverError::new("checkpointCapture.path must be absolute"));
    }
    let post_evidence = required_bool(
        publication.get("postFailureEvidence"),
        "checkpointCapture.postFailureEvidence",
    )?;
    let post_stderr = required_bool(
        publication.get("postFailureStderr"),
        "checkpointCapture.postFailureStderr",
    )?;
    if post_stderr && !post_evidence {
        return Err(DriverError::new(
            "checkpointCapture.postFailureStderr requires postFailureEvidence",
        ));
    }
    let note = format!("Checkpoint capture: {path_text}");
    if !(post_evidence && post_stderr) {
        return Ok(note);
    }
    let capture = read_checkpoint_capture(&path, campaign, task_id)?;
    let stderr = capture
        .as_object()
        .and_then(|capture| capture.get("stderr"))
        .and_then(Json::as_str)
        .expect("validated capture stderr");
    // Error-aware inside the derived window (vestige-sweep V-5): the note
    // carries the first error block plus the tail, not the last ten lines
    // of whatever noise came last.
    let windowed = excerpt_text_window(stderr, CHECKPOINT_STDERR_WINDOW_CHARS);
    if windowed.text.trim().is_empty() {
        return Ok(note);
    }
    let mut excerpt = windowed
        .text
        .lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let heading = format!(
        "{note}\n\nCheckpoint stderr (first error block and tail, before public redaction):\n\n"
    );
    let available = CHECKPOINT_STDERR_WINDOW_CHARS.saturating_sub(heading.chars().count());
    if excerpt.chars().count() > available {
        let marker = "    [... earlier checkpoint stderr lines shortened ...]\n";
        // An error-aware shortening: the lifted first-error block is exactly
        // the evidence the old 10-line window buried, so when the excerpt
        // lifted one, the shortening preserves the block and the gap marker
        // and cuts only the tail region, keeping its end (vestige-sweep V-5).
        let gap_marker = format!(
            "    {}\n",
            tally_core::executor::CAPTURE_EXCERPT_GAP_MARKER.trim_end_matches('\n')
        );
        let tail_region_start = excerpt
            .find(&gap_marker)
            .map(|index| index + gap_marker.len())
            .unwrap_or(0);
        let preserved = &excerpt[..tail_region_start];
        let tail_width = available
            .saturating_sub(preserved.chars().count())
            .saturating_sub(marker.chars().count());
        let tail: String = excerpt[tail_region_start..]
            .chars()
            .rev()
            .take(tail_width)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        excerpt = if preserved.chars().count() + marker.chars().count() >= available {
            take_chars(&format!("{preserved}{marker}"), available)
        } else {
            format!("{preserved}{marker}{tail}")
        };
    }
    Ok(format!("{heading}{excerpt}"))
}

fn append_checkpoint_capture_note(value: &str, note: &str, maximum: usize) -> String {
    append_machine_note(
        value,
        note,
        maximum,
        "\n[... earlier machine detail shortened for checkpoint capture ...]",
    )
}

/// Append a machine-composed note to public prose, keeping the note whole and
/// shortening the prose it follows. The note is the newer, more specific fact,
/// so it is the half that must survive the bound.
fn append_machine_note(value: &str, note: &str, maximum: usize, marker: &str) -> String {
    if note.is_empty() {
        return value.to_owned();
    }
    let suffix = format!("\n\n{note}");
    if value.chars().count() + suffix.chars().count() <= maximum {
        return format!("{value}{suffix}");
    }
    let available = maximum
        .saturating_sub(marker.chars().count())
        .saturating_sub(suffix.chars().count());
    format!(
        "{}{}{}",
        take_chars(value, available).trim_end(),
        marker,
        suffix
    )
}

fn action_checkpoint(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &[
            "campaign",
            "campaignIdentity",
            "repository",
            "repositoryConfig",
            "issue",
            "task",
            "source",
            "workspace",
            "baseRevision",
            "captureRoot",
            "execution",
            "specRepository",
            "issueRepository",
        ],
        "checkpoint brief",
    )?;
    let capture_present = data.contains_key("captureRoot");
    if capture_present != data.contains_key("execution") {
        return Err(DriverError::new(
            "checkpoint brief must carry captureRoot and execution together",
        ));
    }
    let campaign = required_string(data.get("campaign"), "campaign", None)?;
    if !is_component(&campaign) {
        return Err(DriverError::new("campaign is not a safe component"));
    }
    let repository = repository_name(data.get("repository"), "repository")?;
    let config = repo_config(data.get("repositoryConfig"))?;
    campaign_coordinates(data, repository, config.clone())?;
    let issue_number = campaign_issue(data.get("issue"))?.0;
    let task_value = data
        .get("task")
        .ok_or_else(|| DriverError::new("checkpoint task must be an object"))?;
    let task = object_exact(
        task_value,
        &[
            "id",
            "kind",
            "title",
            "argv",
            "runtimeMaxSec",
            "dependencies",
            "brief",
            "revision",
        ],
        "checkpoint task",
    )?;
    let task_id = required_string(task.get("id"), "checkpoint task.id", Some(80))?;
    if !is_task_id(&task_id) || task.get("kind").and_then(Json::as_str) != Some("checkpoint") {
        return Err(DriverError::new(
            "checkpoint task must carry a safe id and kind checkpoint",
        ));
    }
    required_string(task.get("title"), "checkpoint task.title", Some(300))?;
    argv_list(task.get("argv"), "checkpoint task.argv")?;
    positive_u64(task.get("runtimeMaxSec"), "checkpoint task.runtimeMaxSec")?;
    string_list(
        task.get("dependencies"),
        "checkpoint task.dependencies",
        false,
    )?;
    let source_value = data
        .get("source")
        .ok_or_else(|| DriverError::new("source must be an object"))?;
    let source = object_exact(
        source_value,
        &["path", "sha256", "revision", "repository"],
        "source",
    )?;
    required_string(source.get("path"), "source.path", None)?;
    if source.contains_key("repository") {
        repository_name(source.get("repository"), "source.repository")?;
    }
    let source_sha256 = required_string(source.get("sha256"), "source.sha256", None)?;
    let source_revision = full_oid(source.get("revision"), "source.revision")?;
    let code_revision = if data.contains_key("baseRevision") {
        full_oid(data.get("baseRevision"), "baseRevision")?
    } else {
        source_revision
    };
    let workspace = prepared_workspace(data.get("workspace"), "workspace")?;
    if workspace.task_id != task_id {
        return Err(DriverError::new(
            "checkpoint task.id does not match workspace.taskId",
        ));
    }
    if !is_full_oid(&workspace.base_rev) {
        return Err(DriverError::new(
            "workspace.baseRev must be a full Git object ID",
        ));
    }
    if !workspace.worktree.is_absolute() || !workspace.worktree.is_dir() {
        return Err(DriverError::new(
            "workspace.worktreePath must be an absolute existing directory",
        ));
    }
    let capture = if capture_present {
        Some(persist_checkpoint_capture(
            data.get("captureRoot"),
            data.get("execution"),
            &campaign,
            &issue_number,
            &task_id,
        )?)
    } else {
        None
    };
    if let Some(capture) = &capture {
        if capture.verdict != "pass" {
            return Ok(Json::object([
                ("taskId", Json::from(task_id)),
                ("passed", Json::from(false)),
                ("ref", Json::Null),
                ("revision", Json::from(workspace.base_rev)),
                (
                    "capturePath",
                    Json::from(capture.path.to_string_lossy().into_owned()),
                ),
                ("stdoutTruncated", Json::from(capture.stdout_truncated)),
                ("stderrTruncated", Json::from(capture.stderr_truncated)),
            ]));
        }
    }
    if git(&workspace.worktree, ["branch", "--show-current"], true)?.stdout_trimmed()
        != workspace.branch
    {
        return Err(DriverError::new(
            "checkpoint worktree changed branches during validation",
        ));
    }
    if git(&workspace.worktree, ["rev-parse", "HEAD^{commit}"], true)?.stdout_trimmed()
        != workspace.base_rev
    {
        return Err(DriverError::new(
            "checkpoint command changed HEAD instead of validating the prepared base",
        ));
    }
    if !git(
        &workspace.worktree,
        ["status", "--porcelain", "--untracked-files=no"],
        true,
    )?
    .stdout
    .is_empty()
    {
        return Err(DriverError::new(
            "checkpoint command changed tracked files instead of validating the prepared base",
        ));
    }
    git(
        &config.checkout,
        ["fetch", "--prune", "--no-tags", &config.remote],
        true,
    )?;
    let uses_integration = data.contains_key("campaignIdentity");
    let current_base = if uses_integration {
        required_integration_revision(
            &config,
            &campaign,
            &required_string(data.get("campaignIdentity"), "campaignIdentity", Some(128))?,
        )?
    } else {
        git(
            &config.checkout,
            [
                "rev-parse",
                "--verify",
                &format!("{}/{}^{{commit}}", config.remote, config.base_branch),
            ],
            true,
        )?
        .stdout_trimmed()
    };
    if !git(
        &config.checkout,
        [
            "merge-base",
            "--is-ancestor",
            &code_revision,
            &workspace.base_rev,
        ],
        false,
    )?
    .success()
    {
        return Err(DriverError::new(
            "prepared checkpoint base does not descend from the witnessed worklist revision",
        ));
    }
    if !git(
        &config.checkout,
        [
            "merge-base",
            "--is-ancestor",
            &workspace.base_rev,
            &current_base,
        ],
        false,
    )?
    .success()
    {
        return Err(DriverError::new(format!(
            "{} diverged after the checkpoint command was witnessed",
            if uses_integration {
                "integration branch"
            } else {
                "remote base"
            }
        )));
    }
    let reference = checkpoint_ref(
        &campaign,
        &issue_number,
        &task_id,
        &source_sha256,
        &workspace.base_rev,
    )?;
    let existing = remote_ref_oid(&config.checkout, &config.remote, &reference)?;
    if existing
        .as_ref()
        .is_some_and(|existing| existing != &workspace.base_rev)
    {
        return Err(DriverError::new(format!(
            "immutable checkpoint ref {reference:?} already points to another object"
        )));
    }
    if existing.is_none() {
        let pushed = git(
            &workspace.worktree,
            [
                "push",
                &config.remote,
                &format!("{}:{reference}", workspace.base_rev),
            ],
            false,
        )?;
        if !pushed.success()
            && remote_ref_oid(&config.checkout, &config.remote, &reference)?
                != Some(workspace.base_rev.clone())
        {
            return Err(DriverError::new(format!(
                "cannot create immutable checkpoint ref {reference:?}: {}",
                pushed.detail()
            )));
        }
    }
    if remote_ref_oid(&config.checkout, &config.remote, &reference)?
        != Some(workspace.base_rev.clone())
    {
        return Err(DriverError::new(
            "checkpoint completion ref did not expose the witnessed base revision",
        ));
    }
    if current_base != workspace.base_rev {
        return Err(DriverError::new(format!(
            "checkpoint {task_id:?} recorded {} in {reference:?}, but the base branch has already advanced to {current_base}: the receipt cannot complete the task and the base branch is moving faster than this checkpoint runs",
            workspace.base_rev
        )));
    }
    let mut result = BTreeMap::from([
        ("taskId".to_owned(), Json::from(task_id)),
        ("ref".to_owned(), Json::from(reference)),
        ("revision".to_owned(), Json::from(workspace.base_rev)),
    ]);
    if let Some(capture) = capture {
        result.insert("passed".to_owned(), Json::from(true));
        result.insert(
            "capturePath".to_owned(),
            Json::from(capture.path.to_string_lossy().into_owned()),
        );
        result.insert(
            "stdoutTruncated".to_owned(),
            Json::from(capture.stdout_truncated),
        );
        result.insert(
            "stderrTruncated".to_owned(),
            Json::from(capture.stderr_truncated),
        );
    }
    Ok(Json::Object(result))
}

fn merge_method(value: Option<&Json>, context: &str) -> Result<String> {
    let Some(value) = value else {
        return Ok("squash".to_owned());
    };
    if matches!(value, Json::Null) {
        return Ok("squash".to_owned());
    }
    let method = required_string(Some(value), context, None)?;
    if !matches!(method.as_str(), "merge" | "squash") {
        return Err(DriverError::new(format!(
            "{context} must be merge or squash"
        )));
    }
    Ok(method)
}

#[derive(Debug)]
struct AssistedBy {
    adapter: String,
    model: String,
    task_uuid: String,
    witness_sequence: u64,
}

fn assisted_by_record(value: Option<&Json>) -> Result<Option<AssistedBy>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if matches!(value, Json::Null) {
        return Ok(None);
    }
    let record = object_exact(
        value,
        &["adapter", "model", "taskUuid", "witnessSeq"],
        "assistedBy",
    )?;
    let adapter = required_string(record.get("adapter"), "assistedBy.adapter", Some(128))?;
    let model = required_string(record.get("model"), "assistedBy.model", Some(128))?;
    let task_uuid = required_string(record.get("taskUuid"), "assistedBy.taskUuid", Some(36))?;
    Uuid::parse_str(&task_uuid)
        .map_err(|_| DriverError::new("assistedBy.taskUuid must be a UUID"))?;
    let witness_sequence = positive_u64(record.get("witnessSeq"), "assistedBy.witnessSeq")?;
    for (name, value) in [("adapter", &adapter), ("model", &model)] {
        if value.contains(['(', ')', '\n']) {
            return Err(DriverError::new(format!(
                "assistedBy.{name} must not contain trailer punctuation"
            )));
        }
    }
    Ok(Some(AssistedBy {
        adapter,
        model,
        task_uuid,
        witness_sequence,
    }))
}

fn assisted_by_trailer(record: Option<&AssistedBy>) -> Result<Option<String>> {
    let Some(record) = record else {
        return Ok(None);
    };
    let trailer = format!(
        "{ASSISTED_BY_PREFIX} {}:{} (tally:{} witness:{})",
        record.adapter, record.model, record.task_uuid, record.witness_sequence
    );
    if trailer.chars().count() > 200 {
        return Err(DriverError::new(
            "assistedBy renders a trailer over the published cap",
        ));
    }
    Ok(Some(trailer))
}

fn completion_trailer_block(task_id: &str, revision: &str) -> Result<String> {
    if !is_task_id(task_id) {
        return Err(DriverError::new(
            "completion trailer task must be a safe task ID",
        ));
    }
    let valid = revision.len() == 71
        && revision.starts_with("sha256:")
        && revision[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !valid {
        return Err(DriverError::new(
            "completion trailer revision must be a lowercase SHA-256 identity",
        ));
    }
    Ok(format!(
        "{TALLY_TASK_PREFIX} {task_id}\n{TALLY_REVISION_PREFIX} {revision}"
    ))
}

fn local_completion_trailers(data: &BTreeMap<String, Json>) -> Result<String> {
    let task = data
        .get("task")
        .and_then(Json::as_object)
        .ok_or_else(|| DriverError::new("local merge task must be an object"))?;
    let revision = task_revision(task)?
        .ok_or_else(|| DriverError::new("local merge task must carry a completion revision"))?;
    let task_id = required_string(task.get("id"), "task.id", None)?;
    completion_trailer_block(&task_id, &revision)
}

fn valid_trailer_line(line: &str) -> bool {
    let Some((key, value)) = line.split_once(':') else {
        return false;
    };
    !key.is_empty()
        && key.bytes().enumerate().all(|(index, byte)| {
            if index == 0 {
                byte.is_ascii_alphanumeric()
            } else {
                byte.is_ascii_alphanumeric() || byte == b'-'
            }
        })
        && value.starts_with([' ', '\t'])
        && !value.trim().is_empty()
        && !value.ends_with(char::is_whitespace)
}

fn validated_trailer_lines(block: &str, context: &str) -> Result<Vec<String>> {
    if block.is_empty() || block.contains('\r') || !block.lines().all(valid_trailer_line) {
        return Err(DriverError::new(format!(
            "{context} must be one contiguous git trailer block"
        )));
    }
    Ok(block.lines().map(str::to_owned).collect())
}

fn merge_commit_message(
    message: &CommitMessage,
    provenance: Option<&str>,
    completion: Option<&str>,
) -> Result<String> {
    let mut trailers = Vec::new();
    if let Some(completion) = completion {
        trailers.extend(validated_trailer_lines(completion, "completion trailers")?);
    }
    if let Some(provenance) = provenance {
        trailers.extend(validated_trailer_lines(provenance, "provenance trailers")?);
    }
    let trailer_block = trailers.join("\n");
    let commit_body = match (message.body.is_empty(), trailer_block.is_empty()) {
        (true, true) => String::new(),
        (false, true) => message.body.clone(),
        (true, false) => trailer_block,
        (false, false) => format!("{}\n\n{trailer_block}", message.body),
    };
    Ok(if commit_body.is_empty() {
        format!("{}\n", message.subject)
    } else {
        format!("{}\n\n{commit_body}\n", message.subject)
    })
}

fn merge_receipt_reference(
    campaign: &str,
    identity: &str,
    task_id: &str,
    revision: Option<&str>,
) -> String {
    let suffix = revision.map_or_else(String::new, |revision| {
        format!("-{}", &revision.trim_start_matches("sha256:")[..16])
    });
    format!(
        "{}/merge/{task_id}{suffix}",
        local_state_prefix(campaign, identity)
    )
}

fn merge_local(
    data: &BTreeMap<String, Json>,
    config: &RepoConfig,
    integration: &BTreeMap<String, Json>,
    method: &str,
    provenance: Option<&str>,
) -> Result<String> {
    let campaign = required_string(data.get("campaign"), "campaign", None)?;
    let identity = campaign_identity(data, &campaign)?;
    let completion = local_completion_trailers(data)?;
    let integration_name = integration_branch(&campaign, &identity);
    let integration_ref = format!("refs/heads/{integration_name}");
    let published_branch = required_string(integration.get("branch"), "integration.branch", None)?;
    let published_ref = format!("refs/heads/{published_branch}");
    let expected_base = full_oid(integration.get("baseRev"), "integration.baseRev")?;
    let expected_head = full_oid(integration.get("head"), "integration.head")?;
    let actual_base = local_branch_oid(&config.checkout, &integration_name)?;
    let actual_head = local_branch_oid(&config.checkout, &published_branch)?;
    if actual_base.as_deref() != Some(expected_base.as_str()) {
        return Err(DriverError::new(
            "local integration branch moved after the rebased head was gated",
        ));
    }
    if actual_head.as_deref() != Some(expected_head.as_str()) {
        return Err(DriverError::new(
            "published branch moved after the rebased head was gated",
        ));
    }
    let task = data
        .get("task")
        .and_then(Json::as_object)
        .ok_or_else(|| DriverError::new("local merge task must be an object"))?;
    let adopted = lane_tip_commit_message(&config.checkout, &expected_head, task)?;
    let message = merge_commit_message(&adopted, provenance, Some(&completion))?;
    let workspace_root = PathBuf::from(required_string(
        data.get("workspaceRoot"),
        "workspaceRoot",
        None,
    )?);
    fs::create_dir_all(&workspace_root)?;
    let temporary = workspace_root.join(format!("merge-{}", Uuid::new_v4()));
    let integration_checkout = temporary.join("worktree");
    fs::create_dir_all(&temporary)?;
    let checkout_text = integration_checkout.to_string_lossy().into_owned();
    git(
        &config.checkout,
        [
            "worktree",
            "add",
            "--detach",
            "--quiet",
            &checkout_text,
            actual_base.as_deref().expect("validated base"),
        ],
        true,
    )?;
    let merged = (|| {
        if method == "squash" {
            git(
                &integration_checkout,
                [
                    "merge",
                    "--squash",
                    actual_head.as_deref().expect("validated head"),
                ],
                true,
            )?;
            if git(
                &integration_checkout,
                ["diff", "--cached", "--quiet"],
                false,
            )?
            .success()
            {
                return Err(DriverError::new(
                    "squash merge staged no change against the witnessed base",
                ));
            }
            git_with_input(
                &integration_checkout,
                [
                    "-c",
                    "user.name=tally spec-build",
                    "-c",
                    "user.email=tally-spec-build@invalid",
                    "commit",
                    "--quiet",
                    "--file",
                    "-",
                ],
                message.as_bytes(),
                true,
            )?;
        } else {
            git(
                &integration_checkout,
                [
                    "-c",
                    "user.name=tally spec-build",
                    "-c",
                    "user.email=tally-spec-build@invalid",
                    "merge",
                    "--no-ff",
                    "--no-commit",
                    actual_head.as_deref().expect("validated head"),
                ],
                true,
            )?;
            git_with_input(
                &integration_checkout,
                [
                    "-c",
                    "user.name=tally spec-build",
                    "-c",
                    "user.email=tally-spec-build@invalid",
                    "commit",
                    "--quiet",
                    "--file",
                    "-",
                ],
                message.as_bytes(),
                true,
            )?;
        }
        let merge_commit =
            git(&integration_checkout, ["rev-parse", "HEAD"], true)?.stdout_trimmed();
        let mut transaction = vec![
            "start".to_owned(),
            format!("verify {published_ref} {expected_head}"),
            format!("update {integration_ref} {merge_commit} {expected_base}"),
        ];
        if method == "squash" {
            let task = data
                .get("task")
                .and_then(Json::as_object)
                .ok_or_else(|| DriverError::new("local merge task must be an object"))?;
            let task_id = required_string(task.get("id"), "task.id", None)?;
            let revision = task_revision(task)?;
            let receipt =
                merge_receipt_reference(&campaign, &identity, &task_id, revision.as_deref());
            transaction.push(format!("update {receipt} {merge_commit}"));
        }
        transaction.extend(["prepare".to_owned(), "commit".to_owned()]);
        git_with_input(
            &config.checkout,
            ["update-ref", "--stdin"],
            format!("{}\n", transaction.join("\n")).as_bytes(),
            true,
        )?;
        Ok(merge_commit)
    })();
    let _ = git(
        &config.checkout,
        ["worktree", "remove", "--force", &checkout_text],
        false,
    );
    let _ = fs::remove_dir_all(&temporary);
    merged
}

fn action_merge(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &[
            "campaign",
            "campaignIdentity",
            "repository",
            "repositoryConfig",
            "issue",
            "runId",
            "workspaceRoot",
            "task",
            "workspace",
            "integration",
            "domainsRequired",
            "mergeMethod",
            "assistedBy",
            "specRepository",
            "issueRepository",
        ],
        "merge brief",
    )?;
    let config = repo_config(data.get("repositoryConfig"))?;
    let workspace = prepared_workspace(data.get("workspace"), "workspace")?;
    if !is_full_oid(&workspace.base_rev) {
        return Err(DriverError::new(
            "workspace.baseRev must be a full Git object ID",
        ));
    }
    if workspace.worktree.exists() && !workspace.worktree.is_dir() {
        return Err(DriverError::new(
            "workspace.worktreePath exists but is not a directory",
        ));
    }
    let integration_value = data
        .get("integration")
        .ok_or_else(|| DriverError::new("integration must be an object"))?;
    let integration = object_exact(
        integration_value,
        &[
            "taskId",
            "baseRev",
            "branch",
            "head",
            "pullRequest",
            "narration",
            "regate",
            "ownership",
        ],
        "integration",
    )?;
    let method = merge_method(data.get("mergeMethod"), "mergeMethod")?;
    let assisted_by = assisted_by_record(data.get("assistedBy"))?;
    let trailer = if method == "squash" {
        assisted_by_trailer(assisted_by.as_ref())?
    } else {
        None
    };
    let domains_required = required_bool(data.get("domainsRequired"), "domainsRequired")?;
    let task_id = required_string(integration.get("taskId"), "integration.taskId", None)?;
    let base_rev = full_oid(integration.get("baseRev"), "integration.baseRev")?;
    let branch = required_string(integration.get("branch"), "integration.branch", None)?;
    let head = full_oid(integration.get("head"), "integration.head")?;
    let pull_request = required_string(
        integration.get("pullRequest"),
        "integration.pullRequest",
        None,
    )?;
    let regate = required_bool(integration.get("regate"), "integration.regate")?;
    let ownership = normalize_ownership(integration.get("ownership"), "integration.ownership")?;
    if ownership.task_id != task_id {
        return Err(DriverError::new(
            "integration.ownership.taskId does not match integration.taskId",
        ));
    }
    if ownership.domains_required != domains_required {
        return Err(DriverError::new(
            "integration.ownership.domainsRequired does not match domainsRequired",
        ));
    }
    if ownership.base_rev != base_rev {
        return Err(DriverError::new(
            "integration.ownership.baseRev does not match integration.baseRev",
        ));
    }
    if ownership.head != head {
        return Err(DriverError::new(
            "integration.ownership.head does not match integration.head",
        ));
    }
    if task_id != workspace.task_id {
        return Err(DriverError::new(
            "integration.taskId does not match workspace.taskId",
        ));
    }
    if branch != workspace.publish_branch {
        return Err(DriverError::new(
            "integration.branch does not match workspace.publishBranch",
        ));
    }
    let merge_commit = merge_local(data, &config, integration, &method, trailer.as_deref())?;
    Ok(Json::object([
        ("taskId", Json::from(task_id)),
        ("head", Json::from(head)),
        ("mergeCommit", Json::from(merge_commit)),
        ("pullRequest", Json::from(pull_request)),
        ("regated", Json::from(regate)),
        ("ownership", ownership.to_json()),
        ("trailer", trailer.map_or(Json::Null, Json::from)),
    ]))
}

fn required_text(value: Option<&Json>, context: &str, maximum: usize) -> Result<String> {
    let value = value
        .and_then(Json::as_str)
        .ok_or_else(|| DriverError::new(format!("{context} must be non-empty text")))?;
    if value.trim().is_empty() {
        return Err(DriverError::new(format!(
            "{context} must be non-empty text"
        )));
    }
    if value.chars().count() > maximum {
        return Err(DriverError::new(format!(
            "{context} exceeds {maximum} characters"
        )));
    }
    if value
        .chars()
        .any(|character| (character as u32) < 32 && !matches!(character, '\n' | '\t' | '\r'))
    {
        return Err(DriverError::new(format!(
            "{context} contains unsupported control characters"
        )));
    }
    Ok(value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_owned())
}

fn state_scope(campaign: &str, issue_number: &str) -> String {
    let mut identity = campaign.as_bytes().to_vec();
    identity.push(0);
    identity.extend_from_slice(issue_number.as_bytes());
    sha256::digest(&identity).chars().take(24).collect()
}

fn local_state_prefix(campaign: &str, issue_number: &str) -> String {
    format!(
        "refs/tally/spec-build/v1/{}",
        state_scope(campaign, issue_number)
    )
}

fn local_remote_refs(config: &RepoConfig, pattern: &str) -> Result<BTreeMap<String, String>> {
    let viewed = git(
        &config.checkout,
        ["ls-remote", &config.remote, pattern],
        true,
    )?;
    let mut refs = BTreeMap::new();
    for line in viewed.stdout_text().lines() {
        let Some((oid, reference)) = line.split_once('\t') else {
            return Err(DriverError::new(
                "the campaign remote returned a malformed state ref",
            ));
        };
        if !is_full_oid(oid) {
            return Err(DriverError::new(
                "the campaign remote returned a malformed state ref",
            ));
        }
        refs.insert(reference.to_owned(), oid.to_owned());
    }
    Ok(refs)
}

fn read_local_blob(config: &RepoConfig, reference: &str) -> Result<Json> {
    git(
        &config.checkout,
        ["fetch", "--quiet", &config.remote, reference],
        true,
    )?;
    let content = git(&config.checkout, ["cat-file", "blob", "FETCH_HEAD"], true)?.stdout;
    let content = std::str::from_utf8(&content).map_err(|error| {
        DriverError::new(format!(
            "local campaign state {reference:?} is invalid JSON: {error}"
        ))
    })?;
    let value = json::parse(content).map_err(|error| {
        DriverError::new(format!(
            "local campaign state {reference:?} is invalid JSON: {error}"
        ))
    })?;
    if value.as_object().is_none() {
        return Err(DriverError::new(format!(
            "local campaign state {reference:?} must contain an object"
        )));
    }
    Ok(value)
}

fn write_local_blob(config: &RepoConfig, reference: &str, value: &Json) -> Result<(bool, Json)> {
    if local_remote_refs(config, reference)?.contains_key(reference) {
        return Ok((false, read_local_blob(config, reference)?));
    }
    let rendered = value.stringify();
    let object_id = required_string(
        Some(&Json::from(
            git_with_input(
                &config.checkout,
                ["hash-object", "-w", "--stdin"],
                rendered.as_bytes(),
                true,
            )?
            .stdout_trimmed(),
        )),
        "local campaign state object",
        None,
    )?;
    git(
        &config.checkout,
        [
            "push",
            "--quiet",
            &config.remote,
            &format!("{object_id}:{reference}"),
        ],
        true,
    )?;
    Ok((true, value.clone()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiagnosisVerdict {
    Retry,
    Blocked,
    Transient,
}

impl DiagnosisVerdict {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::Blocked => "blocked",
            Self::Transient => "transient",
        }
    }

    fn parse(value: Option<&Json>, context: &str) -> Result<Option<Self>> {
        match value {
            None => Ok(None),
            Some(value) => match value.as_str() {
                Some("retry") => Ok(Some(Self::Retry)),
                Some("blocked") => Ok(Some(Self::Blocked)),
                Some("transient") => Ok(Some(Self::Transient)),
                _ => Err(DriverError::new(format!(
                    "{context} must be retry, blocked, or transient"
                ))),
            },
        }
    }
}

#[derive(Clone, Debug)]
struct VisibleAttempt {
    task_id: String,
    attempt: u64,
    comment: String,
    text: String,
    input_epoch: Option<String>,
    verdict: Option<DiagnosisVerdict>,
    proposal: Option<Json>,
}

impl VisibleAttempt {
    fn diagnosis_json(&self) -> Json {
        let mut object = BTreeMap::from([
            ("taskId".to_owned(), Json::from(self.task_id.clone())),
            ("attempt".to_owned(), Json::Number(self.attempt.to_string())),
            ("comment".to_owned(), Json::from(self.comment.clone())),
            ("diagnosis".to_owned(), Json::from(self.text.clone())),
            (
                "verdict".to_owned(),
                Json::from(self.effective_verdict().as_str()),
            ),
        ]);
        if let Some(proposal) = &self.proposal {
            object.insert("proposal".to_owned(), proposal.clone());
        }
        Json::Object(object)
    }

    fn retry_json(&self) -> Json {
        Json::object([
            ("taskId", Json::from(self.task_id.clone())),
            ("attempt", Json::Number(self.attempt.to_string())),
            ("comment", Json::from(self.comment.clone())),
            ("reason", Json::from(self.text.clone())),
        ])
    }

    const fn effective_verdict(&self) -> DiagnosisVerdict {
        match self.verdict {
            Some(verdict) => verdict,
            None if self.attempt == 2 => DiagnosisVerdict::Blocked,
            None => DiagnosisVerdict::Retry,
        }
    }

    const fn blocks_task(&self) -> bool {
        self.attempt == 2 || matches!(self.effective_verdict(), DiagnosisVerdict::Blocked)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WorkerOutcome {
    NeedsAuthority { paths: Vec<String> },
    Impossible { reason: String },
}

impl WorkerOutcome {
    const fn class(&self) -> &'static str {
        match self {
            Self::NeedsAuthority { .. } => "needs-authority",
            Self::Impossible { .. } => "impossible",
        }
    }

    fn paths_json(&self) -> Json {
        match self {
            Self::NeedsAuthority { paths } => {
                Json::Array(paths.iter().cloned().map(Json::from).collect())
            }
            Self::Impossible { .. } => Json::Null,
        }
    }

    fn reason_json(&self) -> Json {
        match self {
            Self::NeedsAuthority { .. } => Json::Null,
            Self::Impossible { reason } => Json::from(reason.clone()),
        }
    }
}

#[derive(Clone, Debug)]
struct VisibleWorkerOutcome {
    task_id: String,
    task_revision: String,
    task_uuid: String,
    comment: String,
    input_epoch: Option<String>,
    outcome: WorkerOutcome,
}

impl VisibleWorkerOutcome {
    fn to_json(&self) -> Json {
        Json::object([
            ("taskId", Json::from(self.task_id.clone())),
            ("taskRevision", Json::from(self.task_revision.clone())),
            ("taskUuid", Json::from(self.task_uuid.clone())),
            ("outcome", Json::from(self.outcome.class())),
            ("comment", Json::from(self.comment.clone())),
            ("paths", self.outcome.paths_json()),
            ("reason", self.outcome.reason_json()),
        ])
    }
}

#[derive(Clone, Debug)]
enum AttemptEvent {
    Diagnosis(VisibleAttempt),
    Retry(VisibleAttempt),
    WorkerOutcome(VisibleWorkerOutcome),
    Escalation {
        comment: String,
    },
    Pardon {
        tasks: Option<Vec<String>>,
        comment: String,
    },
}

#[derive(Clone, Debug, Default)]
struct AttemptState {
    diagnoses: Vec<VisibleAttempt>,
    retries: Vec<VisibleAttempt>,
    outcomes: Vec<VisibleWorkerOutcome>,
    escalation: Option<String>,
    lifetime_attempts: BTreeMap<String, usize>,
    lifetime_exhausted: BTreeSet<String>,
    warnings: Vec<String>,
}

struct AttemptKinds<'a, T> {
    diagnoses: &'a mut BTreeMap<String, Vec<T>>,
    retries: &'a mut BTreeMap<String, Vec<T>>,
}

struct AttemptFoldInputs<'a> {
    task_revisions: &'a BTreeMap<String, Option<String>>,
    current_epochs: &'a BTreeMap<String, String>,
}

fn attempt_receipts_path(value: Option<&Json>, campaign: &str) -> Result<PathBuf> {
    let source = value.ok_or_else(|| DriverError::new("attemptReceipts must be an object"))?;
    let source = object_exact(
        source,
        &["schemaVersion", "kind", "path", "inputEpochs"],
        "attemptReceipts",
    )?;
    if source.get("schemaVersion").and_then(Json::as_u64) != Some(1)
        || source.get("kind").and_then(Json::as_str) != Some("local-jsonl")
    {
        return Err(DriverError::new(
            "attemptReceipts must use local-jsonl schema version 1",
        ));
    }
    let path = PathBuf::from(required_string(
        source.get("path"),
        "attemptReceipts.path",
        Some(4_096),
    )?);
    if !path.is_absolute() {
        return Err(DriverError::new("attemptReceipts.path must be absolute"));
    }
    let valid = path
        .file_name()
        .is_some_and(|name| name == ATTEMPT_RECEIPTS_FILE)
        && path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == campaign)
        && path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|name| name == "attempt-receipts");
    if !valid {
        return Err(DriverError::new(
            "attemptReceipts.path does not name this campaign's attempt-receipts log",
        ));
    }
    Ok(path)
}

fn attempt_receipt_input_epochs(value: Option<&Json>) -> Result<BTreeMap<String, String>> {
    let source = value.ok_or_else(|| DriverError::new("attemptReceipts must be an object"))?;
    let source = object_exact(
        source,
        &["schemaVersion", "kind", "path", "inputEpochs"],
        "attemptReceipts",
    )?;
    let Some(epochs) = source.get("inputEpochs") else {
        return Ok(BTreeMap::new());
    };
    let epochs = epochs
        .as_object()
        .ok_or_else(|| DriverError::new("attemptReceipts.inputEpochs must be an object"))?;
    if epochs.len() > MAX_CAMPAIGN_TASKS {
        return Err(DriverError::new(
            "attemptReceipts.inputEpochs exceeds 128 tasks",
        ));
    }
    epochs
        .iter()
        .map(|(task_id, epoch)| {
            if !is_task_id(task_id) {
                return Err(DriverError::new(
                    "attemptReceipts.inputEpochs contains an unsafe task ID",
                ));
            }
            let epoch = required_string(
                Some(epoch),
                &format!("attemptReceipts.inputEpochs.{task_id}"),
                Some(71),
            )?;
            if !is_sha256_identity(&epoch) {
                return Err(DriverError::new(format!(
                    "attemptReceipts.inputEpochs.{task_id} must be a lowercase SHA-256 identity"
                )));
            }
            Ok((task_id.clone(), epoch))
        })
        .collect()
}

fn attempt_receipt_url(campaign: &str, sequence: u64) -> String {
    format!("local://campaign/{campaign}/attempt-receipts/{sequence}")
}

fn read_attempt_receipt_authority(
    receipt_path: &Path,
    campaign: &str,
    issue_number: &str,
) -> Result<AttemptReceiptAuthorityV1> {
    let path = receipt_path
        .parent()
        .ok_or_else(|| DriverError::new("attemptReceipts.path has no parent"))?
        .join(ATTEMPT_RECEIPT_AUTHORITY_FILE);
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(O_CLOEXEC | O_NOFOLLOW);
    let mut file = options.open(&path).map_err(|error| {
        DriverError::new(format!(
            "cannot write a stamped attempt receipt without authority {}: {error}",
            path.display()
        ))
    })?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.len() > MAX_ATTEMPT_RECEIPT_AUTHORITY_BYTES
    {
        return Err(DriverError::new(format!(
            "attempt receipt authority is not a bounded private regular file: {}",
            path.display()
        )));
    }
    let mut raw = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut raw)?;
    if raw.len() as u64 > MAX_ATTEMPT_RECEIPT_AUTHORITY_BYTES {
        return Err(DriverError::new(format!(
            "attempt receipt authority exceeds 64 KiB: {}",
            path.display()
        )));
    }
    let authority: AttemptReceiptAuthorityV1 = serde_json::from_slice(&raw).map_err(|error| {
        DriverError::new(format!(
            "attempt receipt authority {} is invalid JSON: {error}",
            path.display()
        ))
    })?;
    authority
        .validate_for(campaign, issue_number)
        .map_err(|error| {
            DriverError::new(format!(
                "attempt receipt authority {} is invalid: {error}",
                path.display()
            ))
        })?;
    Ok(authority)
}

fn validate_attempt_receipt(
    candidate: &Json,
    path: &Path,
    expected_sequence: u64,
    campaign: &str,
    issue_number: &str,
) -> Result<AttemptEvent> {
    let context = format!("attempt receipt {expected_sequence} in {}", path.display());
    let object = candidate
        .as_object()
        .ok_or_else(|| DriverError::new(format!("{context} must be an object")))?;
    let kind = object
        .get("kind")
        .and_then(Json::as_str)
        .unwrap_or_default();
    let schema_version = object
        .get("schemaVersion")
        .and_then(Json::as_u64)
        .ok_or_else(|| DriverError::new(format!("{context}.schemaVersion is invalid")))?;
    if !matches!(
        schema_version,
        LEGACY_ATTEMPT_RECEIPT_SCHEMA_VERSION | ATTEMPT_RECEIPT_SCHEMA_VERSION
    ) {
        return Err(DriverError::new(format!(
            "{context}.schemaVersion must equal {LEGACY_ATTEMPT_RECEIPT_SCHEMA_VERSION} or {ATTEMPT_RECEIPT_SCHEMA_VERSION}"
        )));
    }
    let mut common = vec![
        "schemaVersion",
        "sequence",
        "kind",
        "campaign",
        "issueNumber",
    ];
    if schema_version == ATTEMPT_RECEIPT_SCHEMA_VERSION {
        common.extend(["armSerial", "worklistSha256", "writtenAt", "actor"]);
    }
    let mut fields = common;
    match kind {
        "diagnosis" => fields.extend([
            "taskId",
            "attempt",
            "diagnosis",
            "verdict",
            "proposal",
            "redaction",
        ]),
        "retry" => fields.extend(["taskId", "attempt", "reason", "redaction"]),
        "worker-outcome" => fields.extend([
            "taskId",
            "taskRevision",
            "taskUuid",
            "outcome",
            "paths",
            "reason",
        ]),
        "escalation" => fields.push("body"),
        "pardon" => fields.extend(["tasks", "reason", "actor", "nonce"]),
        _ => {
            return Err(DriverError::new(format!(
                "{context} has unknown kind {kind:?}"
            )))
        }
    }
    if schema_version == ATTEMPT_RECEIPT_SCHEMA_VERSION
        && matches!(kind, "diagnosis" | "retry" | "worker-outcome")
    {
        fields.push("inputEpoch");
    }
    let record = object_exact(candidate, &fields, &context)?;
    if record.get("sequence").and_then(Json::as_u64) != Some(expected_sequence)
        || record.get("campaign").and_then(Json::as_str) != Some(campaign)
        || record.get("issueNumber").and_then(Json::as_str) != Some(issue_number)
    {
        return Err(DriverError::new(format!(
            "{context} has invalid identity or sequence"
        )));
    }
    if schema_version == ATTEMPT_RECEIPT_SCHEMA_VERSION {
        validate_attempt_receipt_stamp(
            record.get("armSerial").and_then(Json::as_u64),
            record.get("worklistSha256").and_then(Json::as_str),
            record.get("writtenAt").and_then(Json::as_str),
            record.get("actor").and_then(Json::as_str),
        )
        .map_err(|error| DriverError::new(format!("{context} has invalid stamp: {error}")))?;
    }
    let input_epoch = record
        .get("inputEpoch")
        .map(|value| {
            let epoch = required_string(Some(value), &format!("{context}.inputEpoch"), Some(71))?;
            if !is_sha256_identity(&epoch) {
                return Err(DriverError::new(format!(
                    "{context}.inputEpoch must be a lowercase SHA-256 identity"
                )));
            }
            Ok(epoch)
        })
        .transpose()?;
    let comment = attempt_receipt_url(campaign, expected_sequence);
    match kind {
        "diagnosis" | "retry" => {
            let task_id =
                required_string(record.get("taskId"), &format!("{context}.taskId"), None)?;
            if !is_task_id(&task_id) {
                return Err(DriverError::new(format!("{context}.taskId is unsafe")));
            }
            let attempt = record.get("attempt").and_then(Json::as_u64);
            if !matches!(attempt, Some(1 | 2)) {
                return Err(DriverError::new(format!(
                    "{context}.attempt must equal 1 or 2"
                )));
            }
            if !matches!(
                record.get("redaction").and_then(Json::as_str),
                Some("conservative-v1" | "conservative-v2")
            ) {
                return Err(DriverError::new(format!(
                    "{context}.redaction is unsupported"
                )));
            }
            let payload = if kind == "diagnosis" {
                "diagnosis"
            } else {
                "reason"
            };
            let verdict = if kind == "diagnosis" {
                DiagnosisVerdict::parse(record.get("verdict"), &format!("{context}.verdict"))?
            } else {
                None
            };
            let proposal = if kind == "diagnosis" {
                normalize_diagnosis_proposal(
                    record.get("proposal"),
                    &format!("{context}.proposal"),
                )?
            } else {
                None
            };
            if proposal.is_some() && verdict != Some(DiagnosisVerdict::Blocked) {
                return Err(DriverError::new(format!(
                    "{context}.proposal is allowed only with a blocked diagnosis verdict"
                )));
            }
            let visible = VisibleAttempt {
                task_id,
                attempt: attempt.expect("validated above"),
                comment,
                text: required_text(
                    record.get(payload),
                    &format!("{context}.{payload}"),
                    if kind == "diagnosis" {
                        MAX_DIAGNOSIS_CHARS
                    } else {
                        MAX_RETRY_CHARS
                    },
                )?,
                input_epoch,
                verdict,
                proposal,
            };
            Ok(if kind == "diagnosis" {
                AttemptEvent::Diagnosis(visible)
            } else {
                AttemptEvent::Retry(visible)
            })
        }
        "worker-outcome" => {
            let task_id =
                required_string(record.get("taskId"), &format!("{context}.taskId"), None)?;
            if !is_task_id(&task_id) {
                return Err(DriverError::new(format!("{context}.taskId is unsafe")));
            }
            let task_revision = required_string(
                record.get("taskRevision"),
                &format!("{context}.taskRevision"),
                Some(71),
            )?;
            let valid_revision = task_revision.len() == 71
                && task_revision.starts_with("sha256:")
                && task_revision[7..]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
            if !valid_revision {
                return Err(DriverError::new(format!(
                    "{context}.taskRevision must be a lowercase SHA-256 identity"
                )));
            }
            let task_uuid = required_string(
                record.get("taskUuid"),
                &format!("{context}.taskUuid"),
                Some(36),
            )?;
            let parsed_uuid = Uuid::parse_str(&task_uuid)
                .map_err(|_| DriverError::new(format!("{context}.taskUuid must be a UUID")))?;
            if parsed_uuid.to_string() != task_uuid {
                return Err(DriverError::new(format!(
                    "{context}.taskUuid must use canonical UUID spelling"
                )));
            }
            let outcome = match record.get("outcome").and_then(Json::as_str) {
                Some("needs-authority") => {
                    if !matches!(record.get("reason"), Some(Json::Null)) {
                        return Err(DriverError::new(format!(
                            "{context}.reason must be null for needs-authority"
                        )));
                    }
                    let paths =
                        normalize_paths(record.get("paths"), &format!("{context}.paths"), true)?
                            .expect("required outcome paths are present");
                    if paths.len() > 128 || paths.iter().any(|path| path.chars().count() > 4_096) {
                        return Err(DriverError::new(format!(
                            "{context}.paths exceeds the worker outcome bound"
                        )));
                    }
                    WorkerOutcome::NeedsAuthority { paths }
                }
                Some("impossible") => {
                    if !matches!(record.get("paths"), Some(Json::Null)) {
                        return Err(DriverError::new(format!(
                            "{context}.paths must be null for impossible"
                        )));
                    }
                    WorkerOutcome::Impossible {
                        reason: required_text(
                            record.get("reason"),
                            &format!("{context}.reason"),
                            12_000,
                        )?,
                    }
                }
                _ => {
                    return Err(DriverError::new(format!(
                        "{context}.outcome must be needs-authority or impossible"
                    )))
                }
            };
            Ok(AttemptEvent::WorkerOutcome(VisibleWorkerOutcome {
                task_id,
                task_revision,
                task_uuid,
                comment,
                input_epoch,
                outcome,
            }))
        }
        "escalation" => {
            required_text(record.get("body"), &format!("{context}.body"), 60_000)?;
            Ok(AttemptEvent::Escalation { comment })
        }
        "pardon" => {
            if !record.contains_key("tasks") {
                return Err(DriverError::new(format!("{context}.tasks is required")));
            }
            let tasks = match record.get("tasks") {
                Some(Json::Null) => None,
                value => {
                    let tasks = string_list(value, &format!("{context}.tasks"), true)?;
                    if tasks.iter().collect::<BTreeSet<_>>().len() != tasks.len()
                        || tasks.iter().any(|task_id| !is_task_id(task_id))
                    {
                        return Err(DriverError::new(format!(
                            "{context}.tasks must contain unique safe task IDs"
                        )));
                    }
                    Some(tasks)
                }
            };
            if record.contains_key("reason") {
                required_text(record.get("reason"), &format!("{context}.reason"), 4_000)?;
            }
            if record.contains_key("actor") {
                required_string(record.get("actor"), &format!("{context}.actor"), Some(128))?;
            }
            if let Some(nonce) = record.get("nonce") {
                let nonce = required_string(Some(nonce), &format!("{context}.nonce"), Some(36))?;
                Uuid::parse_str(&nonce)
                    .map_err(|_| DriverError::new(format!("{context}.nonce must be a UUID")))?;
            }
            Ok(AttemptEvent::Pardon { tasks, comment })
        }
        _ => unreachable!(),
    }
}

fn read_attempt_receipts(
    source: Option<&Json>,
    campaign: &str,
    issue_number: &str,
) -> Result<Vec<AttemptEvent>> {
    let path = attempt_receipts_path(source, campaign)?;
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(O_CLOEXEC | O_NOFOLLOW);
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(DriverError::new(format!(
                "cannot open attempt-receipts log {}: {error}",
                path.display()
            )))
        }
    };
    if unsafe { flock(file.as_raw_fd(), LOCK_SH) } != 0 {
        return Err(DriverError::new(format!(
            "cannot read attempt-receipts log {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    let result = (|| {
        let metadata = file.metadata().map_err(|error| {
            DriverError::new(format!(
                "cannot read attempt-receipts log {}: {error}",
                path.display()
            ))
        })?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.len() > MAX_ATTEMPT_RECEIPTS_LOG_BYTES
        {
            return Err(DriverError::new(format!(
                "attempt-receipts log is not a bounded private regular file: {}",
                path.display()
            )));
        }
        let mut raw = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut raw).map_err(|error| {
            DriverError::new(format!(
                "cannot read attempt-receipts log {}: {error}",
                path.display()
            ))
        })?;
        let complete = raw
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        raw.truncate(complete);
        let text = std::str::from_utf8(&raw).map_err(|error| {
            DriverError::new(format!(
                "attempt-receipts log {} is not UTF-8: {error}",
                path.display()
            ))
        })?;
        let mut events = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let sequence = index as u64 + 1;
            if line.is_empty() {
                return Err(DriverError::new(format!(
                    "attempt-receipts log {} contains a blank record",
                    path.display()
                )));
            }
            let candidate = json::parse(line).map_err(|error| {
                DriverError::new(format!(
                    "attempt receipt {sequence} in {} is invalid JSON: {error}",
                    path.display()
                ))
            })?;
            events.push(validate_attempt_receipt(
                &candidate,
                &path,
                sequence,
                campaign,
                issue_number,
            )?);
        }
        Ok(events)
    })();
    unsafe {
        flock(file.as_raw_fd(), LOCK_UN);
    }
    result
}

fn fold_attempt_receipts(
    events: Vec<AttemptEvent>,
    task_revisions: &BTreeMap<String, Option<String>>,
    current_epochs: &BTreeMap<String, String>,
) -> Result<AttemptState> {
    let inputs = AttemptFoldInputs {
        task_revisions,
        current_epochs,
    };
    let mut visible_diagnoses = BTreeMap::<String, Vec<VisibleAttempt>>::new();
    let mut visible_retries = BTreeMap::<String, Vec<VisibleAttempt>>::new();
    let mut visible_outcomes = BTreeMap::<String, Vec<VisibleWorkerOutcome>>::new();
    let mut diagnosis_counters = BTreeMap::<String, Vec<u64>>::new();
    let mut retry_counters = BTreeMap::<String, Vec<u64>>::new();
    let mut authority_counters = BTreeSet::<String>::new();
    let mut escalations = Vec::<(String, BTreeSet<String>, BTreeSet<String>)>::new();
    let mut lifetime_attempts = BTreeMap::<String, usize>::new();
    let mut warnings = Vec::new();

    fn belongs_to_current_epoch(
        task_id: &str,
        receipt_epoch: Option<&str>,
        current_epochs: &BTreeMap<String, String>,
    ) -> bool {
        current_epochs.is_empty()
            || receipt_epoch.is_none()
            || current_epochs.get(task_id).map(String::as_str) == receipt_epoch
    }

    fn keep_attempt(
        receipt: VisibleAttempt,
        diagnosis: bool,
        inputs: &AttemptFoldInputs<'_>,
        lifetime_attempts: &mut BTreeMap<String, usize>,
        counters: AttemptKinds<'_, u64>,
        visible: AttemptKinds<'_, VisibleAttempt>,
        warnings: &mut Vec<String>,
    ) {
        let kind = if diagnosis { "diagnosis" } else { "retry" };
        *lifetime_attempts
            .entry(receipt.task_id.clone())
            .or_default() += 1;
        if !inputs.task_revisions.contains_key(&receipt.task_id) {
            warnings.push(format!(
                "dropped machine {kind} for '{}': the worklist no longer names that task",
                receipt.task_id
            ));
            return;
        }
        if !belongs_to_current_epoch(
            &receipt.task_id,
            receipt.input_epoch.as_deref(),
            inputs.current_epochs,
        ) {
            return;
        }
        let counters = if diagnosis {
            counters.diagnoses
        } else {
            counters.retries
        };
        counters
            .entry(receipt.task_id.clone())
            .or_default()
            .push(receipt.attempt);
        let visible = if diagnosis {
            visible.diagnoses
        } else {
            visible.retries
        };
        let kept = visible.entry(receipt.task_id.clone()).or_default();
        let expected = kept.len() as u64 + 1;
        if receipt.attempt != expected {
            warnings.push(format!(
                "dropped machine {kind} for '{}' attempt {}: no attempt {expected} receipt precedes it",
                receipt.task_id, receipt.attempt
            ));
            return;
        }
        kept.push(receipt);
    }

    for event in events {
        match event {
            AttemptEvent::Diagnosis(receipt) => keep_attempt(
                receipt,
                true,
                &inputs,
                &mut lifetime_attempts,
                AttemptKinds {
                    diagnoses: &mut diagnosis_counters,
                    retries: &mut retry_counters,
                },
                AttemptKinds {
                    diagnoses: &mut visible_diagnoses,
                    retries: &mut visible_retries,
                },
                &mut warnings,
            ),
            AttemptEvent::Retry(receipt) => keep_attempt(
                receipt,
                false,
                &inputs,
                &mut lifetime_attempts,
                AttemptKinds {
                    diagnoses: &mut diagnosis_counters,
                    retries: &mut retry_counters,
                },
                AttemptKinds {
                    diagnoses: &mut visible_diagnoses,
                    retries: &mut visible_retries,
                },
                &mut warnings,
            ),
            AttemptEvent::WorkerOutcome(receipt) => {
                let Some(expected_revision) = task_revisions.get(&receipt.task_id) else {
                    warnings.push(format!(
                        "dropped worker outcome for '{}': the worklist no longer names that task",
                        receipt.task_id
                    ));
                    continue;
                };
                if expected_revision
                    .as_ref()
                    .is_some_and(|revision| revision != &receipt.task_revision)
                {
                    warnings.push(format!(
                        "dropped worker outcome for '{}': receipt revision {} does not match current revision {}",
                        receipt.task_id,
                        receipt.task_revision,
                        expected_revision.as_deref().expect("a mismatching revision is present")
                    ));
                    continue;
                }
                if !belongs_to_current_epoch(
                    &receipt.task_id,
                    receipt.input_epoch.as_deref(),
                    current_epochs,
                ) {
                    continue;
                }
                if matches!(receipt.outcome, WorkerOutcome::NeedsAuthority { .. }) {
                    authority_counters.insert(receipt.task_id.clone());
                }
                visible_outcomes
                    .entry(receipt.task_id.clone())
                    .or_default()
                    .push(receipt);
            }
            AttemptEvent::Escalation { comment } => {
                let mut contributors: BTreeSet<_> = visible_diagnoses
                    .iter()
                    .filter(|(_, diagnoses)| diagnoses.iter().any(VisibleAttempt::blocks_task))
                    .map(|(task_id, _)| task_id.clone())
                    .collect();
                contributors.extend(authority_counters.iter().cloned());
                contributors.extend(
                    lifetime_attempts
                        .iter()
                        .filter(|(task_id, attempts)| {
                            task_revisions.contains_key(*task_id)
                                && **attempts >= MAX_TASK_LIFETIME_ATTEMPTS
                        })
                        .map(|(task_id, _)| task_id.clone()),
                );
                if !contributors.is_empty() {
                    escalations.push((comment, contributors, BTreeSet::new()));
                }
            }
            AttemptEvent::Pardon { tasks, comment } => {
                let mut pardoned = 0usize;
                if let Some(tasks) = &tasks {
                    let scope: BTreeSet<_> = tasks.iter().cloned().collect();
                    for task_id in &scope {
                        pardoned += diagnosis_counters
                            .remove(task_id)
                            .map_or(0, |rows| rows.len());
                        pardoned += retry_counters.remove(task_id).map_or(0, |rows| rows.len());
                        visible_diagnoses.remove(task_id);
                        visible_retries.remove(task_id);
                        pardoned += visible_outcomes
                            .remove(task_id)
                            .map_or(0, |rows| rows.len());
                        authority_counters.remove(task_id);
                    }
                    let mut remaining = Vec::new();
                    for (comment, contributors, mut covered) in escalations {
                        covered.extend(scope.intersection(&contributors).cloned());
                        if !contributors.is_empty() && contributors.is_subset(&covered) {
                            pardoned += 1;
                        } else {
                            remaining.push((comment, contributors, covered));
                        }
                    }
                    escalations = remaining;
                } else {
                    pardoned += diagnosis_counters.values().map(Vec::len).sum::<usize>();
                    pardoned += retry_counters.values().map(Vec::len).sum::<usize>();
                    diagnosis_counters.clear();
                    retry_counters.clear();
                    visible_diagnoses.clear();
                    visible_retries.clear();
                    pardoned += visible_outcomes.values().map(Vec::len).sum::<usize>();
                    visible_outcomes.clear();
                    authority_counters.clear();
                    pardoned += escalations.len();
                    escalations.clear();
                }
                if pardoned != 0 {
                    let scope = tasks.map_or_else(String::new, |tasks| {
                        let mut tasks = tasks;
                        tasks.sort();
                        format!(
                            " for task(s) {}",
                            tasks
                                .iter()
                                .map(|task_id| format!("'{task_id}'"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    });
                    warnings.push(format!(
                        "campaign pardon {comment} pardoned {pardoned} earlier machine receipt(s){scope}"
                    ));
                }
            }
        }
    }
    if escalations.len() > 1 {
        return Err(DriverError::new(
            "multiple machine escalations claim this campaign",
        ));
    }
    let mut diagnoses: Vec<_> = visible_diagnoses.into_values().flatten().collect();
    let mut retries: Vec<_> = visible_retries.into_values().flatten().collect();
    let outcomes: Vec<_> = visible_outcomes.into_values().flatten().collect();
    diagnoses.sort_by(|left, right| {
        left.task_id
            .cmp(&right.task_id)
            .then(left.attempt.cmp(&right.attempt))
    });
    retries.sort_by(|left, right| {
        left.task_id
            .cmp(&right.task_id)
            .then(left.attempt.cmp(&right.attempt))
    });
    let lifetime_exhausted = lifetime_attempts
        .iter()
        .filter(|(task_id, attempts)| {
            task_revisions.contains_key(*task_id) && **attempts >= MAX_TASK_LIFETIME_ATTEMPTS
        })
        .map(|(task_id, _)| task_id.clone())
        .collect::<BTreeSet<_>>();
    for task_id in &lifetime_exhausted {
        let attempts = lifetime_attempts
            .get(task_id)
            .copied()
            .expect("an exhausted task has a lifetime count");
        warnings.push(format!(
            "task {task_id} latched for human attention after {attempts} lifetime attempt receipts (hard limit {MAX_TASK_LIFETIME_ATTEMPTS})"
        ));
    }
    Ok(AttemptState {
        diagnoses,
        retries,
        outcomes,
        escalation: escalations.into_iter().next().map(|row| row.0),
        lifetime_attempts,
        lifetime_exhausted,
        warnings,
    })
}

fn campaign_attempt_state(
    source: Option<&Json>,
    campaign: &str,
    issue_number: &str,
    task_revisions: &BTreeMap<String, Option<String>>,
    current_epochs: &BTreeMap<String, String>,
) -> Result<AttemptState> {
    fold_attempt_receipts(
        read_attempt_receipts(source, campaign, issue_number)?,
        task_revisions,
        current_epochs,
    )
}

fn attempt_state_all(
    events: Vec<AttemptEvent>,
    current_epochs: &BTreeMap<String, String>,
) -> Result<AttemptState> {
    let mut task_revisions = BTreeMap::new();
    for event in &events {
        match event {
            AttemptEvent::Diagnosis(receipt) | AttemptEvent::Retry(receipt) => {
                task_revisions
                    .entry(receipt.task_id.clone())
                    .or_insert(None);
            }
            AttemptEvent::WorkerOutcome(receipt) => {
                task_revisions.insert(receipt.task_id.clone(), Some(receipt.task_revision.clone()));
            }
            AttemptEvent::Escalation { .. } | AttemptEvent::Pardon { .. } => {}
        }
    }
    fold_attempt_receipts(events, &task_revisions, current_epochs)
}

fn campaign_attempt_state_all(
    source: Option<&Json>,
    campaign: &str,
    issue_number: &str,
) -> Result<AttemptState> {
    let current_epochs = attempt_receipt_input_epochs(source)?;
    attempt_state_all(
        read_attempt_receipts(source, campaign, issue_number)?,
        &current_epochs,
    )
}

fn prepare_attempt_receipts_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| DriverError::new("attemptReceipts.path has no parent"))?;
    fs::create_dir_all(parent).map_err(|error| {
        DriverError::new(format!(
            "cannot prepare attempt-receipts directory {}: {error}",
            parent.display()
        ))
    })?;
    let metadata = fs::symlink_metadata(parent).map_err(|error| {
        DriverError::new(format!(
            "cannot prepare attempt-receipts directory {}: {error}",
            parent.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(DriverError::new(
            "attempt-receipts parent must be a real directory",
        ));
    }
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|error| {
        DriverError::new(format!(
            "cannot prepare attempt-receipts directory {}: {error}",
            parent.display()
        ))
    })
}

fn read_attempt_records_locked(
    file: &mut File,
    path: &Path,
    campaign: &str,
    issue_number: &str,
    repair_tail: bool,
) -> Result<(Vec<Json>, Vec<AttemptEvent>)> {
    let metadata = file.metadata().map_err(|error| {
        DriverError::new(format!(
            "cannot read attempt-receipts log {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.len() > MAX_ATTEMPT_RECEIPTS_LOG_BYTES
    {
        return Err(DriverError::new(format!(
            "attempt-receipts log is not a bounded private regular file: {}",
            path.display()
        )));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut raw = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut raw)?;
    let complete = raw
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    if complete != raw.len() && repair_tail {
        file.set_len(complete as u64)?;
        file.sync_all()?;
    }
    raw.truncate(complete);
    let text = std::str::from_utf8(&raw).map_err(|error| {
        DriverError::new(format!(
            "attempt-receipts log {} is not UTF-8: {error}",
            path.display()
        ))
    })?;
    let mut records = Vec::new();
    let mut events = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            return Err(DriverError::new(format!(
                "attempt-receipts log {} contains a blank record",
                path.display()
            )));
        }
        let sequence = index as u64 + 1;
        let record = json::parse(line).map_err(|error| {
            DriverError::new(format!(
                "attempt receipt {sequence} in {} is invalid JSON: {error}",
                path.display()
            ))
        })?;
        events.push(validate_attempt_receipt(
            &record,
            path,
            sequence,
            campaign,
            issue_number,
        )?);
        records.push(record);
    }
    Ok((records, events))
}

fn record_with_comment(record: &Json, campaign: &str) -> Result<Json> {
    let mut object = record
        .as_object()
        .cloned()
        .ok_or_else(|| DriverError::new("attempt receipt must be an object"))?;
    let sequence = object
        .get("sequence")
        .and_then(Json::as_u64)
        .ok_or_else(|| DriverError::new("attempt receipt sequence is invalid"))?;
    object.insert(
        "comment".to_owned(),
        Json::from(attempt_receipt_url(campaign, sequence)),
    );
    Ok(Json::Object(object))
}

fn comment_sequence(comment: &str) -> Result<usize> {
    comment
        .rsplit_once('/')
        .and_then(|(_, sequence)| sequence.parse::<usize>().ok())
        .filter(|sequence| *sequence > 0)
        .ok_or_else(|| DriverError::new("attempt receipt comment has no sequence"))
}

fn append_attempt_receipt(
    source: Option<&Json>,
    campaign: &str,
    issue_number: &str,
    payload: Json,
) -> Result<(bool, Json)> {
    let input_epochs = attempt_receipt_input_epochs(source)?;
    let path = attempt_receipts_path(source, campaign)?;
    prepare_attempt_receipts_parent(&path)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .append(true)
        .mode(0o600)
        .custom_flags(O_CLOEXEC | O_NOFOLLOW);
    let (mut file, created) = match options.create_new(true).open(&path) {
        Ok(file) => (file, true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            options.create_new(false);
            (
                options.open(&path).map_err(|error| {
                    DriverError::new(format!(
                        "cannot open attempt-receipts log {}: {error}",
                        path.display()
                    ))
                })?,
                false,
            )
        }
        Err(error) => {
            return Err(DriverError::new(format!(
                "cannot create attempt-receipts log {}: {error}",
                path.display()
            )))
        }
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(DriverError::new(format!(
            "attempt-receipts log is not a private regular file: {}",
            path.display()
        )));
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    if unsafe { flock(file.as_raw_fd(), LOCK_EX) } != 0 {
        return Err(DriverError::new(format!(
            "cannot lock attempt-receipts log {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    let result = (|| {
        if created {
            let parent = path.parent().expect("validated parent");
            let directory = OpenOptions::new()
                .read(true)
                .custom_flags(O_CLOEXEC | O_DIRECTORY)
                .open(parent)?;
            directory.sync_all()?;
        }
        let (records, events) =
            read_attempt_records_locked(&mut file, &path, campaign, issue_number, true)?;
        let state = attempt_state_all(events, &input_epochs)?;
        let payload_object = payload
            .as_object()
            .ok_or_else(|| DriverError::new("attempt receipt payload must be an object"))?;
        if payload_object.contains_key("inputEpoch") {
            return Err(DriverError::new(
                "attempt receipt inputEpoch is derived from attemptReceipts authority",
            ));
        }
        let kind = payload_object
            .get("kind")
            .and_then(Json::as_str)
            .unwrap_or_default();
        if matches!(kind, "diagnosis" | "retry") {
            let active = if kind == "diagnosis" {
                &state.diagnoses
            } else {
                &state.retries
            };
            let task_id = payload_object.get("taskId").and_then(Json::as_str);
            let attempt = payload_object.get("attempt").and_then(Json::as_u64);
            if let Some(existing) = active.iter().find(|receipt| {
                Some(receipt.task_id.as_str()) == task_id && Some(receipt.attempt) == attempt
            }) {
                let sequence = comment_sequence(&existing.comment)?;
                return record_with_comment(&records[sequence - 1], campaign)
                    .map(|record| (false, record));
            }
            if task_id
                .and_then(|task_id| state.lifetime_attempts.get(task_id))
                .copied()
                .unwrap_or_default()
                >= MAX_TASK_LIFETIME_ATTEMPTS
            {
                return Err(DriverError::new(format!(
                    "task {:?} reached the hard lifetime limit of {MAX_TASK_LIFETIME_ATTEMPTS} attempt receipts",
                    task_id.unwrap_or_default()
                )));
            }
            let spent = active
                .iter()
                .filter(|receipt| Some(receipt.task_id.as_str()) == task_id)
                .count() as u64;
            if attempt != Some(spent + 1) {
                return Err(DriverError::new(format!(
                    "task {:?} {kind} attempt {:?} is not next after {spent} log receipts",
                    task_id.unwrap_or_default(),
                    attempt.unwrap_or_default()
                )));
            }
        } else if kind == "worker-outcome" {
            let task_uuid = payload_object.get("taskUuid").and_then(Json::as_str);
            if let Some(existing) = state
                .outcomes
                .iter()
                .find(|receipt| Some(receipt.task_uuid.as_str()) == task_uuid)
            {
                let sequence = comment_sequence(&existing.comment)?;
                return record_with_comment(&records[sequence - 1], campaign)
                    .map(|record| (false, record));
            }
        } else if kind == "escalation" {
            if let Some(comment) = &state.escalation {
                let sequence = comment_sequence(comment)?;
                return record_with_comment(&records[sequence - 1], campaign)
                    .map(|record| (false, record));
            }
        }
        let authority = read_attempt_receipt_authority(&path, campaign, issue_number)?;
        let sequence = records.len() as u64 + 1;
        let mut record = BTreeMap::from([
            (
                "schemaVersion".to_owned(),
                Json::Number(ATTEMPT_RECEIPT_SCHEMA_VERSION.to_string()),
            ),
            ("sequence".to_owned(), Json::Number(sequence.to_string())),
            ("campaign".to_owned(), Json::from(campaign)),
            ("issueNumber".to_owned(), Json::from(issue_number)),
            (
                "armSerial".to_owned(),
                Json::Number(authority.arm_serial.to_string()),
            ),
            (
                "worklistSha256".to_owned(),
                Json::from(authority.worklist_sha256),
            ),
            (
                "writtenAt".to_owned(),
                Json::from(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)),
            ),
            (
                "actor".to_owned(),
                Json::from(ATTEMPT_RECEIPT_MACHINE_ACTOR),
            ),
        ]);
        record.extend(payload_object.clone());
        if matches!(kind, "diagnosis" | "retry" | "worker-outcome") {
            let task_id = payload_object
                .get("taskId")
                .and_then(Json::as_str)
                .ok_or_else(|| DriverError::new("attempt receipt taskId is required"))?;
            if let Some(epoch) = input_epochs.get(task_id) {
                record.insert("inputEpoch".to_owned(), Json::from(epoch.clone()));
            } else if !input_epochs.is_empty() {
                return Err(DriverError::new(format!(
                    "attemptReceipts.inputEpochs has no current epoch for task {task_id:?}"
                )));
            }
        }
        let record = Json::Object(record);
        validate_attempt_receipt(&record, &path, sequence, campaign, issue_number)?;
        let line = format!("{}\n", record.stringify());
        if file.metadata()?.len() + line.len() as u64 > MAX_ATTEMPT_RECEIPTS_LOG_BYTES {
            return Err(DriverError::new("attempt-receipts log exceeds 128 MiB"));
        }
        file.write_all(line.as_bytes()).map_err(|error| {
            DriverError::new(format!(
                "cannot append attempt-receipts log {}: {error}",
                path.display()
            ))
        })?;
        file.sync_all()?;
        record_with_comment(&record, campaign).map(|record| (true, record))
    })();
    unsafe {
        flock(file.as_raw_fd(), LOCK_UN);
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn record_diagnosis(
    source: Option<&Json>,
    campaign: &str,
    issue_number: &str,
    task_id: &str,
    attempt: u64,
    diagnosis: &str,
    verdict: DiagnosisVerdict,
    proposal: Option<&Json>,
) -> Result<String> {
    let mut payload = BTreeMap::from([
        ("kind".to_owned(), Json::from("diagnosis")),
        ("taskId".to_owned(), Json::from(task_id)),
        ("attempt".to_owned(), Json::Number(attempt.to_string())),
        ("diagnosis".to_owned(), Json::from(diagnosis)),
        ("verdict".to_owned(), Json::from(verdict.as_str())),
        ("redaction".to_owned(), Json::from(PUBLIC_REDACTION)),
    ]);
    if let Some(proposal) = proposal {
        payload.insert("proposal".to_owned(), proposal.clone());
    }
    let (_, receipt) =
        append_attempt_receipt(source, campaign, issue_number, Json::Object(payload))?;
    Ok(receipt
        .as_object()
        .and_then(|receipt| receipt.get("comment"))
        .and_then(Json::as_str)
        .expect("append receipt returns a comment")
        .to_owned())
}

fn task_revision(task: &BTreeMap<String, Json>) -> Result<Option<String>> {
    let Some(value) = task.get("revision") else {
        return Ok(None);
    };
    if matches!(value, Json::Null) {
        return Ok(None);
    }
    let task_id = task.get("id").and_then(Json::as_str).unwrap_or("None");
    let revision = required_string(Some(value), &format!("task {task_id} revision"), None)?;
    let valid = revision.len() == 71
        && revision.starts_with("sha256:")
        && revision[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !valid {
        return Err(DriverError::new(
            "task revision must be a lowercase SHA-256 identity",
        ));
    }
    Ok(Some(revision))
}

fn campaign_branch_prefix(campaign: &str, identity: &str) -> String {
    format!(
        "tally/{}-campaign-{}",
        safe_slug(campaign, 32),
        safe_slug(identity, 64)
    )
}

fn integration_branch(campaign: &str, identity: &str) -> String {
    format!("{}/integration", campaign_branch_prefix(campaign, identity))
}

fn local_branch_oid(checkout: &Path, branch: &str) -> Result<Option<String>> {
    let reference = format!("refs/heads/{branch}");
    let resolved = git(
        checkout,
        ["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
        false,
    )?;
    if !resolved.success() {
        return Ok(None);
    }
    let oid = resolved.stdout_trimmed();
    if !is_full_oid(&oid) {
        return Err(DriverError::new(
            "local campaign branch listing returned malformed output",
        ));
    }
    Ok(Some(oid))
}

fn ensure_integration_branch(
    config: &RepoConfig,
    campaign: &str,
    identity: &str,
    start_rev: &str,
    lineage_rev: &str,
) -> Result<String> {
    if !is_full_oid(start_rev) {
        return Err(DriverError::new(
            "integration branch start revision must be a full Git object ID",
        ));
    }
    if !is_full_oid(lineage_rev) {
        return Err(DriverError::new(
            "integration branch witnessed revision must be a full Git object ID",
        ));
    }
    git(
        &config.checkout,
        ["cat-file", "-e", &format!("{start_rev}^{{commit}}")],
        true,
    )?;
    git(
        &config.checkout,
        ["cat-file", "-e", &format!("{lineage_rev}^{{commit}}")],
        true,
    )?;
    let branch = integration_branch(campaign, identity);
    let reference = format!("refs/heads/{branch}");
    let mut current = local_branch_oid(&config.checkout, &branch)?;
    if current.is_none() {
        let created = git(
            &config.checkout,
            [
                "update-ref",
                &reference,
                start_rev,
                &"0".repeat(start_rev.len()),
            ],
            false,
        )?;
        if created.success() {
            return Ok(start_rev.to_owned());
        }
        current = local_branch_oid(&config.checkout, &branch)?;
        if current.is_none() {
            return Err(DriverError::new(format!(
                "cannot create local integration branch {branch:?}: {}",
                created.detail()
            )));
        }
    }
    let current = current.expect("initialized above");
    let common = git(&config.checkout, ["merge-base", start_rev, &current], false)?;
    if !common.success() || !is_full_oid(&common.stdout_trimmed()) {
        return Err(DriverError::new(format!(
            "local integration branch {branch:?} shares no history with repository revision {start_rev}"
        )));
    }
    if lineage_rev != start_rev
        && !git(
            &config.checkout,
            ["merge-base", "--is-ancestor", lineage_rev, &current],
            false,
        )?
        .success()
    {
        return Err(DriverError::new(format!(
            "local integration branch {branch:?} does not descend from witnessed revision {lineage_rev}"
        )));
    }
    Ok(current)
}

fn required_integration_revision(
    config: &RepoConfig,
    campaign: &str,
    identity: &str,
) -> Result<String> {
    let branch = integration_branch(campaign, identity);
    local_branch_oid(&config.checkout, &branch)?.ok_or_else(|| {
        DriverError::new(format!(
            "local integration branch {branch:?} does not exist"
        ))
    })
}

fn task_object<'a>(task: &'a Json, context: &str) -> Result<&'a BTreeMap<String, Json>> {
    task.as_object()
        .ok_or_else(|| DriverError::new(format!("{context} must be an object")))
}

fn task_id(task: &Json) -> Result<&str> {
    task.as_object()
        .and_then(|task| task.get("id"))
        .and_then(Json::as_str)
        .ok_or_else(|| DriverError::new("normalized task omitted id"))
}

fn task_kind(task: &Json) -> Result<&str> {
    task.as_object()
        .and_then(|task| task.get("kind"))
        .and_then(Json::as_str)
        .ok_or_else(|| DriverError::new("normalized task omitted kind"))
}

fn task_dependencies(task: &Json) -> Result<Vec<String>> {
    string_list(
        task.as_object().and_then(|task| task.get("dependencies")),
        &format!("task {} dependencies", task_id(task).unwrap_or("<unknown>")),
        false,
    )
}

fn merged_local_tasks(
    repository: &str,
    config: &RepoConfig,
    campaign: &str,
    campaign_id: &str,
    base_rev: &str,
    tasks: &[Json],
) -> Result<Vec<Json>> {
    let Some(branch_tip) =
        local_branch_oid(&config.checkout, &integration_branch(campaign, campaign_id))?
    else {
        return Ok(Vec::new());
    };
    let base_rev = full_oid(Some(&Json::from(base_rev)), "local integration revision")?;
    if !git(
        &config.checkout,
        ["merge-base", "--is-ancestor", &base_rev, &branch_tip],
        false,
    )?
    .success()
    {
        return Err(DriverError::new(
            "witnessed local integration revision is not on the integration branch",
        ));
    }
    let history = git(
        &config.checkout,
        [
            "log",
            "--first-parent",
            "-z",
            "--format=%H%x00%(trailers:key=Tally-Task,valueonly,unfold=true,separator=%x1f)%x00%(trailers:key=Tally-Revision,valueonly,unfold=true,separator=%x1f)",
            &base_rev,
        ],
        true,
    )?
    .stdout;
    let mut fields: Vec<_> = history.split(|byte| *byte == 0).collect();
    if fields.last().is_some_and(|field| field.is_empty()) {
        fields.pop();
    }
    if fields.len() % 3 != 0 {
        return Err(DriverError::new(
            "local integration trailer listing returned malformed output",
        ));
    }
    let mut claims = BTreeMap::<(String, String), Vec<String>>::new();
    for row in fields.chunks_exact(3) {
        let commit = std::str::from_utf8(row[0]).map_err(|_| {
            DriverError::new("local integration trailer listing returned a malformed commit")
        })?;
        if !is_full_oid(commit) {
            return Err(DriverError::new(
                "local integration trailer listing returned a malformed commit",
            ));
        }
        let task_values = std::str::from_utf8(row[1]).unwrap_or_default();
        let revision_values = std::str::from_utf8(row[2]).unwrap_or_default();
        let task_claims: Vec<_> = if task_values.is_empty() {
            Vec::new()
        } else {
            task_values.split('\x1f').collect()
        };
        let revision_claims: Vec<_> = if revision_values.is_empty() {
            Vec::new()
        } else {
            revision_values.split('\x1f').collect()
        };
        if task_claims.len() != 1 || revision_claims.len() != 1 {
            continue;
        }
        let claimed_task = task_claims[0];
        let revision = revision_claims[0];
        let valid_revision = revision.len() == 71
            && revision.starts_with("sha256:")
            && revision[7..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !is_task_id(claimed_task) || !valid_revision {
            continue;
        }
        claims
            .entry((claimed_task.to_owned(), revision.to_owned()))
            .or_default()
            .push(commit.to_owned());
    }
    let mut facts = Vec::new();
    for task in tasks {
        if task_kind(task)? != "implementation" {
            continue;
        }
        let object = task_object(task, "local completion task")?;
        let identifier = task_id(task)?;
        let revision = task_revision(object)?.ok_or_else(|| {
            DriverError::new(format!(
                "local completion task {identifier:?} carries no revision trailer"
            ))
        })?;
        let matches = claims
            .get(&(identifier.to_owned(), revision.clone()))
            .cloned()
            .unwrap_or_default();
        if matches.len() > 1 {
            return Err(DriverError::new(format!(
                "multiple local integration commits claim campaign task {identifier:?} revision {revision}"
            )));
        }
        let Some(merge_commit) = matches.into_iter().next() else {
            continue;
        };
        let branch = stable_publish_branch(campaign, campaign_id, identifier, Some(&revision));
        facts.push(Json::object([
            ("taskId", Json::from(identifier)),
            (
                "pullRequest",
                Json::from(format!("local://{repository}/{branch}")),
            ),
            ("mergeCommit", Json::from(merge_commit)),
            ("revision", Json::from(revision)),
        ]));
    }
    Ok(facts)
}

fn checkpoint_ref(
    campaign: &str,
    issue_number: &str,
    task_id: &str,
    source_sha256: &str,
    base_rev: &str,
) -> Result<String> {
    let valid_sha = source_sha256.len() == 71
        && source_sha256.starts_with("sha256:")
        && source_sha256[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !valid_sha {
        return Err(DriverError::new(
            "worklist source digest is not a lowercase SHA-256 identity",
        ));
    }
    if !is_full_oid(base_rev) {
        return Err(DriverError::new(
            "checkpoint base revision must be a full Git object ID",
        ));
    }
    Ok(format!(
        "{}/checkpoint/{task_id}-{}/{base_rev}",
        local_state_prefix(campaign, issue_number),
        &source_sha256[7..]
    ))
}

fn remote_ref_oid(checkout: &Path, remote: &str, reference: &str) -> Result<Option<String>> {
    let listed = git(checkout, ["ls-remote", "--refs", remote, reference], true)?;
    let lines: Vec<_> = listed
        .stdout_text()
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    if lines.is_empty() {
        return Ok(None);
    }
    if lines.len() != 1 {
        return Err(DriverError::new(format!(
            "remote ref lookup for {reference:?} returned {} rows",
            lines.len()
        )));
    }
    let Some((oid, observed_reference)) = lines[0].split_once('\t') else {
        return Err(DriverError::new(format!(
            "remote ref lookup for {reference:?} returned malformed output"
        )));
    };
    if observed_reference != reference || !is_full_oid(oid) {
        return Err(DriverError::new(format!(
            "remote ref lookup for {reference:?} returned malformed output"
        )));
    }
    Ok(Some(oid.to_owned()))
}

fn completed_checkpoint_tasks(
    config: &RepoConfig,
    campaign: &str,
    issue_number: &str,
    tasks: &[Json],
    source: &BTreeMap<String, Json>,
    merged: &[Json],
    base_rev: &str,
) -> Result<Vec<Json>> {
    if !tasks
        .iter()
        .any(|task| task_kind(task).ok() == Some("checkpoint"))
    {
        return Ok(Vec::new());
    }
    git(
        &config.checkout,
        ["fetch", "--prune", "--no-tags", &config.remote],
        true,
    )?;
    if !is_full_oid(base_rev) {
        return Err(DriverError::new(
            "campaign base revision must be a full Git object ID",
        ));
    }
    let source_sha = required_string(source.get("sha256"), "worklist source digest", None)?;
    let mut completed_revisions = BTreeMap::new();
    for fact in merged {
        let fact = task_object(fact, "merged fact")?;
        completed_revisions.insert(
            required_string(fact.get("taskId"), "merged fact.taskId", None)?,
            full_oid(fact.get("mergeCommit"), "merged fact.mergeCommit")?,
        );
    }
    let mut facts = Vec::new();
    for task in tasks {
        if task_kind(task)? != "checkpoint" {
            continue;
        }
        let identifier = task_id(task)?;
        let dependencies = task_dependencies(task)?;
        if !dependencies
            .iter()
            .all(|dependency| completed_revisions.contains_key(dependency))
        {
            continue;
        }
        let reference = checkpoint_ref(campaign, issue_number, identifier, &source_sha, base_rev)?;
        let Some(target) = remote_ref_oid(&config.checkout, &config.remote, &reference)? else {
            continue;
        };
        git(
            &config.checkout,
            ["fetch", "--no-tags", &config.remote, &reference],
            true,
        )?;
        let fetched = git(
            &config.checkout,
            ["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
            true,
        )?
        .stdout_trimmed();
        let object_type =
            git(&config.checkout, ["cat-file", "-t", &target], true)?.stdout_trimmed();
        if fetched != target || object_type != "commit" {
            return Err(DriverError::new(format!(
                "checkpoint ref {reference:?} must point directly to a commit"
            )));
        }
        if target != base_rev {
            return Err(DriverError::new(format!(
                "checkpoint ref {reference:?} does not point to its named base revision"
            )));
        }
        for dependency in &dependencies {
            let dependency_revision = completed_revisions.get(dependency).expect("checked above");
            if !git(
                &config.checkout,
                ["merge-base", "--is-ancestor", dependency_revision, &target],
                false,
            )?
            .success()
            {
                return Err(DriverError::new(format!(
                    "checkpoint ref {reference:?} does not contain dependency {dependency:?} revision {dependency_revision}"
                )));
            }
        }
        facts.push(Json::object([
            ("taskId", Json::from(identifier)),
            ("ref", Json::from(reference)),
            ("revision", Json::from(target.clone())),
        ]));
        completed_revisions.insert(identifier.to_owned(), target);
    }
    Ok(facts)
}

fn observed_base_revision(config: &RepoConfig) -> Result<String> {
    git(
        &config.checkout,
        ["fetch", "--prune", "--no-tags", &config.remote],
        true,
    )?;
    Ok(git(
        &config.checkout,
        [
            "rev-parse",
            "--verify",
            &format!("{}/{}^{{commit}}", config.remote, config.base_branch),
        ],
        true,
    )?
    .stdout_trimmed())
}

fn closing_summary_marker(
    campaign: &str,
    issue_number: &str,
    outcome: &str,
    source_sha256: &str,
) -> String {
    if outcome == "complete" {
        format!("<!-- tally:campaign-complete:v1 source={source_sha256} -->")
    } else {
        format!(
            "<!-- tally:campaign-summary:v1 campaign={campaign} issue={issue_number} outcome={outcome} -->"
        )
    }
}

fn publish_closing_summary(
    repository: &str,
    config: &RepoConfig,
    campaign: &str,
    issue_number: &str,
    digest: &tally_core::campaign_folds::CampaignDigest,
) -> Result<String> {
    let marker = closing_summary_marker(
        campaign,
        issue_number,
        &digest.outcome,
        &digest.source.sha256,
    );
    let body = format!("{marker}\n\n{}", render_campaign_summary(digest));
    if body.chars().count() > 60_000 {
        return Err(DriverError::new(
            "campaign closing summary exceeds 60,000 characters",
        ));
    }
    let reference = stage_scoped_summary_ref(
        &local_state_prefix(campaign, issue_number),
        &digest.source.sha256,
        &digest.outcome,
    )
    .map_err(|error| DriverError::new(error.to_string()))?;
    let expected = Json::object([
        ("schemaVersion", Json::Number("1".to_owned())),
        ("kind", Json::from("closing-summary")),
        ("campaign", Json::from(campaign)),
        ("issueNumber", Json::from(issue_number)),
        ("outcome", Json::from(digest.outcome.clone())),
        ("body", Json::from(body)),
    ]);
    let (_, observed) = write_local_blob(config, &reference, &expected)?;
    if observed != expected {
        return Err(DriverError::new(format!(
            "local campaign summary {reference:?} disagrees with this outcome"
        )));
    }
    Ok(format!("local://{repository}/{reference}"))
}

fn task_domains(task: &Json) -> Vec<&str> {
    task.as_object()
        .and_then(|task| task.get("conflictDomains"))
        .and_then(Json::as_array)
        .map(|domains| domains.iter().filter_map(Json::as_str).collect())
        .unwrap_or_default()
}

fn task_conflicts(task: &Json, selected: &[Json]) -> bool {
    task_domains(task).iter().any(|left| {
        selected.iter().any(|other| {
            task_domains(other)
                .iter()
                .any(|right| domains_overlap(left, right))
        })
    })
}

fn related_tasks(tasks: &[Json], starting_task: &str) -> Result<BTreeSet<String>> {
    let mut dependencies = BTreeMap::<String, BTreeSet<String>>::new();
    for task in tasks {
        dependencies.insert(
            task_id(task)?.to_owned(),
            task_dependencies(task)?.into_iter().collect(),
        );
    }
    let mut related = BTreeSet::from([starting_task.to_owned()]);
    let mut frontier = vec![starting_task.to_owned()];
    while let Some(current) = frontier.pop() {
        for dependency in dependencies.get(&current).into_iter().flatten() {
            if related.insert(dependency.clone()) {
                frontier.push(dependency.clone());
            }
        }
    }
    let mut frontier = vec![starting_task.to_owned()];
    while let Some(current) = frontier.pop() {
        for (candidate, candidate_dependencies) in &dependencies {
            if candidate_dependencies.contains(&current) && related.insert(candidate.clone()) {
                frontier.push(candidate.clone());
            }
        }
    }
    Ok(related)
}

fn checkpoint_deferrals(
    tasks: &[Json],
    remaining: &[Json],
    completed_ids: &BTreeSet<String>,
    blocked_ids: &BTreeSet<String>,
) -> Result<Vec<Json>> {
    let mut deferrals = Vec::new();
    for task in tasks {
        let identifier = task_id(task)?;
        if task_kind(task)? != "checkpoint" || completed_ids.contains(identifier) {
            continue;
        }
        let related = related_tasks(tasks, identifier)?;
        let waiting: Vec<_> = remaining
            .iter()
            .filter_map(|candidate| {
                let candidate_id = task_id(candidate).ok()?;
                (task_kind(candidate).ok()? != "checkpoint"
                    && !related.contains(candidate_id)
                    && !blocked_ids.contains(candidate_id))
                .then(|| Json::from(candidate_id))
            })
            .collect();
        if !waiting.is_empty() {
            deferrals.push(Json::object([
                ("taskId", Json::from(identifier)),
                ("waitingOn", Json::Array(waiting)),
            ]));
        }
    }
    Ok(deferrals)
}

fn parallelism_warnings(
    ready: &[Json],
    frontier: &[Json],
    max_parallel: usize,
) -> Result<Vec<String>> {
    if ready.len() < max_parallel || frontier.len() >= max_parallel {
        return Ok(Vec::new());
    }
    let selected_ids: BTreeSet<_> = frontier
        .iter()
        .map(task_id)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .collect();
    let blocked: Vec<_> = ready
        .iter()
        .filter(|task| task_id(task).is_ok_and(|id| !selected_ids.contains(id)))
        .collect();
    let mut examples = Vec::new();
    for task in &blocked {
        let mut found = false;
        for other in frontier {
            for left in task_domains(task) {
                for right in task_domains(other) {
                    if domains_overlap(left, right) {
                        examples.push(format!(
                            "{}:{} overlaps {}:{}",
                            task_id(task)?,
                            Json::from(left).stringify(),
                            task_id(other)?,
                            Json::from(right).stringify()
                        ));
                        found = true;
                        break;
                    }
                }
                if found {
                    break;
                }
            }
            if found {
                break;
            }
        }
        if examples.len() == 8 {
            break;
        }
    }
    let mut blocked_ids = blocked
        .iter()
        .take(12)
        .map(|task| task_id(task).map(str::to_owned))
        .collect::<Result<Vec<_>>>()?
        .join(", ");
    if blocked.len() > 12 {
        blocked_ids.push_str(&format!(", and {} more", blocked.len() - 12));
    }
    let detail = if examples.is_empty() {
        "no overlap example available".to_owned()
    } else {
        examples.join("; ")
    };
    Ok(vec![format!(
        "conflictDomains limited this ready frontier to {} of requested maxParallel {max_parallel}; blocked tasks: {blocked_ids}; overlaps: {detail}",
        frontier.len()
    )])
}

fn action_reconcile(brief: &Json) -> Result<Json> {
    let mut fields = vec![
        "campaign",
        "campaignIdentity",
        "repository",
        "repositoryConfig",
        "issue",
        "worklist",
        "maxTasks",
        "maxParallel",
        "gateSet",
        "steeringHighWater",
        "taskInputHashes",
        "attemptReceipts",
    ];
    if brief
        .as_object()
        .is_some_and(|object| object.contains_key("specRepository"))
    {
        fields.push("specRepository");
    }
    if brief
        .as_object()
        .is_some_and(|object| object.contains_key("issueRepository"))
    {
        fields.push("issueRepository");
    }
    let data = object_exact(brief, &fields, "reconcile brief")?;
    let campaign = required_string(data.get("campaign"), "campaign", None)?;
    if !is_component(&campaign) {
        return Err(DriverError::new("campaign is not a safe component"));
    }
    let (issue_number, _issue_url) = campaign_issue(data.get("issue"))?;
    let mut worklist_brief = BTreeMap::new();
    for field in [
        "repository",
        "repositoryConfig",
        "worklist",
        "maxTasks",
        "maxParallel",
    ] {
        worklist_brief.insert(
            field.to_owned(),
            data.get(field).cloned().unwrap_or(Json::Null),
        );
    }
    for field in ["specRepository", "issueRepository"] {
        if let Some(value) = data.get(field) {
            worklist_brief.insert(field.to_owned(), value.clone());
        }
    }
    let worklist = action_worklist(&Json::Object(worklist_brief))?;
    let worklist = worklist
        .as_object()
        .expect("native worklist action returns an object");
    let repository = required_string(worklist.get("repository"), "repository", None)?;
    let config = repo_config(data.get("repositoryConfig"))?;
    let max_parallel = data.get("maxParallel").and_then(Json::as_u64).unwrap_or(1) as usize;
    let (code, spec, issue_target) = campaign_coordinates(data, repository.clone(), config)?;
    let source = worklist
        .get("source")
        .and_then(Json::as_object)
        .ok_or_else(|| DriverError::new("worklist source must be an object"))?;
    let source_revision =
        required_string(source.get("revision"), "worklist source revision", None)?;
    let initial_base = if same_repository(&spec, &code) {
        source_revision
    } else {
        observed_base_revision(&code.config)?
    };
    let local_campaign_id = campaign_identity(data, &campaign)?;
    let base_rev = ensure_integration_branch(
        &code.config,
        &campaign,
        &local_campaign_id,
        &initial_base,
        &initial_base,
    )?;
    let tasks = worklist
        .get("tasks")
        .and_then(Json::as_array)
        .ok_or_else(|| DriverError::new("worklist tasks must be an array"))?
        .to_vec();
    let merged = merged_local_tasks(
        &code.repository,
        &code.config,
        &campaign,
        &local_campaign_id,
        &base_rev,
        &tasks,
    )?;
    let checkpoints = completed_checkpoint_tasks(
        &code.config,
        &campaign,
        &issue_number,
        &tasks,
        source,
        &merged,
        &base_rev,
    )?;
    let mut completed_ids = BTreeSet::new();
    for fact in merged.iter().chain(&checkpoints) {
        completed_ids.insert(
            fact.as_object()
                .and_then(|fact| fact.get("taskId"))
                .and_then(Json::as_str)
                .expect("completion fact has taskId")
                .to_owned(),
        );
    }
    let task_revisions: BTreeMap<_, _> = tasks
        .iter()
        .map(|task| {
            Ok((
                task_id(task)?.to_owned(),
                task_revision(task_object(task, "reconciled task")?)?,
            ))
        })
        .collect::<Result<_>>()?;
    let input_epochs = current_task_input_epochs(
        &tasks,
        data.get("gateSet"),
        data.get("steeringHighWater"),
        data.get("taskInputHashes"),
    )?;
    let carried_epochs = attempt_receipt_input_epochs(data.get("attemptReceipts"))?;
    if !carried_epochs.is_empty() && carried_epochs != input_epochs {
        return Err(DriverError::new(
            "attemptReceipts.inputEpochs disagrees with the reconciled input",
        ));
    }
    let mut attempts = campaign_attempt_state(
        data.get("attemptReceipts"),
        &campaign,
        &issue_number,
        &task_revisions,
        &input_epochs,
    )?;
    let order: BTreeMap<_, _> = tasks
        .iter()
        .enumerate()
        .map(|(index, task)| Ok((task_id(task)?.to_owned(), index)))
        .collect::<Result<_>>()?;
    attempts.diagnoses.sort_by_key(|item| {
        (
            order.get(&item.task_id).copied().unwrap_or(usize::MAX),
            item.attempt,
        )
    });
    attempts.retries.sort_by_key(|item| {
        (
            order.get(&item.task_id).copied().unwrap_or(usize::MAX),
            item.attempt,
        )
    });
    attempts.outcomes.sort_by_key(|item| {
        (
            order.get(&item.task_id).copied().unwrap_or(usize::MAX),
            comment_sequence(&item.comment).unwrap_or(usize::MAX),
        )
    });
    let remaining: Vec<_> = tasks
        .iter()
        .filter(|task| task_id(task).is_ok_and(|id| !completed_ids.contains(id)))
        .cloned()
        .collect();
    let mut direct_blocked: BTreeSet<_> = attempts
        .diagnoses
        .iter()
        .filter(|diagnosis| diagnosis.blocks_task() && !completed_ids.contains(&diagnosis.task_id))
        .map(|diagnosis| diagnosis.task_id.clone())
        .collect();
    direct_blocked.extend(
        attempts
            .outcomes
            .iter()
            .filter(|outcome| {
                matches!(outcome.outcome, WorkerOutcome::NeedsAuthority { .. })
                    && !completed_ids.contains(&outcome.task_id)
            })
            .map(|outcome| outcome.task_id.clone()),
    );
    direct_blocked.extend(
        attempts
            .lifetime_exhausted
            .iter()
            .filter(|task_id| !completed_ids.contains(*task_id))
            .cloned(),
    );
    let mut blocked_by = BTreeMap::<String, BTreeSet<String>>::new();
    let mut blocked = Vec::new();
    for task in &tasks {
        let identifier = task_id(task)?;
        let mut roots = if direct_blocked.contains(identifier) {
            BTreeSet::from([identifier.to_owned()])
        } else {
            BTreeSet::new()
        };
        for dependency in task_dependencies(task)? {
            roots.extend(blocked_by.get(&dependency).into_iter().flatten().cloned());
        }
        blocked_by.insert(identifier.to_owned(), roots.clone());
        if !completed_ids.contains(identifier) && !roots.is_empty() {
            let mut roots: Vec<_> = roots.into_iter().collect();
            roots.sort_by_key(|root| order.get(root).copied().unwrap_or(usize::MAX));
            blocked.push(Json::object([
                ("taskId", Json::from(identifier)),
                (
                    "blockedBy",
                    Json::Array(roots.into_iter().map(Json::from).collect()),
                ),
            ]));
        }
    }
    let blocked_ids: BTreeSet<_> = blocked
        .iter()
        .filter_map(|fact| fact.as_object()?.get("taskId")?.as_str().map(str::to_owned))
        .collect();
    let mut ready = Vec::new();
    for task in &remaining {
        let identifier = task_id(task)?;
        if !blocked_ids.contains(identifier)
            && task_dependencies(task)?
                .iter()
                .all(|dependency| completed_ids.contains(dependency))
        {
            ready.push(task.clone());
        }
    }
    let deferrals = checkpoint_deferrals(&tasks, &remaining, &completed_ids, &blocked_ids)?;
    let deferred_ids: BTreeSet<_> = deferrals
        .iter()
        .filter_map(|fact| fact.as_object()?.get("taskId")?.as_str().map(str::to_owned))
        .collect();
    ready.sort_by_key(|task| {
        task_id(task).is_ok_and(|identifier| deferred_ids.contains(identifier))
    });
    let mut frontier = Vec::new();
    for task in &ready {
        if frontier.len() == max_parallel {
            break;
        }
        if !task_conflicts(task, &frontier) {
            frontier.push(task.clone());
        }
    }
    attempts
        .warnings
        .extend(parallelism_warnings(&ready, &frontier, max_parallel)?);
    let diagnoses: Vec<_> = attempts
        .diagnoses
        .iter()
        .map(VisibleAttempt::diagnosis_json)
        .collect();
    let retries: Vec<_> = attempts
        .retries
        .iter()
        .map(VisibleAttempt::retry_json)
        .collect();
    let outcomes: Vec<_> = attempts
        .outcomes
        .iter()
        .map(VisibleWorkerOutcome::to_json)
        .collect();
    let remaining_ids: Vec<_> = remaining
        .iter()
        .map(|task| task_id(task).map(Json::from))
        .collect::<Result<_>>()?;
    let quiescent = !remaining.is_empty() && frontier.is_empty();
    let mut result = BTreeMap::from([
        ("schemaVersion".to_owned(), Json::Number("1".to_owned())),
        ("campaign".to_owned(), Json::from(campaign.clone())),
        ("repository".to_owned(), Json::from(repository)),
        (
            "source".to_owned(),
            worklist.get("source").cloned().expect("source exists"),
        ),
        ("baseRevision".to_owned(), Json::from(base_rev)),
        ("tasks".to_owned(), Json::Array(tasks)),
        (
            "inputEpochs".to_owned(),
            Json::Object(
                input_epochs
                    .into_iter()
                    .map(|(task_id, epoch)| (task_id, Json::from(epoch)))
                    .collect(),
            ),
        ),
        ("merged".to_owned(), Json::Array(merged)),
        ("checkpoints".to_owned(), Json::Array(checkpoints)),
        ("remaining".to_owned(), Json::Array(remaining_ids)),
        ("frontier".to_owned(), Json::Array(frontier)),
        ("diagnoses".to_owned(), Json::Array(diagnoses)),
        ("retries".to_owned(), Json::Array(retries)),
        ("outcomes".to_owned(), Json::Array(outcomes)),
        ("deferrals".to_owned(), Json::Array(deferrals)),
        ("blocked".to_owned(), Json::Array(blocked)),
        ("quiescent".to_owned(), Json::from(quiescent)),
        (
            "escalation".to_owned(),
            attempts.escalation.map_or(Json::Null, Json::from),
        ),
        ("complete".to_owned(), Json::from(remaining.is_empty())),
        (
            "warnings".to_owned(),
            Json::Array(attempts.warnings.into_iter().map(Json::from).collect()),
        ),
        ("closingSummary".to_owned(), Json::Null),
    ]);
    if remaining.is_empty() {
        let reconciliation: CampaignReconciliation =
            serde_json::from_str(&Json::Object(result.clone()).stringify()).map_err(|error| {
                DriverError::new(format!(
                    "cannot project campaign reconciliation through tally-core: {error}"
                ))
            })?;
        let digest = fold_campaign_digest(&reconciliation, "complete");
        let summary = publish_closing_summary(
            &issue_target.repository,
            &issue_target.config,
            &campaign,
            &issue_number,
            &digest,
        )?;
        result.insert("closingSummary".to_owned(), Json::from(summary));
    }
    Ok(Json::Object(result))
}

#[derive(Clone, Debug)]
struct LiveFlowJob {
    anchor: String,
    live_state: String,
    task_ref: Option<String>,
}

impl LiveFlowJob {
    fn to_json(&self) -> Json {
        Json::object([
            ("anchor", Json::from(self.anchor.clone())),
            ("liveState", Json::from(self.live_state.clone())),
            (
                "taskRef",
                self.task_ref.clone().map_or(Json::Null, Json::from),
            ),
        ])
    }
}

#[derive(Clone, Debug)]
struct LiveCampaignJob {
    anchor: String,
    flow_run_id: String,
    live_state: String,
    task_ref: String,
}

impl LiveCampaignJob {
    fn to_json(&self) -> Json {
        Json::object([
            ("anchor", Json::from(self.anchor.clone())),
            ("flowRunId", Json::from(self.flow_run_id.clone())),
            ("liveState", Json::from(self.live_state.clone())),
            ("taskRef", Json::from(self.task_ref.clone())),
        ])
    }
}

fn tally_executable(value: Option<&Json>) -> Result<PathBuf> {
    let executable = PathBuf::from(required_string(value, "tally", None)?);
    let executable_ok = executable.is_absolute()
        && executable.is_file()
        && fs::metadata(&executable)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
    if !executable_ok {
        return Err(DriverError::new(
            "tally must name an absolute executable file",
        ));
    }
    Ok(executable)
}

fn json_command(program: &Path, arguments: &[String], context: &str) -> Result<Json> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| {
            DriverError::new(format!(
                "cannot execute {:?}: {error}",
                program.to_string_lossy()
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim()
        } else if !stdout.trim().is_empty() {
            stdout.trim()
        } else {
            "no output"
        };
        let rendered = std::iter::once(program.to_string_lossy().into_owned())
            .chain(arguments.iter().cloned())
            .collect::<Vec<_>>();
        return Err(DriverError::new(format!(
            "command {rendered:?} exited {}: {detail}",
            output.status.code().unwrap_or(128)
        )));
    }
    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|error| DriverError::new(format!("{context} returned invalid JSON: {error}")))?;
    let value = json::parse(stdout)
        .map_err(|error| DriverError::new(format!("{context} returned invalid JSON: {error}")))?;
    if value.as_object().is_none() {
        return Err(DriverError::new(format!(
            "{context} must return a JSON object"
        )));
    }
    Ok(value)
}

fn uuid_string(value: Option<&Json>, context: &str) -> Result<String> {
    let value = required_string(value, context, None)?;
    Uuid::parse_str(&value).map_err(|_| DriverError::new(format!("{context} must be a UUID")))?;
    Ok(value)
}

fn current_flow_run_id(tally: &Path) -> Result<String> {
    let task_uuid = env::var("TALLY_TASK_UUID").map_err(|_| {
        DriverError::new("TALLY_TASK_UUID must be a non-empty string without control characters")
    })?;
    let task_uuid = required_string(Some(&Json::from(task_uuid)), "TALLY_TASK_UUID", None)?;
    Uuid::parse_str(&task_uuid).map_err(|_| DriverError::new("TALLY_TASK_UUID must be a UUID"))?;
    let response = json_command(
        tally,
        &["query".to_owned(), "job".to_owned(), task_uuid],
        "tally query job for the sweep node",
    )?;
    let job = response
        .as_object()
        .and_then(|response| response.get("job"))
        .and_then(Json::as_object)
        .ok_or_else(|| DriverError::new("tally query job for the sweep node omitted job"))?;
    let orchestration = job
        .get("orchestration")
        .and_then(Json::as_object)
        .ok_or_else(|| {
            DriverError::new("tally query job for the sweep node omitted orchestration")
        })?;
    uuid_string(
        orchestration.get("flowRunId"),
        "sweep node orchestration.flowRunId",
    )
}

fn response_items<'a>(response: &'a Json, context: &str) -> Result<&'a [Json]> {
    response
        .as_object()
        .and_then(|response| response.get("items"))
        .and_then(Json::as_array)
        .ok_or_else(|| DriverError::new(format!("{context} omitted items")))
}

fn query_live_flow_jobs(tally: &Path, flow_run_id: &str) -> Result<Vec<LiveFlowJob>> {
    let mut cursor: Option<String> = None;
    let mut seen_cursors = BTreeSet::new();
    let mut live = Vec::new();
    for _ in 0..128 {
        let mut arguments = vec![
            "query".to_owned(),
            "jobs".to_owned(),
            "--flow-run".to_owned(),
            flow_run_id.to_owned(),
            "--limit".to_owned(),
            "1000".to_owned(),
        ];
        if let Some(cursor) = &cursor {
            arguments.extend(["--cursor".to_owned(), cursor.clone()]);
        }
        let context = format!("tally query jobs for flow {flow_run_id}");
        let response = json_command(tally, &arguments, &context)?;
        for (index, candidate) in response_items(&response, &context)?.iter().enumerate() {
            let candidate = candidate.as_object().ok_or_else(|| {
                DriverError::new(format!("{context} item {index} is not an object"))
            })?;
            let Some(state_value) = candidate.get("liveState") else {
                continue;
            };
            if matches!(state_value, Json::Null) {
                continue;
            }
            let state = state_value.as_str().ok_or_else(|| {
                DriverError::new(format!(
                    "{context} returned unknown live state {state_value:?}"
                ))
            })?;
            if !LIVE_JOB_STATES.contains(&state) {
                return Err(DriverError::new(format!(
                    "{context} returned unknown live state {state:?}"
                )));
            }
            let orchestration = candidate
                .get("orchestration")
                .and_then(Json::as_object)
                .filter(|orchestration| {
                    orchestration.get("flowRunId").and_then(Json::as_str) == Some(flow_run_id)
                })
                .ok_or_else(|| DriverError::new(format!("{context} returned a mismatched job")))?;
            let _ = orchestration;
            let task_ref = match candidate.get("taskRef") {
                Some(Json::Null) | None => None,
                Some(value) => Some(
                    value
                        .as_str()
                        .ok_or_else(|| {
                            DriverError::new(format!("{context} returned an invalid taskRef"))
                        })?
                        .to_owned(),
                ),
            };
            live.push(LiveFlowJob {
                anchor: required_string(
                    candidate.get("anchor"),
                    &format!("{context} item {index}.anchor"),
                    None,
                )?,
                live_state: state.to_owned(),
                task_ref,
            });
        }
        let next = response
            .as_object()
            .expect("JSON command returned an object")
            .get("nextCursor");
        match next {
            Some(Json::Null) | None => return Ok(live),
            Some(value) => {
                let next = required_string(Some(value), "tally query jobs nextCursor", None)?;
                if !seen_cursors.insert(next.clone()) {
                    return Err(DriverError::new(format!(
                        "tally query jobs for flow {flow_run_id} repeated a pagination cursor"
                    )));
                }
                cursor = Some(next);
            }
        }
    }
    Err(DriverError::new(format!(
        "tally query jobs for flow {flow_run_id} exceeded 128 pages"
    )))
}

fn query_live_campaign_jobs(
    tally: &Path,
    campaign_identity: &str,
    current_flow_run_id: &str,
) -> Result<Vec<LiveCampaignJob>> {
    let mut live = Vec::new();
    for state in LIVE_JOB_STATES {
        let mut cursor: Option<String> = None;
        let mut seen_cursors = BTreeSet::new();
        let mut ended = false;
        for _ in 0..128 {
            let mut arguments = vec![
                "query".to_owned(),
                "jobs".to_owned(),
                "--state".to_owned(),
                state.to_owned(),
                "--limit".to_owned(),
                "1000".to_owned(),
            ];
            if let Some(cursor) = &cursor {
                arguments.extend(["--cursor".to_owned(), cursor.clone()]);
            }
            let context = format!("tally query jobs in state {state}");
            let response = json_command(tally, &arguments, &context)?;
            for (index, candidate) in response_items(&response, &context)?.iter().enumerate() {
                let candidate = candidate.as_object().ok_or_else(|| {
                    DriverError::new(format!("{context} item {index} is not an object"))
                })?;
                if candidate.get("liveState").and_then(Json::as_str) != Some(state) {
                    return Err(DriverError::new(format!(
                        "{context} returned a mismatched live state"
                    )));
                }
                let Some(task_ref) = candidate.get("taskRef").and_then(Json::as_str) else {
                    continue;
                };
                if !task_ref.starts_with(&format!("{campaign_identity}/")) {
                    continue;
                }
                let flow_run_id = candidate
                    .get("orchestration")
                    .and_then(Json::as_object)
                    .and_then(|orchestration| orchestration.get("flowRunId"))
                    .and_then(Json::as_str)
                    .ok_or_else(|| {
                        DriverError::new(format!(
                            "live campaign job {:?} omitted orchestration.flowRunId",
                            candidate.get("anchor").and_then(Json::as_str)
                        ))
                    })?;
                if flow_run_id == current_flow_run_id {
                    continue;
                }
                live.push(LiveCampaignJob {
                    anchor: required_string(
                        candidate.get("anchor"),
                        &format!("{context} item {index}.anchor"),
                        None,
                    )?,
                    flow_run_id: flow_run_id.to_owned(),
                    live_state: state.to_owned(),
                    task_ref: task_ref.to_owned(),
                });
            }
            let next = response
                .as_object()
                .expect("JSON command returned an object")
                .get("nextCursor");
            match next {
                Some(Json::Null) | None => {
                    ended = true;
                    break;
                }
                Some(value) => {
                    let next = required_string(Some(value), "tally query jobs nextCursor", None)?;
                    if !seen_cursors.insert(next.clone()) {
                        return Err(DriverError::new(format!(
                            "tally query jobs in state {state} repeated a pagination cursor"
                        )));
                    }
                    cursor = Some(next);
                }
            }
        }
        if !ended {
            return Err(DriverError::new(format!(
                "tally query jobs in state {state} exceeded 128 pages"
            )));
        }
    }
    live.sort_by(|left, right| {
        left.flow_run_id
            .cmp(&right.flow_run_id)
            .then(left.anchor.cmp(&right.anchor))
    });
    Ok(live)
}

fn pass_record_path(workspace_root: &Path, run_hash: &str) -> PathBuf {
    workspace_root
        .join(".state")
        .join("passes")
        .join(format!("{run_hash}.json"))
}

fn is_run_hash(value: &str) -> bool {
    value.len() == 12
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validated_pass_record(
    workspace_root: &Path,
    run_hash: &str,
    campaign_identity: &str,
    repository: &str,
) -> (Option<String>, Option<String>) {
    let path = pass_record_path(workspace_root, run_hash);
    if !path.exists() {
        return (None, Some("no daemon liveness record exists".to_owned()));
    }
    if is_symlink(&path) || !path.is_file() {
        return (
            None,
            Some(format!(
                "daemon liveness record is not a regular file: {}",
                path.display()
            )),
        );
    }
    let saved = match fs::read_to_string(&path)
        .map_err(|error| error.to_string())
        .and_then(|text| json::parse(&text).map_err(|error| error.to_string()))
    {
        Ok(saved) => saved,
        Err(error) => {
            return (
                None,
                Some(format!(
                    "cannot read daemon liveness record {}: {error}",
                    path.display()
                )),
            )
        }
    };
    let Some(saved) = saved.as_object() else {
        return (
            None,
            Some(format!(
                "daemon liveness record is not an object: {}",
                path.display()
            )),
        );
    };
    let expected: BTreeSet<_> = [
        "schemaVersion",
        "campaign",
        "campaignIdentity",
        "repository",
        "runId",
        "runHash",
        "flowRunId",
    ]
    .into_iter()
    .collect();
    if saved.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected {
        return (
            None,
            Some(format!(
                "daemon liveness record has an unexpected shape: {}",
                path.display()
            )),
        );
    }
    let run_id = saved.get("runId").and_then(Json::as_str);
    let flow_run_id = saved.get("flowRunId").and_then(Json::as_str);
    let identity_matches = saved.get("schemaVersion").and_then(Json::as_u64) == Some(2)
        && saved.get("campaign").and_then(Json::as_str).is_some()
        && saved.get("campaignIdentity").and_then(Json::as_str) == Some(campaign_identity)
        && saved.get("repository").and_then(Json::as_str) == Some(repository)
        && run_id.is_some_and(|run_id| {
            !run_id.is_empty()
                && sha256::digest(run_id.as_bytes())
                    .chars()
                    .take(12)
                    .collect::<String>()
                    == run_hash
        })
        && saved.get("runHash").and_then(Json::as_str) == Some(run_hash)
        && flow_run_id.is_some();
    if !identity_matches {
        return (
            None,
            Some(format!(
                "daemon liveness record identity does not match run {run_hash}: {}",
                path.display()
            )),
        );
    }
    let flow_run_id = flow_run_id.expect("validated above");
    if Uuid::parse_str(flow_run_id).is_err() {
        return (
            None,
            Some(format!(
                "daemon liveness record has an invalid flowRunId: {}",
                path.display()
            )),
        );
    }
    (Some(flow_run_id.to_owned()), None)
}

fn write_atomic(path: &Path, value: &Json) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| DriverError::new("atomic output path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_file_name(format!(
        "{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("record"),
        std::process::id()
    ));
    fs::write(&temporary, value.stringify())?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn lane_branch_parts<'a>(branch: &'a str, campaign_slug: &str) -> Option<(&'a str, &'a str)> {
    let remainder = branch.strip_prefix(&format!("tally-work/{campaign_slug}-"))?;
    let (run_hash, lane) = remainder.split_once('/')?;
    if remainder[run_hash.len() + 1..].contains('/')
        || !is_run_hash(run_hash)
        || (lane != "_campaign-preflight" && !is_task_id(lane))
    {
        return None;
    }
    Some((run_hash, lane))
}

fn sorted_directory_entries(path: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort();
    Ok(entries)
}

fn action_sweep(brief: &Json) -> Result<Json> {
    if brief.as_object().is_none() {
        return Err(DriverError::new("sweep brief must be an object"));
    }
    let workspace_root = PathBuf::from(required_string(
        brief
            .as_object()
            .and_then(|brief| brief.get("workspaceRoot")),
        "workspaceRoot",
        None,
    )?);
    if !workspace_root.is_absolute() {
        return Err(DriverError::new("workspaceRoot must be absolute"));
    }
    let state_root = workspace_root.join(".state");
    if state_root.exists() && (is_symlink(&state_root) || !state_root.is_dir()) {
        return Err(DriverError::new(
            "workspaceRoot .state must be a real directory",
        ));
    }
    fs::create_dir_all(&state_root)?;
    let lock_path = state_root.join("sweep.lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(O_CLOEXEC | O_NOFOLLOW)
        .open(&lock_path)
        .map_err(|error| {
            DriverError::new(format!(
                "cannot open campaign sweep lock {}: {error}",
                lock_path.display()
            ))
        })?;
    if unsafe { flock(lock.as_raw_fd(), LOCK_EX) } != 0 {
        return Err(DriverError::new(format!(
            "cannot acquire campaign sweep lock {}: {}",
            lock_path.display(),
            std::io::Error::last_os_error()
        )));
    }
    let result = action_sweep_locked(brief);
    unsafe {
        flock(lock.as_raw_fd(), LOCK_UN);
    }
    result
}

fn action_sweep_locked(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &[
            "campaign",
            "campaignIdentity",
            "repository",
            "repositoryConfig",
            "runId",
            "workspaceRoot",
            "tally",
        ],
        "sweep brief",
    )?;
    let campaign = required_string(data.get("campaign"), "campaign", None)?;
    if !is_component(&campaign) {
        return Err(DriverError::new("campaign is not a safe component"));
    }
    let default_identity = Json::from(campaign.clone());
    let campaign_identity = required_string(
        data.get("campaignIdentity").or(Some(&default_identity)),
        "campaignIdentity",
        Some(80),
    )?;
    let repository = repository_name(data.get("repository"), "repository")?;
    let run_id = required_string(data.get("runId"), "runId", Some(512))?;
    let workspace_root = PathBuf::from(required_string(
        data.get("workspaceRoot"),
        "workspaceRoot",
        None,
    )?);
    if !workspace_root.is_absolute() {
        return Err(DriverError::new("workspaceRoot must be absolute"));
    }
    let tally = tally_executable(data.get("tally"))?;
    let config = repo_config(data.get("repositoryConfig"))?;
    let checkout = &config.checkout;
    let campaign_slug = safe_slug(&campaign, 24);
    let repository_slug = safe_slug(repository.split_once('/').map_or("", |(_, name)| name), 40);
    let repository_root = resolve(&workspace_root.join(repository_slug))?;
    let current_hash: String = sha256::digest(run_id.as_bytes()).chars().take(12).collect();
    let mut cleaned = BTreeSet::new();
    let mut warnings = Vec::new();
    let mut live_runs = Vec::new();

    let state_root = workspace_root.join(".state");
    let passes_root = state_root.join("passes");
    if passes_root.exists() && (is_symlink(&passes_root) || !passes_root.is_dir()) {
        return Err(DriverError::new(
            "workspaceRoot .state/passes must be a real directory",
        ));
    }
    fs::create_dir_all(&passes_root)?;
    let flow_run_id = current_flow_run_id(&tally)?;
    write_atomic(
        &pass_record_path(&workspace_root, &current_hash),
        &Json::object([
            ("schemaVersion", Json::Number("2".to_owned())),
            ("campaign", Json::from(campaign.clone())),
            ("campaignIdentity", Json::from(campaign_identity.clone())),
            ("repository", Json::from(repository.clone())),
            ("runId", Json::from(run_id.clone())),
            ("runHash", Json::from(current_hash.clone())),
            ("flowRunId", Json::from(flow_run_id.clone())),
        ]),
    )?;
    let blocking_jobs = query_live_campaign_jobs(&tally, &campaign_identity, &flow_run_id)?;

    let worktree_records = worktrees::parse_worktrees(checkout)?;
    let listed = git(
        checkout,
        [
            "for-each-ref",
            "--format=%(refname:short)",
            &format!("refs/heads/tally-work/{campaign_slug}-"),
        ],
        true,
    )?
    .stdout_text()
    .lines()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let mut candidate_hashes = BTreeSet::new();
    for record in &worktree_records {
        let Some(raw_path) = record.get("worktree") else {
            continue;
        };
        let resolved = match resolve(Path::new(raw_path)) {
            Ok(resolved) => resolved,
            Err(_) => continue,
        };
        let Ok(relative) = resolved.strip_prefix(&repository_root) else {
            continue;
        };
        if let Some(first) = relative
            .components()
            .next()
            .and_then(|part| part.as_os_str().to_str())
        {
            if is_run_hash(first) {
                candidate_hashes.insert(first.to_owned());
            }
        }
    }
    for branch in &listed {
        if let Some((run_hash, _)) = lane_branch_parts(branch, &campaign_slug) {
            candidate_hashes.insert(run_hash.to_owned());
        }
    }
    for record in &worktree_records {
        let Some(raw_path) = record.get("worktree") else {
            continue;
        };
        let path = Path::new(raw_path);
        let identity = if path.is_dir() {
            worktrees::read_identity(path)?
        } else {
            Identity::new()
        };
        if identity.get("campaign") == Some(&campaign)
            && identity.get("repository") == Some(&repository)
        {
            if let Some(run_id) = identity.get("runid").filter(|run_id| !run_id.is_empty()) {
                candidate_hashes
                    .insert(sha256::digest(run_id.as_bytes()).chars().take(12).collect());
            }
        }
    }
    for record in sorted_directory_entries(&passes_root)? {
        let Some(name) = record.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(run_hash) = name.strip_suffix(".json").filter(|stem| is_run_hash(stem)) {
            candidate_hashes.insert(run_hash.to_owned());
        }
    }
    if repository_root.is_dir() {
        for child in sorted_directory_entries(&repository_root)? {
            if child.is_dir() {
                if let Some(name) = child.file_name().and_then(|name| name.to_str()) {
                    if is_run_hash(name) {
                        candidate_hashes.insert(name.to_owned());
                    }
                }
            }
        }
    }
    candidate_hashes.remove(&current_hash);

    let mut protected_hashes = BTreeSet::new();
    for candidate_hash in &candidate_hashes {
        let (pass_flow_run_id, reason) = validated_pass_record(
            &workspace_root,
            candidate_hash,
            &campaign_identity,
            &repository,
        );
        let Some(pass_flow_run_id) = pass_flow_run_id else {
            protected_hashes.insert(candidate_hash.clone());
            warnings.push(format!(
                "left campaign run {candidate_hash} untouched because {}",
                reason.unwrap_or_else(|| "its daemon liveness record is invalid".to_owned())
            ));
            continue;
        };
        let jobs = query_live_flow_jobs(&tally, &pass_flow_run_id)?;
        if !jobs.is_empty() {
            protected_hashes.insert(candidate_hash.clone());
            live_runs.push(Json::object([
                ("runHash", Json::from(candidate_hash.clone())),
                ("flowRunId", Json::from(pass_flow_run_id)),
                (
                    "jobs",
                    Json::Array(jobs.iter().map(LiveFlowJob::to_json).collect()),
                ),
            ]));
            let summary = jobs
                .iter()
                .map(|job| {
                    format!(
                        "{}:{}{}",
                        job.live_state,
                        job.anchor,
                        job.task_ref
                            .as_ref()
                            .map_or_else(String::new, |task_ref| format!(":{task_ref}"))
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            warnings.push(format!(
                "left live campaign run {candidate_hash} untouched: {summary}"
            ));
        }
    }
    if !blocking_jobs.is_empty() {
        protected_hashes.extend(candidate_hashes.iter().cloned());
        let summary = blocking_jobs
            .iter()
            .map(|job| format!("{}:{}:{}", job.live_state, job.anchor, job.task_ref))
            .collect::<Vec<_>>()
            .join(", ");
        warnings.push(format!(
            "deferred campaign reconciliation because older campaign jobs remain live: {summary}"
        ));
    }

    for record in &worktree_records {
        let Some(raw_path) = record.get("worktree") else {
            continue;
        };
        let worktree = match resolve(Path::new(raw_path)) {
            Ok(worktree) => worktree,
            Err(_) => continue,
        };
        let Ok(relative) = worktree.strip_prefix(&repository_root) else {
            continue;
        };
        let parts: Vec<_> = relative
            .components()
            .filter_map(|part| part.as_os_str().to_str())
            .collect();
        if parts.len() != 2
            || !is_run_hash(parts[0])
            || (parts[1] != "_campaign-preflight" && !is_task_id(parts[1]))
        {
            warnings.push(format!(
                "left unexpected campaign worktree path untouched: {}",
                worktree.display()
            ));
            continue;
        }
        let lane_hash = parts[0];
        if lane_hash == current_hash || protected_hashes.contains(lane_hash) {
            continue;
        }
        let branch = record
            .get("branch")
            .map(|branch| {
                branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch.as_str())
            })
            .unwrap_or("");
        if !branch.is_empty() {
            let expected = lane_branch_parts(branch, &campaign_slug);
            if expected != Some((lane_hash, parts[1])) {
                warnings.push(format!(
                    "left campaign worktree with unexpected branch untouched: {}",
                    worktree.display()
                ));
                continue;
            }
        }
        let removed = git(
            checkout,
            [
                "worktree",
                "remove",
                "--force",
                worktree.to_string_lossy().as_ref(),
            ],
            false,
        )?;
        if !removed.success() && worktree.exists() {
            warnings.push(format!(
                "could not sweep worktree {}: {}",
                worktree.display(),
                removed.detail()
            ));
            continue;
        }
        if !branch.is_empty() {
            git(checkout, ["branch", "-D", branch], false)?;
        }
        cleaned.insert(format!("worktree:{}", worktree.display()));
    }

    worktrees::prune(checkout)?;
    for branch in &listed {
        let Some((run_hash, _)) = lane_branch_parts(branch, &campaign_slug) else {
            continue;
        };
        if run_hash == current_hash || protected_hashes.contains(run_hash) {
            continue;
        }
        let deleted = git(checkout, ["branch", "-D", branch], false)?;
        if deleted.success() {
            cleaned.insert(format!("branch:{branch}"));
        } else {
            warnings.push(format!(
                "could not sweep branch {branch:?}: {}",
                deleted.detail()
            ));
        }
    }

    let registered_paths: BTreeSet<_> = worktrees::parse_worktrees(checkout)?
        .into_iter()
        .filter_map(|record| record.get("worktree").cloned())
        .filter_map(|path| resolve(Path::new(&path)).ok())
        .collect();
    if repository_root.is_dir() {
        for run_directory in sorted_directory_entries(&repository_root)? {
            if !run_directory.is_dir() || is_symlink(&run_directory) {
                continue;
            }
            let Some(lane_hash) = run_directory.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !is_run_hash(lane_hash)
                || lane_hash == current_hash
                || protected_hashes.contains(lane_hash)
            {
                continue;
            }
            for lane_directory in sorted_directory_entries(&run_directory)? {
                let resolved = match resolve(&lane_directory) {
                    Ok(resolved) => resolved,
                    Err(_) => continue,
                };
                if registered_paths.contains(&resolved) {
                    continue;
                }
                let lane_name = lane_directory
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default();
                if lane_name != "_campaign-preflight" && !is_task_id(lane_name) {
                    warnings.push(format!(
                        "left unexpected campaign workspace entry untouched: {}",
                        lane_directory.display()
                    ));
                    continue;
                }
                if is_symlink(&lane_directory) || !lane_directory.is_dir() {
                    warnings.push(format!(
                        "left non-directory campaign worktree path untouched: {}",
                        lane_directory.display()
                    ));
                    continue;
                }
                if let Err(error) = fs::remove_dir_all(&lane_directory) {
                    warnings.push(format!(
                        "could not sweep unregistered campaign worktree {}: {error}",
                        lane_directory.display()
                    ));
                    continue;
                }
                cleaned.insert(format!("worktree:{}", lane_directory.display()));
                git(
                    checkout,
                    [
                        "branch",
                        "-D",
                        &format!("tally-work/{campaign_slug}-{lane_hash}/{lane_name}"),
                    ],
                    false,
                )?;
            }
        }
    }

    if repository_root.is_dir() {
        for child in sorted_directory_entries(&repository_root)? {
            if child.is_dir() {
                prune_empty_ancestors(&child, &repository_root);
            }
        }
    }

    let mut remaining_hashes = BTreeSet::new();
    for record in worktrees::parse_worktrees(checkout)? {
        let Some(raw_path) = record.get("worktree") else {
            continue;
        };
        let Ok(worktree) = resolve(Path::new(raw_path)) else {
            continue;
        };
        let Ok(relative) = worktree.strip_prefix(&repository_root) else {
            continue;
        };
        if let Some(first) = relative
            .components()
            .next()
            .and_then(|part| part.as_os_str().to_str())
        {
            if is_run_hash(first) {
                remaining_hashes.insert(first.to_owned());
            }
        }
    }
    let remaining_branches = git(
        checkout,
        [
            "for-each-ref",
            "--format=%(refname:short)",
            &format!("refs/heads/tally-work/{campaign_slug}-"),
        ],
        true,
    )?
    .stdout_text();
    for branch in remaining_branches.lines() {
        if let Some((run_hash, _)) = lane_branch_parts(branch, &campaign_slug) {
            remaining_hashes.insert(run_hash.to_owned());
        }
    }
    if repository_root.is_dir() {
        for child in sorted_directory_entries(&repository_root)? {
            if child.is_dir() {
                if let Some(name) = child.file_name().and_then(|name| name.to_str()) {
                    if is_run_hash(name) {
                        remaining_hashes.insert(name.to_owned());
                    }
                }
            }
        }
    }
    for candidate_hash in candidate_hashes
        .difference(&protected_hashes)
        .filter(|candidate_hash| !remaining_hashes.contains(*candidate_hash))
    {
        let record = pass_record_path(&workspace_root, candidate_hash);
        if record.is_file() && !is_symlink(&record) {
            match fs::remove_file(&record) {
                Ok(()) => {
                    if let Some(parent) = record.parent() {
                        prune_empty_ancestors(parent, &state_root);
                    }
                    cleaned.insert(format!("pass:{}", record.display()));
                }
                Err(error) => warnings.push(format!(
                    "could not remove swept pass record {}: {error}",
                    record.display()
                )),
            }
        }
    }
    let mut seen_warnings = BTreeSet::new();
    warnings.retain(|warning| seen_warnings.insert(warning.clone()));
    Ok(Json::object([
        ("currentRunHash", Json::from(current_hash)),
        (
            "blockingJobs",
            Json::Array(blocking_jobs.iter().map(LiveCampaignJob::to_json).collect()),
        ),
        (
            "cleaned",
            Json::Array(cleaned.into_iter().map(Json::from).collect()),
        ),
        ("liveRuns", Json::Array(live_runs)),
        (
            "warnings",
            Json::Array(warnings.into_iter().map(Json::from).collect()),
        ),
    ]))
}

fn lane_identity(
    campaign: &str,
    repository: &str,
    run_id: &str,
    task_id: &str,
    task_kind: &str,
    branch: &str,
    publish_branch: &str,
) -> Identity {
    BTreeMap::from([
        ("branch".to_owned(), branch.to_owned()),
        ("campaign".to_owned(), campaign.to_owned()),
        ("driver".to_owned(), "spec-build".to_owned()),
        ("publishbranch".to_owned(), publish_branch.to_owned()),
        ("repository".to_owned(), repository.to_owned()),
        ("runid".to_owned(), run_id.to_owned()),
        ("taskid".to_owned(), task_id.to_owned()),
        ("taskkind".to_owned(), task_kind.to_owned()),
    ])
}

fn snapshot_before_agent(worktree: &Path) -> Result<()> {
    if worktrees::snapshot_exists(worktree)? {
        return Ok(());
    }
    let fingerprint = worktrees::change_set_fingerprint(worktree)?;
    worktrees::write_snapshot(worktree, &fingerprint)
}

fn prepared_result(
    task_id: &str,
    base_rev: &str,
    branch: &str,
    publish_branch: &str,
    worktree: &Path,
    conflict_domains: Option<&[String]>,
) -> Json {
    let mut result = BTreeMap::from([
        ("baseRev".to_owned(), Json::from(base_rev)),
        ("branch".to_owned(), Json::from(branch)),
        ("publishBranch".to_owned(), Json::from(publish_branch)),
        ("taskId".to_owned(), Json::from(task_id)),
        (
            "worktreePath".to_owned(),
            Json::from(worktree.to_string_lossy().into_owned()),
        ),
    ]);
    if let Some(domains) = conflict_domains {
        result.insert(
            "conflictDomains".to_owned(),
            Json::Array(domains.iter().cloned().map(Json::from).collect()),
        );
    }
    Json::Object(result)
}

fn action_prep(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &[
            "campaign",
            "campaignIdentity",
            "repository",
            "repositoryConfig",
            "issue",
            "runId",
            "workspaceRoot",
            "task",
            "sourceRevision",
        ],
        "prep brief",
    )?;
    let task_value =
        member(data, "task").ok_or_else(|| DriverError::new("task must be an object"))?;
    let task = task_value
        .as_object()
        .ok_or_else(|| DriverError::new("task must be an object"))?;
    let task_id = required_string(task.get("id"), "task.id", None)?;
    if !is_task_id(&task_id) {
        return Err(DriverError::new("task.id is not safe"));
    }
    let task_kind = task.get("kind").and_then(Json::as_str).unwrap_or_default();
    if !matches!(task_kind, "implementation" | "checkpoint") {
        return Err(DriverError::new(
            "task.kind must equal implementation or checkpoint",
        ));
    }
    let campaign = required_string(member(data, "campaign"), "campaign", None)?;
    let repository = required_string(member(data, "repository"), "repository", None)?;
    campaign_issue(member(data, "issue"))?;
    let run_id = required_string(member(data, "runId"), "runId", Some(512))?;
    let workspace_root = PathBuf::from(required_string(
        member(data, "workspaceRoot"),
        "workspaceRoot",
        None,
    )?);
    if !workspace_root.is_absolute() {
        return Err(DriverError::new("workspaceRoot must be absolute"));
    }
    let config = repo_config(member(data, "repositoryConfig"))?;
    let source_revision = full_oid(member(data, "sourceRevision"), "sourceRevision")?;
    let conflict_domains = if task_kind == "implementation" {
        normalize_paths(
            task.get("conflictDomains"),
            "prep brief task.conflictDomains",
            false,
        )?
    } else {
        None
    };
    let identity = campaign_identity(data, &campaign)?;
    let run_hash = &sha256::digest(run_id.as_bytes())[..12];
    let campaign_slug = safe_slug(&campaign, 24);
    let repository_name = repository
        .split_once('/')
        .map_or(repository.as_str(), |(_, name)| name);
    let repository_slug = safe_slug(repository_name, 40);
    let branch = format!("tally-work/{campaign_slug}-{run_hash}/{task_id}");
    let revision = task_revision(task)?;
    let publish_branch = stable_publish_branch(&campaign, &identity, &task_id, revision.as_deref());
    let worktree = resolve(
        &workspace_root
            .join(repository_slug)
            .join(run_hash)
            .join(&task_id),
    )?;
    let expected = lane_identity(
        &campaign,
        &repository,
        &run_id,
        &task_id,
        task_kind,
        &branch,
        &publish_branch,
    );

    git(&config.checkout, ["fetch", "--prune", &config.remote], true)?;
    let base_ref = format!("{}/{}", config.remote, config.base_branch);
    let remote_tip = git(
        &config.checkout,
        ["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
        true,
    )?
    .stdout_trimmed();
    let base_tip =
        ensure_integration_branch(&config, &campaign, &identity, &remote_tip, &source_revision)?;

    if let Some(resumed) = worktrees::resume(&config.checkout, &worktree, &expected, &["baserev"])?
    {
        if resumed.complete {
            let resumed_base = resumed
                .identity
                .get("baserev")
                .filter(|value| {
                    !value.is_empty() && !value.chars().any(|character| (character as u32) < 32)
                })
                .cloned()
                .ok_or_else(|| {
                    DriverError::new(
                        "lane baseRev must be a non-empty string without control characters",
                    )
                })?;
            if !git(
                &config.checkout,
                [
                    "merge-base",
                    "--is-ancestor",
                    &source_revision,
                    &resumed_base,
                ],
                false,
            )?
            .success()
            {
                return Err(DriverError::new(
                    "resumed lane base does not descend from the witnessed worklist revision",
                ));
            }
            if task_kind == "implementation" {
                snapshot_before_agent(&worktree)?;
            }
            return Ok(prepared_result(
                &task_id,
                &resumed_base,
                &branch,
                &publish_branch,
                &worktree,
                conflict_domains.as_deref(),
            ));
        }
        return finish_preparation(
            &config,
            &campaign,
            &repository,
            &run_id,
            &task_id,
            task_kind,
            &source_revision,
            &base_tip,
            &branch,
            &publish_branch,
            &worktree,
            &expected,
            conflict_domains.as_deref(),
            Some(resumed.head),
        );
    }

    finish_preparation(
        &config,
        &campaign,
        &repository,
        &run_id,
        &task_id,
        task_kind,
        &source_revision,
        &base_tip,
        &branch,
        &publish_branch,
        &worktree,
        &expected,
        conflict_domains.as_deref(),
        None,
    )
}

fn action_preflight(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &[
            "campaign",
            "repository",
            "repositoryConfig",
            "issue",
            "runId",
            "workspaceRoot",
        ],
        "preflight brief",
    )?;
    let campaign = required_string(data.get("campaign"), "campaign", None)?;
    if !is_component(&campaign) {
        return Err(DriverError::new("campaign is not a safe component"));
    }
    let repository = repository_name(data.get("repository"), "repository")?;
    campaign_issue(data.get("issue"))?;
    let run_id = required_string(data.get("runId"), "runId", Some(512))?;
    let workspace_root = PathBuf::from(required_string(
        data.get("workspaceRoot"),
        "workspaceRoot",
        None,
    )?);
    if !workspace_root.is_absolute() {
        return Err(DriverError::new("workspaceRoot must be absolute"));
    }
    let config = repo_config(data.get("repositoryConfig"))?;
    let run_hash = &sha256::digest(run_id.as_bytes())[..12];
    let campaign_slug = safe_slug(&campaign, 24);
    let repository_slug = safe_slug(
        repository
            .split_once('/')
            .map_or(repository.as_str(), |(_, name)| name),
        40,
    );
    let task_id = "campaign-preflight";
    let branch = format!("tally-work/{campaign_slug}-{run_hash}/_campaign-preflight");
    let worktree = resolve(
        &workspace_root
            .join(repository_slug)
            .join(run_hash)
            .join("_campaign-preflight"),
    )?;
    let expected = lane_identity(
        &campaign,
        &repository,
        &run_id,
        task_id,
        "preflight",
        &branch,
        &branch,
    );

    let resumed = worktrees::resume(&config.checkout, &worktree, &expected, &["baserev"])?;
    if let Some(resumed) = &resumed {
        if resumed.complete {
            let base_rev = resumed
                .identity
                .get("baserev")
                .cloned()
                .ok_or_else(|| DriverError::new("preflight lane baseRev is required"))?;
            return Ok(prepared_result(
                task_id, &base_rev, &branch, &branch, &worktree, None,
            ));
        }
    }

    git(&config.checkout, ["fetch", "--prune", &config.remote], true)?;
    let base_ref = format!("{}/{}", config.remote, config.base_branch);
    let base_tip = git(
        &config.checkout,
        ["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
        true,
    )?
    .stdout_trimmed();
    let lane_head = if let Some(resumed) = resumed {
        resumed.head
    } else {
        let start_rev = if worktrees::branch_exists(&config.checkout, &branch)? {
            format!("refs/heads/{branch}")
        } else {
            base_tip.clone()
        };
        worktrees::add(&config.checkout, &worktree, &branch, &start_rev)?
    };
    let base_rev = git(
        &config.checkout,
        ["merge-base", &lane_head, &base_tip],
        true,
    )?
    .stdout_trimmed();
    if !is_full_oid(&base_rev) {
        return Err(DriverError::new(format!(
            "cannot derive a base revision for campaign lane {branch:?}"
        )));
    }
    let mut recorded = expected;
    recorded.insert("baserev".to_owned(), base_rev.clone());
    worktrees::write_identity(&worktree, &recorded)?;
    Ok(prepared_result(
        task_id, &base_rev, &branch, &branch, &worktree, None,
    ))
}

#[allow(clippy::too_many_arguments)]
fn finish_preparation(
    config: &RepoConfig,
    _campaign: &str,
    _repository: &str,
    _run_id: &str,
    task_id: &str,
    task_kind: &str,
    _source_revision: &str,
    base_tip: &str,
    branch: &str,
    publish_branch: &str,
    worktree: &Path,
    expected: &Identity,
    conflict_domains: Option<&[String]>,
    resumed_head: Option<String>,
) -> Result<Json> {
    let lane_head = if let Some(head) = resumed_head {
        head
    } else {
        let publish_ref = format!("refs/heads/{publish_branch}");
        let published = task_kind == "implementation"
            && git(
                &config.checkout,
                ["show-ref", "--verify", "--quiet", &publish_ref],
                false,
            )?
            .success();
        let start_rev = if worktrees::branch_exists(&config.checkout, branch)? {
            format!("refs/heads/{branch}")
        } else if published {
            publish_ref
        } else {
            base_tip.to_owned()
        };
        worktrees::add(&config.checkout, worktree, branch, &start_rev)?
    };
    let base_rev =
        git(&config.checkout, ["merge-base", &lane_head, base_tip], true)?.stdout_trimmed();
    if !is_full_oid(&base_rev) {
        return Err(DriverError::new(format!(
            "cannot derive a base revision for campaign lane {branch:?}"
        )));
    }
    let mut recorded = expected.clone();
    recorded.insert("baserev".to_owned(), base_rev.clone());
    worktrees::write_identity(worktree, &recorded)?;
    if task_kind == "implementation" {
        snapshot_before_agent(worktree)?;
    }
    Ok(prepared_result(
        task_id,
        &base_rev,
        branch,
        publish_branch,
        worktree,
        conflict_domains,
    ))
}

fn prepared_workspace(value: Option<&Json>, context: &str) -> Result<Workspace> {
    let value = value.ok_or_else(|| DriverError::new(format!("{context} must be an object")))?;
    let object = object_exact(
        value,
        &[
            "taskId",
            "baseRev",
            "branch",
            "publishBranch",
            "worktreePath",
            "conflictDomains",
        ],
        context,
    )?;
    normalize_paths(
        object.get("conflictDomains"),
        &format!("{context}.conflictDomains"),
        false,
    )?;
    Ok(Workspace {
        task_id: required_string(object.get("taskId"), &format!("{context}.taskId"), None)?,
        base_rev: required_string(object.get("baseRev"), &format!("{context}.baseRev"), None)?,
        branch: required_string(object.get("branch"), &format!("{context}.branch"), None)?,
        publish_branch: required_string(
            object.get("publishBranch"),
            &format!("{context}.publishBranch"),
            None,
        )?,
        worktree: PathBuf::from(required_string(
            object.get("worktreePath"),
            &format!("{context}.worktreePath"),
            None,
        )?),
    })
}

fn normalize_owned_paths(value: Option<&Json>, context: &str) -> Result<Vec<String>> {
    normalize_paths(value, context, false)?
        .ok_or_else(|| DriverError::new(format!("{context} must be an array")))
}

fn normalize_ownership(value: Option<&Json>, context: &str) -> Result<Ownership> {
    let value = value.ok_or_else(|| DriverError::new(format!("{context} must be an object")))?;
    let object = object_exact(
        value,
        &[
            "taskId",
            "domainsRequired",
            "conflictDomains",
            "ownedPaths",
            "baseRev",
            "head",
        ],
        context,
    )?;
    let task_id = required_string(object.get("taskId"), &format!("{context}.taskId"), None)?;
    if !is_task_id(&task_id) {
        return Err(DriverError::new(format!("{context}.taskId is not safe")));
    }
    let domains_required = required_bool(
        object.get("domainsRequired"),
        &format!("{context}.domainsRequired"),
    )?;
    let conflict_domains = normalize_paths(
        object.get("conflictDomains"),
        &format!("{context}.conflictDomains"),
        domains_required,
    )?;
    let owned_paths =
        normalize_owned_paths(object.get("ownedPaths"), &format!("{context}.ownedPaths"))?;
    let mut sorted = owned_paths.clone();
    sorted.sort();
    if owned_paths != sorted {
        return Err(DriverError::new(format!(
            "{context}.ownedPaths must be sorted"
        )));
    }
    Ok(Ownership {
        task_id,
        domains_required,
        conflict_domains,
        owned_paths,
        base_rev: full_oid(object.get("baseRev"), &format!("{context}.baseRev"))?,
        head: full_oid(object.get("head"), &format!("{context}.head"))?,
    })
}

fn narration_record(value: Option<&Json>, context: &str) -> Result<Json> {
    let value = value.ok_or_else(|| DriverError::new(format!("{context} must be an object")))?;
    let object = object_exact(value, &["source", "subject", "body"], context)?;
    let source = required_string(object.get("source"), &format!("{context}.source"), None)?;
    if !matches!(source.as_str(), "steward" | "template") {
        return Err(DriverError::new(format!(
            "{context}.source must be steward or template"
        )));
    }
    let subject = required_string(
        object.get("subject"),
        &format!("{context}.subject"),
        Some(COMMIT_SUBJECT_MAX),
    )?;
    let body = match object.get("body") {
        None => String::new(),
        Some(Json::String(body))
            if body.chars().count() <= COMMIT_BODY_MAX && !body.contains('\0') =>
        {
            body.clone()
        }
        _ => {
            return Err(DriverError::new(format!(
                "{context}.body must be a string of at most {COMMIT_BODY_MAX} characters"
            )))
        }
    };
    Ok(Json::object([
        ("source", Json::from(source)),
        ("subject", Json::from(subject)),
        ("body", Json::from(body)),
    ]))
}

fn publication(value: Option<&Json>) -> Result<Publication> {
    let value = value.ok_or_else(|| DriverError::new("publication must be an object"))?;
    let object = object_exact(
        value,
        &[
            "taskId",
            "branch",
            "head",
            "pullRequest",
            "narration",
            "narrationAttempts",
            "ownership",
        ],
        "publication",
    )?;
    let task_id = required_string(object.get("taskId"), "publication.taskId", None)?;
    if !is_task_id(&task_id) {
        return Err(DriverError::new("publication.taskId is not safe"));
    }
    let head = full_oid(object.get("head"), "publication.head")?;
    let ownership = normalize_ownership(object.get("ownership"), "publication.ownership")?;
    if ownership.task_id != task_id {
        return Err(DriverError::new(
            "publication.ownership.taskId does not match publication.taskId",
        ));
    }
    if ownership.head != head {
        return Err(DriverError::new(
            "publication.ownership.head does not match publication.head",
        ));
    }
    Ok(Publication {
        task_id,
        branch: required_string(object.get("branch"), "publication.branch", None)?,
        head,
        pull_request: required_string(object.get("pullRequest"), "publication.pullRequest", None)?,
        narration: narration_record(object.get("narration"), "publication.narration")?,
        ownership,
    })
}

fn normalize_constraints(value: Option<&Json>, context: &str) -> Result<Vec<Constraint>> {
    let values = value
        .and_then(Json::as_array)
        .ok_or_else(|| DriverError::new(format!("{context} must be an array")))?;
    let mut constraints = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let item_context = format!("{context}[{index}]");
        let object = object_exact(
            value,
            &[
                "gateId",
                "kind",
                "patterns",
                "checkedPaths",
                "baseRev",
                "head",
            ],
            &item_context,
        )?;
        if object.get("kind").and_then(Json::as_str) != Some("forbidPaths") {
            return Err(DriverError::new(format!(
                "{item_context}.kind must equal forbidPaths"
            )));
        }
        let gate_id = required_string(
            object.get("gateId"),
            &format!("{item_context}.id"),
            Some(80),
        )?;
        if !is_component(&gate_id) {
            return Err(DriverError::new(format!(
                "{item_context}.id is not a safe component"
            )));
        }
        if !seen.insert(gate_id.clone()) {
            return Err(DriverError::new(format!(
                "{context} repeats gateId {gate_id:?}"
            )));
        }
        let patterns = string_list(
            object.get("patterns"),
            &format!("{item_context}.forbidPaths"),
            true,
        )?;
        if patterns.len() > 128 {
            return Err(DriverError::new(format!(
                "{item_context}.forbidPaths exceeds 128 entries"
            )));
        }
        let mut pattern_set = BTreeSet::new();
        for (pattern_index, pattern) in patterns.iter().enumerate() {
            let components: Vec<_> = pattern.split('/').collect();
            let canonical = pattern.chars().count() <= 1024
                && !pattern.starts_with('/')
                && !pattern.ends_with('/')
                && !components.contains(&"..")
                && components
                    .iter()
                    .all(|component| !component.contains("**") || *component == "**")
                && pattern_set.insert(pattern.clone());
            if !canonical {
                return Err(DriverError::new(format!(
                    "internal campaign contract violation: {item_context}.forbidPaths[{pattern_index}] is not canonical"
                )));
            }
        }
        let checked_paths = object
            .get("checkedPaths")
            .and_then(Json::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| {
                DriverError::new(format!(
                    "{item_context}.checkedPaths must be a non-negative integer"
                ))
            })?;
        let head = full_oid(object.get("head"), &format!("{item_context}.head"))?;
        constraints.push(Constraint {
            gate_id,
            patterns,
            checked_paths,
            base_rev: full_oid(object.get("baseRev"), &format!("{item_context}.baseRev"))?,
            head,
        });
    }
    Ok(constraints)
}

fn domains_overlap(left: &str, right: &str) -> bool {
    let left: Vec<_> = left.split('/').map(str::to_lowercase).collect();
    let right: Vec<_> = right.split('/').map(str::to_lowercase).collect();
    let width = left.len().min(right.len());
    left[..width] == right[..width]
}

fn reject_merge_commits(worktree: &Path, union_base: &str, head: &str) -> Result<()> {
    let range = format!("{union_base}..{head}");
    let listed = git(
        worktree,
        ["rev-list", "--merges", "--end-of-options", &range, "--"],
        true,
    )?
    .stdout_text();
    let commits: Vec<_> = listed.split_whitespace().collect();
    if commits.is_empty() {
        return Ok(());
    }
    let mut preview = commits
        .iter()
        .take(5)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    if commits.len() > 5 {
        preview.push_str(&format!(", and {} more", commits.len() - 5));
    }
    Err(DriverError::new(format!(
        "task lane history contains {} merge commit(s) ({preview}); rebase instead of merging the base into your lane",
        commits.len()
    )))
}

fn lane_union_base(
    worktree: &Path,
    base_rev: &str,
    head: &str,
    current_base: Option<&str>,
) -> Result<String> {
    let Some(current_base) = current_base else {
        return Ok(base_rev.to_owned());
    };
    if current_base == base_rev {
        return Ok(base_rev.to_owned());
    }
    let resolved = git(
        worktree,
        ["merge-base", "--end-of-options", head, current_base],
        false,
    )?;
    let fork = resolved.stdout_trimmed();
    if !resolved.success() || !is_full_oid(&fork) || fork == base_rev {
        return Ok(base_rev.to_owned());
    }
    if !git(
        worktree,
        ["merge-base", "--is-ancestor", base_rev, &fork],
        false,
    )?
    .success()
    {
        return Ok(base_rev.to_owned());
    }
    Ok(fork)
}

fn changed_paths_in_history(
    worktree: &Path,
    union_base: &str,
    head: &str,
    include_deletions: bool,
) -> Result<Vec<String>> {
    if !is_full_oid(union_base) {
        return Err(DriverError::new(
            "base revision must be a full Git object ID",
        ));
    }
    if !is_full_oid(head) {
        return Err(DriverError::new(
            "head revision must be a full Git object ID",
        ));
    }
    reject_merge_commits(worktree, union_base, head)?;
    let range = format!("{union_base}..{head}");
    let filter = if include_deletions {
        "--diff-filter=ACDMTUXB"
    } else {
        "--diff-filter=ACMTUXB"
    };
    let changed = git(
        worktree,
        [
            "log",
            "-m",
            "--format=",
            "--name-only",
            "--no-renames",
            filter,
            "-z",
            "--end-of-options",
            &range,
            "--",
        ],
        true,
    )?;
    Ok(changed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn enforce_conflict_domains(
    worktree: &Path,
    base_rev: &str,
    head: &str,
    task: Option<&Json>,
    expected_task_id: &str,
    domains_required: bool,
    current_base: Option<&str>,
) -> Result<Ownership> {
    let task = task
        .and_then(Json::as_object)
        .ok_or_else(|| DriverError::new("task must be an object"))?;
    let task_id = required_string(task.get("id"), "task.id", None)?;
    if !is_task_id(&task_id) {
        return Err(DriverError::new("task.id is not safe"));
    }
    if task_id != expected_task_id {
        return Err(DriverError::new("task.id does not match workspace.taskId"));
    }
    let domains = normalize_paths(
        task.get("conflictDomains"),
        "task.conflictDomains",
        domains_required,
    )?;
    if !is_full_oid(base_rev) {
        return Err(DriverError::new(
            "base revision must be a full Git object ID",
        ));
    }
    if !is_full_oid(head) {
        return Err(DriverError::new(
            "head revision must be a full Git object ID",
        ));
    }
    let union_base = lane_union_base(worktree, base_rev, head, current_base)?;
    let changed_paths = changed_paths_in_history(worktree, &union_base, head, true)?;
    let outside: Vec<_> = domains.as_ref().map_or_else(Vec::new, |domains| {
        changed_paths
            .iter()
            .filter(|path| !domains.iter().any(|domain| domains_overlap(path, domain)))
            .cloned()
            .collect()
    });
    if !outside.is_empty() {
        let mut preview = outside
            .iter()
            .take(20)
            .map(|path| Json::from(path.clone()).stringify())
            .collect::<Vec<_>>()
            .join(", ");
        if outside.len() > 20 {
            preview.push_str(&format!(", and {} more", outside.len() - 20));
        }
        let declared = domains
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|domain| Json::from(domain.clone()).stringify())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(DriverError::new(format!(
            "task {task_id:?} changed {} path(s) outside its declared conflictDomains: {preview}; declared domains: {declared}",
            outside.len()
        )));
    }
    Ok(Ownership {
        task_id,
        domains_required,
        conflict_domains: domains,
        owned_paths: changed_paths,
        base_rev: base_rev.to_owned(),
        head: head.to_owned(),
    })
}

impl Ownership {
    fn to_json(&self) -> Json {
        let mut object = BTreeMap::from([
            ("baseRev".to_owned(), Json::from(self.base_rev.clone())),
            (
                "domainsRequired".to_owned(),
                Json::from(self.domains_required),
            ),
            ("head".to_owned(), Json::from(self.head.clone())),
            (
                "ownedPaths".to_owned(),
                Json::Array(self.owned_paths.iter().cloned().map(Json::from).collect()),
            ),
            ("taskId".to_owned(), Json::from(self.task_id.clone())),
        ]);
        if let Some(domains) = &self.conflict_domains {
            object.insert(
                "conflictDomains".to_owned(),
                Json::Array(domains.iter().cloned().map(Json::from).collect()),
            );
        }
        Json::Object(object)
    }
}

fn component_glob_matches(text: &str, pattern: &str) -> bool {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let mut memo = BTreeMap::new();

    fn matches(
        text: &[char],
        pattern: &[char],
        text_index: usize,
        pattern_index: usize,
        memo: &mut BTreeMap<(usize, usize), bool>,
    ) -> bool {
        if let Some(value) = memo.get(&(text_index, pattern_index)) {
            return *value;
        }
        let result = if pattern_index == pattern.len() {
            text_index == text.len()
        } else {
            match pattern[pattern_index] {
                '*' => {
                    matches(text, pattern, text_index, pattern_index + 1, memo)
                        || (text_index < text.len()
                            && matches(text, pattern, text_index + 1, pattern_index, memo))
                }
                '?' => {
                    text_index < text.len()
                        && matches(text, pattern, text_index + 1, pattern_index + 1, memo)
                }
                '[' => {
                    let mut closing = pattern_index + 1;
                    if closing < pattern.len() && matches!(pattern[closing], '!' | '^') {
                        closing += 1;
                    }
                    if closing < pattern.len() && pattern[closing] == ']' {
                        closing += 1;
                    }
                    while closing < pattern.len() && pattern[closing] != ']' {
                        closing += 1;
                    }
                    if closing == pattern.len() {
                        text_index < text.len()
                            && text[text_index] == '['
                            && matches(text, pattern, text_index + 1, pattern_index + 1, memo)
                    } else if text_index >= text.len() {
                        false
                    } else {
                        let mut cursor = pattern_index + 1;
                        let negated = cursor < closing && matches!(pattern[cursor], '!' | '^');
                        if negated {
                            cursor += 1;
                        }
                        let mut included = false;
                        while cursor < closing {
                            if cursor + 2 < closing && pattern[cursor + 1] == '-' {
                                included |= pattern[cursor] <= text[text_index]
                                    && text[text_index] <= pattern[cursor + 2];
                                cursor += 3;
                            } else {
                                included |= pattern[cursor] == text[text_index];
                                cursor += 1;
                            }
                        }
                        (included != negated)
                            && matches(text, pattern, text_index + 1, closing + 1, memo)
                    }
                }
                literal => {
                    text_index < text.len()
                        && text[text_index] == literal
                        && matches(text, pattern, text_index + 1, pattern_index + 1, memo)
                }
            }
        };
        memo.insert((text_index, pattern_index), result);
        result
    }

    matches(&text, &pattern, 0, 0, &mut memo)
}

fn path_glob_matches(path: &str, pattern: &str) -> bool {
    let path: Vec<_> = path.split('/').collect();
    let pattern: Vec<_> = pattern.split('/').collect();
    if pattern.len() == 1 {
        return path
            .last()
            .is_some_and(|name| component_glob_matches(name, pattern[0]));
    }
    let mut memo = BTreeMap::new();
    fn matches(
        path: &[&str],
        pattern: &[&str],
        path_index: usize,
        pattern_index: usize,
        memo: &mut BTreeMap<(usize, usize), bool>,
    ) -> bool {
        if let Some(value) = memo.get(&(path_index, pattern_index)) {
            return *value;
        }
        let result = if pattern_index == pattern.len() {
            path_index == path.len()
        } else if pattern[pattern_index] == "**" {
            matches(path, pattern, path_index, pattern_index + 1, memo)
                || (path_index < path.len()
                    && matches(path, pattern, path_index + 1, pattern_index, memo))
        } else {
            path_index < path.len()
                && component_glob_matches(path[path_index], pattern[pattern_index])
                && matches(path, pattern, path_index + 1, pattern_index + 1, memo)
        };
        memo.insert((path_index, pattern_index), result);
        result
    }
    matches(&path, &pattern, 0, 0, &mut memo)
}

fn evaluate_forbid_paths(
    worktree: &Path,
    union_base: &str,
    head: &str,
    gate_id: &str,
    patterns: &[String],
) -> Result<usize> {
    let changed = changed_paths_in_history(worktree, union_base, head, false)?;
    let violations: Vec<_> = changed
        .iter()
        .filter_map(|path| {
            let matched: Vec<_> = patterns
                .iter()
                .filter(|pattern| path_glob_matches(path, pattern))
                .cloned()
                .collect();
            (!matched.is_empty()).then(|| (path.clone(), matched))
        })
        .collect();
    if !violations.is_empty() {
        let mut preview = violations
            .iter()
            .take(20)
            .map(|(path, patterns)| {
                format!(
                    "{} (matched {})",
                    Json::from(path.clone()).stringify(),
                    patterns
                        .iter()
                        .map(|pattern| Json::from(pattern.clone()).stringify())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        if violations.len() > 20 {
            preview.push_str(&format!("; and {} more", violations.len() - 20));
        }
        return Err(DriverError::new(format!(
            "forbidPaths gate '{gate_id}' rejected {} path(s) touched in lane history (a later removal does not clear this; the path must never appear in any lane commit): {preview}",
            violations.len()
        )));
    }
    Ok(changed.len())
}

fn current_base_revision(worktree: &Path, config: &RepoConfig) -> Result<String> {
    let reference = format!("refs/heads/{}", config.base_branch);
    let fetched = git(
        worktree,
        [
            "fetch",
            "--no-tags",
            "--end-of-options",
            &config.remote,
            &reference,
        ],
        false,
    )?;
    if !fetched.success() {
        return Err(DriverError::new(format!(
            "cannot read base branch {:?} from {:?}: {}",
            config.base_branch,
            config.remote,
            fetched.detail()
        )));
    }
    full_oid(
        Some(&Json::from(
            git(
                worktree,
                ["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
                true,
            )?
            .stdout_trimmed(),
        )),
        "fetched base branch revision",
    )
}

fn action_ownership(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &["task", "domainsRequired", "repositoryConfig", "workspace"],
        "ownership brief",
    )?;
    let config = repo_config(data.get("repositoryConfig"))?;
    let workspace = prepared_workspace(data.get("workspace"), "workspace")?;
    if !is_task_id(&workspace.task_id) {
        return Err(DriverError::new("workspace.taskId is not safe"));
    }
    if !is_full_oid(&workspace.base_rev) {
        return Err(DriverError::new(
            "workspace.baseRev must be a full Git object ID",
        ));
    }
    if !workspace.worktree.is_absolute() || !workspace.worktree.is_dir() {
        return Err(DriverError::new(
            "workspace.worktreePath must be an absolute existing directory",
        ));
    }
    git(&workspace.worktree, ["rev-parse", "--git-dir"], true)?;
    let actual_branch =
        git(&workspace.worktree, ["branch", "--show-current"], true)?.stdout_trimmed();
    if actual_branch != workspace.branch {
        return Err(DriverError::new(format!(
            "worktree is on branch {actual_branch:?}, expected {:?}",
            workspace.branch
        )));
    }
    if !git(&workspace.worktree, ["status", "--porcelain"], true)?
        .stdout
        .is_empty()
    {
        return Err(DriverError::new(
            "agent left uncommitted changes; commit the task before ownership validation",
        ));
    }
    let head = git(
        &workspace.worktree,
        ["rev-parse", "--verify", "HEAD^{commit}"],
        true,
    )?
    .stdout_trimmed();
    if head == workspace.base_rev {
        return Err(DriverError::new(
            "agent produced no commit relative to the prepared base",
        ));
    }
    if !git(
        &workspace.worktree,
        ["merge-base", "--is-ancestor", &workspace.base_rev, &head],
        false,
    )?
    .success()
    {
        return Err(DriverError::new(
            "task head is not descended from its prepared base revision",
        ));
    }
    let domains_required = required_bool(data.get("domainsRequired"), "domainsRequired")?;
    let current_base = current_base_revision(&workspace.worktree, &config)?;
    Ok(enforce_conflict_domains(
        &workspace.worktree,
        &workspace.base_rev,
        &head,
        data.get("task"),
        &workspace.task_id,
        domains_required,
        Some(&current_base),
    )?
    .to_json())
}

fn action_tree_delta(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &["task", "workspace", "ownedPaths", "ownershipRan"],
        "tree-delta brief",
    )?;
    let ownership_ran = data
        .get("ownershipRan")
        .map(|value| required_bool(Some(value), "ownershipRan"))
        .transpose()?
        .unwrap_or(true);
    let task = data
        .get("task")
        .and_then(Json::as_object)
        .ok_or_else(|| DriverError::new("task must be an object"))?;
    let task_id = required_string(task.get("id"), "task.id", None)?;
    if !is_task_id(&task_id) {
        return Err(DriverError::new("task.id is not safe"));
    }
    let workspace = prepared_workspace(data.get("workspace"), "workspace")?;
    if workspace.task_id != task_id {
        return Err(DriverError::new("workspace.taskId does not match task.id"));
    }
    let worktree = workspace.worktree;
    if !worktree.is_absolute() || !worktree.is_dir() {
        return Err(DriverError::new(
            "workspace.worktreePath must be an absolute existing directory",
        ));
    }
    git(&worktree, ["rev-parse", "--git-dir"], true)?;

    let (allowlist, basis) = if task.contains_key("conflictDomains") {
        let domains = normalize_paths(task.get("conflictDomains"), "task.conflictDomains", false)?
            .expect("present conflictDomains stays present");
        let basis = if domains.is_empty() {
            "declared-empty"
        } else {
            "declared"
        };
        (domains, basis)
    } else if ownership_ran {
        let owned = match data.get("ownedPaths") {
            Some(value) if !matches!(value, Json::Null) => {
                string_list(Some(value), "ownedPaths", false)?
            }
            _ => Vec::new(),
        };
        (owned, "owned-paths-fallback")
    } else {
        return Err(DriverError::new(format!(
            "tree-delta gate refuses to judge task {task_id:?}: its agent node failed, so the ownership node never ran and certified no ownedPaths, and the task declares no conflictDomains -- there is no allowlist to judge the worktree against. Declare conflictDomains for this task and re-arm. The pre-agent baseline is left in place, so the writes this pass could not judge are still judgeable then."
        )));
    };

    let before = worktrees::read_snapshot(&worktree)?.ok_or_else(|| {
        DriverError::new(
            "no change-set snapshot was recorded before the agent node; cannot evaluate the tree-delta gate",
        )
    })?;
    let after = worktrees::change_set_fingerprint(&worktree)?;
    let deltas = worktrees::change_set_delta(&before, &after);
    let breaches: Vec<_> = deltas
        .iter()
        .filter(|(path, _)| !allowlist.iter().any(|domain| domains_overlap(path, domain)))
        .collect();
    worktrees::clear_snapshot(&worktree)?;
    if !breaches.is_empty() {
        let mut preview = breaches
            .iter()
            .take(20)
            .map(|(path, kind)| format!("{kind} {}", Json::from(path.clone()).stringify()))
            .collect::<Vec<_>>()
            .join("; ");
        if breaches.len() > 20 {
            preview.push_str(&format!("; and {} more", breaches.len() - 20));
        }
        return Err(DriverError::new(format!(
            "tree-delta gate detected {} out-of-allowlist change(s) ({basis} allowlist): {preview}",
            breaches.len()
        )));
    }
    Ok(Json::object([
        ("taskId", Json::from(task_id)),
        ("checkedPaths", Json::from(deltas.len())),
        ("allowlistBasis", Json::from(basis)),
        (
            "allowlist",
            Json::Array(allowlist.into_iter().map(Json::from).collect()),
        ),
        ("ownershipRan", Json::from(ownership_ran)),
    ]))
}

#[derive(Clone, Debug)]
struct ForbidGate {
    id: String,
    patterns: Vec<String>,
}

fn canonical_forbid_paths_gate(value: Option<&Json>, context: &str) -> Result<ForbidGate> {
    let value = value.ok_or_else(|| DriverError::new(format!("{context} must be an object")))?;
    let gate = object_complete(
        value,
        &["kind", "id", "forbidPaths", "runtimeMaxSec"],
        context,
    )?;
    if gate.get("kind").and_then(Json::as_str) != Some("forbidPaths") {
        return Err(DriverError::new(format!(
            "{context}.kind must equal forbidPaths"
        )));
    }
    let id = required_string(gate.get("id"), &format!("{context}.id"), Some(80))?;
    if !is_component(&id) {
        return Err(DriverError::new(format!(
            "{context}.id is not a safe component"
        )));
    }
    let patterns = string_list(
        gate.get("forbidPaths"),
        &format!("{context}.forbidPaths"),
        true,
    )?;
    if patterns.len() > 128 {
        return Err(DriverError::new(format!(
            "{context}.forbidPaths exceeds 128 entries"
        )));
    }
    let mut seen = BTreeSet::new();
    for (index, pattern) in patterns.iter().enumerate() {
        let pieces: Vec<_> = pattern.split('/').collect();
        if pattern.chars().count() > 1_024
            || pattern.starts_with('/')
            || pattern.ends_with('/')
            || pieces.contains(&"..")
            || pieces
                .iter()
                .any(|piece| piece.contains("**") && *piece != "**")
            || !seen.insert(pattern.clone())
        {
            return Err(DriverError::new(format!(
                "internal campaign contract violation: {context}.forbidPaths[{index}] is not canonical"
            )));
        }
    }
    positive_u64(
        gate.get("runtimeMaxSec"),
        &format!("{context}.runtimeMaxSec"),
    )?;
    Ok(ForbidGate { id, patterns })
}

fn action_constraint(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &["gate", "repositoryConfig", "workspace"],
        "constraint brief",
    )?;
    let gate = canonical_forbid_paths_gate(data.get("gate"), "constraint gate")?;
    let config = repo_config(data.get("repositoryConfig"))?;
    let workspace_value = data
        .get("workspace")
        .ok_or_else(|| DriverError::new("workspace must be an object"))?;
    let workspace = object_exact(
        workspace_value,
        &[
            "taskId",
            "baseRev",
            "branch",
            "worktreePath",
            "conflictDomains",
        ],
        "workspace",
    )?;
    if workspace.contains_key("conflictDomains") {
        normalize_paths(
            workspace.get("conflictDomains"),
            "workspace.conflictDomains",
            false,
        )?;
    }
    let task_id = required_string(workspace.get("taskId"), "workspace.taskId", None)?;
    if !is_task_id(&task_id) {
        return Err(DriverError::new("workspace.taskId is not safe"));
    }
    let base_rev = full_oid(workspace.get("baseRev"), "workspace.baseRev")?;
    let worktree = PathBuf::from(required_string(
        workspace.get("worktreePath"),
        "workspace.worktreePath",
        None,
    )?);
    if !worktree.is_absolute() || !worktree.is_dir() {
        return Err(DriverError::new(
            "workspace.worktreePath must be an absolute existing directory",
        ));
    }
    git(&worktree, ["rev-parse", "--git-dir"], true)?;
    git(
        &worktree,
        ["rev-parse", "--verify", &format!("{base_rev}^{{commit}}")],
        true,
    )?;
    let head = git(&worktree, ["rev-parse", "--verify", "HEAD^{commit}"], true)?.stdout_trimmed();
    if !git(
        &worktree,
        ["merge-base", "--is-ancestor", &base_rev, &head],
        false,
    )?
    .success()
    {
        return Err(DriverError::new(
            "task head is not descended from its prepared base revision",
        ));
    }
    let current_base = current_base_revision(&worktree, &config)?;
    let union_base = lane_union_base(&worktree, &base_rev, &head, Some(&current_base))?;
    let checked_paths =
        evaluate_forbid_paths(&worktree, &union_base, &head, &gate.id, &gate.patterns)?;
    Ok(Json::object([
        ("gateId", Json::from(gate.id)),
        ("kind", Json::from("forbidPaths")),
        (
            "patterns",
            Json::Array(gate.patterns.into_iter().map(Json::from).collect()),
        ),
        ("checkedPaths", Json::from(checked_paths)),
        ("baseRev", Json::from(base_rev)),
        ("head", Json::from(head)),
    ]))
}

const COMMIT_TYPES: [&str; 11] = [
    "build", "chore", "ci", "docs", "feat", "fix", "perf", "refactor", "revert", "style", "test",
];

const OUTCOME_FIRST_IRREGULAR_VERBS: &[&str] = &[
    "began",
    "bound",
    "brought",
    "bought",
    "built",
    "came",
    "caught",
    "chose",
    "cut",
    "did",
    "fell",
    "found",
    "gave",
    "grew",
    "held",
    "hit",
    "kept",
    "knew",
    "left",
    "lost",
    "made",
    "met",
    "put",
    "ran",
    "read",
    "said",
    "sat",
    "saw",
    "sent",
    "set",
    "shut",
    "spent",
    "stood",
    "sold",
    "sought",
    "spoke",
    "taught",
    "thought",
    "took",
    "understood",
    "went",
    "won",
    "wrote",
];

fn take_chars(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

fn trim_end_punctuation(value: &str) -> &str {
    value.trim_end_matches([' ', '.', ':', ';', '?'])
}

/// Byte ranges with the exact stage-0 inline-code negation semantics.
///
/// Each opener is one non-empty run of backticks and only an equal-width run
/// closes it. An unmatched or mismatched run grants no exception. Byte offsets
/// are safe because both the fences and the exclamation predicate are ASCII.
fn closed_inline_code_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let Some(relative) = bytes[cursor..].iter().position(|byte| *byte == b'`') else {
            break;
        };
        let opening = cursor + relative;
        let mut opening_end = opening;
        while opening_end < bytes.len() && bytes[opening_end] == b'`' {
            opening_end += 1;
        }
        let fence_width = opening_end - opening;
        let mut search = opening_end;
        let mut closing_end = None;
        while search < bytes.len() {
            let Some(relative) = bytes[search..].iter().position(|byte| *byte == b'`') else {
                break;
            };
            let closing = search + relative;
            let mut candidate_end = closing;
            while candidate_end < bytes.len() && bytes[candidate_end] == b'`' {
                candidate_end += 1;
            }
            if candidate_end - closing == fence_width {
                closing_end = Some(candidate_end);
                break;
            }
            search = candidate_end;
        }
        let Some(closing_end) = closing_end else {
            cursor = opening_end;
            continue;
        };
        spans.push((opening, closing_end));
        cursor = closing_end;
    }
    spans
}

fn contains_bare_exclamation_mark(text: &str) -> bool {
    let mut plain_start = 0;
    for (code_start, code_end) in closed_inline_code_spans(text) {
        if text[plain_start..code_start].contains('!') {
            return true;
        }
        plain_start = code_end;
    }
    text[plain_start..].contains('!')
}

fn replace_bare_exclamation_marks(text: &str, replacement: &str) -> String {
    let mut output = String::new();
    let mut plain_start = 0;
    for (code_start, code_end) in closed_inline_code_spans(text) {
        output.push_str(&text[plain_start..code_start].replace('!', replacement));
        output.push_str(&text[code_start..code_end]);
        plain_start = code_end;
    }
    output.push_str(&text[plain_start..].replace('!', replacement));
    output
}

fn opens_with_list_marker(line: &str) -> bool {
    let line = line.trim_start();
    if line
        .strip_prefix('-')
        .or_else(|| line.strip_prefix('*'))
        .is_some_and(|rest| rest.chars().next().is_some_and(char::is_whitespace))
    {
        return true;
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    digits != 0
        && line
            .as_bytes()
            .get(digits)
            .is_some_and(|byte| matches!(byte, b'.' | b')'))
        && line[digits + 1..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
}

fn validate_outcome_first(text: &str, maximum: usize, context: &str) -> Option<String> {
    if text.trim().is_empty() {
        return Some(format!("{context} must be non-empty text"));
    }
    if text.chars().count() > maximum {
        return Some(format!("{context} is over the {maximum} character cap"));
    }
    if contains_bare_exclamation_mark(text) {
        return Some(format!("{context} contains an exclamation mark"));
    }
    let first_line = text
        .trim()
        .split_once('\n')
        .map_or(text.trim(), |row| row.0)
        .trim();
    if opens_with_list_marker(first_line) {
        return Some(format!("{context} must open with a sentence, not a list"));
    }
    let mut split_at = None;
    let mut prior = None;
    for (index, character) in first_line.char_indices() {
        if character.is_whitespace() && matches!(prior, Some('.' | ':')) {
            split_at = Some(index);
            break;
        }
        prior = Some(character);
    }
    let first_sentence = first_line[..split_at.unwrap_or(first_line.len())].trim();
    if !first_sentence.ends_with(['.', ':']) {
        return Some(format!("{context} leading sentence must end with a period"));
    }
    if first_sentence.chars().count() > OUTCOME_FIRST_LEAD_MAX {
        return Some(format!(
            "{context} leading sentence is over {OUTCOME_FIRST_LEAD_MAX} characters"
        ));
    }
    let opening: String = first_sentence
        .chars()
        .take_while(|character| character.is_ascii_alphabetic())
        .collect();
    let folded = opening.to_ascii_lowercase();
    if opening.is_empty()
        || !(folded.ends_with("ed") || OUTCOME_FIRST_IRREGULAR_VERBS.contains(&folded.as_str()))
    {
        return Some(format!("{context} must open with a past-tense verb"));
    }
    None
}

fn line_starts_case_insensitive(text: &str, prefixes: &[&str]) -> bool {
    text.lines().any(|line| {
        prefixes.iter().any(|prefix| {
            line.get(..prefix.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
        })
    })
}

fn fold_commit_line(line: &str, maximum: usize) -> (Vec<String>, bool) {
    let mut remaining = line.trim_end();
    let mut folded = Vec::new();
    let mut changed = false;
    while remaining.chars().count() > maximum {
        changed = true;
        let limit = remaining
            .char_indices()
            .nth(maximum)
            .map_or(remaining.len(), |(index, _)| index);
        let split = remaining[..limit]
            .char_indices()
            .rev()
            .find(|(index, character)| *index != 0 && character.is_whitespace())
            .map(|(index, _)| index)
            .unwrap_or(limit);
        folded.push(remaining[..split].trim_end().to_owned());
        remaining = remaining[split..].trim_start();
    }
    folded.push(remaining.to_owned());
    (folded, changed)
}

fn fold_commit_body(body: &str) -> (String, bool) {
    let mut lines = Vec::new();
    let mut changed = false;
    for line in body.split('\n') {
        let (folded, line_changed) = fold_commit_line(line, COMMIT_BODY_LINE_MAX);
        changed |= line_changed;
        lines.extend(folded);
    }
    (lines.join("\n"), changed)
}

fn add_period_to_first_line(body: &str) -> String {
    body.split_once('\n').map_or_else(
        || format!("{body}."),
        |(first, rest)| format!("{first}.\n{rest}"),
    )
}

fn validate_lane_tip_message(
    raw_header: &str,
    raw_body: &str,
) -> std::result::Result<(CommitMessage, bool), String> {
    let mut repaired = false;
    let header = raw_header.trim();
    repaired |= header != raw_header;
    if header.is_empty() {
        return Err("subject is empty".to_owned());
    }
    if header
        .chars()
        .any(|character| (character as u32) < 32 || character == '\u{7f}')
    {
        return Err("subject contains control characters".to_owned());
    }
    let pattern = Regex::new(r"^([a-z]+)(?:\(([a-z0-9][a-z0-9._/-]{0,31})\))?(!)?: (.+)$")
        .expect("static conventional-commit regex");
    let captures = pattern.captures(header).ok_or_else(|| {
        "subject does not match '<type>(<scope>)[!]: <lowercase subject>'".to_owned()
    })?;
    let kind = captures.get(1).expect("type capture").as_str();
    if !COMMIT_TYPES.contains(&kind) {
        return Err(format!("type must be one of {}", COMMIT_TYPES.join(", ")));
    }
    let scope = captures.get(2).map(|capture| capture.as_str());
    let breaking = captures.get(3).is_some();
    let mut subject = captures
        .get(4)
        .expect("subject capture")
        .as_str()
        .trim()
        .to_owned();
    repaired |= subject != captures.get(4).expect("subject capture").as_str();
    if subject.ends_with('.') {
        subject.pop();
        repaired = true;
    }
    if subject.is_empty() || subject.ends_with('.') {
        return Err("subject must be non-empty and not end with a period".to_owned());
    }
    if subject.chars().next().is_some_and(char::is_uppercase) {
        return Err("subject must not start with a capital letter".to_owned());
    }
    let prefix = match (scope, breaking) {
        (None, false) => format!("{kind}:"),
        (None, true) => format!("{kind}!:"),
        (Some(scope), false) => format!("{kind}({scope}):"),
        (Some(scope), true) => format!("{kind}({scope})!:"),
    };
    let header = format!("{prefix} {subject}");
    if header.chars().count() > COMMIT_HEADER_MAX {
        return Err(format!(
            "header is {} characters, over the {COMMIT_HEADER_MAX} cap",
            header.chars().count()
        ));
    }

    let mut body = raw_body
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_owned();
    if body.chars().count() > COMMIT_BODY_MAX {
        return Err(format!("body is over the {COMMIT_BODY_MAX} character cap"));
    }
    if body
        .chars()
        .any(|character| ((character as u32) < 32 && character != '\n') || character == '\u{7f}')
    {
        return Err("body contains control characters".to_owned());
    }

    let closing = Regex::new(
        r"(?i)\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\b\s*:?\s*(?:#\d+|GH-\d+|[\w.-]+/[\w.-]+#\d+|https?://\S+/(?:issues|pull)/\d+)",
    )
    .expect("static closing-keyword regex");
    let mention = Regex::new(r"(?:^|[^0-9A-Za-z._-])@[0-9A-Za-z][0-9A-Za-z-]*")
        .expect("static mention regex");
    for text in [&header, &body] {
        if line_starts_case_insensitive(text, &[TALLY_TASK_PREFIX, TALLY_REVISION_PREFIX]) {
            return Err("message contains a managed completion trailer".to_owned());
        }
        if line_starts_case_insensitive(text, &[ASSISTED_BY_PREFIX]) {
            return Err("message contains an Assisted-by trailer".to_owned());
        }
        if closing.is_match(text) {
            return Err("message contains a GitHub closing keyword".to_owned());
        }
        if mention.is_match(text) {
            return Err("message contains an @mention".to_owned());
        }
    }
    if !body.is_empty() {
        if validate_outcome_first(&body, COMMIT_BODY_MAX, "lane-tip body").as_deref()
            == Some("lane-tip body leading sentence must end with a period")
        {
            body = add_period_to_first_line(&body);
            repaired = true;
        }
        if let Some(reason) = validate_outcome_first(&body, COMMIT_BODY_MAX, "lane-tip body") {
            return Err(reason);
        }
        let (folded, body_repaired) = fold_commit_body(&body);
        body = folded;
        repaired |= body_repaired;
    }

    Ok((
        CommitMessage {
            subject: header,
            body,
        },
        repaired,
    ))
}

fn template_commit_message(task: &BTreeMap<String, Json>, body: &str) -> Result<CommitMessage> {
    let task_id = required_string(task.get("id"), "task.id", None)?;
    let title = required_string(task.get("title"), "task.title", None)?;
    Ok(CommitMessage {
        subject: format!("{task_id}: {title}"),
        body: body.to_owned(),
    })
}

fn template_narration(task: &BTreeMap<String, Json>, body: &str) -> Result<Json> {
    let message = template_commit_message(task, body)?;
    Ok(Json::object([
        ("source", Json::from("template")),
        ("subject", Json::from(message.subject)),
        ("body", Json::from(message.body)),
    ]))
}

fn lane_tip_commit_message(
    checkout: &Path,
    head: &str,
    task: &BTreeMap<String, Json>,
) -> Result<CommitMessage> {
    let raw_header = git(checkout, ["show", "-s", "--format=%s", head], true)?.stdout_text();
    let raw_header = raw_header.trim_end_matches(['\r', '\n']);
    let raw_body = git(checkout, ["show", "-s", "--format=%b", head], true)?.stdout_text();
    match validate_lane_tip_message(raw_header, &raw_body) {
        Ok((mut message, repaired)) => {
            let note = if repaired {
                "Adopted the lane-tip commit message after deterministic formatting."
            } else {
                "Adopted the lane-tip commit message without repair."
            };
            message.body = if message.body.is_empty() {
                note.to_owned()
            } else {
                format!("{}\n\n{note}", message.body)
            };
            Ok(message)
        }
        Err(reason) => {
            let reason = take_chars(&reason, COMMIT_REASON_MAX);
            let reason = reason.trim_end_matches('.');
            template_commit_message(
                task,
                &format!(
                    "Rejected the lane-tip commit message and used the task-id template instead. Reason: {reason}."
                ),
            )
        }
    }
}

fn environment_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && (bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

fn portable_steward_pattern(pattern: &str, context: &str) -> Result<Regex> {
    let bytes = pattern.as_bytes();
    let mut index = 0;
    let mut in_class = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                index += 1;
                if index == bytes.len() {
                    return Err(DriverError::new(format!(
                        "internal campaign contract violation: {context} ends in an escape"
                    )));
                }
                if bytes[index].is_ascii_digit() || matches!(bytes[index], b'k' | b'g') {
                    return Err(DriverError::new(format!(
                        "internal campaign contract violation: {context} contains a backreference"
                    )));
                }
            }
            b'[' => {
                if in_class {
                    return Err(DriverError::new(format!(
                        "internal campaign contract violation: {context} contains a nested character class"
                    )));
                }
                in_class = true;
            }
            b']' => in_class = false,
            b'(' if !in_class
                && bytes.get(index + 1) == Some(&b'?')
                && bytes.get(index + 2) != Some(&b':') =>
            {
                return Err(DriverError::new(format!(
                    "internal campaign contract violation: {context} contains a non-portable group"
                )));
            }
            _ => {}
        }
        index += 1;
    }
    let compiled = Regex::new(pattern).map_err(|error| {
        DriverError::new(format!(
            "internal campaign contract violation: {context} did not compile in Rust: {error}"
        ))
    })?;
    if compiled.captures_len() != 2 {
        return Err(DriverError::new(format!(
            "internal campaign contract violation: {context} does not have exactly one capture group"
        )));
    }
    Ok(compiled)
}

fn validate_steward_catalog_role(value: Option<&Json>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if matches!(value, Json::Null) {
        return Ok(());
    }
    let role = object_complete(
        value,
        &[
            "adapter",
            "argv",
            "env",
            "finalMessagePattern",
            "runtimeMaxSec",
        ],
        "campaign steward",
    )?;
    let adapter = required_string(role.get("adapter"), "campaign steward.adapter", Some(80))?;
    if !is_component(&adapter) {
        return Err(DriverError::new(
            "campaign steward.adapter is not a safe component",
        ));
    }
    argv_list(role.get("argv"), "campaign steward.argv")?;
    let environment = role
        .get("env")
        .and_then(Json::as_object)
        .ok_or_else(|| DriverError::new("campaign steward.env must be an object"))?;
    if environment.len() > 64 {
        return Err(DriverError::new(
            "internal campaign contract violation: campaign steward.env exceeds 64 entries",
        ));
    }
    for (key, value) in environment {
        if !environment_name(key) {
            return Err(DriverError::new(
                "campaign steward.env names must be environment identifiers",
            ));
        }
        if key == "TALLY_BRIEF" {
            return Err(DriverError::new(
                "campaign steward.env must not set reserved variable TALLY_BRIEF",
            ));
        }
        required_string(
            Some(value),
            &format!("campaign steward.env.{key}"),
            Some(4_096),
        )?;
    }
    let pattern_text = required_string(
        role.get("finalMessagePattern"),
        "campaign steward.finalMessagePattern",
        Some(1_024),
    )?;
    portable_steward_pattern(&pattern_text, "campaign steward.finalMessagePattern")?;
    let _runtime_seconds = match role.get("runtimeMaxSec") {
        Some(Json::Null) => None,
        value => Some(positive_u64(value, "campaign steward.runtimeMaxSec")?),
    };
    Ok(())
}

const SECRET_TOKEN_PREFIXES: [&str; 10] = [
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "sk-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
];

const EXTRA_SECRET_TOKEN_PREFIXES: [&str; 1] = ["xoxr-"];

const SENSITIVE_LINE_MARKERS: [&str; 21] = [
    "authorization",
    "bearer",
    "token",
    "secret",
    "password",
    "passwd",
    "credential",
    "credentials",
    "api_key",
    "api-key",
    "apikey",
    "private key",
    "access key",
    "access_key",
    "secret_key",
    "client_secret",
    "client key",
    "cookie",
    "dsn",
    "session_id",
    "sessionid",
];

fn public_token_is_sensitive(value: &str) -> bool {
    let token = value.trim_matches(['\'', '"', '`', '(', ')', '[', ']', '{', '}', ',', ';']);
    let lower = token.to_ascii_lowercase();
    if SECRET_TOKEN_PREFIXES
        .iter()
        .chain(EXTRA_SECRET_TOKEN_PREFIXES.iter())
        .any(|prefix| lower.starts_with(prefix))
        || ((token.starts_with("AKIA") || token.starts_with("ASIA")) && token.chars().count() >= 16)
        || (token.contains("://") && (token.contains('@') || token.contains('?')))
    {
        return true;
    }
    let jwt: Vec<_> = token.split('.').collect();
    if jwt.len() == 3 && jwt.iter().all(|part| part.chars().count() >= 8) {
        return true;
    }
    if token.chars().count() < 32 || !token.is_ascii() {
        return false;
    }
    let has_lower = token.bytes().any(|byte| byte.is_ascii_lowercase());
    let has_upper = token.bytes().any(|byte| byte.is_ascii_uppercase());
    let has_digit = token.bytes().any(|byte| byte.is_ascii_digit());
    if token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        if token == lower && matches!(token.len(), 40 | 64) {
            return false;
        }
        return true;
    }
    let token_like = token.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'_' | b'-' | b'=')
    });
    token_like && has_digit && ((has_lower && has_upper) || token.len() >= 40)
}

fn public_line_is_sensitive(lower: &str) -> bool {
    for marker in SENSITIVE_LINE_MARKERS {
        let mut offset = 0;
        while let Some(relative) = lower[offset..].find(marker) {
            let mut index = offset + relative + marker.len();
            while lower
                .as_bytes()
                .get(index)
                .is_some_and(|byte| matches!(byte, b'\'' | b'"' | b'`' | b' ' | b'\t'))
            {
                index += 1;
            }
            if lower
                .as_bytes()
                .get(index)
                .is_some_and(|byte| matches!(byte, b':' | b'='))
            {
                return true;
            }
            offset += relative + 1;
        }
    }
    false
}

fn redact_public_text(value: &str) -> (String, bool) {
    let mut output = String::new();
    let mut redacted = false;
    let mut private_key_block = false;
    for line in value.split_inclusive('\n') {
        let lower = line.to_lowercase();
        if lower.contains("-----begin ") && lower.contains("private key-----") {
            private_key_block = true;
        }
        if private_key_block || public_line_is_sensitive(&lower) {
            output.push_str("[redacted sensitive diagnosis line]");
            if line.ends_with('\n') {
                output.push('\n');
            }
            redacted = true;
        } else {
            let mut start = 0;
            let mut whitespace = line.chars().next().is_some_and(char::is_whitespace);
            for (index, character) in line.char_indices().skip(1) {
                if character.is_whitespace() != whitespace {
                    let chunk = &line[start..index];
                    if !whitespace && public_token_is_sensitive(chunk) {
                        output.push_str("[redacted-token]");
                        redacted = true;
                    } else {
                        output.push_str(chunk);
                    }
                    start = index;
                    whitespace = character.is_whitespace();
                }
            }
            let chunk = &line[start..];
            if !chunk.is_empty() && !whitespace && public_token_is_sensitive(chunk) {
                output.push_str("[redacted-token]");
                redacted = true;
            } else {
                output.push_str(chunk);
            }
        }
        if lower.contains("-----end ") && lower.contains("private key-----") {
            private_key_block = false;
        }
    }
    (output.trim().to_owned(), redacted)
}

fn normalized_worker_findings(value: Option<&Json>) -> Result<Option<(String, String)>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if matches!(value, Json::Null) {
        return Ok(None);
    }
    let findings = object_complete(value, &["taskUuid", "message"], "workerFindings")?;
    let task_uuid = required_string(
        findings.get("taskUuid"),
        "workerFindings.taskUuid",
        Some(64),
    )?;
    let parsed = Uuid::parse_str(&task_uuid)
        .map_err(|_| DriverError::new("workerFindings.taskUuid must be a UUID"))?;
    if parsed.to_string() != task_uuid.to_ascii_lowercase() {
        return Err(DriverError::new(
            "workerFindings.taskUuid must use canonical UUID spelling",
        ));
    }
    let message = findings
        .get("message")
        .and_then(Json::as_str)
        .ok_or_else(|| DriverError::new("workerFindings.message must be text"))?;
    let normalized = message
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .map(|character| {
            if matches!(character, '\n' | '\t')
                || !((character as u32) < 32 || (127..160).contains(&(character as u32)))
            {
                character
            } else {
                '�'
            }
        })
        .collect::<String>()
        .trim()
        .to_owned();
    if normalized.is_empty() {
        return Ok(None);
    }
    Ok(Some((parsed.to_string(), normalized)))
}

fn utf8_prefix(value: &str, maximum_bytes: usize) -> &str {
    let mut end = maximum_bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn bound_worker_findings(prefix: &str, text: &str) -> Result<String> {
    if prefix.len() >= MAX_WORKER_FINDINGS_BYTES {
        return Err(DriverError::new(
            "worker findings marker exceeds its receipt bound",
        ));
    }
    let available = MAX_WORKER_FINDINGS_BYTES - prefix.len();
    if text.len() <= available {
        return Ok(format!("{prefix}{text}"));
    }
    let width = available.saturating_sub(WORKER_FINDINGS_TRUNCATION.len());
    let clipped = utf8_prefix(text, width).trim_end();
    let body = format!("{prefix}{clipped}{WORKER_FINDINGS_TRUNCATION}");
    if body.len() > MAX_WORKER_FINDINGS_BYTES {
        return Err(DriverError::new(
            "worker findings escaped its receipt byte bound",
        ));
    }
    Ok(body)
}

fn publish_worker_findings(data: &BTreeMap<String, Json>) -> Result<Option<String>> {
    let Some((task_uuid, message)) = normalized_worker_findings(data.get("workerFindings"))? else {
        return Ok(None);
    };
    let campaign = required_string(data.get("campaign"), "campaign", None)?;
    if !is_component(&campaign) {
        return Err(DriverError::new("campaign is not a safe component"));
    }
    let code_repository = repository_name(data.get("repository"), "repository")?;
    let code_config = repo_config(data.get("repositoryConfig"))?;
    let (_, _, target) = campaign_coordinates(data, code_repository, code_config)?;
    let issue_number = campaign_issue(data.get("issue"))?.0;
    let task = data
        .get("task")
        .and_then(Json::as_object)
        .ok_or_else(|| DriverError::new("task must be an object"))?;
    let task_id = required_string(task.get("id"), "task.id", None)?;
    if !is_task_id(&task_id) {
        return Err(DriverError::new("task.id is not safe"));
    }
    let (public_text, _) = redact_public_text(&message);
    let prefix = "### Worker findings\n\n_Captured from the implementation worker's final message; redacted and bounded by tally._\n\n";
    let body = bound_worker_findings(prefix, &public_text)?;
    let reference = format!(
        "{}/findings/{task_id}/{task_uuid}",
        local_state_prefix(&campaign, &issue_number)
    );
    let expected = Json::object([
        ("schemaVersion", Json::Number("1".to_owned())),
        ("kind", Json::from("worker-findings")),
        ("campaign", Json::from(campaign)),
        ("issueNumber", Json::from(issue_number)),
        ("taskId", Json::from(task_id)),
        ("agentTaskUuid", Json::from(task_uuid)),
        ("body", Json::from(body)),
        ("redaction", Json::from(PUBLIC_REDACTION)),
    ]);
    let (_, observed) = write_local_blob(&target.config, &reference, &expected)?;
    if observed != expected {
        return Err(DriverError::new(format!(
            "local campaign worker findings {reference:?} disagree with this attempt"
        )));
    }
    Ok(Some(format!("local://{}/{reference}", target.repository)))
}

fn configured_forbid_gates(
    value: Option<&Json>,
    context: &str,
) -> Result<BTreeMap<String, Vec<String>>> {
    let values = value
        .and_then(Json::as_array)
        .ok_or_else(|| DriverError::new(format!("{context} must be an array")))?;
    if values.len() > 16 {
        return Err(DriverError::new(format!("{context} exceeds 16 gates")));
    }
    let mut seen = BTreeSet::new();
    let mut gates = BTreeMap::new();
    for (index, candidate) in values.iter().enumerate() {
        let object = candidate
            .as_object()
            .ok_or_else(|| DriverError::new(format!("{context}[{index}] must be an object")))?;
        let id = required_string(
            object.get("id"),
            &format!("{context}[{index}].id"),
            Some(80),
        )?;
        if !seen.insert(id.clone()) {
            return Err(DriverError::new(format!(
                "{context} repeats gate id {id:?}"
            )));
        }
        if object.get("kind").and_then(Json::as_str) == Some("forbidPaths") {
            let gate =
                canonical_forbid_paths_gate(Some(candidate), &format!("{context}[{index}]"))?;
            gates.insert(gate.id, gate.patterns);
        }
    }
    Ok(gates)
}

fn enforce_configured_gates(
    constraints: &[Constraint],
    configured: &BTreeMap<String, Vec<String>>,
) -> Result<()> {
    let witnessed: BTreeMap<_, _> = constraints
        .iter()
        .map(|constraint| (constraint.gate_id.clone(), constraint.patterns.clone()))
        .collect();
    for (id, patterns) in configured {
        let Some(observed) = witnessed.get(id) else {
            return Err(DriverError::new(format!(
                "forbidPaths gate '{id}' is configured for this campaign but no witnessed receipt reached publication"
            )));
        };
        if observed != patterns {
            return Err(DriverError::new(format!(
                "forbidPaths gate '{id}' was witnessed against patterns {}, but the campaign configures {}",
                Json::Array(observed.iter().cloned().map(Json::from).collect()).stringify(),
                Json::Array(patterns.iter().cloned().map(Json::from).collect()).stringify()
            )));
        }
    }
    if let Some(id) = witnessed.keys().find(|id| !configured.contains_key(*id)) {
        return Err(DriverError::new(format!(
            "forbidPaths gate '{id}' presented a receipt for a gate this campaign does not configure"
        )));
    }
    Ok(())
}

fn enforce_constraint_results(
    worktree: &Path,
    base_rev: &str,
    union_base: &str,
    head: &str,
    constraints: &[Constraint],
) -> Result<()> {
    for constraint in constraints {
        if constraint.base_rev != base_rev {
            return Err(DriverError::new(format!(
                "forbidPaths gate '{}' was witnessed against base {}, expected {base_rev}",
                constraint.gate_id, constraint.base_rev
            )));
        }
        let checked = evaluate_forbid_paths(
            worktree,
            union_base,
            head,
            &constraint.gate_id,
            &constraint.patterns,
        )?;
        if constraint.head == head && constraint.checked_paths != checked {
            return Err(DriverError::new(format!(
                "forbidPaths gate '{}' receipt counted {} paths at {head}, publication counted {checked}",
                constraint.gate_id, constraint.checked_paths
            )));
        }
    }
    Ok(())
}

fn action_publish(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &[
            "campaign",
            "campaignIdentity",
            "repository",
            "repositoryConfig",
            "issue",
            "runId",
            "workspaceRoot",
            "task",
            "workspace",
            "constraints",
            "domainsRequired",
            "gates",
            "steward",
            "workerFindings",
            "specRepository",
            "issueRepository",
        ],
        "publish brief",
    )?;
    let config = repo_config(data.get("repositoryConfig"))?;
    let workspace = prepared_workspace(data.get("workspace"), "workspace")?;
    if !is_full_oid(&workspace.base_rev) {
        return Err(DriverError::new(
            "workspace.baseRev must be a full Git object ID",
        ));
    }
    if !workspace.worktree.is_absolute() || !workspace.worktree.is_dir() {
        return Err(DriverError::new(
            "workspace.worktreePath must be an existing directory for publish",
        ));
    }
    git(&workspace.worktree, ["rev-parse", "--git-dir"], true)?;
    let constraints = normalize_constraints(data.get("constraints"), "publish constraints")?;
    let configured = configured_forbid_gates(data.get("gates"), "publish gates")?;
    enforce_configured_gates(&constraints, &configured)?;
    if !is_task_id(&workspace.task_id) {
        return Err(DriverError::new("workspace.taskId is not safe"));
    }
    let actual_branch =
        git(&workspace.worktree, ["branch", "--show-current"], true)?.stdout_trimmed();
    if actual_branch != workspace.branch {
        return Err(DriverError::new(format!(
            "worktree is on branch {actual_branch:?}, expected {:?}",
            workspace.branch
        )));
    }
    if !git(&workspace.worktree, ["status", "--porcelain"], true)?
        .stdout
        .is_empty()
    {
        return Err(DriverError::new(
            "agent left uncommitted changes; commit the task before publication",
        ));
    }
    let head = git(&workspace.worktree, ["rev-parse", "HEAD"], true)?.stdout_trimmed();
    if head == workspace.base_rev {
        return Err(DriverError::new(
            "agent produced no commit relative to the prepared base",
        ));
    }
    if !git(
        &workspace.worktree,
        ["merge-base", "--is-ancestor", &workspace.base_rev, &head],
        false,
    )?
    .success()
    {
        return Err(DriverError::new(
            "task head is not descended from its prepared base revision",
        ));
    }
    let campaign = required_string(data.get("campaign"), "campaign", None)?;
    let identity = campaign_identity(data, &campaign)?;
    let current_base = required_integration_revision(&config, &campaign, &identity)?;
    let domains_required = required_bool(data.get("domainsRequired"), "domainsRequired")?;
    let ownership = enforce_conflict_domains(
        &workspace.worktree,
        &workspace.base_rev,
        &head,
        data.get("task"),
        &workspace.task_id,
        domains_required,
        Some(&current_base),
    )?;
    let union_base = lane_union_base(
        &workspace.worktree,
        &workspace.base_rev,
        &head,
        Some(&current_base),
    )?;
    enforce_constraint_results(
        &workspace.worktree,
        &workspace.base_rev,
        &union_base,
        &head,
        &constraints,
    )?;
    publish_worker_findings(data)?;
    let task = data
        .get("task")
        .and_then(Json::as_object)
        .ok_or_else(|| DriverError::new("task must be an object"))?;
    let expected_branch = stable_publish_branch(
        &campaign,
        &identity,
        &workspace.task_id,
        task_revision(task)?.as_deref(),
    );
    if workspace.publish_branch != expected_branch {
        return Err(DriverError::new(format!(
            "workspace.publishBranch is {:?}, expected local stable branch {expected_branch:?}",
            workspace.publish_branch
        )));
    }
    git(
        &config.checkout,
        [
            "update-ref",
            &format!("refs/heads/{}", workspace.publish_branch),
            &head,
        ],
        true,
    )?;
    // The steward remains a bound catalog role for diagnosis. Publication
    // validates that binding but does not occupy it: the squash reads its
    // message from the lane tip at merge time.
    validate_steward_catalog_role(data.get("steward"))?;
    let narration = template_narration(task, "")?;
    let repository = repository_name(data.get("repository"), "repository")?;
    Ok(Json::object([
        ("taskId", Json::from(workspace.task_id)),
        ("branch", Json::from(workspace.publish_branch.clone())),
        ("head", Json::from(head)),
        (
            "pullRequest",
            Json::from(format!("local://{repository}/{}", workspace.publish_branch)),
        ),
        ("narration", narration),
        // Retained as an empty compatibility field for the deployed flow
        // schema. There is no narration request and therefore no transcript.
        ("narrationAttempts", Json::Array(Vec::new())),
        ("ownership", ownership.to_json()),
    ]))
}

fn integration_result(
    published: &Publication,
    base_rev: &str,
    head: &str,
    regate: bool,
    ownership: &Ownership,
) -> Json {
    Json::object([
        ("taskId", Json::from(published.task_id.clone())),
        ("baseRev", Json::from(base_rev.to_owned())),
        ("branch", Json::from(published.branch.clone())),
        ("head", Json::from(head.to_owned())),
        ("pullRequest", Json::from(published.pull_request.clone())),
        ("narration", published.narration.clone()),
        ("regate", Json::from(regate)),
        ("ownership", ownership.to_json()),
    ])
}

fn abandon_published_head(checkout: &Path, published: &Publication, context: &str) -> Result<()> {
    let abandoned = git(
        checkout,
        [
            "update-ref",
            "-d",
            &format!("refs/heads/{}", published.branch),
            &published.head,
        ],
        false,
    )?;
    if abandoned.success() {
        return Ok(());
    }
    Err(DriverError::new(format!(
        "{context}; exact published head {} could not be abandoned: {}",
        published.head,
        abandoned.detail()
    )))
}

fn action_rebase(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &[
            "campaign",
            "campaignIdentity",
            "repository",
            "repositoryConfig",
            "issue",
            "runId",
            "workspaceRoot",
            "task",
            "workspace",
            "publication",
            "constraints",
            "domainsRequired",
            "specRepository",
            "issueRepository",
        ],
        "rebase brief",
    )?;
    let config = repo_config(data.get("repositoryConfig"))?;
    let workspace = prepared_workspace(data.get("workspace"), "workspace")?;
    if !is_full_oid(&workspace.base_rev) {
        return Err(DriverError::new(
            "workspace.baseRev must be a full Git object ID",
        ));
    }
    if !workspace.worktree.is_absolute() {
        return Err(DriverError::new("workspace.worktreePath must be absolute"));
    }
    if !workspace.worktree.is_dir() {
        return Err(DriverError::new(
            "workspace.worktreePath must be an existing directory for rebase",
        ));
    }
    git(&workspace.worktree, ["rev-parse", "--git-dir"], true)?;

    let published = publication(data.get("publication"))?;
    let constraints = normalize_constraints(data.get("constraints"), "rebase constraints")?;
    let domains_required = required_bool(data.get("domainsRequired"), "domainsRequired")?;
    if published.task_id != workspace.task_id {
        return Err(DriverError::new(
            "publication.taskId does not match workspace.taskId",
        ));
    }
    if published.branch != workspace.publish_branch {
        return Err(DriverError::new(
            "publication.branch does not match workspace.publishBranch",
        ));
    }
    if published.ownership.base_rev != workspace.base_rev {
        return Err(DriverError::new(
            "publication.ownership.baseRev does not match workspace.baseRev",
        ));
    }
    if published.ownership.domains_required != domains_required {
        return Err(DriverError::new(
            "publication.ownership.domainsRequired does not match domainsRequired",
        ));
    }
    let local_head = git(&workspace.worktree, ["rev-parse", "HEAD"], true)?.stdout_trimmed();
    if local_head != published.head {
        return Err(DriverError::new("worktree head changed after publication"));
    }
    for constraint in &constraints {
        if constraint.base_rev != workspace.base_rev {
            return Err(DriverError::new(format!(
                "forbidPaths gate '{}' was witnessed against base {}, expected prepared base {}",
                constraint.gate_id, constraint.base_rev, workspace.base_rev
            )));
        }
    }

    let campaign = required_string(data.get("campaign"), "campaign", None)?;
    let identity = campaign_identity(data, &campaign)?;
    let base_rev = required_integration_revision(&config, &campaign, &identity)?;
    let branch_head = local_branch_oid(&config.checkout, &published.branch)?;
    if branch_head.as_deref() != Some(published.head.as_str()) {
        return Err(DriverError::new(
            "published branch moved before integration",
        ));
    }

    if git(
        &workspace.worktree,
        ["merge-base", "--is-ancestor", &base_rev, &local_head],
        false,
    )?
    .success()
    {
        let ownership = enforce_conflict_domains(
            &workspace.worktree,
            &base_rev,
            &local_head,
            data.get("task"),
            &published.task_id,
            domains_required,
            None,
        )?;
        return Ok(integration_result(
            &published,
            &base_rev,
            &local_head,
            false,
            &ownership,
        ));
    }

    let rebased = git(&workspace.worktree, ["rebase", &base_rev], false)?;
    if !rebased.success() {
        let detail = rebased.detail();
        let aborted = git(&workspace.worktree, ["rebase", "--abort"], false)?;
        let context = format!(
            "cannot rebase task onto current base {base_rev}: {detail}; rebase abort exited {}",
            aborted.status
        );
        abandon_published_head(&config.checkout, &published, &context)?;
        if !aborted.success() {
            return Err(DriverError::new(format!(
                "cannot rebase task onto current base {base_rev}: {detail}; published head {} was abandoned, but rebase abort failed: {}",
                published.head,
                aborted.detail()
            )));
        }
        return Err(DriverError::new(format!(
            "cannot rebase task onto current base {base_rev}: {detail}; rebase was aborted and exact published head {} was abandoned; a fresh pass can rebuild the task from current base",
            published.head
        )));
    }

    let rebased_head = git(&workspace.worktree, ["rev-parse", "HEAD"], true)?.stdout_trimmed();
    let checked: Result<Ownership> = (|| {
        let ownership = enforce_conflict_domains(
            &workspace.worktree,
            &base_rev,
            &rebased_head,
            data.get("task"),
            &published.task_id,
            domains_required,
            None,
        )?;
        for constraint in &constraints {
            evaluate_forbid_paths(
                &workspace.worktree,
                &base_rev,
                &rebased_head,
                &constraint.gate_id,
                &constraint.patterns,
            )?;
        }
        Ok(ownership)
    })();
    let ownership = match checked {
        Ok(ownership) => ownership,
        Err(error) => {
            let context = format!(
                "rebased task failed integration policy against current base {base_rev}: {error}"
            );
            abandon_published_head(&config.checkout, &published, &context)?;
            return Err(DriverError::new(format!(
                "rebased task failed integration policy against current base {base_rev}: {error}; exact published head {} was abandoned so a fresh pass can rebuild the task",
                published.head
            )));
        }
    };
    let advanced = git(
        &config.checkout,
        [
            "update-ref",
            &format!("refs/heads/{}", published.branch),
            &rebased_head,
            &published.head,
        ],
        false,
    )?;
    if !advanced.success() {
        return Err(DriverError::new(format!(
            "published branch moved while rebasing exact head {}: {}",
            published.head,
            advanced.detail()
        )));
    }
    Ok(integration_result(
        &published,
        &base_rev,
        &rebased_head,
        true,
        &ownership,
    ))
}

fn prune_empty_ancestors(path: &Path, stop: &Path) {
    let mut current = path.to_owned();
    while current != stop {
        if fs::remove_dir(&current).is_err() {
            return;
        }
        let Some(parent) = current.parent() else {
            return;
        };
        current = parent.to_owned();
    }
}

fn action_cleanup(brief: &Json) -> Result<Json> {
    let data = object_exact(
        brief,
        &[
            "campaign",
            "repository",
            "repositoryConfig",
            "runId",
            "taskId",
            "workspaceRoot",
            "workspace",
        ],
        "cleanup brief",
    )?;
    let campaign = required_string(data.get("campaign"), "campaign", None)?;
    if !is_component(&campaign) {
        return Err(DriverError::new("campaign is not a safe component"));
    }
    let repository = required_string(data.get("repository"), "repository", None)?;
    if !is_repository(&repository) {
        return Err(DriverError::new("repository must use owner/name form"));
    }
    let run_id = required_string(data.get("runId"), "runId", Some(512))?;
    let workspace_root = PathBuf::from(required_string(
        data.get("workspaceRoot"),
        "workspaceRoot",
        None,
    )?);
    if !workspace_root.is_absolute() {
        return Err(DriverError::new("workspaceRoot must be absolute"));
    }
    let config = repo_config(data.get("repositoryConfig"))?;
    let task_id = required_string(data.get("taskId"), "taskId", None)?;
    if !is_task_id(&task_id) {
        return Err(DriverError::new("taskId is not safe"));
    }
    let run_hash = &sha256::digest(run_id.as_bytes())[..12];
    let campaign_slug = safe_slug(&campaign, 24);
    let repository_name = repository
        .split_once('/')
        .map(|(_, name)| name)
        .expect("validated owner/name repository");
    let repository_slug = safe_slug(repository_name, 40);
    let lane_name = if task_id == "campaign-preflight" {
        "_campaign-preflight"
    } else {
        &task_id
    };
    let repository_root = resolve(&workspace_root.join(repository_slug))?;
    let run_root = repository_root.join(run_hash);
    let expected_worktree = run_root.join(lane_name);
    if is_symlink(&run_root) || is_symlink(&expected_worktree) {
        return Err(DriverError::new(
            "cleanup campaign lane must not traverse a symlink",
        ));
    }
    let expected_resolved = resolve(&expected_worktree)?;
    let expected_branch = format!("tally-work/{campaign_slug}-{run_hash}/{lane_name}");

    let workspace_value = data.get("workspace");
    let (worktree, branch) =
        if workspace_value.is_none() || matches!(workspace_value, Some(Json::Null)) {
            (expected_worktree.clone(), expected_branch.clone())
        } else {
            let workspace = prepared_workspace(workspace_value, "workspace")?;
            if workspace.task_id != task_id {
                return Err(DriverError::new(
                    "cleanup workspace.taskId does not match taskId",
                ));
            }
            // These are part of the prepared-workspace contract even though
            // cleanup needs only the lane coordinates.
            if workspace.base_rev.is_empty() {
                return Err(DriverError::new(
                    "workspace.baseRev must be a non-empty string without control characters",
                ));
            }
            if workspace.publish_branch.is_empty() {
                return Err(DriverError::new(
                    "workspace.publishBranch must be a non-empty string without control characters",
                ));
            }
            if !workspace.worktree.is_absolute() {
                return Err(DriverError::new("workspace.worktreePath must be absolute"));
            }
            (workspace.worktree, workspace.branch)
        };
    if resolve(&worktree)? != expected_resolved {
        return Err(DriverError::new(format!(
            "cleanup worktree {} is outside this campaign lane",
            worktree.display()
        )));
    }
    if branch != expected_branch {
        return Err(DriverError::new(format!(
            "cleanup branch {branch:?} does not match this campaign lane"
        )));
    }
    if expected_worktree.exists() || fs::symlink_metadata(&expected_worktree).is_ok() {
        if !expected_worktree.is_dir() {
            return Err(DriverError::new(
                "workspace.worktreePath exists but is not a directory",
            ));
        }
        if worktrees::is_registered(&config.checkout, &expected_worktree)? {
            git(&expected_worktree, ["rev-parse", "--git-dir"], true)?;
            let actual_branch =
                git(&expected_worktree, ["branch", "--show-current"], true)?.stdout_trimmed();
            if !actual_branch.is_empty() && actual_branch != branch {
                return Err(DriverError::new(format!(
                    "cleanup worktree is on branch {actual_branch:?}, expected {branch:?} or a detached prep head"
                )));
            }
        } else {
            fs::remove_dir_all(&expected_worktree).map_err(|error| {
                DriverError::new(format!(
                    "cannot remove partial campaign worktree {}: {error}",
                    expected_worktree.display()
                ))
            })?;
        }
    }
    worktrees::remove(&config.checkout, &expected_worktree, Some(&branch))?;
    if let Some(parent) = expected_worktree.parent() {
        prune_empty_ancestors(parent, &repository_root);
    }
    Ok(Json::object([
        ("taskId", Json::from(task_id)),
        ("cleaned", Json::from(true)),
    ]))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{
        action_retry, action_steer, action_worker_outcome, append_attempt_receipt,
        append_diagnosis_report, campaign_attempt_state, campaign_attempt_state_all,
        checkpoint_capture_note, classify_worker_outcome, closed_inline_code_spans,
        contains_bare_exclamation_mark, current_task_input_epochs, excerpt_text_window,
        fold_attempt_receipts, git_with_input, integration_branch, json, merge_local,
        publish_closing_summary, read_capture_tail, read_local_blob,
        replace_bare_exclamation_marks, validate_attempt_receipt, validate_outcome_first,
        DiagnosisVerdict, Json, RepoConfig, WorkerOutcome, CHECKPOINT_CAPTURE_MAX_BYTES,
        CHECKPOINT_STDERR_WINDOW_CHARS,
    };
    use tally_core::attempt_receipts::{
        AttemptReceiptAuthorityV1, ATTEMPT_RECEIPT_AUTHORITY_FILE, ATTEMPT_RECEIPT_MACHINE_ACTOR,
        ATTEMPT_RECEIPT_SCHEMA_VERSION, MAX_TASK_LIFETIME_ATTEMPTS,
    };
    use tally_core::campaign_folds::{campaign_digest, CampaignReconciliation, CampaignSource};
    use uuid::Uuid;

    fn summary_ref_test_git(directory: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn squash_subject_adoption(message: &str) -> (PathBuf, PathBuf, String) {
        let root =
            std::env::temp_dir().join(format!("tally-subject-adoption-test-{}", Uuid::new_v4()));
        let checkout = root.join("checkout");
        fs::create_dir_all(&checkout).unwrap();
        summary_ref_test_git(&checkout, &["init", "--quiet", "--initial-branch=main"]);
        summary_ref_test_git(&checkout, &["config", "user.name", "Subject Adoption Test"]);
        summary_ref_test_git(
            &checkout,
            &["config", "user.email", "subject-adoption@example.invalid"],
        );
        fs::write(checkout.join("README.md"), "fixture\n").unwrap();
        summary_ref_test_git(&checkout, &["add", "README.md"]);
        summary_ref_test_git(&checkout, &["commit", "--quiet", "-m", "fixture"]);
        let base = summary_ref_test_git(&checkout, &["rev-parse", "HEAD"]);
        let integration = integration_branch("fixture", "subject-adoption");
        summary_ref_test_git(&checkout, &["branch", &integration, &base]);
        summary_ref_test_git(&checkout, &["switch", "--quiet", "-c", "published"]);
        fs::write(checkout.join("task.txt"), "implemented\n").unwrap();
        summary_ref_test_git(&checkout, &["add", "task.txt"]);
        git_with_input(
            &checkout,
            [
                "-c",
                "user.name=Subject Adoption Test",
                "-c",
                "user.email=subject-adoption@example.invalid",
                "commit",
                "--quiet",
                "--file",
                "-",
            ],
            format!("{message}\n").as_bytes(),
            true,
        )
        .unwrap();
        let head = summary_ref_test_git(&checkout, &["rev-parse", "HEAD"]);
        let data = BTreeMap::from([
            ("campaign".to_owned(), Json::from("fixture")),
            (
                "campaignIdentity".to_owned(),
                Json::from("subject-adoption"),
            ),
            (
                "workspaceRoot".to_owned(),
                Json::from(root.join("workspaces").display().to_string()),
            ),
            (
                "task".to_owned(),
                Json::object([
                    ("id", Json::from("task-1")),
                    ("title", Json::from("Task one")),
                    ("revision", Json::from(format!("sha256:{}", "a".repeat(64)))),
                ]),
            ),
        ]);
        let published = BTreeMap::from([
            ("baseRev".to_owned(), Json::from(base)),
            ("branch".to_owned(), Json::from("published")),
            ("head".to_owned(), Json::from(head)),
        ]);
        let config = RepoConfig {
            checkout: checkout.clone(),
            base_branch: "main".to_owned(),
            remote: "origin".to_owned(),
        };
        let merged = merge_local(&data, &config, &published, "squash", None).unwrap();
        (root, checkout, merged)
    }

    #[test]
    fn subject_adoption_squash_preserves_a_valid_lane_tip() {
        let (root, checkout, merged) = squash_subject_adoption(
            "feat(driver): adopt the lane subject\n\nRecorded the lane-authored rationale.",
        );
        assert_eq!(
            summary_ref_test_git(&checkout, &["show", "-s", "--format=%s", &merged]),
            "feat(driver): adopt the lane subject"
        );
        let message = summary_ref_test_git(&checkout, &["show", "-s", "--format=%B", &merged]);
        assert!(message.contains("Recorded the lane-authored rationale."));
        assert!(message.contains("Adopted the lane-tip commit message without repair."));
        assert_eq!(
            summary_ref_test_git(&checkout, &["show", "-s", "--format=%P", &merged])
                .split_whitespace()
                .count(),
            1
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn subject_adoption_squash_falls_back_for_an_invalid_lane_tip() {
        let (root, checkout, merged) = squash_subject_adoption("implement task one");
        assert_eq!(
            summary_ref_test_git(&checkout, &["show", "-s", "--format=%s", &merged]),
            "task-1: Task one"
        );
        let message = summary_ref_test_git(&checkout, &["show", "-s", "--format=%B", &merged]);
        assert!(message.contains(
            "Rejected the lane-tip commit message and used the task-id template instead."
        ));
        assert!(message.contains("does not match"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn subject_adoption_squash_folds_a_101_column_body_and_adds_its_period() {
        let body = format!("Recorded {}", "x".repeat(92));
        assert_eq!(body.chars().count(), 101);
        let (root, checkout, merged) =
            squash_subject_adoption(&format!("fix(driver): format the lane body\n\n{body}"));
        assert_eq!(
            summary_ref_test_git(&checkout, &["show", "-s", "--format=%s", &merged]),
            "fix(driver): format the lane body"
        );
        let message = summary_ref_test_git(&checkout, &["show", "-s", "--format=%B", &merged]);
        assert!(message.lines().any(|line| line == "Recorded"));
        assert!(message
            .lines()
            .any(|line| line == format!("{}.", "x".repeat(92))));
        assert!(message.lines().all(|line| line.chars().count() <= 100));
        assert!(message.contains("after deterministic formatting"));
        fs::remove_dir_all(root).unwrap();
    }

    fn summary_digest(source_sha256: &str) -> tally_core::campaign_folds::CampaignDigest {
        campaign_digest(
            &CampaignReconciliation {
                campaign: "fixture".to_owned(),
                repository: "acme/spec".to_owned(),
                source: CampaignSource {
                    path: Some("campaign.json".to_owned()),
                    sha256: source_sha256.to_owned(),
                    revision: "c".repeat(40),
                    repository: None,
                    extra: serde_json::Map::new(),
                },
                base_revision: "c".repeat(40),
                tasks: Vec::new(),
                merged: Vec::new(),
                checkpoints: Vec::new(),
                remaining: Vec::new(),
                diagnoses: Vec::new(),
                retries: Vec::new(),
                deferrals: Vec::new(),
                blocked: Vec::new(),
                warnings: Vec::new(),
            },
            "complete",
        )
    }

    fn test_receipt_path(root: &Path) -> std::path::PathBuf {
        root.join("campaigns/attempt-receipts/fixture/attempt-receipts-v1.jsonl")
    }

    fn write_test_receipt_authority(root: &Path) {
        let path = test_receipt_path(root);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let authority = AttemptReceiptAuthorityV1::new(
            "fixture",
            "7",
            11,
            format!("sha256:{}", "a".repeat(64)),
        )
        .unwrap();
        fs::write(
            path.parent().unwrap().join(ATTEMPT_RECEIPT_AUTHORITY_FILE),
            serde_json::to_vec(&authority).unwrap(),
        )
        .unwrap();
    }

    fn test_receipt_source(root: &Path) -> Json {
        Json::object([
            ("schemaVersion", Json::Number("1".to_owned())),
            ("kind", Json::from("local-jsonl")),
            (
                "path",
                Json::from(test_receipt_path(root).display().to_string()),
            ),
        ])
    }

    fn test_receipt_source_with_epochs(root: &Path, epochs: &BTreeMap<String, String>) -> Json {
        let mut source = test_receipt_source(root)
            .as_object()
            .expect("test receipt source is an object")
            .clone();
        source.insert(
            "inputEpochs".to_owned(),
            Json::Object(
                epochs
                    .iter()
                    .map(|(task_id, epoch)| (task_id.clone(), Json::from(epoch.clone())))
                    .collect(),
            ),
        );
        Json::Object(source)
    }

    fn epoch_test_task(goal: &str) -> Json {
        Json::object([
            ("id", Json::from("task-1")),
            ("kind", Json::from("implementation")),
            ("title", Json::from("Task one")),
            ("goal", Json::from(goal)),
            ("deliveredBehaviors", Json::Array(vec![Json::from("Works")])),
            (
                "readFirst",
                Json::object([
                    ("specSections", Json::Array(vec![Json::from("Spec one")])),
                    ("styleReferences", Json::Array(Vec::new())),
                ]),
            ),
            (
                "acceptanceCriteria",
                Json::Array(vec![Json::object([
                    ("id", Json::from("green")),
                    ("description", Json::from("The check passes.")),
                    ("argv", Json::Array(vec![Json::from("true")])),
                ])]),
            ),
            ("dependencies", Json::Array(Vec::new())),
            (
                "conflictDomains",
                Json::Array(vec![Json::from("crates/example/src")]),
            ),
            ("revision", Json::from(format!("sha256:{}", "e".repeat(64)))),
        ])
    }

    fn epoch_test_gates() -> Json {
        Json::Array(vec![Json::object([
            ("kind", Json::from("command")),
            ("id", Json::from("tests")),
            ("preflightArgv", Json::Array(vec![Json::from("true")])),
            (
                "argv",
                Json::Array(vec![Json::from("cargo"), Json::from("test")]),
            ),
            ("runtimeMaxSec", Json::Number("900".to_owned())),
        ])])
    }

    fn epoch_test_steering(task_high_water: u64) -> Json {
        Json::object([
            ("campaign", Json::Number("0".to_owned())),
            (
                "tasks",
                Json::object([("task-1", Json::Number(task_high_water.to_string()))]),
            ),
        ])
    }

    fn diagnosis_payload(verdict: &str) -> Json {
        Json::object([
            ("kind", Json::from("diagnosis")),
            ("taskId", Json::from("task-1")),
            ("attempt", Json::Number("1".to_owned())),
            (
                "diagnosis",
                Json::from("Identified the task-scoped failure."),
            ),
            ("verdict", Json::from(verdict)),
            ("redaction", Json::from("conservative-v2")),
        ])
    }

    fn worker_outcome_test_brief(root: &Path, task_uuid: &str, message: Json) -> Json {
        write_test_receipt_authority(root);
        Json::object([
            ("campaign", Json::from("fixture")),
            (
                "issue",
                Json::object([
                    ("number", Json::from("7")),
                    ("url", Json::from("local://acme/spec/issues/7")),
                ]),
            ),
            (
                "task",
                Json::object([
                    ("id", Json::from("task-1")),
                    ("revision", Json::from(format!("sha256:{}", "a".repeat(64)))),
                ]),
            ),
            ("taskUuid", Json::from(task_uuid)),
            ("message", message),
            ("attemptReceipts", test_receipt_source(root)),
        ])
    }

    fn diagnosis_test_checkout(root: &Path) -> std::path::PathBuf {
        let checkout = root.join("checkout");
        fs::create_dir_all(&checkout).unwrap();
        summary_ref_test_git(&checkout, &["init", "--quiet", "--initial-branch=main"]);
        checkout
    }

    fn diagnosis_test_brief(
        root: &Path,
        checkout: &Path,
        task_kind: &str,
        verdict: &str,
        proposal: Option<Json>,
    ) -> Json {
        write_test_receipt_authority(root);
        let mut brief = BTreeMap::from([
            ("campaign".to_owned(), Json::from("fixture")),
            ("repository".to_owned(), Json::from("acme/spec")),
            (
                "repositoryConfig".to_owned(),
                Json::object([
                    ("checkout", Json::from(checkout.display().to_string())),
                    ("baseBranch", Json::from("main")),
                    ("remote", Json::from("origin")),
                    ("forge", Json::from("local")),
                ]),
            ),
            (
                "issue".to_owned(),
                Json::object([
                    ("number", Json::from("7")),
                    ("url", Json::from("local://acme/spec/issues/7")),
                ]),
            ),
            ("taskId".to_owned(), Json::from("task-1")),
            ("taskKind".to_owned(), Json::from(task_kind)),
            ("stage".to_owned(), Json::from("agent")),
            ("attempt".to_owned(), Json::Number("1".to_owned())),
            (
                "diagnosis".to_owned(),
                Json::from("Identified the bounded failure and its remedy."),
            ),
            ("verdict".to_owned(), Json::from(verdict)),
            ("attemptReceipts".to_owned(), test_receipt_source(root)),
        ]);
        if let Some(proposal) = proposal {
            brief.insert("proposal".to_owned(), proposal);
        }
        Json::Object(brief)
    }

    fn actionable_diagnosis_proposal() -> Json {
        Json::object([
            ("kind", Json::from("amendment-task")),
            (
                "paths",
                Json::Array(vec![Json::from("test/final-bar/cases/driver.py")]),
            ),
            (
                "goal",
                Json::from("Synchronize the final-bar assertion with the shipped driver."),
            ),
            (
                "acceptanceCriteria",
                Json::Array(vec![Json::object([
                    ("id", Json::from("final-bar-sync")),
                    (
                        "description",
                        Json::from("The synchronized final-bar case passes."),
                    ),
                    (
                        "argv",
                        Json::Array(vec![
                            Json::from("python3"),
                            Json::from("test/final-bar/cases/driver.py"),
                        ]),
                    ),
                ])]),
            ),
            (
                "dependencies",
                Json::Array(vec![Json::from("driver-foundation")]),
            ),
        ])
    }

    #[test]
    fn every_machine_attempt_receipt_kind_is_authority_stamped() {
        let temporary =
            std::env::temp_dir().join(format!("tally-receipt-stamp-test-{}", Uuid::new_v4()));
        write_test_receipt_authority(&temporary);
        let source = test_receipt_source(&temporary);
        let source = Some(&source);
        let payloads = [
            Json::object([
                ("kind", Json::from("diagnosis")),
                ("taskId", Json::from("task-1")),
                ("attempt", Json::Number("1".to_owned())),
                ("diagnosis", Json::from("Identified the failed check.")),
                ("redaction", Json::from("conservative-v2")),
            ]),
            Json::object([
                ("kind", Json::from("retry")),
                ("taskId", Json::from("task-1")),
                ("attempt", Json::Number("1".to_owned())),
                ("reason", Json::from("The merge worker exited transiently.")),
                ("redaction", Json::from("conservative-v2")),
            ]),
            Json::object([
                ("kind", Json::from("worker-outcome")),
                ("taskId", Json::from("task-2")),
                (
                    "taskRevision",
                    Json::from(format!("sha256:{}", "b".repeat(64))),
                ),
                (
                    "taskUuid",
                    Json::from("00000000-0000-4000-8000-000000000703"),
                ),
                ("outcome", Json::from("impossible")),
                ("paths", Json::Null),
                ("reason", Json::from("The required input does not exist.")),
            ]),
            Json::object([
                ("kind", Json::from("escalation")),
                ("body", Json::from("The campaign frontier is quiescent.")),
            ]),
        ];
        for payload in payloads {
            assert!(
                append_attempt_receipt(source, "fixture", "7", payload)
                    .unwrap()
                    .0
            );
        }

        let log = fs::read_to_string(test_receipt_path(&temporary)).unwrap();
        let records = log
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 4);
        assert_eq!(
            records
                .iter()
                .map(|record| record["kind"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["diagnosis", "retry", "worker-outcome", "escalation"]
        );
        for record in records {
            assert_eq!(
                record["schemaVersion"].as_u64(),
                Some(ATTEMPT_RECEIPT_SCHEMA_VERSION)
            );
            assert_eq!(record["armSerial"].as_u64(), Some(11));
            assert_eq!(
                record["worklistSha256"].as_str(),
                Some(format!("sha256:{}", "a".repeat(64)).as_str())
            );
            assert_eq!(
                record["actor"].as_str(),
                Some(ATTEMPT_RECEIPT_MACHINE_ACTOR)
            );
            assert!(
                chrono::DateTime::parse_from_rfc3339(record["writtenAt"].as_str().unwrap()).is_ok()
            );
        }
        fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn a_new_receipt_refuses_to_write_without_receipt_authority() {
        let temporary =
            std::env::temp_dir().join(format!("tally-receipt-refusal-test-{}", Uuid::new_v4()));
        let source = test_receipt_source(&temporary);
        let error = append_attempt_receipt(
            Some(&source),
            "fixture",
            "7",
            Json::object([
                ("kind", Json::from("escalation")),
                ("body", Json::from("The frontier is quiescent.")),
            ]),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("without authority"), "{error}");
        fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn legacy_receipt_fixture_decodes_and_reconciles() {
        let line =
            include_str!("../../../test/fixtures/spec-build/epsilon-attempt-receipt-v1.jsonl")
                .trim_end();
        let receipt = crate::json::parse(line).unwrap();
        let event = validate_attempt_receipt(
            &receipt,
            Path::new("epsilon-attempt-receipt-v1.jsonl"),
            1,
            "epsilon",
            "1",
        )
        .unwrap();
        let state = fold_attempt_receipts(
            vec![event],
            &BTreeMap::from([("chapter-gate".to_owned(), None)]),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(state.diagnoses.len(), 1);
        assert_eq!(state.diagnoses[0].task_id, "chapter-gate");
        assert_eq!(state.diagnoses[0].attempt, 1);
        assert!(state.warnings.is_empty());
    }

    #[test]
    fn epoch_refresh_reopens_an_amended_task_without_a_pardon() {
        let temporary =
            std::env::temp_dir().join(format!("tally-epoch-refresh-test-{}", Uuid::new_v4()));
        write_test_receipt_authority(&temporary);
        let gates = epoch_test_gates();
        let steering = epoch_test_steering(0);
        let original_tasks = vec![epoch_test_task("Implement the original behavior.")];
        let original_epochs =
            current_task_input_epochs(&original_tasks, Some(&gates), Some(&steering), None)
                .unwrap();
        let unrelated_steering = Json::object([
            ("campaign", Json::Number("0".to_owned())),
            (
                "tasks",
                Json::object([("retired-task", Json::Number("8".to_owned()))]),
            ),
        ]);
        assert_eq!(
            current_task_input_epochs(
                &original_tasks,
                Some(&gates),
                Some(&unrelated_steering),
                None,
            )
            .unwrap(),
            original_epochs,
            "steering for another task cannot spend this task's budget"
        );
        assert_ne!(
            current_task_input_epochs(
                &original_tasks,
                Some(&gates),
                Some(&epoch_test_steering(8)),
                None,
            )
            .unwrap(),
            original_epochs,
            "task-addressed steering is new attempt input"
        );
        let original_source = test_receipt_source_with_epochs(&temporary, &original_epochs);
        assert!(
            append_attempt_receipt(
                Some(&original_source),
                "fixture",
                "7",
                diagnosis_payload("blocked"),
            )
            .unwrap()
            .0
        );
        let revisions = BTreeMap::from([(
            "task-1".to_owned(),
            Some(format!("sha256:{}", "e".repeat(64))),
        )]);
        let spent = campaign_attempt_state(
            Some(&original_source),
            "fixture",
            "7",
            &revisions,
            &original_epochs,
        )
        .unwrap();
        assert!(spent.diagnoses[0].blocks_task());

        // Re-admission changed the task's own authored bytes. The old receipt
        // remains immutable but is no longer part of the active budget.
        let amended_tasks = vec![epoch_test_task(
            "Implement the original behavior and the amended edge case.",
        )];
        let amended_epochs =
            current_task_input_epochs(&amended_tasks, Some(&gates), Some(&steering), None).unwrap();
        assert_ne!(original_epochs, amended_epochs);
        let amended_source = test_receipt_source_with_epochs(&temporary, &amended_epochs);
        let refreshed = campaign_attempt_state(
            Some(&amended_source),
            "fixture",
            "7",
            &revisions,
            &amended_epochs,
        )
        .unwrap();
        assert!(
            refreshed.diagnoses.is_empty(),
            "the amended task is dispatchable with a fresh active budget"
        );
        assert_eq!(refreshed.lifetime_attempts.get("task-1"), Some(&1));

        assert!(
            append_attempt_receipt(
                Some(&amended_source),
                "fixture",
                "7",
                diagnosis_payload("retry"),
            )
            .unwrap()
            .0,
            "attempt one is available again in the derived epoch"
        );
        let log = fs::read_to_string(test_receipt_path(&temporary)).unwrap();
        let records = log
            .lines()
            .map(|line| crate::json::parse(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|record| {
            record
                .as_object()
                .and_then(|record| record.get("kind"))
                .and_then(Json::as_str)
                != Some("pardon")
        }));
        assert_eq!(
            records[0]
                .as_object()
                .and_then(|record| record.get("inputEpoch"))
                .and_then(Json::as_str),
            original_epochs.get("task-1").map(String::as_str)
        );
        assert_eq!(
            records[1]
                .as_object()
                .and_then(|record| record.get("inputEpoch"))
                .and_then(Json::as_str),
            amended_epochs.get("task-1").map(String::as_str)
        );
        fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn epoch_lifetime_backstop_latches_and_reports_after_ten_attempts() {
        let temporary =
            std::env::temp_dir().join(format!("tally-epoch-lifetime-test-{}", Uuid::new_v4()));
        write_test_receipt_authority(&temporary);
        let mut current_source = Json::Null;
        for ordinal in 1..=MAX_TASK_LIFETIME_ATTEMPTS {
            let epochs = BTreeMap::from([("task-1".to_owned(), format!("sha256:{ordinal:064x}"))]);
            current_source = test_receipt_source_with_epochs(&temporary, &epochs);
            assert!(
                append_attempt_receipt(
                    Some(&current_source),
                    "fixture",
                    "7",
                    diagnosis_payload("retry"),
                )
                .unwrap()
                .0
            );
        }
        assert!(
            append_attempt_receipt(
                Some(&current_source),
                "fixture",
                "7",
                Json::object([
                    ("kind", Json::from("escalation")),
                    (
                        "body",
                        Json::from("The lifetime attempt latch requires human attention."),
                    ),
                ]),
            )
            .unwrap()
            .0
        );
        let state = campaign_attempt_state_all(Some(&current_source), "fixture", "7").unwrap();
        assert_eq!(
            state.lifetime_attempts.get("task-1"),
            Some(&MAX_TASK_LIFETIME_ATTEMPTS)
        );
        assert!(state.lifetime_exhausted.contains("task-1"));
        assert!(state.escalation.is_some());
        assert!(state
            .warnings
            .iter()
            .any(|warning| warning.contains("latched for human attention")));

        let next_epochs = BTreeMap::from([(
            "task-1".to_owned(),
            format!("sha256:{:064x}", MAX_TASK_LIFETIME_ATTEMPTS + 1),
        )]);
        let next_source = test_receipt_source_with_epochs(&temporary, &next_epochs);
        let error = append_attempt_receipt(
            Some(&next_source),
            "fixture",
            "7",
            diagnosis_payload("retry"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("hard lifetime limit"), "{error}");
        let log = fs::read_to_string(test_receipt_path(&temporary)).unwrap();
        assert_eq!(log.lines().count(), MAX_TASK_LIFETIME_ATTEMPTS + 1);
        assert!(!log.lines().any(|line| line.contains("\"kind\":\"pardon\"")));
        fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn needs_authority_and_impossible_classify_distinctly_without_reclassifying_no_envelope() {
        let requested_paths = vec![
            Json::from(".github/workflows/release.yml"),
            Json::from("test/fleet-gate.sh"),
        ];
        let needs_authority = Json::object([
            ("outcome", Json::from("needs-authority")),
            ("paths", Json::Array(requested_paths.clone())),
        ]);
        let impossible = Json::object([
            ("outcome", Json::from("impossible")),
            (
                "reason",
                Json::from("The required upstream proof does not exist."),
            ),
        ]);
        assert_eq!(
            classify_worker_outcome(Some(&needs_authority)).unwrap(),
            Some(WorkerOutcome::NeedsAuthority {
                paths: requested_paths
                    .iter()
                    .map(|path| path.as_str().unwrap().to_owned())
                    .collect(),
            })
        );
        assert!(matches!(
            classify_worker_outcome(Some(&impossible)).unwrap(),
            Some(WorkerOutcome::Impossible { .. })
        ));
        assert_eq!(
            classify_worker_outcome(Some(&Json::from("ordinary final message"))).unwrap(),
            None,
            "a final message without an envelope stays on the existing agent signal path"
        );

        let temporary =
            std::env::temp_dir().join(format!("tally-worker-outcome-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&temporary).unwrap();
        let first_uuid = "00000000-0000-4000-8000-000000000701";
        let first = action_worker_outcome(&worker_outcome_test_brief(
            &temporary,
            first_uuid,
            needs_authority.clone(),
        ))
        .unwrap();
        let first = first.as_object().unwrap();
        assert_eq!(
            first.get("outcome").and_then(Json::as_str),
            Some("needs-authority")
        );
        assert_eq!(first.get("attemptCost").and_then(Json::as_u64), Some(0));
        assert_eq!(first.get("recorded").and_then(Json::as_bool), Some(true));
        assert_eq!(
            first.get("paths").unwrap().stringify(),
            Json::Array(requested_paths).stringify()
        );

        let repeated = action_worker_outcome(&worker_outcome_test_brief(
            &temporary,
            first_uuid,
            needs_authority,
        ))
        .unwrap();
        assert_eq!(
            repeated
                .as_object()
                .and_then(|result| result.get("recorded"))
                .and_then(Json::as_bool),
            Some(false)
        );

        let second = action_worker_outcome(&worker_outcome_test_brief(
            &temporary,
            "00000000-0000-4000-8000-000000000702",
            impossible,
        ))
        .unwrap();
        assert_eq!(
            second
                .as_object()
                .and_then(|result| result.get("outcome"))
                .and_then(Json::as_str),
            Some("impossible")
        );
        assert_eq!(
            second
                .as_object()
                .and_then(|result| result.get("attemptCost"))
                .and_then(Json::as_u64),
            Some(0)
        );

        let revisions = BTreeMap::from([(
            "task-1".to_owned(),
            Some(format!("sha256:{}", "a".repeat(64))),
        )]);
        let state = campaign_attempt_state(
            worker_outcome_test_brief(&temporary, first_uuid, Json::Null)
                .as_object()
                .and_then(|brief| brief.get("attemptReceipts")),
            "fixture",
            "7",
            &revisions,
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(state.diagnoses.is_empty());
        assert!(state.retries.is_empty());
        assert_eq!(state.outcomes.len(), 2);
        let log = fs::read_to_string(
            temporary.join("campaigns/attempt-receipts/fixture/attempt-receipts-v1.jsonl"),
        )
        .unwrap();
        assert_eq!(
            log.lines().count(),
            2,
            "the repeated worker UUID is idempotent"
        );
        assert!(log.contains(".github/workflows/release.yml"));
        assert!(log.contains("test/fleet-gate.sh"));
        fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn blocked_verdict_stops_at_attempt_one() {
        let temporary =
            std::env::temp_dir().join(format!("tally-blocked-verdict-test-{}", Uuid::new_v4()));
        let checkout = diagnosis_test_checkout(&temporary);
        let brief = diagnosis_test_brief(&temporary, &checkout, "implementation", "blocked", None);

        let result = action_steer(&brief).unwrap();
        let result = result.as_object().unwrap();
        assert_eq!(
            result.get("verdict").and_then(Json::as_str),
            Some("blocked")
        );
        assert_eq!(result.get("blocked").and_then(Json::as_bool), Some(true));
        assert!(matches!(result.get("retry"), Some(Json::Null)));

        let state = campaign_attempt_state_all(
            brief
                .as_object()
                .and_then(|brief| brief.get("attemptReceipts")),
            "fixture",
            "7",
        )
        .unwrap();
        assert_eq!(state.diagnoses.len(), 1);
        assert!(state.diagnoses[0].blocks_task());
        assert_eq!(
            state.diagnoses[0].effective_verdict(),
            DiagnosisVerdict::Blocked
        );
        assert!(state.retries.is_empty());
        fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn retry_verdict_exposes_attempt_two_with_the_diagnosis_as_steering() {
        let temporary =
            std::env::temp_dir().join(format!("tally-retry-verdict-test-{}", Uuid::new_v4()));
        let checkout = diagnosis_test_checkout(&temporary);
        let brief = diagnosis_test_brief(&temporary, &checkout, "implementation", "retry", None);

        let result = action_steer(&brief).unwrap();
        let result = result.as_object().unwrap();
        assert_eq!(result.get("verdict").and_then(Json::as_str), Some("retry"));
        assert_eq!(result.get("blocked").and_then(Json::as_bool), Some(false));

        let state = campaign_attempt_state_all(
            brief
                .as_object()
                .and_then(|brief| brief.get("attemptReceipts")),
            "fixture",
            "7",
        )
        .unwrap();
        assert_eq!(state.diagnoses.len(), 1);
        assert!(!state.diagnoses[0].blocks_task());
        let steering = state.diagnoses[0].diagnosis_json();
        assert_eq!(
            steering
                .as_object()
                .and_then(|diagnosis| diagnosis.get("verdict"))
                .and_then(Json::as_str),
            Some("retry")
        );
        assert_eq!(
            steering
                .as_object()
                .and_then(|diagnosis| diagnosis.get("diagnosis"))
                .and_then(Json::as_str),
            Some("Identified the bounded failure and its remedy.")
        );
        fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn checkpoint_retry_verdict_is_forced_blocked_without_a_retry() {
        let temporary =
            std::env::temp_dir().join(format!("tally-checkpoint-verdict-test-{}", Uuid::new_v4()));
        let checkout = diagnosis_test_checkout(&temporary);
        let brief = diagnosis_test_brief(&temporary, &checkout, "checkpoint", "retry", None);

        let result = action_steer(&brief).unwrap();
        let result = result.as_object().unwrap();
        assert_eq!(
            result.get("verdict").and_then(Json::as_str),
            Some("blocked")
        );
        assert_eq!(result.get("blocked").and_then(Json::as_bool), Some(true));
        assert!(matches!(result.get("retry"), Some(Json::Null)));
        let state = campaign_attempt_state_all(
            brief
                .as_object()
                .and_then(|brief| brief.get("attemptReceipts")),
            "fixture",
            "7",
        )
        .unwrap();
        assert_eq!(state.diagnoses.len(), 1);
        assert!(state.diagnoses[0].blocks_task());
        assert!(state.retries.is_empty());
        fs::remove_dir_all(temporary).unwrap();
    }

    /// A quota-terminated claude-code capture, verbatim from the corpus the
    /// adapter presets are declared against.
    const QUOTA_WALLED_CAPTURE: &str =
        include_str!("../../../test/fixtures/traces/claude-code-quota.jsonl");

    /// One lane capture written where a retained archive would have it,
    /// beside the adapter declarations the catalog states for it. The
    /// declarations travel in the brief because the driver holds no adapter
    /// catalog of its own.
    fn lane_capture_fixture(root: &Path, adapter: &str, stream: &str) -> Json {
        let catalog = json::parse(include_str!(
            "../../../test/fixtures/spec-build/adapter-terminal-catalog.json"
        ))
        .expect("the committed catalog snapshot parses");
        let declarations = catalog
            .as_object()
            .and_then(|catalog| catalog.get(adapter))
            .unwrap_or_else(|| panic!("the catalog snapshot declares {adapter:?}"))
            .clone();
        let captures = root.join("captures");
        fs::create_dir_all(&captures).unwrap();
        let path = captures.join(format!("{adapter}.out"));
        fs::write(&path, stream).unwrap();
        Json::object([
            ("adapter", Json::from(adapter)),
            ("adapterConfig", declarations),
            ("stdoutPath", Json::from(path.display().to_string())),
            ("failureCode", Json::from("result-projection-timeout")),
        ])
    }

    fn lane_retry_brief(root: &Path, checkout: &Path, lane_capture: Option<Json>) -> Json {
        write_test_receipt_authority(root);
        let mut brief = BTreeMap::from([
            ("campaign".to_owned(), Json::from("fixture")),
            ("repository".to_owned(), Json::from("acme/spec")),
            (
                "repositoryConfig".to_owned(),
                Json::object([
                    ("checkout", Json::from(checkout.display().to_string())),
                    ("baseBranch", Json::from("main")),
                    ("remote", Json::from("origin")),
                    ("forge", Json::from("local")),
                ]),
            ),
            (
                "issue".to_owned(),
                Json::object([
                    ("number", Json::from("7")),
                    ("url", Json::from("local://acme/spec/issues/7")),
                ]),
            ),
            ("taskId".to_owned(), Json::from("task-1")),
            ("stage".to_owned(), Json::from("agent")),
            (
                "detail".to_owned(),
                Json::from("The agent stage faulted and its stderr was empty."),
            ),
            ("attemptReceipts".to_owned(), test_receipt_source(root)),
        ]);
        if let Some(lane_capture) = lane_capture {
            brief.insert("laneCapture".to_owned(), lane_capture);
        }
        Json::Object(brief)
    }

    /// The ladder stop, at the layer that spends the budget. Before this, an
    /// empty-stderr agent fault was a machinery fault by default and bought a
    /// retry against a wall that had already named the hour it lifts.
    #[test]
    fn an_adapter_terminal_lane_capture_buys_no_machinery_retry() {
        let temporary =
            std::env::temp_dir().join(format!("tally-adapter-terminal-retry-{}", Uuid::new_v4()));
        let checkout = diagnosis_test_checkout(&temporary);
        let brief = lane_retry_brief(
            &temporary,
            &checkout,
            Some(lane_capture_fixture(
                &temporary,
                "claude-code",
                QUOTA_WALLED_CAPTURE,
            )),
        );

        let result = action_retry(&brief).unwrap();
        let result = result.as_object().unwrap();
        assert_eq!(result.get("posted").and_then(Json::as_bool), Some(false));
        assert_eq!(result.get("exhausted").and_then(Json::as_bool), Some(true));
        assert!(matches!(result.get("comment"), Some(Json::Null)));

        let state = campaign_attempt_state_all(
            brief
                .as_object()
                .and_then(|brief| brief.get("attemptReceipts")),
            "fixture",
            "7",
        )
        .unwrap();
        assert!(
            state.retries.is_empty(),
            "a stated wall charged the machinery budget"
        );
        fs::remove_dir_all(temporary).unwrap();
    }

    /// The complement, and the reason the stop is scraped rather than
    /// assumed: a lane whose stream states nothing terminal is priced exactly
    /// as it was before.
    #[test]
    fn a_lane_capture_with_no_adapter_terminal_event_still_buys_the_machinery_retry() {
        let temporary =
            std::env::temp_dir().join(format!("tally-adapter-terminal-none-{}", Uuid::new_v4()));
        let checkout = diagnosis_test_checkout(&temporary);
        let healthy = concat!(
            r#"{"type":"system","subtype":"init","session_id":"claude-session-healthy"}"#,
            "\n",
            r#"{"type":"result","subtype":"success","result":"Done."}"#,
            "\n",
        );
        let brief = lane_retry_brief(
            &temporary,
            &checkout,
            Some(lane_capture_fixture(&temporary, "claude-code", healthy)),
        );

        let result = action_retry(&brief).unwrap();
        let result = result.as_object().unwrap();
        assert_eq!(result.get("posted").and_then(Json::as_bool), Some(true));
        assert_eq!(result.get("exhausted").and_then(Json::as_bool), Some(false));

        let state = campaign_attempt_state_all(
            brief
                .as_object()
                .and_then(|brief| brief.get("attemptReceipts")),
            "fixture",
            "7",
        )
        .unwrap();
        assert_eq!(state.retries.len(), 1);
        fs::remove_dir_all(temporary).unwrap();
    }

    /// A capture the retention horizon reaped states nothing, and asking
    /// again must not turn a settled pass into a driver failure. The
    /// classification degrades to the reading this pass would have reached
    /// without the capture at all.
    #[test]
    fn a_reaped_lane_capture_claims_no_adapter_terminal_outcome() {
        let temporary =
            std::env::temp_dir().join(format!("tally-adapter-terminal-reaped-{}", Uuid::new_v4()));
        let checkout = diagnosis_test_checkout(&temporary);
        let lane_capture = lane_capture_fixture(&temporary, "claude-code", QUOTA_WALLED_CAPTURE);
        let path = lane_capture
            .as_object()
            .and_then(|capture| capture.get("stdoutPath"))
            .and_then(Json::as_str)
            .expect("the fixture names its capture")
            .to_owned();
        fs::remove_file(&path).unwrap();
        let brief = lane_retry_brief(&temporary, &checkout, Some(lane_capture));

        let result = action_retry(&brief).unwrap();
        let result = result.as_object().unwrap();
        assert_eq!(result.get("posted").and_then(Json::as_bool), Some(true));
        fs::remove_dir_all(temporary).unwrap();
    }

    /// The other half of the ladder: the in-epoch retry the judge proposed.
    /// The adapter stated the outcome, so the verdict is settled before the
    /// judgment is consulted -- and the receipt carries the sentence the wall
    /// wrote plus what the lane spent reaching it.
    #[test]
    fn an_adapter_terminal_lane_capture_blocks_the_verdict_the_judge_proposed() {
        let temporary =
            std::env::temp_dir().join(format!("tally-adapter-terminal-steer-{}", Uuid::new_v4()));
        let checkout = diagnosis_test_checkout(&temporary);
        let mut brief =
            diagnosis_test_brief(&temporary, &checkout, "implementation", "retry", None);
        brief.as_object_mut().unwrap().insert(
            "laneCapture".to_owned(),
            lane_capture_fixture(&temporary, "claude-code", QUOTA_WALLED_CAPTURE),
        );

        let result = action_steer(&brief).unwrap();
        let result = result.as_object().unwrap();
        assert_eq!(
            result.get("verdict").and_then(Json::as_str),
            Some("blocked")
        );
        assert_eq!(result.get("blocked").and_then(Json::as_bool), Some(true));
        assert!(matches!(result.get("retry"), Some(Json::Null)));

        let state = campaign_attempt_state_all(
            brief
                .as_object()
                .and_then(|brief| brief.get("attemptReceipts")),
            "fixture",
            "7",
        )
        .unwrap();
        assert_eq!(state.diagnoses.len(), 1);
        assert!(state.diagnoses[0].blocks_task());
        assert!(state.retries.is_empty());
        let recorded = state.diagnoses[0].diagnosis_json();
        let recorded = recorded
            .as_object()
            .and_then(|diagnosis| diagnosis.get("diagnosis"))
            .and_then(Json::as_str)
            .expect("the durable receipt carries its diagnosis");
        assert!(recorded.contains("Adapter-terminal outcome"), "{recorded}");
        assert!(recorded.contains("usage limit"), "{recorded}");
        assert!(recorded.contains("Token spend scraped"), "{recorded}");
        fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn transient_verdict_spends_the_machinery_retry_budget() {
        let temporary =
            std::env::temp_dir().join(format!("tally-transient-verdict-test-{}", Uuid::new_v4()));
        let checkout = diagnosis_test_checkout(&temporary);
        let brief =
            diagnosis_test_brief(&temporary, &checkout, "implementation", "transient", None);

        let result = action_steer(&brief).unwrap();
        let result = result.as_object().unwrap();
        assert_eq!(
            result.get("verdict").and_then(Json::as_str),
            Some("transient")
        );
        assert_eq!(result.get("kind").and_then(Json::as_str), Some("retry"));
        assert_eq!(result.get("blocked").and_then(Json::as_bool), Some(false));
        assert_eq!(result.get("posted").and_then(Json::as_bool), Some(true));
        assert!(matches!(result.get("retry"), Some(Json::Null)));
        let state = campaign_attempt_state_all(
            brief
                .as_object()
                .and_then(|brief| brief.get("attemptReceipts")),
            "fixture",
            "7",
        )
        .unwrap();
        assert!(state.diagnoses.is_empty());
        assert_eq!(state.retries.len(), 1);

        let second = action_steer(&brief).unwrap();
        let second = second.as_object().unwrap();
        assert_eq!(second.get("kind").and_then(Json::as_str), Some("retry"));
        assert_eq!(second.get("attempt").and_then(Json::as_u64), Some(2));
        assert_eq!(second.get("exhausted").and_then(Json::as_bool), Some(true));
        let state = campaign_attempt_state_all(
            brief
                .as_object()
                .and_then(|brief| brief.get("attemptReceipts")),
            "fixture",
            "7",
        )
        .unwrap();
        assert!(state.diagnoses.is_empty());
        assert_eq!(state.retries.len(), 2);

        let exhausted = action_steer(&brief).unwrap();
        let exhausted = exhausted.as_object().unwrap();
        assert_eq!(
            exhausted.get("verdict").and_then(Json::as_str),
            Some("blocked")
        );
        assert_eq!(exhausted.get("blocked").and_then(Json::as_bool), Some(true));
        let state = campaign_attempt_state_all(
            brief
                .as_object()
                .and_then(|brief| brief.get("attemptReceipts")),
            "fixture",
            "7",
        )
        .unwrap();
        assert_eq!(state.diagnoses.len(), 1);
        assert_eq!(state.retries.len(), 2);
        fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn blocked_proposal_is_rendered_into_the_escalation_report_as_a_ready_diff() {
        let temporary =
            std::env::temp_dir().join(format!("tally-proposal-report-test-{}", Uuid::new_v4()));
        let checkout = diagnosis_test_checkout(&temporary);
        let proposal = actionable_diagnosis_proposal();
        let brief = diagnosis_test_brief(
            &temporary,
            &checkout,
            "implementation",
            "blocked",
            Some(proposal),
        );
        action_steer(&brief).unwrap();
        let state = campaign_attempt_state_all(
            brief
                .as_object()
                .and_then(|brief| brief.get("attemptReceipts")),
            "fixture",
            "7",
        )
        .unwrap();
        let diagnoses = state
            .diagnoses
            .iter()
            .map(super::VisibleAttempt::diagnosis_json)
            .collect::<Vec<_>>();
        let mut lines = Vec::new();
        let mut public_values = Vec::new();
        append_diagnosis_report(&mut lines, &diagnoses, &mut public_values).unwrap();
        let report = lines.join("\n");

        assert!(report.contains("Prepared worklist proposals (ready diffs):"));
        assert!(report.contains("+   \"kind\": \"amendment-task\""));
        assert!(report.contains("test/final-bar/cases/driver.py"));
        assert!(report.contains("Synchronize the final-bar assertion"));
        assert!(report.contains("final-bar-sync"));
        assert!(report.contains("driver-foundation"));
        fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn summary_refs_for_two_admitted_digests_do_not_collide() {
        let temporary =
            std::env::temp_dir().join(format!("tally-summary-ref-test-{}", Uuid::new_v4()));
        let checkout = temporary.join("checkout");
        let remote = temporary.join("remote.git");
        fs::create_dir_all(&checkout).unwrap();
        summary_ref_test_git(
            &temporary,
            &[
                "init",
                "--bare",
                "--quiet",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        summary_ref_test_git(&checkout, &["init", "--quiet", "--initial-branch=main"]);
        summary_ref_test_git(&checkout, &["config", "user.name", "Summary Ref Test"]);
        summary_ref_test_git(
            &checkout,
            &["config", "user.email", "summary-ref@example.invalid"],
        );
        fs::write(checkout.join("README.md"), "fixture\n").unwrap();
        summary_ref_test_git(&checkout, &["add", "README.md"]);
        summary_ref_test_git(&checkout, &["commit", "--quiet", "-m", "fixture"]);
        summary_ref_test_git(
            &checkout,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        summary_ref_test_git(&checkout, &["push", "--quiet", "origin", "main"]);

        let config = RepoConfig {
            checkout: checkout.clone(),
            base_branch: "main".to_owned(),
            remote: "origin".to_owned(),
        };
        let first_digest = format!("sha256:{}", "a".repeat(64));
        let second_digest = format!("sha256:{}", "b".repeat(64));
        let first = publish_closing_summary(
            "acme/spec",
            &config,
            "fixture",
            "7",
            &summary_digest(&first_digest),
        )
        .unwrap();
        let second = publish_closing_summary(
            "acme/spec",
            &config,
            "fixture",
            "7",
            &summary_digest(&second_digest),
        )
        .unwrap();

        assert_ne!(first, second);
        assert!(first.ends_with(&format!("{}/summary/complete", "a".repeat(64))));
        assert!(second.ends_with(&format!("{}/summary/complete", "b".repeat(64))));
        let references = BTreeSet::from([
            first.split("acme/spec/").nth(1).unwrap(),
            second.split("acme/spec/").nth(1).unwrap(),
        ]);
        assert_eq!(references.len(), 2);
        for reference in references {
            assert_eq!(
                read_local_blob(&config, reference)
                    .unwrap()
                    .as_object()
                    .and_then(|summary| summary.get("kind"))
                    .and_then(Json::as_str),
                Some("closing-summary")
            );
        }

        fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn stage_zero_negation_ignores_only_closed_equal_width_code_fences() {
        assert!(!contains_bare_exclamation_mark(
            "Recorded `! grep -n stale test/x.py`."
        ));
        assert!(!contains_bare_exclamation_mark(
            "Recorded ``! grep `literal` test/x.py``."
        ));
        assert!(contains_bare_exclamation_mark(
            "Recorded `! grep -n stale test/x.py."
        ));
        assert!(contains_bare_exclamation_mark(
            "Recorded ``! grep -n stale test/x.py`."
        ));
        assert!(contains_bare_exclamation_mark(
            "Recorded `! grep` and finished!"
        ));
    }

    #[test]
    fn stage_zero_negation_replacement_preserves_closed_code_verbatim() {
        assert_eq!(
            replace_bare_exclamation_marks("Failed! Reproduced with `! false`!", "."),
            "Failed. Reproduced with `! false`."
        );
        assert_eq!(
            replace_bare_exclamation_marks("Failed with ``! echo `x`!``!", "."),
            "Failed with ``! echo `x`!``."
        );
    }

    #[test]
    fn stage_zero_negation_spans_use_utf8_safe_byte_offsets() {
        let text = "Préparé `! false` puis vérifié.";
        assert_eq!(closed_inline_code_spans(text), vec![(10, 19)]);
        assert!(!contains_bare_exclamation_mark(text));
    }

    #[test]
    fn outcome_first_validation_applies_the_same_negation_rule() {
        assert_eq!(
            validate_outcome_first(
                "Reproduced with `! grep -n stale test/x.py`.",
                1_000,
                "proposal body"
            ),
            None
        );
        assert_eq!(
            validate_outcome_first("Finished the work!", 1_000, "proposal body"),
            Some("proposal body contains an exclamation mark".to_owned())
        );
    }

    fn excerpt_test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("tally-{label}-test-{}", Uuid::new_v4()))
    }

    #[test]
    fn checkpoint_capture_tail_excerpt_lifts_a_causal_error_buried_above_the_window() {
        // A gate-run capture whose causal error sits far above the tail: the
        // stored checkpoint stream must carry the lifted error block, not
        // only whatever noise came last (vestige-sweep V-5).
        let root = excerpt_test_root("checkpoint-tail-excerpt");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("capture.adapter.err");
        let mut capture = Vec::new();
        capture.extend_from_slice(b"note: gate setup noise line\n");
        capture.extend_from_slice(b"error: the buried causal fixture line (checkpoint excerpt)\n");
        capture.extend_from_slice(b"\n");
        let block_end = capture.len();
        while capture.len() < block_end + 4 * CHECKPOINT_CAPTURE_MAX_BYTES {
            capture.extend_from_slice(b"warning: intervening gate noise that came later\n");
        }
        capture.extend_from_slice(b"noise: gate process exited with status 1\n");
        fs::write(&path, &capture).unwrap();

        let result = read_capture_tail(&path);
        fs::remove_dir_all(&root).unwrap();
        let (text, truncated) = result.unwrap();
        assert!(truncated);
        assert!(text.len() <= CHECKPOINT_CAPTURE_MAX_BYTES);
        assert!(text.contains("error: the buried causal fixture line (checkpoint excerpt)"));
        assert!(text.ends_with("noise: gate process exited with status 1\n"));
    }

    #[test]
    fn checkpoint_note_excerpt_surfaces_a_causal_error_five_kb_above_the_tail() {
        // The stored checkpoint stderr carries its causal error exactly five
        // kilobytes above the tail: inside the stored stream's derived bound,
        // but far outside any plain tail window of the note, so only the
        // note's error-aware excerpt surfaces it (vestige-sweep V-5).
        let root = excerpt_test_root("checkpoint-note-excerpt");
        fs::create_dir_all(&root).unwrap();
        let capture_path = root.join("checkpoint.json");

        let causal = "error: the buried causal fixture line (checkpoint note)";
        let tail_line = "noise: gate process exited with status 1";
        let mut stderr = String::new();
        while stderr.len() < 3 * 1024 {
            stderr.push_str("note: gate setup noise line\n");
        }
        stderr.push_str(causal);
        stderr.push('\n');
        let error_end = stderr.len();
        let chatter = "noise: trailing gate chatter after the causal error\n";
        let mut remaining = 5_000 - tail_line.len() - 1;
        while remaining >= chatter.len() {
            stderr.push_str(chatter);
            remaining -= chatter.len();
        }
        if remaining > 0 {
            stderr.push_str(&"x".repeat(remaining - 1));
            stderr.push('\n');
        }
        stderr.push_str(tail_line);
        stderr.push('\n');
        assert_eq!(
            stderr.len() - error_end,
            5_000,
            "the causal error sits exactly five kilobytes above the tail"
        );
        assert!(stderr.len() <= CHECKPOINT_CAPTURE_MAX_BYTES);

        let capture_json = format!(
            concat!(
                "{{\"schemaVersion\":1,\"campaign\":\"camp\",\"issueNumber\":7,",
                "\"taskId\":\"task-a\",",
                "\"taskUuid\":\"00000000-0000-4000-8000-000000000001\",",
                "\"verdict\":\"failed\",\"exitCode\":1,\"stdout\":\"\",",
                "\"stdoutTruncated\":false,\"stderr\":{stderr_json},",
                "\"stderrTruncated\":true}}"
            ),
            stderr_json = serde_json::to_string(&stderr).unwrap()
        );
        fs::write(&capture_path, capture_json).unwrap();

        let brief = Json::object([
            ("path", Json::from(capture_path.display().to_string())),
            ("postFailureEvidence", Json::from(true)),
            ("postFailureStderr", Json::from(true)),
        ]);
        let note = checkpoint_capture_note(Some(&brief), "camp", "task-a");
        fs::remove_dir_all(&root).unwrap();
        let note = note.unwrap();
        assert!(
            note.contains(causal),
            "the buried causal error must reach the checkpoint note"
        );
        assert!(note.contains(tail_line));
    }

    #[test]
    fn text_window_excerpt_is_bounded_and_error_aware() {
        // The in-memory window the checkpoint note applies keeps the same
        // shape as the executor's file-backed excerpt: bounded, truncated,
        // first-error-block-plus-tail.
        let mut text = String::new();
        text.push_str("note: setup noise\n");
        text.push_str("fatal: the buried cause (text window)\n");
        text.push('\n');
        let block_end = text.len();
        while text.len() < block_end + 3 * CHECKPOINT_STDERR_WINDOW_CHARS {
            text.push_str("warning: noise that came later\n");
        }
        text.push_str("noise: exit chatter\n");
        let excerpt = excerpt_text_window(&text, CHECKPOINT_STDERR_WINDOW_CHARS);
        assert!(excerpt.truncated);
        assert!(excerpt.text.len() <= CHECKPOINT_STDERR_WINDOW_CHARS);
        assert!(excerpt
            .text
            .contains("fatal: the buried cause (text window)"));
        assert!(excerpt.text.ends_with("noise: exit chatter\n"));

        let small = excerpt_text_window("short capture\n", CHECKPOINT_STDERR_WINDOW_CHARS);
        assert!(!small.truncated);
        assert_eq!(small.text, "short capture\n");
    }
}
