use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{DriverError, Result};
use crate::git::git;
use crate::json::{self, Json};
use crate::path::{is_symlink, resolve};
use crate::sha256;
use crate::worktrees::{self, Identity};

const NARRATION_SUBJECT_MAX: usize = 200;
const NARRATION_BODY_MAX: usize = 4_000;

#[derive(Clone, Debug)]
struct RepoConfig {
    checkout: PathBuf,
    base_branch: String,
    remote: String,
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

fn stable_publish_branch(
    campaign: &str,
    identity: &str,
    task_id: &str,
    revision: Option<&str>,
) -> String {
    let suffix = revision.map_or_else(String::new, |revision| {
        format!(
            "-{}",
            revision
                .strip_prefix("sha256:")
                .unwrap_or(revision)
                .chars()
                .take(16)
                .collect::<String>()
        )
    });
    format!(
        "{}/{task_id}{suffix}",
        campaign_branch_prefix(campaign, identity)
    )
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
