use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use tally_core::campaign_folds::{
    campaign_digest as fold_campaign_digest, render_campaign_summary, stable_publish_branch,
    CampaignReconciliation,
};
use uuid::Uuid;

use crate::error::{DriverError, Result};
use crate::git::{git, git_with_input};
use crate::json::{self, Json};
use crate::path::{is_symlink, resolve};
use crate::sha256;
use crate::worktrees::{self, Identity};

const NARRATION_SUBJECT_MAX: usize = 200;
const NARRATION_BODY_MAX: usize = 4_000;
const MAX_CAMPAIGN_TASKS: usize = 128;
const MAX_DIFF_CHARS: usize = 128 * 1024;
const MAX_ATTEMPT_RECEIPTS_LOG_BYTES: u64 = 128 * 1024 * 1024;
const MAX_DIAGNOSIS_CHARS: usize = 12_000;
const MAX_RETRY_CHARS: usize = 2_000;
const ATTEMPT_RECEIPTS_FILE: &str = "attempt-receipts-v1.jsonl";
const BRIEF_SENTINEL: &str = "Read the file whose path is in the TALLY_BRIEF environment variable and execute the mission it contains. That brief is your complete instruction set.";
const LIVE_JOB_STATES: [&str; 3] = ["paused", "queued", "running"];

#[cfg(target_os = "linux")]
const O_CLOEXEC: i32 = 0o2000000;
#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0o400000;
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

#[derive(Clone, Debug)]
struct Constraint {
    gate_id: String,
    patterns: Vec<String>,
    base_rev: String,
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
        "prep" => action_prep(brief),
        "rebase" => action_rebase(brief),
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
    for (name, default) in [
        ("approvalPolicy", Some("never")),
        ("sandboxPolicy", Some("danger-full-access")),
        ("diagnosisSandboxPolicy", Some("read-only")),
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

#[derive(Clone, Debug)]
struct VisibleAttempt {
    task_id: String,
    attempt: u64,
    comment: String,
    text: String,
}

impl VisibleAttempt {
    fn diagnosis_json(&self) -> Json {
        Json::object([
            ("taskId", Json::from(self.task_id.clone())),
            ("attempt", Json::Number(self.attempt.to_string())),
            ("comment", Json::from(self.comment.clone())),
            ("diagnosis", Json::from(self.text.clone())),
        ])
    }

    fn retry_json(&self) -> Json {
        Json::object([
            ("taskId", Json::from(self.task_id.clone())),
            ("attempt", Json::Number(self.attempt.to_string())),
            ("comment", Json::from(self.comment.clone())),
            ("reason", Json::from(self.text.clone())),
        ])
    }
}

#[derive(Clone, Debug)]
enum AttemptEvent {
    Diagnosis(VisibleAttempt),
    Retry(VisibleAttempt),
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
    escalation: Option<String>,
    warnings: Vec<String>,
}

struct AttemptKinds<'a, T> {
    diagnoses: &'a mut BTreeMap<String, Vec<T>>,
    retries: &'a mut BTreeMap<String, Vec<T>>,
}

fn attempt_receipts_path(value: Option<&Json>, campaign: &str) -> Result<PathBuf> {
    let source = value.ok_or_else(|| DriverError::new("attemptReceipts must be an object"))?;
    let source = object_exact(
        source,
        &["schemaVersion", "kind", "path"],
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

fn attempt_receipt_url(campaign: &str, sequence: u64) -> String {
    format!("local://campaign/{campaign}/attempt-receipts/{sequence}")
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
    let common = [
        "schemaVersion",
        "sequence",
        "kind",
        "campaign",
        "issueNumber",
    ];
    let mut fields = common.to_vec();
    match kind {
        "diagnosis" => fields.extend(["taskId", "attempt", "diagnosis", "redaction"]),
        "retry" => fields.extend(["taskId", "attempt", "reason", "redaction"]),
        "escalation" => fields.push("body"),
        "pardon" => fields.extend(["tasks", "reason", "actor", "nonce"]),
        _ => {
            return Err(DriverError::new(format!(
                "{context} has unknown kind {kind:?}"
            )))
        }
    }
    let record = object_exact(candidate, &fields, &context)?;
    if record.get("schemaVersion").and_then(Json::as_u64) != Some(1)
        || record.get("sequence").and_then(Json::as_u64) != Some(expected_sequence)
        || record.get("campaign").and_then(Json::as_str) != Some(campaign)
        || record.get("issueNumber").and_then(Json::as_str) != Some(issue_number)
    {
        return Err(DriverError::new(format!(
            "{context} has invalid identity or sequence"
        )));
    }
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
            };
            Ok(if kind == "diagnosis" {
                AttemptEvent::Diagnosis(visible)
            } else {
                AttemptEvent::Retry(visible)
            })
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
    task_ids: &BTreeSet<String>,
) -> Result<AttemptState> {
    let mut visible_diagnoses = BTreeMap::<String, Vec<VisibleAttempt>>::new();
    let mut visible_retries = BTreeMap::<String, Vec<VisibleAttempt>>::new();
    let mut diagnosis_counters = BTreeMap::<String, Vec<u64>>::new();
    let mut retry_counters = BTreeMap::<String, Vec<u64>>::new();
    let mut escalations = Vec::<(String, BTreeSet<String>, BTreeSet<String>)>::new();
    let mut warnings = Vec::new();

    fn keep_attempt(
        receipt: VisibleAttempt,
        diagnosis: bool,
        task_ids: &BTreeSet<String>,
        counters: AttemptKinds<'_, u64>,
        visible: AttemptKinds<'_, VisibleAttempt>,
        warnings: &mut Vec<String>,
    ) {
        let kind = if diagnosis { "diagnosis" } else { "retry" };
        if !task_ids.contains(&receipt.task_id) {
            warnings.push(format!(
                "dropped machine {kind} for '{}': the worklist no longer names that task",
                receipt.task_id
            ));
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
                task_ids,
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
                task_ids,
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
            AttemptEvent::Escalation { comment } => {
                let contributors = diagnosis_counters
                    .iter()
                    .filter_map(|(task_id, attempts)| {
                        let attempts: BTreeSet<_> = attempts.iter().copied().collect();
                        (attempts == BTreeSet::from([1, 2])).then(|| task_id.clone())
                    })
                    .collect();
                escalations.push((comment, contributors, BTreeSet::new()));
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
    Ok(AttemptState {
        diagnoses,
        retries,
        escalation: escalations.into_iter().next().map(|row| row.0),
        warnings,
    })
}

fn campaign_attempt_state(
    source: Option<&Json>,
    campaign: &str,
    issue_number: &str,
    task_ids: &BTreeSet<String>,
) -> Result<AttemptState> {
    fold_attempt_receipts(
        read_attempt_receipts(source, campaign, issue_number)?,
        task_ids,
    )
}

fn task_revision(task: &BTreeMap<String, Json>) -> Result<Option<String>> {
    let Some(value) = task.get("revision") else {
        return Ok(None);
    };
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
    let reference = format!(
        "{}/summary/{}",
        local_state_prefix(campaign, issue_number),
        digest.outcome
    );
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
    let task_ids: BTreeSet<_> = tasks
        .iter()
        .map(task_id)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut attempts = campaign_attempt_state(
        data.get("attemptReceipts"),
        &campaign,
        &issue_number,
        &task_ids,
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
    let remaining: Vec<_> = tasks
        .iter()
        .filter(|task| task_id(task).is_ok_and(|id| !completed_ids.contains(id)))
        .cloned()
        .collect();
    let direct_blocked: BTreeSet<_> = attempts
        .diagnoses
        .iter()
        .filter(|diagnosis| diagnosis.attempt == 2 && !completed_ids.contains(&diagnosis.task_id))
        .map(|diagnosis| diagnosis.task_id.clone())
        .collect();
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
        ("merged".to_owned(), Json::Array(merged)),
        ("checkpoints".to_owned(), Json::Array(checkpoints)),
        ("remaining".to_owned(), Json::Array(remaining_ids)),
        ("frontier".to_owned(), Json::Array(frontier)),
        ("diagnoses".to_owned(), Json::Array(diagnoses)),
        ("retries".to_owned(), Json::Array(retries)),
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
        Some(NARRATION_SUBJECT_MAX),
    )?;
    let body = match object.get("body") {
        None => String::new(),
        Some(Json::String(body))
            if body.chars().count() <= NARRATION_BODY_MAX && !body.contains('\0') =>
        {
            body.clone()
        }
        _ => {
            return Err(DriverError::new(format!(
                "{context}.body must be a string of at most {NARRATION_BODY_MAX} characters"
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
        object
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
            base_rev: full_oid(object.get("baseRev"), &format!("{item_context}.baseRev"))?,
        });
        let _ = head;
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
            "forbidPaths gate {gate_id:?} rejected {} path(s) touched in lane history (a later removal does not clear this; the path must never appear in any lane commit): {preview}",
            violations.len()
        )));
    }
    Ok(changed.len())
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
                "forbidPaths gate {:?} was witnessed against base {}, expected prepared base {}",
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
